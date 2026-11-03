// Shared helper for the criterion benches.
//
// `test-images/` is gitignored, so a fresh clone has no images at all. The
// benches used to hardcode `test-images\booklet_front.jpg` and die on an
// `unwrap` panic with "No such file or directory", which reads like a bug in the
// viewer rather than a missing fixture. They also measure a *real* large photo,
// so generating a synthetic image would quietly change what they report - hence
// skip-with-a-message rather than self-generate.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

/// Largest JPEG in `test-images`, or `None` if there are none.
pub fn largest_jpeg(dir: &str) -> Option<PathBuf> {
    let mut best: Option<(u64, PathBuf)> = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if !p
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.eq_ignore_ascii_case("jpg") || s.eq_ignore_ascii_case("jpeg"))
            .unwrap_or(false)
        {
            continue;
        }
        let len = e.metadata().map(|m| m.len()).unwrap_or(0);
        if best.as_ref().is_none_or(|(b, _)| len > *b) {
            best = Some((len, p));
        }
    }
    best.map(|(_, p)| p)
}

/// A large JPEG to benchmark, or `None` after explaining why there isn't one.
pub fn bench_image() -> Option<PathBuf> {
    match largest_jpeg("test-images") {
        Some(p) => Some(p),
        None => {
            eprintln!(
                "\n  skipping: test-images/ is gitignored and this machine has no JPEG in it.\n  \
                 Drop a large photo (a few MB is plenty) into test-images/ and re-run to benchmark.\n"
            );
            None
        }
    }
}

/// True when `test-images/` holds at least one image, so the folder-scanning
/// benches measure something real.
pub fn have_folder() -> bool {
    Path::new("test-images").read_dir().map(|mut d| d.next().is_some()).unwrap_or(false)
}
