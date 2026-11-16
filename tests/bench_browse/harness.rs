//! The engine every browse arm drives: fixtures, a session, and the queue counters.
//!
//! `tests/bench_browse.rs` became a target directory (see `tests/source_size.rs`)
//! so the arms could be split by question without turning one benchmark into
//! several binaries: this module holds what all of them share.

use image::ImageEncoder as _;
use image_viewer::{
    ImgKey, Job, Kind, Loader, Plan, jpeg_comment, load_image_preview_full, plan_jobs,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const W: u32 = 3000;
pub const H: u32 = 2000;
pub const PREVIEW_DIM: u32 = 8192;
pub const RADIUS: usize = 3;
/// Preview entries held, mirroring `PREVIEW_CACHE_CAP` in the app.
pub const CAP: usize = 16;
/// Shipped defaults from app/consts.rs, duplicated here so the bench can vary
/// them independently without recompiling the app crate.
pub const PRELOAD_BUDGET_BYTES: u64 = 256 * 1024 * 1024;
/// Shipped default for full-res decode dwell (ms).
pub const FULL_DWELL_MS: u64 = 120;
/// Quality the generated fixtures are written at. `image`'s default JPEG
/// quality, spelled out because the EXIF arm's cost is a whole-file read and
/// therefore a function of how big the file is.
pub const QUALITY: u8 = 75;
/// Build, or reuse, a folder of `n` distinct JPEGs.
///
/// Reused across runs on purpose: generating them each time would put tens of
/// seconds of JPEG encoding into every measurement and make two commits'
/// numbers hard to compare.
pub fn fixture_dir(n: usize) -> PathBuf {
    fixture_dir_at(n, W, H, QUALITY)
}

pub fn fixture_dir_at(n: usize, w: u32, h: u32, quality: u8) -> PathBuf {
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
                img.put_pixel(x, y, image::Rgb([(g / 2 + 60), g, b]));
            }
        }
        let mut out = std::io::Cursor::new(Vec::new());
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
        enc.write_image(img.as_raw(), w, h, image::ExtendedColorType::Rgb8).expect("write fixture");
        std::fs::write(&p, out.into_inner()).expect("write fixture");
    }
    dir
}

/// Mean file size of a fixture folder, in MB. Reported rather than assumed: the
/// EXIF read is `fs::read`, so its cost is whatever this says it is.
pub fn mean_mb(dir: &std::path::Path) -> f64 {
    let v: Vec<u64> =
        files(dir).iter().filter_map(|p| std::fs::metadata(p).ok().map(|m| m.len())).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.iter().sum::<u64>() as f64 / v.len() as f64 / (1024.0 * 1024.0)
}

pub fn files(dir: &std::path::Path) -> Vec<PathBuf> {
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
pub struct Cached {
    pub bytes: usize,
    pub used: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Decoded {
    pub bytes: usize,
    pub below_target: bool,
}

/// What a worker hands back: the file, what the job was for, its wall time, and
/// what it produced. The kind travels with it because an EXIF read reports the
/// same *path* as the preview for that file and must not be filed as pixels.
pub type Delivery = (PathBuf, Kind, f64, Result<Decoded, String>);

pub struct Msg {
    pub path: PathBuf,
    pub kind: Kind,
    /// Worker wall time. Counted whether or not anyone still wanted the result.
    pub ms: f64,
    pub outcome: Result<Decoded, String>,
}

#[derive(Default, Debug)]
pub struct Session {
    /// Time from stepping to a picture until its pixels were in hand.
    pub ttfp_ms: Vec<f64>,
    /// Worker time whose result nobody wanted by the time it arrived.
    pub wasted_ms: f64,
    /// Worker time on results that were wanted.
    pub useful_ms: f64,
    /// Decodes started, including retries and results nobody used.
    pub runs: usize,
    /// Steps that landed on an already-decoded picture.
    pub arrived_cached: usize,
    /// Steps that had to wait.
    pub arrived_decode: usize,
    pub peak_bytes: usize,
    pub dropped: u64,
    /// Total worker time, useful and wasted together.
    pub decode_ms: f64,
    /// Worker time on full-resolution decodes.
    pub full_ms: f64,
    /// Per-image full decode times.
    pub full_each: Vec<f64>,
    /// Number of full decodes.
    pub full_jobs: usize,
    /// Previews that came back below the requested density.
    pub below_target: usize,
    pub wall_ms: f64,
    /// Worker time on metadata reads (`Kind::Exif`), which produce no pixels.
    /// Kept out of `useful_ms`/`wasted_ms` on purpose: those two are about
    /// pictures, and an EXIF read cannot be "wasted" in the sense a decode is.
    pub exif_ms: f64,
    pub exif_jobs: usize,
    /// Per-read metadata times, for the cost figure.
    pub exif_each: Vec<f64>,
}

impl Session {
    pub fn ttfp_p(&self, q: f64) -> f64 {
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
pub fn browse(dir: &std::path::Path, hold_ms: u64, exif: bool) -> Session {
    let files = files(dir);
    let n = files.len();
    let (tx, rx): (Sender<Msg>, _) = channel();

    let decode_ms = Arc::new(Mutex::new(Vec::<Delivery>::new()));
    let dm = decode_ms.clone();
    let loader = Loader::spawn(
        std::thread::available_parallelism().map(|x| x.get()).unwrap_or(2).clamp(2, 4),
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
                    Ok(Decoded { bytes: 0, below_target: false })
                }
                _ => Err("benchmark only decodes previews and reads exif".to_string()),
            };
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            let logged = out.clone();
            dm.lock().unwrap().push((job.path().to_path_buf(), job.kind, ms, logged));
            (Msg { path: job.path().to_path_buf(), kind: job.kind, ms, outcome: out }, true)
        },
        |job: &Job, e: String| {
            (Msg { path: job.path().to_path_buf(), kind: job.kind, ms: 0.0, outcome: Err(e) }, true)
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
                    cache.insert(path.clone(), Cached { bytes: d.bytes, used: clock });
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
        s.peak_bytes = s.peak_bytes.max(cache.values().map(|c| c.bytes).sum::<usize>());
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
            take_delivery(
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
                cache.insert(path.clone(), Cached { bytes: d.bytes, used: clock });
                if let Some(t) = pending_since.remove(&path) {
                    s.ttfp_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                }
            }
        }
        if left == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "the queue never drained: {left} outstanding");
        std::thread::sleep(Duration::from_millis(2));
    }
    s.wall_ms = t_start.elapsed().as_secs_f64() * 1000.0;
    s.dropped = image_viewer::perf::counter(image_viewer::perf::C_JOBS_DROPPED);
    s.runs = cache.len();
    s
}

pub fn drain(
    rx: &std::sync::mpsc::Receiver<Msg>,
    log: &Arc<Mutex<Vec<Delivery>>>,
) -> Vec<Delivery> {
    let _ = rx.try_iter().count();
    let mut out = std::mem::take(&mut *log.lock().unwrap());
    // Keep anything that arrived but was not yet taken.
    out.extend(rx.try_iter().map(|m| (m.path, m.kind, m.ms, m.outcome)));
    out.sort_by_key(|(p, _, _, _)| p.clone());
    out
}

/// Drop least-recently-painted previews once the count cap is hit, never
/// touching the current picture or its preload window — the policy the app uses.
pub fn evict(
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
pub fn take_delivery(
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
                cache.insert(path.clone(), Cached { bytes: d.bytes, used: *clock });
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
pub fn med(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

/// Total bytes under a directory, one level of fanout as the cache uses.
pub fn dir_bytes(root: &std::path::Path) -> u64 {
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
