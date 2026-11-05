//! The parametric sweeps: the same session at every radius and cache size.

use crate::harness::*;
use image_viewer::{
    ImgKey, Job, Kind, Loader, Plan, jpeg_comment, load_image_full, load_image_preview_full,
    plan_jobs,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    full_dwell_ms: u64,
) -> Session {
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
                Kind::Full => load_image_full(job.path(), 0)
                    .map(|r| Decoded { bytes: r.rgba.len(), below_target: false })
                    .map_err(|e| e.to_string()),
                Kind::Exif => {
                    let _ = nom_exif::read_exif(job.path());
                    if let Ok(mut f) = std::fs::File::open(job.path()) {
                        let mut head = Vec::new();
                        let _ = f.by_ref().take(262144).read_to_end(&mut head);
                        let _ = jpeg_comment(&head);
                    }
                    Ok(Decoded { bytes: 0, below_target: false })
                }
                _ => Err("benchmark only decodes previews, full, and reads exif".to_string()),
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
    let mut pending_since: HashMap<PathBuf, Instant> = HashMap::new();
    let mut failed: HashSet<(ImgKey, Kind)> = HashSet::new();

    let mut index = 0usize;
    let t_start = Instant::now();
    let mut pending_full: HashMap<PathBuf, Instant> = HashMap::new();
    for step in 0..n {
        // Take delivery of anything that finished
        for (path, kind, ms, outcome) in drain(&rx, &decode_ms) {
            if kind != Kind::Preview {
                s.full_ms += ms;
                s.full_each.push(ms);
                s.full_jobs += 1;
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
                    cache.insert(path.clone(), Cached { bytes: d.bytes, used: clock });
                    if let Some(t) = pending_since.remove(&path) {
                        s.ttfp_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Err(_) => {
                    failed.insert((ImgKey::first(&path), Kind::Preview));
                }
            }
        }
        s.peak_bytes = s.peak_bytes.max(cache.values().map(|c| c.bytes).sum::<usize>());
        evict_parametric(&mut cache, &files, index, n, &mut clock, cache_cap);

        let cur = &files[index];
        if cache.contains_key(cur) {
            s.arrived_cached += 1;
            pending_since.remove(cur);
            pending_full.remove(cur);
            s.ttfp_ms.push(0.0);
        } else {
            s.arrived_decode += 1;
            pending_since.insert(cur.clone(), Instant::now());
            pending_full.insert(cur.clone(), Instant::now());
        }

        // Request full-res decode after full_dwell_ms has elapsed since first seen
        let want_full = if full_dwell_ms > 0 {
            let since_pending =
                pending_full.get(cur).map(|t| t.elapsed()).unwrap_or(Duration::ZERO);
            since_pending >= Duration::from_millis(full_dwell_ms)
        } else {
            true
        };

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
            wants_full: want_full,
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
                if kind == Kind::Full {
                    s.full_ms += ms;
                    s.full_each.push(ms);
                    s.full_jobs += 1;
                } else {
                    s.exif_ms += ms;
                    s.exif_each.push(ms);
                    s.exif_jobs += 1;
                }
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
        let protect: HashSet<&PathBuf> =
            (0..=6).map(|k| (index + n * 2 + k - 3) % n).filter_map(|i| files.get(i)).collect();
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
            if kind == Kind::Full {
                s.full_ms += ms;
                s.full_each.push(ms);
                s.full_jobs += 1;
            } else {
                s.exif_ms += ms;
                s.exif_each.push(ms);
                s.exif_jobs += 1;
            }
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
                cache.insert(path.clone(), Cached { bytes: d.bytes, used: *clock });
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
    full_dwell_ms: u64,
    runs: usize,
) {
    let mut sessions = Vec::new();
    for _ in 0..runs {
        sessions.push(browse_parametric(
            dir,
            hold_ms,
            radius,
            preload_budget_bytes,
            target_dim,
            cache_cap,
            full_dwell_ms,
        ));
    }
    let hits = med(sessions
        .iter()
        .map(|x| x.arrived_cached as f64 / sessions[0].ttfp_ms.len().max(1) as f64 * 100.0)
        .collect());
    let wasted = med(sessions
        .iter()
        .map(|x| {
            let total = x.useful_ms + x.wasted_ms;
            if total > 0.0 { x.wasted_ms / total * 100.0 } else { 0.0 }
        })
        .collect());
    eprintln!(
        "  {:>6}  {:>5}  {:>12}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}",
        hold_ms,
        radius,
        preload_budget_bytes / 1_048_576,
        target_dim,
        cache_cap,
        full_dwell_ms,
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
        "  {:>6}  {:>5}  {:>12}  {:>9}  {:>9}  {:>9}  {:>9}  {:>8}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}",
        "hold_ms",
        "radius",
        "preload_MB",
        "target_px",
        "cache_cap",
        "full_dwell_ms",
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
    let n: usize = std::env::var("IV_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let runs: usize = std::env::var("IV_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let dir = fixture_dir(n);
    let files = files(&dir);
    assert!(files.len() >= n, "fixture is short: {} of {n}", files.len());

    // Warm-up so first-touch page faults don't dominate.
    let _ =
        browse_parametric(&dir, 60, RADIUS, PRELOAD_BUDGET_BYTES, PREVIEW_DIM, CAP, FULL_DWELL_MS);

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
        browse_parametric_row(
            &dir,
            30,
            radius,
            PRELOAD_BUDGET_BYTES,
            PREVIEW_DIM,
            CAP,
            FULL_DWELL_MS,
            runs,
        );
    }
    eprintln!();

    // ===== Sweep 2: Preload budget =====
    eprintln!("  ## 2: Preload budget (byte budget for neighbour preloads)");
    heuristic_header();
    for budget_mb in [64usize, 128, 256, 512, 1024] {
        browse_parametric_row(
            &dir,
            30,
            RADIUS,
            budget_mb as u64 * 1_048_576,
            PREVIEW_DIM,
            CAP,
            FULL_DWELL_MS,
            runs,
        );
    }
    eprintln!();

    // ===== Sweep 3: Preview target dimension =====
    eprintln!("  ## 3: Preview target dimension (panel size in physical px)");
    heuristic_header();
    for target_dim in [1024u32, 2048, 4096, 8192] {
        browse_parametric_row(
            &dir,
            30,
            RADIUS,
            PRELOAD_BUDGET_BYTES,
            target_dim,
            CAP,
            FULL_DWELL_MS,
            runs,
        );
    }
    eprintln!();

    // ===== Sweep 4: Preview cache cap =====
    eprintln!("  ## 4: Preview cache cap (max entries in preview cache)");
    heuristic_header();
    for cap in [4usize, 8, 16, 32] {
        browse_parametric_row(
            &dir,
            30,
            RADIUS,
            PRELOAD_BUDGET_BYTES,
            PREVIEW_DIM,
            cap,
            FULL_DWELL_MS,
            runs,
        );
    }
    eprintln!();

    // ===== Sweep 5: Hold time (navigation speed) =====
    eprintln!("  ## 5: Hold time between steps (navigation speed)");
    heuristic_header();
    for hold_ms in [0u64, 15, 30, 60, 120, 250, 500] {
        browse_parametric_row(
            &dir,
            hold_ms,
            RADIUS,
            PRELOAD_BUDGET_BYTES,
            PREVIEW_DIM,
            CAP,
            FULL_DWELL_MS,
            runs,
        );
    }
    eprintln!();

    // ===== Sweep 6: Full dwell time (when to start full-res decode) =====
    eprintln!("  ## 6: Full dwell time (ms before starting full-res decode)");
    heuristic_header();
    for full_dwell_ms in [0u64, 60, 120, 250, 500, 1000] {
        browse_parametric_row(
            &dir,
            30,
            RADIUS,
            PRELOAD_BUDGET_BYTES,
            PREVIEW_DIM,
            CAP,
            full_dwell_ms,
            runs,
        );
    }
    eprintln!();

    eprintln!("  ## End of heuristic sweep");
}

pub fn full_dwell_sweep(dir: &std::path::Path, runs: usize) {
    let n: usize = std::env::var("IV_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let files = files(&dir);
    assert!(files.len() >= n, "fixture is short: {} of {n}", files.len());

    // Warm-up
    let _ =
        browse_parametric(&dir, 60, RADIUS, PRELOAD_BUDGET_BYTES, PREVIEW_DIM, CAP, FULL_DWELL_MS);

    eprintln!(
        "\n  Full dwell sweep: {} pictures at {W}x{H}, {} images, median of {} runs",
        n,
        files.len(),
        runs
    );
    eprintln!(
        "  Varying full-res decode dwell time (ms): how long to wait before full-res decode starts"
    );
    eprintln!(
        "  {:>6}  {:>12}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}  {:>9}",
        "hold_ms",
        "full_dwell_ms",
        "ttfp_p50_ms",
        "ttfp_p95_ms",
        "cache_hit%",
        "full_ms",
        "peak_MB",
        "wall_s",
    );
    eprintln!("  {}", "-".repeat(120));

    for full_dwell_ms in [0u64, 60, 120, 250, 500, 1000] {
        browse_parametric_row(
            &dir,
            30,
            RADIUS,
            PRELOAD_BUDGET_BYTES,
            PREVIEW_DIM,
            CAP,
            full_dwell_ms,
            runs,
        );
    }
    eprintln!();
}
