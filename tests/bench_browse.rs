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

use image_viewer::{load_image_preview_full, plan_jobs, Job, Kind, Loader, Plan};
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

/// Build, or reuse, a folder of `n` distinct JPEGs.
///
/// Reused across runs on purpose: generating them each time would put tens of
/// seconds of JPEG encoding into every measurement and make two commits'
/// numbers hard to compare.
fn fixture_dir(n: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("iv-bench-browse-{W}x{H}-{n}"));
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
        let mut img = image::RgbImage::new(W, H);
        for y in 0..H {
            for x in 0..W {
                let g = ((x * 7 + y * 3) % 256) as u8;
                let b = (((x / 16) as i32 + (y / 16) as i32 + (i as i32 * 5)) % 256) as u8;
                img.put_pixel(x, y, image::Rgb([(g / 2 + 60) as u8, g, b]));
            }
        }
        image::DynamicImage::ImageRgb8(img)
            .save_with_format(&p, image::ImageFormat::Jpeg)
            .expect("write fixture");
    }
    dir
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

struct Msg {
    path: PathBuf,
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
fn browse(dir: &std::path::Path, hold_ms: u64) -> Session {
    let files = files(dir);
    let n = files.len();
    let (tx, rx): (Sender<Msg>, _) = channel();

    let decode_ms = Arc::new(Mutex::new(Vec::<(PathBuf, f64, Result<Decoded, String>)>::new()));
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
                Kind::Preview => load_image_preview_full(&job.path, job.target_dim)
                    .map(|r| Decoded {
                        bytes: r.preview.rgba.len()
                            + r.full.as_ref().map(|f| f.rgba.len()).unwrap_or(0),
                        below_target: r.below_target,
                    })
                    .map_err(|e| e.to_string()),
                _ => Err("benchmark only decodes previews".to_string()),
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let logged = out.clone();
            dm.lock().unwrap().push((job.path.clone(), ms, logged));
            (
                Msg {
                    path: job.path.clone(),
                    ms,
                    outcome: out,
                },
                true,
            )
        },
        |job: &Job, e: String| {
            (
                Msg {
                    path: job.path.clone(),
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
    let mut failed: HashSet<(PathBuf, Kind)> = HashSet::new();

    let mut index = 0usize;
    let t_start = Instant::now();
    for step in 0..n {
        // 1. Take delivery of anything that finished, the way a frame's `pump`
        //    does, before deciding what is wanted.
        for (path, ms, outcome) in drain(&rx, &decode_ms) {
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
                    failed.insert((path.clone(), Kind::Preview));
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
        let cached_set: HashSet<PathBuf> = cache.keys().cloned().collect();
        let jobs = plan_jobs(&Plan {
            files: &files,
            index,
            cached: &cached_set,
            failed: &failed,
            wants_exif: false,
            wants_full: false,
        current_stale: false,
            target_dim: PREVIEW_DIM,
            radius: RADIUS,
        });
        wanted = jobs.iter().map(|j| j.path.clone()).collect();
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
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        for (path, ms, outcome) in drain(&rx, &decode_ms) {
            if let Ok(d) = outcome {
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
        if loader.outstanding() == 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the queue never drained: {} outstanding",
            loader.outstanding()
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    s.wall_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    s.dropped = image_viewer::perf::counter(image_viewer::perf::C_JOBS_DROPPED);
    s.runs = cache.len();
    s
}

fn drain(
    rx: &std::sync::mpsc::Receiver<Msg>,
    log: &Arc<Mutex<Vec<(PathBuf, f64, Result<Decoded, String>)>>>,
) -> Vec<(PathBuf, f64, Result<Decoded, String>)> {
    let _ = rx.try_iter().count();
    let mut out = std::mem::take(&mut *log.lock().unwrap());
    // Keep anything that arrived but was not yet taken.
    out.extend(rx.try_iter().map(|m| (m.path, m.ms, m.outcome)));
    out.sort_by_key(|(p, _, _)| p.clone());
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
    log: &Arc<Mutex<Vec<(PathBuf, f64, Result<Decoded, String>)>>>,
    cache: &mut HashMap<PathBuf, Cached>,
    pending_since: &mut HashMap<PathBuf, Instant>,
    failed: &mut HashSet<(PathBuf, Kind)>,
    wanted: &HashSet<PathBuf>,
    clock: &mut u64,
) {
    for (path, ms, outcome) in drain(rx, log) {
        *clock += 1;
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
                failed.insert((path.clone(), Kind::Preview));
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
    let _ = browse(&dir, 60);

    for hold in [0u64, 30, 60, 150, 400] {
        let mut s = Vec::new();
        for _ in 0..runs {
            s.push(browse(&dir, hold));
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