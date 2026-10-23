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
            let sa: Vec<u8> = ab[i..]
                .iter()
                .copied()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            i += sa.len();
            let sb: Vec<u8> = bb[j..]
                .iter()
                .copied()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            j += sb.len();
            // Numeric compare: fewer significant digits first, then by value,
            // then shorter digit runs (a1 before a01).
            let na = trim_leading_zeros(&sa);
            let nb = trim_leading_zeros(&sb);
            let ord = na
                .len()
                .cmp(&nb.len())
                .then_with(|| na.cmp(nb))
                .then_with(|| sa.len().cmp(&sb.len()));
            if ord != Ordering::Equal {
                return ord;
            }
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
    let mut files = scan_folder(folder);
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
        SortMode::Name => {}
        SortMode::Date => {
            files.sort_by_cached_key(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .map(std::cmp::Reverse)
                    .ok()
            });
        }
        SortMode::Size => {
            files.sort_by_cached_key(|p| {
                std::fs::metadata(p).map(|m| std::cmp::Reverse(m.len())).ok()
            });
        }
        SortMode::Random => shuffle(files),
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

