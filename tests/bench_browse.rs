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
//! The EXIF arm (`exif_ab`) exists because the metadata job is not free and was
//! not small: it used to call `color_info`, whose `fs::read` pulls the **whole
//! file**, and `nom_exif` then seeks out the APP1 segment. That read is gone
//! now — the colour profile is classified from the bytes the preview decode
//! already read (`PreviewAndFull::colour`), so the job's shape here mirrors the
//! app's: the EXIF seek plus the 256 KiB header read for the JPEG `COM`
//! segment. Modelled with the same calls rather than as a no-op — the question
//! "does not probing metadata while the panel is hidden make navigation
//! faster" has no answer if the benchmark pretends the job costs nothing —
//! but expect the per-read numbers an order of magnitude under the old ones.
//! It is the one job per navigation that exists only to fill a panel, and it
//! is queued *ahead* of every prefetch.
//!
//! The thumbnail arm (`thumb_cache_ab`) is the third kind of measurement here: not
//! a scheduling question but a **"can this be made faster at all"** one. A 95 MP
//! baseline JPEG has no cheaper full-image read — every pixel needs every DCT
//! coefficient, so the entropy decode is the floor for any conformant decoder — and
//! the grid's answer to that is a cache rather than a faster decode. Which means
//! the number that matters is the **cold** one, and printing only the warm number
//! would be how the limit of the change got forgotten.

