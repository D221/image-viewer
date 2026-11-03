//! Module `sorting`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use sorting::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Case-insensitive "natural" string comparison: runs of digits compare
/// numerically, so `IMG_10` sorts after `IMG_2` like Explorer does.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    while i < ab.len() && j < bb.len() {
        let (ca, cb) = (ab[i].to_ascii_lowercase(), bb[j].to_ascii_lowercase());
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let mut ia = i;
            let mut ja = j;
            while ia < ab.len() && ab[ia].is_ascii_digit() {
                ia += 1;
            }
            while ja < bb.len() && bb[ja].is_ascii_digit() {
                ja += 1;
            }
            // Numeric compare: fewer significant digits first, then by value,
            // then shorter digit runs (a1 before a01). Compare the digit-run
            // slices directly instead of collecting each run into a Vec.
            let na = trim_leading_zeros(&ab[i..ia]);
            let nb = trim_leading_zeros(&bb[j..ja]);
            let ord = na
                .len()
                .cmp(&nb.len())
                .then_with(|| na.cmp(nb))
                .then_with(|| (ia - i).cmp(&(ja - j)));
            if ord != Ordering::Equal {
                return ord;
            }
            i = ia;
            j = ja;
        } else if ca != cb {
            return ca.cmp(&cb);
        } else {
            i += 1;
            j += 1;
        }
    }
    (ab.len() - i).cmp(&(bb.len() - j))
}

pub(crate) fn trim_leading_zeros(digits: &[u8]) -> &[u8] {
    let first = digits.iter().position(|&c| c != b'0').unwrap_or(digits.len());
    &digits[first..]
}

/// Scan and sort with the given mode. Date sorts newest-first, Size
/// largest-first, Random shuffles once per call.
pub fn scan_folder_sorted(folder: &Path, mode: SortMode) -> Vec<PathBuf> {
    scan_folder_sorted_dir(folder, mode, false)
}

/// Gallery order with an explicit direction.
///
/// `descending == false` is exactly [`scan_folder_sorted`]: Name natural,
/// Date newest-first, Size largest-first, Random shuffled. `true` reverses
/// whichever of those the mode produced — including Random, where the reverse
/// of a shuffle is still a shuffle. A reversal rather than per-mode
/// ascending/descending semantics, so no shipped order changes meaning.
pub fn scan_folder_sorted_dir(folder: &Path, mode: SortMode, descending: bool) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && is_browsable(&p) {
                files.push(p);
            }
        }
    }
    apply_sort_order(&mut files, mode, descending);
    files
}

/// Same, over the whole subtree. Name order compares full relative paths
/// naturally, so each folder's files group together in natural order.
pub fn scan_folder_sorted_recursive(
    folder: &Path,
    mode: SortMode,
    descending: bool,
) -> Vec<PathBuf> {
    let mut files = scan_folder_recursive(folder);
    apply_sort_order(&mut files, mode, descending);
    files
}

/// The sort half of both scans above, so recursive and flat can never
/// disagree about what an order means.
pub(crate) fn apply_sort_order(files: &mut [PathBuf], mode: SortMode, descending: bool) {
    match mode {
        SortMode::Name => {
            files.sort_by(|a, b| {
                let an = a.file_name().unwrap_or_default().to_string_lossy();
                let bn = b.file_name().unwrap_or_default().to_string_lossy();
                natural_cmp(&an, &bn)
            });
        }
        SortMode::Date => {
            files.sort_by_cached_key(|p| {
                std::fs::metadata(p).and_then(|m| m.modified()).map(std::cmp::Reverse).ok()
            });
        }
        SortMode::Size => {
            files.sort_by_cached_key(|p| {
                std::fs::metadata(p).map(|m| std::cmp::Reverse(m.len())).ok()
            });
        }
        SortMode::Random => shuffle(files),
        SortMode::Taken => {
            // Newest capture date first, from EXIF `DateTimeOriginal`. Files
            // without a capture date sort oldest (empty string sorts first).
            // The date string "YYYY:MM:DD HH:MM:SS" sorts lexicographically,
            // so `Reverse` on it gives newest first. Each file's EXIF is read
            // once per sort (cached by `sort_by_cached_key`); the persisted
            // shape cache makes this cheaper for files that have been viewed,
            // but the first sort of a large folder still pays the EXIF read.
            files.sort_by_cached_key(|p| std::cmp::Reverse(exif_date_taken(p)));
        }
    }
    if descending {
        files.reverse();
    }
}

/// Tiny xorshift shuffle (avoids a `rand` dependency). Public so a shuffled
/// slideshow can reuse the same order Random sort gets, rather than a second
/// implementation that could disagree with it about what "shuffled" means.
pub fn shuffle(files: &mut [PathBuf]) {
    if files.len() < 2 {
        return;
    }
    let mut s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 8))
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    for i in (1..files.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let j = (s % (i as u64 + 1)) as usize;
        files.swap(i, j);
    }
}
