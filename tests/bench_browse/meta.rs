//! The metadata read, the thumbnail cache, and what Explorer's cache holds.

use crate::harness::*;
use image_viewer::jpeg_comment;
use std::io::Read;
use std::path::PathBuf;
use std::time::Instant;

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
    let n: usize = std::env::var("IV_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let runs: usize = std::env::var("IV_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(9);
    let dir = fixture_dir_at(n, EXIF_W, EXIF_H, EXIF_QUALITY);
    let files = files(&dir);
    assert!(files.len() >= n, "fixture is short: {} of {n}", files.len());

    // Warm-up, both arms: page cache and lazily faulted JPEG tables.
    let _ = browse(&dir, 60, true);
    let _ = browse(&dir, 60, false);

    // Cost of one metadata read, and what it is worth next to a decode.
    let probe = browse(&dir, 400, true);
    let per_read = med(probe.exif_each.clone());
    let per_decode = if probe.runs > 0 { probe.decode_ms / probe.runs as f64 } else { 0.0 };
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
        "hold",
        "arm",
        "ttfp p50",
        "ttfp mean",
        "ttfp p95",
        "cache hit",
        "cached",
        "exif s",
        "wasted",
        "wall s"
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
            let wasted = med(s
                .iter()
                .map(|x| {
                    let total = x.useful_ms + x.wasted_ms;
                    if total > 0.0 { x.wasted_ms / total * 100.0 } else { 0.0 }
                })
                .collect());
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
    let n: usize = std::env::var("IV_BENCH_N").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let runs: usize = std::env::var("IV_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(5);
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
            assert!(d.width.max(d.height) <= 256, "a cell came back {}x{}", d.width, d.height);
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
            p.is_file() && std::fs::metadata(p).map(|m| m.len() > 8_000_000).unwrap_or(false)
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