use image::ImageEncoder as _;
use std::io::Read;
use image_viewer::{jpeg_comment, load_image_preview_full, plan_jobs, ImgKey, Job, Kind, Loader, Plan};
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
/// Shipped defaults from app/consts.rs, duplicated here so the bench can vary
/// them independently without recompiling the app crate.
const PRELOAD_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
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
                // EXIF block, which seeks, plus a 256 KiB header read for the
                // JPEG `COM` segment. The colour profiling is **not** here any
                // more: it is classified from the bytes the preview decode
                // already read and rides on the preview (`PreviewAndFull::colour`),
                // which removed the second whole-file read per navigation.
                // No pixels come back, hence `bytes: 0`.
                Kind::Exif => {
                    let _ = nom_exif::read_exif(job.path());
                    if let Ok(mut f) = std::fs::File::open(job.path()) {
                        let mut head = Vec::new();
                        let _ = f.by_ref().take(262144).read_to_end(&mut head);
                        let _ = jpeg_comment(&head);
                    }
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
            // These fixtures sit under any sane budget, so this benchmark
            // faces the planner the production plan would too — lengths
            // unknown `[]` (the fail-open reading) and an unlimited budget:
            // the budget policy has policy tests of its own, and a stat per
            // emitted job here would only re-derive `0`.
            file_bytes: &[],
            preload_budget: u64::MAX,
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
            // The metadata job's own reads, exactly as the app runs them: the
            // EXIF seek, plus the bounded header read for the `COM` segment.
            let _ = nom_exif::read_exif(p);
            if let Ok(mut f) = std::fs::File::open(p) {
                let mut head = Vec::new();
                let _ = f.by_ref().take(262144).read_to_end(&mut head);
                let _ = jpeg_comment(&head);
            }
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

/// Idle warm-up: sit still at image 0 long enough for workers to preload a
/// wide radius, then start flipping fast. Measures whether the warm cells
/// actually land on screen when navigation suddenly starts.
///
/// Two arms, interleaved per run:
///   * cold — start fast navigation immediately (current behavior without idle
///     warm-up; radius stays at RADIUS=3)
///   * warm — first idle at image 0 with radius=min(n,50), draining all
///     deliveries before stepping, then flip fast with radius=RADIUS=3
///
/// The question being answered: does warming cells while idle reduce
/// time-to-first-pixels when the user suddenly starts arrowing through?
fn idle_warmup(dir: &std::path::Path, hold_ms: u64, runs: usize) {
    let files = files(dir);
    let n = files.len();
    assert!(n >= 2, "need at least 2 fixtures, got {n}");

    eprintln!(
        "\n  idle warm-up: {} pictures at {W}x{H}, radius={} idle vs {RADIUS} fast, median of {runs}",
        n,
        files.len().min(50),
    );
    eprintln!(
        "  {:>5}  {:>5}  {:>9}  {:>9}  {:>9}  {:>8}  {:>9}  {:>9}  {:>9}",
        "hold", "arm", "ttfp p50", "ttfp p95", "cache hit", "wasted %", "decode s", "peak MB", "wall s"
    );
    eprintln!("  {}", "-".repeat(96));

    let mut cold: Vec<Session> = Vec::new();
    let mut warm: Vec<Session> = Vec::new();

    for i in 0..runs {
        // Arm A: start fast immediately (no idle warm-up).
        cold.push(browse_idle(&dir, hold_ms, false));

        // Arm B: idle at image 0 first, then fast navigation.
        warm.push(browse_idle(&dir, hold_ms, true));

        // Alternate which arm runs first so machine drift doesn't invent a win.
        if i % 2 == 1 {
            std::mem::swap(&mut cold, &mut warm);
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
            "  {:>5}  {:>5}  {:>9.0}  {:>9.0}  {:>9.0}  {:>8.0}%  {:>6}  {:>9.2}  {:>9}",
            hold_ms,
            arm,
            med(s.iter().map(|x| x.ttfp_p(0.50)).collect()),
            med(s.iter().map(|x| x.ttfp_p(0.95)).collect()),
            hits,
            wasted,
            med(s.iter().map(|x| x.decode_ms / 1000.0).collect()),
            med(s.iter().map(|x| x.peak_bytes as f64 / (1024.0 * 1024.0)).collect()),
            med(s.iter().map(|x| x.wall_ms / 1000.0).collect()),
        );
    };

    row("cold", &cold);
    row("warm", &warm);
    eprintln!();
}

/// Browse through all pictures, optionally idling at image 0 first.
///
/// `idle` first plans jobs at image 0 with radius=min(n,50) and drains every
/// delivery before stepping — simulating the user sitting still while workers
/// preload widely. Then steps through every picture every `hold_ms` with the
/// normal radius of RADIUS.
fn browse_idle(dir: &std::path::Path, hold_ms: u64, idle: bool) -> Session {
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
                Kind::Exif => {
                    let _ = nom_exif::read_exif(job.path());
                    if let Ok(mut f) = std::fs::File::open(job.path()) {
                        let mut head = Vec::new();
                        let _ = f.by_ref().take(262144).read_to_end(&mut head);
                        let _ = jpeg_comment(&head);
                    }
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
    let mut pending_since: HashMap<PathBuf, Instant> = HashMap::new();
    let mut failed: HashSet<(ImgKey, Kind)> = HashSet::new();

    let mut index = 0usize;
    let t_start = Instant::now();

    // ---- Idle phase: preload widely at image 0 -------------------------------
    if idle {
        let cached_set: HashSet<ImgKey> = cache.keys().map(|p| ImgKey::first(p)).collect();
        let jobs = plan_jobs(&Plan {
            files: &files,
            index: 0,
            current_page: 0,
            partner: None,
            partner_needs_full: false,
            cached: &cached_set,
            failed: &failed,
            wants_exif: false,
            wants_full: false,
            current_stale: false,
            target_dim: PREVIEW_DIM,
            radius: files.len().min(50),
            file_bytes: &[],
            preload_budget: u64::MAX,
        });
        wanted = jobs.iter().map(|j| j.path().to_path_buf()).collect();
        loader.replace(jobs);

        // Drain until the loader is idle: every warm-up decode has landed.
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let left = loader.outstanding();
            for (path, kind, ms, outcome) in drain(&rx, &decode_ms) {
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
                            s.wasted_ms += ms;
                        }
                        cache.insert(
                            path.clone(),
                            Cached {
                                bytes: d.bytes,
                                used: clock,
                            },
                        );
                    }
                    Err(_e) => {
                        failed.insert((ImgKey::first(&path), Kind::Preview));
                    }
                }
            }
            if left == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "idle warm-up never drained: {left} outstanding"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        clock = 0; // reset clock after idle phase for eviction fairness
    }

    // ---- Fast navigation phase ----------------------------------------------
    for step in 0..n {
        for (path, kind, ms, outcome) in drain(&rx, &decode_ms) {
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
                Err(_e) => {
                    failed.insert((ImgKey::first(&path), Kind::Preview));
                }
            }
        }
        s.peak_bytes = s
            .peak_bytes
            .max(cache.values().map(|c| c.bytes).sum::<usize>());
        evict(&mut cache, &files, index, n, &mut clock);

        let cur = &files[index];
        if cache.contains_key(cur) {
            s.arrived_cached += 1;
            pending_since.remove(cur);
            s.ttfp_ms.push(0.0);
        } else {
            s.arrived_decode += 1;
            pending_since.insert(cur.clone(), Instant::now());
        }

        let cached_set: HashSet<ImgKey> = cache.keys().map(|p| ImgKey::first(p)).collect();
        let jobs = plan_jobs(&Plan {
            files: &files,
            index,
            current_page: 0,
            partner: None,
            partner_needs_full: false,
            cached: &cached_set,
            failed: &failed,
            wants_exif: false,
            wants_full: false,
            current_stale: false,
            target_dim: PREVIEW_DIM,
            radius: RADIUS,
            file_bytes: &[],
            preload_budget: u64::MAX,
        });
        wanted = jobs.iter().map(|j| j.path().to_path_buf()).collect();
        loader.replace(jobs);

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

    // Drain what is left.
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
        assert!(Instant::now() < deadline, "final drain never finished: {left}");
        std::thread::sleep(Duration::from_millis(2));
    }
    s.wall_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    s.dropped = image_viewer::perf::counter(image_viewer::perf::C_JOBS_DROPPED);
    s.runs = cache.len();
    s
}

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn idle_warmup_bench() {
    let n: usize = std::env::var("IV_BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let runs: usize = std::env::var("IV_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let dir = fixture_dir(n);

    // Warm-up so first-touch page faults don't dominate.
    let _ = browse(&dir, 60, false);

    idle_warmup(&dir, 0, runs);
    idle_warmup(&dir, 30, runs);
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
    shell_cache_cells();
    eprintln!();
}

/// The mean absolute difference per colour channel between two cells, at 64x64
/// so two decodes of different sizes can be compared at all.
fn cell_distance(a: &image_viewer::ImageData, b: &image_viewer::ImageData) -> f64 {
    let at = |d: &image_viewer::ImageData| {
        let img = image::RgbaImage::from_raw(d.width, d.height, d.rgba.clone()).expect("rgba");
        image::imageops::resize(&img, 64, 64, image::imageops::FilterType::Triangle)
    };
    let (a, b) = (at(a), at(b));
    let mut sum = 0f64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        for c in 0..3 {
            sum += (pa.0[c] as f64 - pb.0[c] as f64).abs();
        }
    }
    sum / (64.0 * 64.0 * 3.0)
}

