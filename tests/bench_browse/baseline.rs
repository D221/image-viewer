//! What flipping through a folder costs, and what sitting still costs.

use crate::harness::*;
use crate::sweep::full_dwell_sweep;
use image_viewer::{
    ImgKey, Job, Kind, Loader, Plan, jpeg_comment, load_image_preview_full, plan_jobs,
};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn browse_baseline() {
    let n: usize = std::env::var("IV_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let runs: usize = std::env::var("IV_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let dir = fixture_dir(n);
    let files = files(&dir);
    eprintln!(
        "\n  browse benchmark: {} pictures at {W}x{H}, {n} images, median of {runs}",
        files.len()
    );
    eprintln!(
        "  {:>6}  {:>8}  {:>9}  {:>9}  {:>8}  {:>8}  {:>10}  {:>9}  {:>9}",
        "hold",
        "wall s",
        "ttfp p50",
        "ttfp p95",
        "wasted",
        "peak MB",
        "cache hit",
        "decode s",
        "below tgt"
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
        let hits: Vec<f64> = s.iter().map(|x| x.arrived_cached as f64 / n as f64 * 100.0).collect();
        let wasted: Vec<f64> = s
            .iter()
            .map(|x| {
                let total = x.useful_ms + x.wasted_ms;
                if total > 0.0 { x.wasted_ms / total * 100.0 } else { 0.0 }
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
        "hold",
        "arm",
        "ttfp p50",
        "ttfp p95",
        "cache hit",
        "wasted %",
        "decode s",
        "peak MB",
        "wall s"
    );
    eprintln!("  {}", "-".repeat(96));

    let mut cold: Vec<Session> = Vec::new();
    let mut warm: Vec<Session> = Vec::new();

    for i in 0..runs {
        // Arm A: start fast immediately (no idle warm-up).
        cold.push(browse_idle(dir, hold_ms, false));

        // Arm B: idle at image 0 first, then fast navigation.
        warm.push(browse_idle(dir, hold_ms, true));

        // Alternate which arm runs first so machine drift doesn't invent a win.
        if i % 2 == 1 {
            std::mem::swap(&mut cold, &mut warm);
        }
    }

    let row = |arm: &str, s: &Vec<Session>| {
        let hits = med(s.iter().map(|x| x.arrived_cached as f64 / n as f64 * 100.0).collect());
        let wasted = med(s
            .iter()
            .map(|x| {
                let total = x.useful_ms + x.wasted_ms;
                if total > 0.0 { x.wasted_ms / total * 100.0 } else { 0.0 }
            })
            .collect());
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
                        cache.insert(path.clone(), Cached { bytes: d.bytes, used: clock });
                    }
                    Err(_e) => {
                        failed.insert((ImgKey::first(&path), Kind::Preview));
                    }
                }
            }
            if left == 0 {
                break;
            }
            assert!(Instant::now() < deadline, "idle warm-up never drained: {left} outstanding");
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
                    cache.insert(path.clone(), Cached { bytes: d.bytes, used: clock });
                    if let Some(t) = pending_since.remove(&path) {
                        s.ttfp_ms.push(t.elapsed().as_secs_f64() * 1000.0);
                    }
                }
                Err(_e) => {
                    failed.insert((ImgKey::first(&path), Kind::Preview));
                }
            }
        }
        s.peak_bytes = s.peak_bytes.max(cache.values().map(|c| c.bytes).sum::<usize>());
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

#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn idle_warmup_bench() {
    let n: usize = std::env::var("IV_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    let runs: usize = std::env::var("IV_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    let dir = fixture_dir(n);

    // Warm-up so first-touch page faults don't dominate.
    let _ = browse(&dir, 60, false);

    idle_warmup(&dir, 0, runs);
    idle_warmup(&dir, 30, runs);

    full_dwell_sweep(&dir, runs);
}
