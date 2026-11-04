//! The launch listing, started before the window can paint.
//!
//! The first frame needs one thing off the file system: the sorted list of
//! images. Everything else on the way to it — creating the window, enumerating
//! the adapter, creating the device and the surface — is measured in the
//! hundreds of milliseconds and touches neither the folder nor the CPU the walk
//! needs. Running the walk *after* all of that made the two costs add up; run on
//! a thread first, they overlap and the total is the larger of the two instead
//! of the sum.
//!
//! Measured on the development machine (release, 1405-image folder, warm cache):
//! window + GL context = ~150 ms, the sorted walk = ~30-70 ms, ~600 ms on a cold
//! cache. So the overlap is the whole walk, and a cold launch is where it shows
//! most.
//!
//! **The folder this scans must be the folder the app opens.** A mismatch is not
//! a correctness bug here — the app compares the path before accepting the list
//! and falls back to its own synchronous walk — but it is a silent return to the
//! old behaviour, which is why [`startup_folder`] is the one place the saved
//! folder is resolved and why `tests/startup_scan.rs` pins it.

use crate::*;
use std::sync::mpsc;

/// The folder a launch with no path argument browses.
///
/// The saved folder when it still exists, so a launch reopens what the last
/// session had; `test-images`, relative to the working directory, otherwise —
/// the folder the project ships fixtures in, so a fresh checkout has something
/// to show rather than an empty window.
///
/// The existence check is the point: `load_settings` can leave a folder that was
/// moved or unmounted, and opening it would show an empty gallery with no way to
/// tell "gone" from "empty".
pub fn startup_folder(saved: Option<&str>) -> PathBuf {
    saved.map(PathBuf::from).filter(|p| p.is_dir()).unwrap_or_else(|| PathBuf::from("test-images"))
}

/// Start the launch listing on a worker thread and hand back its result.
///
/// The caller is expected to be the binary's `main`, before the window exists;
/// the receiver is drained by the app creator. The thread does exactly what the
/// synchronous path does — same two scan functions, same sort arguments — so the
/// list cannot differ depending on whether the prefetch won the race.
pub fn prefetch_folder_scan(
    folder: &Path,
    sort: SortMode,
    descending: bool,
    recursive: bool,
) -> mpsc::Receiver<Vec<PathBuf>> {
    let (tx, rx) = mpsc::channel();
    let folder = folder.to_path_buf();
    std::thread::spawn(move || {
        let files = scan_folder_for_startup(&folder, sort, descending, recursive);
        // A closed receiver means the app decided not to wait for this list
        // (a mismatched folder, or a renderer restart). Dropping the result is
        // the correct outcome; there is nothing to report it to.
        let _ = tx.send(files);
    });
    rx
}

/// The walk itself, shared by the prefetch above and the synchronous fallback in
/// the binary, so the two cannot drift into different orders or filters.
pub fn scan_folder_for_startup(
    folder: &Path,
    sort: SortMode,
    descending: bool,
    recursive: bool,
) -> Vec<PathBuf> {
    if recursive {
        scan_folder_sorted_recursive(folder, sort, descending)
    } else {
        scan_folder_sorted_dir(folder, sort, descending)
    }
}