/// What Explorer's cache has for the real large files, and what its cell is worth.
///
/// The three questions this feature left unverified, answered on real photographs
/// rather than on a synthetic gradient: does a cache-only ask hit, what does a hit
/// cost, and is the shell's cell the same picture? The distance is printed next to
/// our own decode's, because "close enough" only means something against a scale —
/// `tests/thumb_cache.rs` uses a different file for that, here it is the same file.
///
/// **The entry is made with one extraction**, because this box has no Explorer
/// session filling the cache and the app itself never extracts. The side effect is
/// the point of the measurement: those files get the Explorer thumbnail entries
/// browsing the folder would have given them, so a *second* run reports the first
/// ask as already cached rather than as a miss, and the miss figure is only
/// meaningful on the first run after the cache forgets the file.
fn shell_cache_cells() {
    let dir = std::path::Path::new("test-images");
    if !dir.exists() {
        eprintln!("  (no test-images/ - skipping the shell-cache figures)");
        return;
    }
    let mut big: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && std::fs::metadata(p)
                    .map(|m| m.len() > 8_000_000)
                    .unwrap_or(false)
        })
        .collect();
    big.sort_by_key(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0));
    // Four of the largest: each row costs a full decode of the file, and the
    // shape of the answer does not change after the first few.
    big.truncate(4);
    if big.is_empty() {
        eprintln!("  (no files over 8 MB in test-images/ - skipping)");
        return;
    }
    eprintln!("  Explorer's thumbnail cache, real files in test-images (256px cells):");
    for p in &big {
        let mb = std::fs::metadata(p).map(|m| m.len() as f64 / (1024.0 * 1024.0)).unwrap_or(0.0);
        let name = p.file_name().unwrap_or_default().to_string_lossy().to_string();

        // The ask the app makes, before this run has extracted anything.
        let t = Instant::now();
        let cached = image_viewer::shell_thumbnail(p, 256);
        let ask_ms = t.elapsed().as_secs_f64() * 1000.0;
        match &cached {
            None => eprintln!("    {:<40.40} {:>6.1} MB  cold miss {ask_ms:>6.2} ms", name, mb),
            Some(d) => eprintln!(
                "    {:<40.40} {:>6.1} MB  already in the cache ({}x{})",
                name, mb, d.width, d.height
            ),
        }

        // The entry Explorer would have made; the hit below is what a folder the
        // user browsed in Explorer first looks like.
        let extract = image_viewer::shell_thumbnail_allowing_extraction(p, 256);
        let t = Instant::now();
        let hit = image_viewer::shell_thumbnail(p, 256);
        let hit_ms = t.elapsed().as_secs_f64() * 1000.0;
        let ours = image_viewer::load_thumbnail(p, 256).expect("cell");
        match &hit {
            Some(cell) => eprintln!(
                "      hit {:>6.2} ms  shell {}x{}  ours {}x{}  mean|d| {:.2} codes",
                hit_ms,
                cell.width,
                cell.height,
                ours.width,
                ours.height,
                cell_distance(cell, &ours)
            ),
            None => eprintln!(
                "      no hit after extraction {:?} - the shell refused this one",
                extract.as_ref().map(|d| (d.width, d.height))
            ),
        }
    }
}

