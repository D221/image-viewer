//! Browsing benchmark: what actually happens when you flip through a folder.
//!
//! This measures the things the re-introduced optimisations are supposed to
//! change, rather than decode throughput in isolation:
//!
//!   * **time to first pixels** per picture — the number a user feels
//!   * **wasted decode work** — worker time spent on pictures nobody wanted by
//!     the time the result arrived, which is what cancellation exists to remove
//!   * **cache hit rate on arrival** — how often the picture you stepped to was
//!     already decoded, which is what preloading exists to improve
//!   * **peak bytes held** — what the cache policy is supposed to bound
//!   * **jobs dropped by rebuild** — how far ahead of the decoder you are
//!
//! It drives the *real* `Loader` and the *real* `plan_jobs` from the library, so
//! it reflects the real scheduling decisions rather than a model of them. The
//! preview cache is a stand-in that tracks the same quantities the real one does
//! (bytes per entry, an LRU clock, a count cap); it becomes the real
//! `PreviewCache` when that moves into the library.
//!
//! Not criterion, on purpose: one session is seconds long, and what matters is
//! the distribution and the counters rather than a nanosecond figure. Run it
//! explicitly:
//!
//! ```sh
//! cargo test --release --test bench_browse -- --ignored --nocapture
//! ```
//!
//! Images are generated once into a temp dir and reused, so runs are comparable
//! to each other and to other commits. The size is chosen so one preview decode
//! takes tens of milliseconds: slow enough that the queue is under real
//! pressure, fast enough to flip through.
//!
//! Two accounting bugs were fixed here while measuring something else, and both
//! made the totals *under*-report, which is the direction that hides an effect:
//!
//!   * the final drain checked `outstanding() == 0` *after* taking delivery, so
//!     the last worker's result was thrown away. A result is sent before the
//!     worker frees its busy slot, so "idle" can be true while a message is still
//!     in flight. `outstanding()` is now read first. This is the same trap the
//!     app's queue has, and the same fix.
//!   * that drain never added to `decode_ms`, so at the shortest holds — where
//!     essentially all the work finishes after the last step — the `decode s`
//!     column read 0.00. **It is larger now, and correct**; a `decode s` of 0
//!     against a non-zero time-to-first-pixels was never possible to be right.
//!
//! The EXIF arm (`exif_ab`) exists because the metadata read is not free and not
//! small: `color_info` calls `fs::read` on the **whole file** to find the ICC
//! profile, and `nom_exif` then seeks out the APP1 segment. That is the app's
//! `Kind::Exif` job, modelled here with the same two calls rather than as a
//! no-op — the question "does not probing metadata while the panel is hidden
//! make navigation faster" has no answer if the benchmark pretends the job
//! costs nothing. It is the one job per navigation that exists only to fill a
//! panel, and it is queued *ahead* of every prefetch.
//!
//! The thumbnail arm (`thumb_cache_ab`) is the third kind of measurement here: not
//! a scheduling question but a **"can this be made faster at all"** one. A 95 MP
//! baseline JPEG has no cheaper full-image read — every pixel needs every DCT
//! coefficient, so the entropy decode is the floor for any conformant decoder — and
//! the grid's answer to that is a cache rather than a faster decode. Which means
//! the number that matters is the **cold** one, and printing only the warm number
//! would be how the limit of the change got forgotten.

use image::ImageEncoder as _;
use image_viewer::{color_info, load_image_preview_full, plan_jobs, ImgKey, Job, Kind, Loader, Plan};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const W: u32 = 3000;
const H: u32 = 2000;
const PREVIEW_DIM: u32 = 1600;
const RADIUS: usize = 3;
/// Preview entries held, mirroring `PREVIEW_CACHE_CAP` in the app.
const CAP: usize = 16;
/// Quality the generated fixtures are written at. `image`'s default JPEG
/// quality, spelled out because the EXIF arm's cost is a whole-file read and
/// therefore a function of how big the file is.
const QUALITY: u8 = 75;
/// Fixture for the EXIF arm: the same shape as the browse fixtures but written
/// at `image`'s maximum quality, which makes one file 7 MB instead of the 1.9 MB
/// a synthetic gradient compresses to. The real photographs measured below are
/// 17–27 MB, and the read is `fs::read`, so its cost is very nearly linear in
/// file size: 4.4 ms here against 7.5–12.3 ms on those. A preview decode is *not*
/// linear in file size, so this fixture still *under*-states the ratio on a real
/// photo — which is the safe direction for a change that is being justified, and
/// `real_photo_reads` prints the real figure rather than leaving it to be
/// extrapolated.
const EXIF_W: u32 = 3000;
const EXIF_H: u32 = 2000;
const EXIF_QUALITY: u8 = 100;

