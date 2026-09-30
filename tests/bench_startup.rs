//! Where the start-up time actually goes.
//!
//! `bench_launch.ps1` says the app takes ~1.4 s longer than JPEGView to settle on
//! a 1001-image folder, and the `--perf` report says the decode itself is 7-48 ms.
//! So ~490 ms is spent somewhere that is not decoding, and this measures the
//! candidates rather than guessing at them.
//!
//! `#[ignore]`d like the other benches: it reads a real folder on disk.
//!
//! Run:
//!   cargo test --release --test bench_startup -- --ignored --nocapture
//!   IV_BENCH_FOLDER=<folder> cargo test --release --test bench_startup -- --ignored --nocapture

use image_viewer::{scan_folder_sorted, Config, Gallery, SortMode};
use std::path::PathBuf;
use std::time::Instant;

/// The folder to measure. An env var, not `args().nth(2)`: libtest's own flags sit
/// in the argument list too, and `--nocapture` was being taken for a path.
fn folder() -> PathBuf {
    std::env::var("IV_BENCH_FOLDER")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("C:/Users/user/Downloads/phone_backup/Download/Ttg"))
}

/// Median of `n` runs, because a single sample of anything touching a filesystem
/// is mostly noise.
fn time_ms<F: FnMut()>(n: usize, mut f: F) -> f64 {
    let mut xs = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        f();
        xs.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs[n / 2]
}

#[test]
#[ignore = "reads a real folder from disk and prints a report"]
fn where_does_startup_go() {
    let dir = folder();
    if !dir.is_dir() {
        panic!("not a folder: {}", dir.display());
    }
    let n = std::fs::read_dir(&dir).unwrap().count();

    println!("\n  start-up breakdown: {}\n  {n} entries\n", dir.display());

    // 1. The scan, on its own. This is what the app does once per open.
    let scan = time_ms(7, || {
        std::hint::black_box(scan_folder_sorted(&dir, SortMode::Name));
    });
    println!("  {:<38} {:>7.1} ms", "scan_folder_sorted (name)", scan);

    // 2. The other sort modes, because two of them stat every file *during* the
    //    sort, which is a different cost profile from the name order.
    for mode in [SortMode::Date, SortMode::Size, SortMode::Random] {
        let ms = time_ms(5, || {
            std::hint::black_box(scan_folder_sorted(&dir, mode));
        });
        println!("  {:<38} {:>7.1} ms", format!("scan_folder_sorted ({mode:?})"), ms);
    }

    // 3. `load_all_on_startup` calls `Gallery::load_all_from_folder`, which calls
    //    `scan_folder` *again*. So the default path scans twice before the window
    //    is even up. This is what that second scan costs.
    let mut g = Gallery::new(&dir);
    g.load_all_from_folder(&dir);
    let second = time_ms(5, || {
        let mut g = Gallery::new(&dir);
        g.load_all_from_folder(&dir);
        std::hint::black_box(&g);
    });
    println!("  {:<38} {:>7.1} ms", "Gallery::load_all_from_folder", second);
    println!(
        "  {:<38} {:>7.1} ms   (startup pays the scan twice)",
        "  -> duplicate scan",
        second
    );

    // 4. Re-stat every file. This is what the 2 s folder watcher does, and it runs
    //    on the UI thread; if it is slow it can collide with a decode.
    let files: Vec<PathBuf> = scan_folder_sorted(&dir, SortMode::Name);
    let restat = time_ms(5, || {
        let mut acc = 0u64;
        for f in &files {
            if let Ok(m) = std::fs::metadata(f) {
                acc = acc.wrapping_add(m.len());
            }
        }
        std::hint::black_box(acc);
    });
    println!("  {:<38} {:>7.1} ms", "re-stat all files (watcher poll)", restat);

    // 5. The resume lookup, which is a linear scan over the file list comparing
    //    file names. Cheap, but it is on the same path.
    println!();

    // 6. Config load + save. Small, but it is JSON on the startup path.
    let cfg_path = Config::default_path();
    let load = time_ms(20, || {
        std::hint::black_box(std::fs::read_to_string(&cfg_path).ok());
    });
    println!("  {:<38} {:>7.1} ms", "read config.json", load);
    println!("  {:<38} {:>7}", "config path", cfg_path.display());
    println!();
    println!("  A decode is 7-48 ms (see --perf), so anything here in the hundreds of");
    println!("  milliseconds is time the first picture is waiting on something else.\n");
}