/// Sizes the drop-copy arm measures, in megabytes. Synthetic files in a
/// `tempfile::tempdir()` — never `test-images/`, which is gitignored, and a
/// missing file there must not read as a fast copy.
const DROP_SIZES_MB: [u64; 3] = [5, 50, 500];

/// A file of `mb` megabytes in `dir`, written once and reused by every round.
///
/// Not a JPEG: nothing here decodes it, and the cost being measured is
/// `fs::copy`'s read-and-write, which is a function of the byte count and not of
/// what is in the bytes.
fn drop_fixture(dir: &std::path::Path, mb: u64) -> PathBuf {
    let path = dir.join(format!("drop-{mb}mb.bin"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == mb * 1024 * 1024) {
        return path;
    }
    let f = std::fs::File::create(&path).expect("create the drop fixture");
    let mut w = std::io::BufWriter::new(f);
    // One 1 MiB block of varied bytes, written `mb` times: a run of zeroes would
    // let the filesystem's compression or sparse handling make the copy look
    // faster than a photograph's bytes would.
    let block: Vec<u8> = (0..1024 * 1024u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    for _ in 0..mb {
        std::io::Write::write_all(&mut w, &block).expect("write the drop fixture");
    }
    std::io::Write::flush(&mut w).expect("flush the drop fixture");
    path
}

/// What one drop of a file costs, split into "must copy" and "the same unchanged
/// file again".
///
/// **Both arms matter and the point of the benchmark is their ratio.** A first
/// drop of a file has to read and write it, and nothing can make that cheaper —
/// the same "a baseline JPEG has no cheaper full-image read" limit as a grid
/// cell. A *repeated* drop of an unchanged file does not: the copy is
/// content-addressed by path + mtime + length, so a valid one is already sitting
/// in `%TEMP%` and the only honest work is a `stat`. A benchmark that printed only
/// the first number would be how the second got forgotten.
///
/// **The cold arm *is* what the UI thread used to pay**, because it is the same
/// work `own_dropped` did when it was called from the drop handler: one
/// `metadata`, one `create_dir_all`, one `remove_file`, one `fs::copy`. Moving the
/// copy to a thread moves *when* it is paid, not how much — so this column is the
/// answer to "how long was the UI frozen", and after the move it is the answer to
/// "how long is the drop invisible", which is a different cost and not a smaller
/// one.
///
/// The cold arm's copy is released **outside** the timed region, so the arm is
/// cold by construction (the destination is gone) without paying a delete inside
/// the measurement.
///
/// `ctl` is a second, identical cold block: two arms of the same measurement,
/// interleaved, so the run reports **its own drift floor** rather than asking
/// anyone to take the difference between two blocks for one. On this box that
/// floor is 1-5%, which is what makes the cold/repeat ratio readable at all.
#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn drop_copy_ab() {
    let runs: usize = std::env::var("IV_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7);
    let dir = tempfile::tempdir().expect("temp dir");
    eprintln!(
        "\n  drop copy A/B: {} MB files, median of {runs} interleaved rounds, \
         each arm warmed first\n",
        DROP_SIZES_MB.len()
    );

    struct Arms {
        cold: Vec<f64>,
        repeat: Vec<f64>,
        ctl: Vec<f64>,
    }

    for mb in DROP_SIZES_MB {
        let src = drop_fixture(dir.path(), mb);
        // Both arms return what the copy reported alongside the time, so the
        // loop below can assert the arm took the path it claims without the
        // warm-up calls tripping the same assertion.
        //
        // The cold arm's directory is removed **before** the clock starts, not
        // after it stopped: forgetting afterwards left the copy on disk for the
        // reuse arm's warm-up, and the next "cold" call then reused it and
        // measured nothing — an arm that reported a reuse while asserting it had
        // not, which is what the assertion below is for.
        let forget = || {
            let m = std::fs::metadata(&src).expect("stat the source");
            // The library's own path function, so the arm cannot drift from where
            // the app puts the copy.
            let _ = std::fs::remove_dir_all(image_viewer::drop_copy_dir(&src, &m));
        };
        let copy = |_round: usize| -> (f64, bool) {
            forget();
            let t = Instant::now();
            let c = image_viewer::own_dropped(&src).expect("own the dropped file");
            (t.elapsed().as_secs_f64() * 1000.0, c.reused)
        };
        let reuse = |_round: usize| -> (f64, bool) {
            let t = Instant::now();
            let c = image_viewer::own_dropped(&src).expect("own it again");
            (t.elapsed().as_secs_f64() * 1000.0, c.reused)
        };
        // Warm both arms: the first `fs::copy` of a file faults in its pages and
        // is not what a drop in the middle of a session pays. `reuse` twice,
        // because its first call after a cold arm has nothing to reuse and so
        // takes the path it exists not to measure.
        let _ = copy(usize::MAX);
        let _ = reuse(usize::MAX);
        let _ = reuse(usize::MAX);

        let mut a = Arms {
            cold: Vec::new(),
            repeat: Vec::new(),
            ctl: Vec::new(),
        };
        for i in 0..runs {
            // Alternate which arm goes first, so neither systematically inherits
            // the other's warm-up — and put the control between them, so a drift
            // shows up as cold-vs-ctl disagreeing within the same round.
            let (c, r, k) = if i % 2 == 0 {
                let c = copy(i);
                let r = reuse(i);
                let k = copy(i);
                (c, r, k)
            } else {
                let k = copy(i);
                let r = reuse(i);
                let c = copy(i);
                (c, r, k)
            };
            assert!(!c.1, "the cold arm must really copy, or it is not measuring that");
            assert!(r.1, "an unchanged source must reuse its copy");
            assert!(!k.1, "the control must really copy, or it is not a control");
            a.cold.push(c.0);
            a.repeat.push(r.0);
            a.ctl.push(k.0);
        }
        eprintln!(
            "  {:>5} MB   {:>10}  {:>10}  {:>10}   (ms)",
            "size", "cold", "repeat", "ctl"
        );
        eprintln!("  {}", "-".repeat(46));
        for i in 0..runs {
            eprintln!(
                "  run {i:<2}   {:>10.2}  {:>10.2}  {:>10.2}",
                a.cold[i], a.repeat[i], a.ctl[i]
            );
        }
        let (cold, repeat, ctl) = (med(a.cold.clone()), med(a.repeat.clone()), med(a.ctl.clone()));
        let drift = (ctl - cold).abs() / cold.max(1e-9) * 100.0;
        eprintln!(
            "  {:>5} MB   {:>10.2}  {:>10.2}  {:>10.2}   <- median  \
             (control drift {drift:.1}%)",
            mb, cold, repeat, ctl
        );
        if mb == DROP_SIZES_MB[0] {
            eprintln!(
                "\n  `cold` is what a drop cost the UI thread before the copy moved off\n  \
                 it — and what it costs a worker now, so the drop is invisible for\n  \
                 this long instead. `repeat` is the same drop of an *unchanged*\n  \
                 file: the common case, and two stats."
            );
        }
        eprintln!();
    }
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
/// =========================================================================
/// Heuristic benchmark suite
/// =========================================================================
///
/// Each heuristic in the app is a knob that trades off latency, throughput,
/// memory, and wasted work. This suite sweeps each knob across its plausible
/// range and reports the impact, so we know which knobs actually matter and
/// which are decorative.
///
/// All arms are run on the same fixture set, interleaved per run, with warm-up
/// first. The metrics that matter:
///   * ttfp p50/p95 — time to first pixels, what the user feels
///   * cache hit — fraction of steps landing on an already-decoded preview
///   * wasted % — worker time on results nobody wanted by arrival time
///   * decode s — total worker time spent decoding
///   * peak MB — peak preview texture memory
///   * wall s — total wall time
/// =========================================================================

/// Parameterized browse: same as `browse` but with explicit heuristic knobs
/// instead of the compiled-in constants.
fn browse_parametric(
    dir: &std::path::Path,
    hold_ms: u64,
    radius: usize,
    preload_budget_bytes: u64,
    target_dim: u32,
    cache_cap: usize,
) -> Session {
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
                Kind::Exif => {
                    let _ = nom_exif::read_exif(job.path());
                    if let Ok(mut f) = std::fs::File::open(job.path()) {
                        let mut head = Vec::new();
                        let _ = f.by_ref().take(262144).read_to_end(&mut head);
                        let _ = jpeg_comment(&head);
                    }
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
    let mut pending_since: HashMap<PathBuf, Instant> = HashMap::new();
    let mut failed: HashSet<(ImgKey, Kind)> = HashSet::new();

    let mut index = 0usize;
    let t_start = Instant::now();
    for step in 0..n {
        // Take delivery of anything that finished
        for (path, kind, ms, outcome) in drain(&rx, &decode_ms) {
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
                Err(_) => {
                    failed.insert((ImgKey::first(&path), Kind::Preview));
                }
            }
        }
        s.peak_bytes = s
            .peak_bytes
            .max(cache.values().map(|c| c.bytes).sum::<usize>());
        evict_parametric(&mut cache, &files, index, n, &mut clock, cache_cap);

        let cur = &files[index];
        if cache.contains_key(cur) {
            s.arrived_cached += 1;
            pending_since.remove(cur);
            s.ttfp_ms.push(0.0);
        } else {
            s.arrived_decode += 1;
            pending_since.insert(cur.clone(), Instant::now());
        }

        let cached_set: HashSet<ImgKey> = cache.keys().map(|p| ImgKey::first(p)).collect();
        let jobs = plan_jobs(&Plan {
            files: &files,
            index,
            current_page: 0,
            partner: None,
            partner_needs_full: false,
            cached: &cached_set,
            failed: &failed,
            wants_exif: false,
            wants_full: false,
            current_stale: false,
            target_dim,
            radius,
            file_bytes: &[],
            preload_budget: preload_budget_bytes,
        });
        wanted = jobs.iter().map(|j| j.path().to_path_buf()).collect();
        loader.replace(jobs);

        let hold = Duration::from_millis(hold_ms);
        let slice = Duration::from_millis(4);
        let mut waited = Duration::ZERO;
        while waited < hold {
            let chunk = slice.min(hold - waited);
            std::thread::sleep(chunk);
            waited += chunk;
            take_delivery_parametric(
                &mut s,
                &rx,
                &decode_ms,
                &mut cache,
                &mut pending_since,
                &mut failed,
                &wanted,
                &mut clock,
            );
        }
        index = (index + 1) % n;
        let _ = step;
    }

    // Drain what is left
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
        assert!(Instant::now() < deadline, "final drain never finished: {left}");
        std::thread::sleep(Duration::from_millis(2));
    }
    s.wall_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    s.dropped = image_viewer::perf::counter(image_viewer::perf::C_JOBS_DROPPED);
    s.runs = cache.len();
    s
}

/// Eviction with configurable cache cap (replaces the hardcoded CAP).
fn evict_parametric(
    cache: &mut HashMap<PathBuf, Cached>,
    files: &[PathBuf],
    index: usize,
    n: usize,
    clock: &mut u64,
    cap: usize,
) {
    *clock += 1;
    while cache.len() > cap {
        let protect: HashSet<&PathBuf> = (0..=6)
            .map(|k| (index + n * 2 + k - 3) % n)
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

/// Take delivery with configurable cache (same as `take_delivery` but inline
/// for the parametric browse).
fn take_delivery_parametric(
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
            Err(_) => {
                failed.insert((ImgKey::first(&path), Kind::Preview));
            }
        }
    }
}

/// Run one parametric browse config and print a row.
fn browse_parametric_row(
    dir: &std::path::Path,
    hold_ms: u64,
    radius: usize,
    preload_budget_bytes: u64,
    target_dim: u32,
    cache_cap: usize,
    runs: usize,
) {
    let mut sessions = Vec::new();
    for _ in 0..runs {
        sessions.push(browse_parametric(
            dir, hold_ms, radius, preload_budget_bytes, target_dim, cache_cap,
        ));
    }
    let hits = med(
        sessions
            .iter()
            .map(|x| x.arrived_cached as f64 / sessions[0].ttfp_ms.len().max(1) as f64 * 100.0)
            .collect(),
    );
    let wasted = med(
        sessions
            .iter()
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
        "  {:>6}  {:>5}  {:>12}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}  {:>9}  {:>9}  {:>9}  {:>9}",
        hold_ms,
        radius,
        preload_budget_bytes / 1_048_576,
        target_dim,
        cache_cap,
        med(sessions.iter().map(|x| x.ttfp_p(0.50)).collect()),
        med(sessions.iter().map(|x| x.ttfp_p(0.95)).collect()),
        hits,
        wasted,
        med(sessions.iter().map(|x| x.decode_ms / 1000.0).collect()),
        med(sessions.iter().map(|x| x.peak_bytes as f64 / (1024.0 * 1024.0)).collect()),
        med(sessions.iter().map(|x| x.wall_ms / 1000.0).collect()),
    );
}

/// Header for the heuristic benchmark table.
fn heuristic_header() {
    eprintln!(
        "  {:>6}  {:>5}  {:>12}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}  {:>9}  {:>9}  {:>9}  {:>9}",
        "hold_ms",
        "radius",
        "preload_MB",
        "target_px",
        "cache_cap",
        "ttfp_p50_ms",
        "ttfp_p95_ms",
        "cache_hit%",
        "wasted%",
        "decode_s",
        "peak_MB",
        "wall_s",
    );
    eprintln!("  {}", "-".repeat(120));
}

/// Comprehensive heuristic sweep: vary each knob across its plausible range
/// while holding the others at the shipped defaults.
#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn heuristic_sweep() {
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
    assert!(files.len() >= n, "fixture is short: {} of {n}", files.len());

    // Warm-up so first-touch page faults don't dominate.
    let _ = browse_parametric(&dir, 60, RADIUS, PRELOAD_BUDGET_BYTES, PREVIEW_DIM, CAP);

    eprintln!(
        "\n  Heuristic sweep: {} pictures at {W}x{H}, {} images, median of {} runs",
        n,
        files.len(),
        runs
    );
    eprintln!(
        "  Shipped defaults: radius={RADIUS}, preload_budget={}MB, target_dim={PREVIEW_DIM}, cache_cap={CAP}, hold=30ms",
        PRELOAD_BUDGET_BYTES / 1_048_576
    );
    eprintln!();

    // ===== Sweep 1: Preload radius =====
    eprintln!("  ## 1: Preload radius (how many neighbours to preload each side)");
    heuristic_header();
    for radius in [1usize, 2, 3, 5, 10] {
        browse_parametric_row(&dir, 30, radius, PRELOAD_BUDGET_BYTES, PREVIEW_DIM, CAP, runs);
    }
    eprintln!();

    // ===== Sweep 2: Preload budget =====
    eprintln!("  ## 2: Preload budget (byte budget for neighbour preloads)");
    heuristic_header();
    for budget_mb in [64usize, 128, 256, 512, 1024] {
        browse_parametric_row(&dir, 30, RADIUS, budget_mb as u64 * 1_048_576, PREVIEW_DIM, CAP, runs);
    }
    eprintln!();

    // ===== Sweep 3: Preview target dimension =====
    eprintln!("  ## 3: Preview target dimension (panel size in physical px)");
    heuristic_header();
    for target_dim in [1024u32, 2048, 4096, 8192] {
        browse_parametric_row(&dir, 30, RADIUS, PRELOAD_BUDGET_BYTES, target_dim, CAP, runs);
    }
    eprintln!();

    // ===== Sweep 4: Preview cache cap =====
    eprintln!("  ## 4: Preview cache cap (max entries in preview cache)");
    heuristic_header();
    for cap in [4usize, 8, 16, 32] {
        browse_parametric_row(&dir, 30, RADIUS, PRELOAD_BUDGET_BYTES, PREVIEW_DIM, cap, runs);
    }
    eprintln!();

    // ===== Sweep 5: Hold time (navigation speed) =====
    eprintln!("  ## 5: Hold time between steps (navigation speed)");
    heuristic_header();
    for hold_ms in [0u64, 15, 30, 60, 120, 250, 500] {
        browse_parametric_row(&dir, hold_ms, RADIUS, PRELOAD_BUDGET_BYTES, PREVIEW_DIM, CAP, runs);
    }
    eprintln!();

    eprintln!("  ## End of heuristic sweep");
}

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