/// Build, or reuse, a folder of `n` distinct JPEGs.
///
/// Reused across runs on purpose: generating them each time would put tens of
/// seconds of JPEG encoding into every measurement and make two commits'
/// numbers hard to compare.
fn fixture_dir(n: usize) -> PathBuf {
    fixture_dir_at(n, W, H, QUALITY)
}

fn fixture_dir_at(n: usize, w: u32, h: u32, quality: u8) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iv-bench-browse-{w}x{h}q{quality}-{n}"));
    let last = dir.join(format!("img{:03}.jpg", n - 1));
    if last.exists() {
        return dir;
    }
    let _ = std::fs::create_dir_all(&dir);
    for i in 0..n {
        let p = dir.join(format!("img{i:03}.jpg"));
        if p.exists() {
            continue;
        }
        // Deterministic and distinct per file, and not trivially compressible:
        // a flat gradient decodes far too fast to put the queue under pressure.
        let mut img = image::RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let g = ((x * 7 + y * 3) % 256) as u8;
                let b = (((x / 16) as i32 + (y / 16) as i32 + (i as i32 * 5)) % 256) as u8;
                img.put_pixel(x, y, image::Rgb([(g / 2 + 60) as u8, g, b]));
            }
        }
        let mut out = std::io::Cursor::new(Vec::new());
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .expect("write fixture");
        std::fs::write(&p, out.into_inner()).expect("write fixture");
    }
    dir
}

/// Mean file size of a fixture folder, in MB. Reported rather than assumed: the
/// EXIF read is `fs::read`, so its cost is whatever this says it is.
fn mean_mb(dir: &std::path::Path) -> f64 {
    let v: Vec<u64> = files(dir)
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len()))
        .collect();
    if v.is_empty() {
        return 0.0;
    }
    v.iter().sum::<u64>() as f64 / v.len() as f64 / (1024.0 * 1024.0)
}

fn files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("fixture dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|s| s == "jpg"))
        .collect();
    v.sort();
    v
}

#[derive(Clone, Copy, Debug)]
struct Cached {
    bytes: usize,
    used: u64,
}

#[derive(Clone, Copy, Debug)]
struct Decoded {
    bytes: usize,
    below_target: bool,
}

/// What a worker hands back: the file, what the job was for, its wall time, and
/// what it produced. The kind travels with it because an EXIF read reports the
/// same *path* as the preview for that file and must not be filed as pixels.
type Delivery = (PathBuf, Kind, f64, Result<Decoded, String>);

struct Msg {
    path: PathBuf,
    kind: Kind,
    /// Worker wall time. Counted whether or not anyone still wanted the result.
    ms: f64,
    outcome: Result<Decoded, String>,
}

#[derive(Default, Debug)]
struct Session {
    /// Time from stepping to a picture until its pixels were in hand.
    ttfp_ms: Vec<f64>,
    /// Worker time whose result nobody wanted by the time it arrived.
    wasted_ms: f64,
    /// Worker time on results that were wanted.
    useful_ms: f64,
    /// Decodes started, including retries and results nobody used.
    runs: usize,
    /// Steps that landed on an already-decoded picture.
    arrived_cached: usize,
    /// Steps that had to wait.
    arrived_decode: usize,
    peak_bytes: usize,
    dropped: u64,
    /// Total worker time, useful and wasted together.
    decode_ms: f64,
    /// Previews that came back below the requested density.
    below_target: usize,
    wall_ms: f64,
    /// Worker time on metadata reads (`Kind::Exif`), which produce no pixels.
    /// Kept out of `useful_ms`/`wasted_ms` on purpose: those two are about
    /// pictures, and an EXIF read cannot be "wasted" in the sense a decode is.
    exif_ms: f64,
    exif_jobs: usize,
    /// Per-read metadata times, for the cost figure.
    exif_each: Vec<f64>,
}

impl Session {
    fn ttfp_p(&self, q: f64) -> f64 {
        if self.ttfp_ms.is_empty() {
            return 0.0;
        }
        let mut v = self.ttfp_ms.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let i = ((v.len() - 1) as f64 * q).round() as usize;
        v[i]
    }
}

/// Flip through every picture in `dir`, pressing "next" every `hold_ms`.
///
/// `hold_ms` is the knob that matters: it is how far ahead of the decoder the
/// user is, and it decides whether prefetch helps, whether work is wasted, and
/// whether the cache is thrashing.
///
/// `exif` is the one thing being A/B'd: whether the current picture's metadata
/// read is part of the plan. In the app that is `Plan::wants_exif`, which the
/// fullscreen path now leaves false — the metadata panel is the only thing that
/// displays EXIF, and it is not drawn in fullscreen.
fn browse(dir: &std::path::Path, hold_ms: u64, exif: bool) -> Session {
    let files = files(dir);
    let n = files.len();
    let (tx, rx): (Sender<Msg>, _) = channel();

    let decode_ms = Arc::new(Mutex::new(Vec::<Delivery>::new()));
    let dm = decode_ms.clone();
    let loader = Loader::spawn(
        std::thread::available_parallelism()
            .map(|x| x.get())
            .unwrap_or(2)
            .clamp(2, 4),
        tx,
        move |job: &Job| {
            let t0 = Instant::now();
            let out: Result<Decoded, String> = match job.kind {
                Kind::Preview => load_image_preview_full(job.path(), job.target_dim)
                    .map(|r| Decoded {
                        bytes: r.preview.rgba.len()
                            + r.full.as_ref().map(|f| f.rgba.len()).unwrap_or(0),
                        below_target: r.below_target,
                    })
                    .map_err(|e| e.to_string()),
                // Exactly what `run_job` does for `Kind::Exif` in the app: the
                // colour profile, which reads the whole file, then the EXIF
                // block, which seeks. No pixels come back, hence `bytes: 0`.
                Kind::Exif => {
                    let _ = color_info(job.path()).panel_line();
                    let _ = nom_exif::read_exif(job.path());
                    Ok(Decoded {
                        bytes: 0,
                        below_target: false,
                    })
                }
                _ => Err("benchmark only decodes previews and reads exif".to_string()),
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let logged = out.clone();
            dm.lock().unwrap().push((
                job.path().to_path_buf(),
                job.kind,
                ms,
                logged,
            ));
            (
                Msg {
                    path: job.path().to_path_buf(),
                    kind: job.kind,
                    ms,
                    outcome: out,
                },
                true,
            )
        },
        |job: &Job, e: String| {
            (
                Msg {
                    path: job.path().to_path_buf(),
                    kind: job.kind,
                    ms: 0.0,
                    outcome: Err(e),
                },
                true,
            )
        },
        || {},
    );

    let mut s = Session::default();
    let mut cache: HashMap<PathBuf, Cached> = HashMap::new();
    let mut wanted: HashSet<PathBuf> = HashSet::new();
    let mut clock: u64 = 0;
    // When each visited picture was stepped to, for time-to-first-pixels.
    let mut pending_since: HashMap<PathBuf, Instant> = HashMap::new();
    let mut failed: HashSet<(ImgKey, Kind)> = HashSet::new();

    let mut index = 0usize;
    let t_start = Instant::now();
    for step in 0..n {
        // 1. Take delivery of anything that finished, the way a frame's `pump`
        //    does, before deciding what is wanted.
        for (path, kind, ms, outcome) in drain(&rx, &decode_ms) {
            if kind != Kind::Preview {
                // Metadata: worker time, no pixels, never a decode failure.
                s.exif_ms += ms;
                s.exif_each.push(ms);
                s.exif_jobs += 1;
                continue;
            }
            match outcome {
                Ok(d) => {
                    s.decode_ms += ms;
                    if d.below_target {
                        s.below_target += 1;
                    }
                    if wanted.contains(&path) {
                        s.useful_ms += ms;
                    } else {
                        // The result nobody asked for by the time it landed.
                        // Cancellation's entire reason to exist.
                        s.wasted_ms += ms;
                    }
                    cache.insert(
                        path.clone(),
                        Cached {
                            bytes: d.bytes,
                            used: clock,
                        },
                    );
                    if let Some(t) = pending_since.remove(&path) {
                        s.ttfp_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Err(e) => {
                    failed.insert((ImgKey::first(&path), Kind::Preview));
                    eprintln!("  decode failed: {e}");
                }
            }
        }
        s.peak_bytes = s
            .peak_bytes
            .max(cache.values().map(|c| c.bytes).sum::<usize>());
        evict(&mut cache, &files, index, n, &mut clock);

        // 2. Look at the picture: cached, or a wait?
        let cur = &files[index];
        if cache.contains_key(cur) {
            s.arrived_cached += 1;
            pending_since.remove(cur);
            // Pixels are already on screen, so this counts as zero. Recording
            // only the pictures that had to wait made the percentile meaningless
            // exactly when the cache was working: at a 96% hit rate the p50 was
            // being computed from a single sample.
            s.ttfp_ms.push(0.0);
        } else {
            s.arrived_decode += 1;
            pending_since.insert(cur.clone(), Instant::now());
        }

        // 3. Ask for what is wanted, exactly as `schedule` does.
        // The planner's caches are keyed by (file, page); this benchmark only
        // ever holds page 0, so every key is `ImgKey::first`.
        let cached_set: HashSet<ImgKey> = cache.keys().map(|p| ImgKey::first(p)).collect();
        let jobs = plan_jobs(&Plan {
            files: &files,
            index,
            current_page: 0,
            // No turn's second half: this benchmark measures single-image
            // browsing, which is what the planner planned before manga mode and
            // what `turn_partner` still answers for whenever the mode is off.
            partner: None,
            partner_needs_full: false,
            cached: &cached_set,
            failed: &failed,
            wants_exif: exif,
            wants_full: false,
            current_stale: false,
            target_dim: PREVIEW_DIM,
            radius: RADIUS,
        });
        wanted = jobs.iter().map(|j| j.path().to_path_buf()).collect();
        loader.replace(jobs);

        // Wait out the hold, but keep taking delivery while waiting — the app
        // pumps messages continuously, and only checking at step boundaries
        // would fold the hold time into every time-to-first-pixels figure.
        let hold = Duration::from_millis(hold_ms);
        let slice = Duration::from_millis(4);
        let mut waited = Duration::ZERO;
        while waited < hold {
            let chunk = slice.min(hold - waited);
            std::thread::sleep(chunk);
            waited += chunk;
            take_delivery(&mut s, &rx, &decode_ms, &mut cache, &mut pending_since, &mut failed, &wanted, &mut clock);
        }
        index = (index + 1) % n;
        let _ = step;
    }

    // Drain what is left, so the totals describe the whole session.
    //
    // `outstanding` is read *before* the drain, not after: a result is sent
    // before the worker frees its busy slot, so "queued + running is zero" can
    // be true while the last message is still in flight. Checking it the other
    // way round drops exactly the last worker's work — and the last worker is
    // the metadata read (lowest priority), which is the one thing this file now
    // has to account for honestly.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = loader.outstanding();
        for (path, kind, ms, outcome) in drain(&rx, &decode_ms) {
            if kind != Kind::Preview {
                s.exif_ms += ms;
                s.exif_each.push(ms);
                s.exif_jobs += 1;
                continue;
            }
            if let Ok(d) = outcome {
                s.decode_ms += ms;
                if wanted.contains(&path) {
                    s.useful_ms += ms;
                } else {
                    s.wasted_ms += ms;
                }
                cache.insert(
                    path.clone(),
                    Cached {
                        bytes: d.bytes,
                        used: clock,
                    },
                );
                if let Some(t) = pending_since.remove(&path) {
                    s.ttfp_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        if left == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the queue never drained: {left} outstanding"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    s.wall_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    s.dropped = image_viewer::perf::counter(image_viewer::perf::C_JOBS_DROPPED);
    s.runs = cache.len();
    s
}

fn drain(rx: &std::sync::mpsc::Receiver<Msg>, log: &Arc<Mutex<Vec<Delivery>>>) -> Vec<Delivery> {
    let _ = rx.try_iter().count();
    let mut out = std::mem::take(&mut *log.lock().unwrap());
    // Keep anything that arrived but was not yet taken.
    out.extend(rx.try_iter().map(|m| (m.path, m.kind, m.ms, m.outcome)));
    out.sort_by_key(|(p, _, _, _)| p.clone());
    out
}

/// Drop least-recently-painted previews once the count cap is hit, never
/// touching the current picture or its preload window — the policy the app uses.
fn evict(
    cache: &mut HashMap<PathBuf, Cached>,
    files: &[PathBuf],
    index: usize,
    n: usize,
    clock: &mut u64,
) {
    *clock += 1;
    while cache.len() > CAP {
        let protect: HashSet<&PathBuf> = (0..=2 * RADIUS)
            .map(|k| (index + n * 2 + k - RADIUS) % n)
            .filter_map(|i| files.get(i))
            .collect();
        let victim = cache
            .iter()
            .filter(|(p, _)| !protect.contains(*p))
            .min_by_key(|(_, c)| c.used)
            .map(|(p, _)| p.clone());
        match victim {
            Some(v) => {
                cache.remove(&v);
            }
            None => break,
        }
    }
}

/// Take delivery of everything that finished, the way a frame's `pump` does.
/// Called at step boundaries *and* throughout each hold, because the app's event
/// loop does not wait for the next step to process results.
#[allow(clippy::too_many_arguments)]
fn take_delivery(
    s: &mut Session,
    rx: &std::sync::mpsc::Receiver<Msg>,
    log: &Arc<Mutex<Vec<Delivery>>>,
    cache: &mut HashMap<PathBuf, Cached>,
    pending_since: &mut HashMap<PathBuf, Instant>,
    failed: &mut HashSet<(ImgKey, Kind)>,
    wanted: &HashSet<PathBuf>,
    clock: &mut u64,
) {
    for (path, kind, ms, outcome) in drain(rx, log) {
        *clock += 1;
        if kind != Kind::Preview {
            s.exif_ms += ms;
            s.exif_each.push(ms);
            s.exif_jobs += 1;
            continue;
        }
        match outcome {
            Ok(d) => {
                s.decode_ms += ms;
                if d.below_target {
                    s.below_target += 1;
                }
                if wanted.contains(&path) {
                    s.useful_ms += ms;
                } else {
                    // Nobody wanted this by the time it landed. This is the
                    // number cancellation exists to reduce.
                    s.wasted_ms += ms;
                }
                cache.insert(
                    path.clone(),
                    Cached {
                        bytes: d.bytes,
                        used: *clock,
                    },
                );
                if let Some(t) = pending_since.remove(&path) {
                    s.ttfp_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
            Err(e) => {
                failed.insert((ImgKey::first(&path), Kind::Preview));
                eprintln!("  decode failed: {e}");
            }
        }
    }
}
/// Median of a per-run metric. One pass is not a baseline: the cache state at/// the end depends on exactly when decodes landed, and a single run moved the
/// hold=60 cache-hit figure between 0% and 50%.
fn med(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// What one metadata read costs on real photographs, if there are any.
///
/// The generated fixture is a gradient at a chosen quality; a real photo is a
/// real size, and the read is `fs::read` — so the cost is set by how many
/// megabytes the camera wrote, not by anything this code decides. `test-images`
/// is gitignored, so this is skipped rather than failed on a clean clone: the
/// same exemption the criterion targets take, and for the same reason (a
/// measurement of real files has no synthetic substitute).
fn real_photo_reads() {
    let dir = std::path::Path::new("test-images");
    let Ok(rd) = std::fs::read_dir(dir) else {
        eprintln!("  (no test-images/ — skipping the real-file read cost)");
        return;
    };
    let mut photos: Vec<(u64, std::path::PathBuf)> = rd
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|s| s.eq_ignore_ascii_case("jpg") || s.eq_ignore_ascii_case("jpeg"))
        })
        .filter_map(|e| e.metadata().ok().map(|m| (m.len(), e.path())))
        .collect();
    photos.sort_by_key(|(len, _)| std::cmp::Reverse(*len));
    photos.truncate(5);
    if photos.is_empty() {
        eprintln!("  (no JPEGs in test-images/ — skipping the real-file read cost)");
        return;
    }
    for (len, p) in &photos {
        // Warm it first: the app's exif job runs *after* the preview decode has
        // already put the file in the page cache, so a cold read would measure
        // something that does not happen on a navigation.
        let _ = std::fs::read(p).is_ok();
        let mut each = Vec::new();
        for _ in 0..5 {
            let t0 = Instant::now();
            let _ = color_info(p).panel_line();
            let _ = nom_exif::read_exif(p);
            each.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        eprintln!(
            "  real {:<28.28} {:>6.1} MB -> {:>6.2} ms per read",
            p.file_name().unwrap_or_default().to_string_lossy(),
            *len as f64 / (1024.0 * 1024.0),
            med(each)
        );
    }
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn browse_baseline() {
    let n: usize = std::env::var("IV_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let runs: usize = std::env::var("IV_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let dir = fixture_dir(n);
    let files = files(&dir);
    eprintln!(
        "\n  browse benchmark: {} pictures at {W}x{H}, {n} images, median of {runs}",
        files.len()
    );
    eprintln!(
        "  {:>6}  {:>8}  {:>9}  {:>9}  {:>8}  {:>8}  {:>10}  {:>9}  {:>9}",
        "hold", "wall s", "ttfp p50", "ttfp p95", "wasted", "peak MB", "cache hit", "decode s", "below tgt"
    );
    eprintln!("  {}", "-".repeat(94));

    // Warm-up so the numbers are not dominated by first-touch page faults and
    // lazily faulted JPEG tables.
    let _ = browse(&dir, 60, false);

    for hold in [0u64, 30, 60, 150, 400] {
        let mut s = Vec::new();
        for _ in 0..runs {
            s.push(browse(&dir, hold, false));
        }
        let hits: Vec<f64> = s
            .iter()
            .map(|x| x.arrived_cached as f64 / n as f64 * 100.0)
            .collect();
        let wasted: Vec<f64> = s
            .iter()
            .map(|x| {
                let total = x.useful_ms + x.wasted_ms;
                if total > 0.0 {
                    x.wasted_ms / total * 100.0
                } else {
                    0.0
                }
            })
            .collect();
        eprintln!(
            "  {:>6}  {:>8.2}  {:>9.0}  {:>9.0}  {:>7.0}%  {:>8.0}  {:>9.0}%  {:>9.2}  {:>9}",
            hold,
            med(s.iter().map(|x| x.wall_ms / 1000.0).collect()),
            med(s.iter().map(|x| x.ttfp_p(0.50)).collect()),
            med(s.iter().map(|x| x.ttfp_p(0.95)).collect()),
            med(wasted),
            med(s.iter().map(|x| x.peak_bytes as f64 / (1024.0 * 1024.0)).collect()),
            med(hits),
            med(s.iter().map(|x| x.decode_ms / 1000.0).collect()),
            med(s.iter().map(|x| x.below_target as f64).collect()),
        );
    }
    eprintln!();
}

/// The A/B for the fullscreen change: probe the current picture's metadata on
/// every step, or not at all.
///
/// The two arms are **interleaved** and the order alternates per run. All of A
/// then all of B lets the machine drift invent a win, which on this box is
/// ±100 ms, and the effect being looked for is smaller than that.
///
/// The cost figure printed first is the one to read before the latency table:
/// the metadata read is `fs::read` on the whole file, so what it costs depends
/// entirely on how big the photograph is. `share` is that cost as a fraction of
/// one preview decode, which is the only ratio that says whether displacing a
/// worker matters at all.
#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn exif_ab() {
    let n: usize = std::env::var("IV_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let runs: usize = std::env::var("IV_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    let dir = fixture_dir_at(n, EXIF_W, EXIF_H, EXIF_QUALITY);
    let files = files(&dir);
    assert!(files.len() >= n, "fixture is short: {} of {n}", files.len());

    // Warm-up, both arms: page cache and lazily faulted JPEG tables.
    let _ = browse(&dir, 60, true);
    let _ = browse(&dir, 60, false);

    // Cost of one metadata read, and what it is worth next to a decode.
    let probe = browse(&dir, 400, true);
    let per_read = med(probe.exif_each.clone());
    let per_decode = if probe.runs > 0 {
        probe.decode_ms / probe.runs as f64
    } else {
        0.0
    };
    eprintln!(
        "\n  exif A/B: {} pictures at {EXIF_W}x{EXIF_H} q{EXIF_QUALITY} ({:.1} MB each), median of {runs} interleaved\n",
        files.len(),
        mean_mb(&dir)
    );
    eprintln!(
        "  one metadata read: {per_read:.2} ms   one preview decode: {per_decode:.0} ms   share: {:.1}%",
        per_read / per_decode.max(0.001) * 100.0
    );
    real_photo_reads();
    eprintln!(
        "  {:>5}  {:>4}  {:>9}  {:>9}  {:>9}  {:>8}  {:>6}  {:>9}  {:>7}  {:>7}",
        "hold", "arm", "ttfp p50", "ttfp mean", "ttfp p95", "cache hit", "cached", "exif s", "wasted", "wall s"
    );
    eprintln!("  {}", "-".repeat(96));

    // Only the two short holds can answer the question. At 150 ms and above the
    // prefetch window keeps up and *both* arms land every picture from cache
    // (`ttfp p50` 0), so the metric has no resolution left: a change that helps
    // cannot show up in a number that is already zero. 0 and 30 ms are where the
    // user is outrunning the decoder, which is the only regime where a job queued
    // ahead of every prefetch has anything to take from it.
    for hold in [0u64, 30] {
        let mut on: Vec<Session> = Vec::new();
        let mut off: Vec<Session> = Vec::new();
        for i in 0..runs {
            // Alternate which arm goes first so neither systematically inherits
            // the other's warm-up.
            if i % 2 == 0 {
                off.push(browse(&dir, hold, false));
                on.push(browse(&dir, hold, true));
            } else {
                on.push(browse(&dir, hold, true));
                off.push(browse(&dir, hold, false));
            }
        }
        let row = |arm: &str, s: &Vec<Session>| {
            let hits = med(s.iter().map(|x| x.arrived_cached as f64 / n as f64 * 100.0).collect());
            let wasted = med(
                s.iter()
                    .map(|x| {
                        let total = x.useful_ms + x.wasted_ms;
                        if total > 0.0 {
                            x.wasted_ms / total * 100.0
                        } else {
                            0.0
                        }
                    })
                    .collect(),
            );
            eprintln!(
                "  {:>5}  {:>4}  {:>9.0}  {:>9.0}  {:>9.0}  {:>8.0}%  {:>6}  {:>9.2}  {:>6.0}%  {:>7.2}",
                hold,
                arm,
                med(s.iter().map(|x| x.ttfp_p(0.50)).collect()),
                // The sum over the whole flip: what the user waits in total, and
                // a far steadier number than a percentile of 24 samples.
                med(s.iter().map(|x| x.ttfp_ms.iter().sum::<f64>() / n as f64).collect()),
                med(s.iter().map(|x| x.ttfp_p(0.95)).collect()),
                hits,
                // How many pictures the session got decoded. The metadata read
                // runs at priority 5, ahead of every prefetch, so this is the
                // count it takes - and unlike a latency percentile it is not a
                // rounding of the noise.
                med(s.iter().map(|x| x.runs as f64).collect()),
                med(s.iter().map(|x| x.exif_ms / 1000.0).collect()),
                wasted,
                med(s.iter().map(|x| x.wall_ms / 1000.0).collect()),
            );
        };
        row("off", &off);
        row("on", &on);
        // The effect being looked for is a couple of percent, so the median on its
        // own is not evidence. Print the runs: a difference that is inside the
        // spread is a difference that is not there, and saying so needs the
        // spread.
        for (arm, s) in [("off", &off), ("on", &on)] {
            eprintln!(
                "  {hold:>5}  {arm:>4}x  cached {:?}   ttfp mean {:?}",
                s.iter().map(|x| x.runs).collect::<Vec<_>>(),
                s.iter()
                    .map(|x| (x.ttfp_ms.iter().sum::<f64>() / n as f64).round() as i64)
                    .collect::<Vec<_>>(),
            );
        }
        // How many of the reads were actually paid for. A fast flip mostly *drops*
        // them: the next navigation rebuilds the plan and the read never starts.
        // So the cost per navigation is bounded by dwell, and the dwell-heavy case
        // is the one where the latency metric above has no resolution left.
        eprintln!(
            "  {hold:>5}  reads run: off {}  on {}  of {n} navigations",
            med(off.iter().map(|x| x.exif_jobs as f64).collect()),
            med(on.iter().map(|x| x.exif_jobs as f64).collect()),
        );
    }
    eprintln!();
}

/// Fill a grid: decode one thumbnail per file and report what it cost.
///
/// Two arms, interleaved, and the honest question is not "is the warm arm fast" —
/// it obviously is — but **whether the cold arm is unchanged**. The cache exists
/// because a baseline JPEG has no cheaper full-image read (every pixel needs every
/// DCT coefficient), so a cache cannot make a *first* visit to a folder of large
/// scans fast, and a benchmark that only prints the warm number is how that gets
/// forgotten. Both arms are printed side by side for exactly that reason.
///
/// The cold arm is measured by pointing `LOCALAPPDATA` at a fresh directory per
/// round rather than by deleting entries, so each round starts from nothing.
#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn thumb_cache_ab() {
    let n: usize = std::env::var("IV_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(24);
    let runs: usize = std::env::var("IV_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    // 3000x2000 is the browse fixture's size and is deliberately *below* the size
    // that made this interesting: the point of the measurement is the ratio, and a
    // fixture big enough to take 700 ms per cell would make the benchmark take
    // minutes. `real_scan_cells` below prints the real per-cell figure from
    // `test-images` so the absolute number is not left to be extrapolated.
    let dir = fixture_dir(n);
    let list = files(&dir);
    assert!(list.len() >= n, "fixture is short: {} of {n}", list.len());
    eprintln!(
        "\n  thumbnail cache A/B: {n} cells at {W}x{H}, median of {runs} interleaved, \
         {:.1} MB each\n",
        mean_mb(&dir)
    );

    // One session = fill every cell once, on this thread, and report the total.
    // Single-threaded on purpose: this measures the cost of a cell, and letting
    // four workers divide the same total would just hide it behind a core count.
    let fill = |round: usize, cold: bool| -> (f64, u64) {
        if cold {
            let scratch = std::env::temp_dir().join(format!("iv-bench-thumbcache-cold-{round}"));
            let _ = std::fs::remove_dir_all(&scratch);
            // SAFETY: single-threaded at this point in the test, and the value is
            // restored by the process exiting.
            unsafe { std::env::set_var("LOCALAPPDATA", &scratch) };
        }
        let t = Instant::now();
        for p in &list {
            let d = image_viewer::load_grid_thumbnail(p, 256).expect("cell must load");
            assert!(
                d.width.max(d.height) <= 256,
                "a cell came back {}x{}",
                d.width,
                d.height
            );
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        let bytes = dir_bytes(&image_viewer::thumb_cache_root());
        (ms, bytes)
    };

    // Warm-up both arms: page cache, lazily faulted JPEG tables, and the rayon's
    // global thread pool that the first `downscale_image` builds (~450 ms in a debug
    // build, which is large enough to invent a 10x regression if it lands on one
    // arm — see the note in `tests/thumb_cache.rs`).
    let _ = fill(0, true);
    let _ = fill(0, false);

    let mut cold_ms = Vec::new();
    let mut warm_ms = Vec::new();
    let mut cold_bytes = 0u64;
    for i in 0..runs {
        // Alternate the order so neither arm systematically inherits the other's
        // warm-up, exactly as `exif_ab` does.
        if i % 2 == 0 {
            let (c, b) = fill(i, true);
            cold_ms.push(c);
            cold_bytes = b;
            warm_ms.push(fill(i, false).0);
        } else {
            warm_ms.push(fill(i, false).0);
            let (c, b) = fill(i, true);
            cold_ms.push(c);
            cold_bytes = b;
        }
    }

    eprintln!(
        "  {:>6}  {:>11}  {:>11}  {:>9}  {:>9}",
        "run", "cold ms", "warm ms", "cold/cell", "warm/cell"
    );
    eprintln!("  {}", "-".repeat(54));
    for i in 0..runs {
        eprintln!(
            "  {i:>6}  {:>11.0}  {:>11.0}  {:>9.1}  {:>9.2}",
            cold_ms[i],
            warm_ms[i],
            cold_ms[i] / n as f64,
            warm_ms[i] / n as f64
        );
    }
    let cold = med(cold_ms.clone());
    let warm = med(warm_ms.clone());
    eprintln!(
        "  {:>6}  {:>11.0}  {:>11.0}  {:>9.1}  {:>9.2}   <- median",
        "-",
        cold,
        warm,
        cold / n as f64,
        warm / n as f64
    );
    eprintln!(
        "  cache on disk after one fill: {:.1} MB ({:.0} bytes/cell)",
        cold_bytes as f64 / (1024.0 * 1024.0),
        cold_bytes as f64 / n as f64
    );
    eprintln!(
        "  cold is {cold:.0} ms, which is the floor: nothing here makes a *first* visit\n  \
         to a folder of large scans faster, and that is the honest limit of the change."
    );
    real_scan_cells();
    eprintln!();
}

/// Total bytes under a directory, one level of fanout as the cache uses.
fn dir_bytes(root: &std::path::Path) -> u64 {
    let Ok(fanouts) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut total = 0;
    for f in fanouts.flatten() {
        let Ok(files) = std::fs::read_dir(f.path()) else {
            continue;
        };
        for file in files.flatten() {
            total += file.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

/// The per-cell cost on the real large scans in `test-images`, skipped if the
/// directory is absent (it is gitignored, and a test that reads it passes on the
/// machine that wrote it and dies with a bare `NotFound` on a fresh clone).
///
/// This is the number the fixture above deliberately does not try to reproduce: a
/// synthetic gradient at 6 MP decodes in tens of milliseconds, where a 95 MP
/// baseline scan takes 700-800 ms, because the cost is the entropy decode and that
/// is a function of how much real detail is in the file. Skipped, not faked.
fn real_scan_cells() {
    let dir = std::path::Path::new("test-images");
    if !dir.exists() {
        eprintln!("  (no test-images/ — skipping the real-scan per-cell figure)");
        return;
    }
    let mut big: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && std::fs::metadata(p).map(|m| m.len() > 8_000_000).unwrap_or(false)
                && image_viewer::is_jpeg_bytes(&std::fs::read(p).unwrap_or_default())
        })
        .collect();
    big.sort();
    if big.is_empty() {
        eprintln!("  (no large JPEGs in test-images/ — skipping)");
        return;
    }
    eprintln!("  per cell, real files in test-images (no cache):");
    for p in big {
        let mb = std::fs::metadata(&p).map(|m| m.len() as f64 / (1024.0 * 1024.0)).unwrap_or(0.0);
        // Warm the file cache so the figure is the decode and not the read, then
        // take the min of three: a single sample on a shared machine is noise.
        let _ = image_viewer::load_thumbnail(&p, 256);
        let mut each = Vec::new();
        let mut dims = (0, 0);
        for _ in 0..3 {
            let t = Instant::now();
            let d = image_viewer::load_thumbnail(&p, 256).expect("cell");
            each.push(t.elapsed().as_secs_f64() * 1000.0);
            assert!(d.width.max(d.height) <= 256);
            dims = (d.width, d.height);
        }
        eprintln!(
            "    {:<44.44} {:>6.1} MB  {:>7.0} ms  cell {}x{}",
            p.file_name().unwrap_or_default().to_string_lossy(),
            mb,
            med(each),
            dims.0,
            dims.1
        );
    }
}