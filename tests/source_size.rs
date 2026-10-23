//! The size of the source files, and the rule that keeps them from growing.
//!
//! **A source file should be under [`TARGET`] lines.** That is not a style
//! preference: `src/lib.rs` at 13k lines is one file where every rule in this
//! crate lives, so a change to any of them is a change to a file nobody can
//! hold in their head, and the sections drift into each other.
//!
//! Two checks, and the second is the one that does the work:
//!
//! * **A new file starts under the target.** Every file in `src/`, `tests/` and
//!   `examples/` that is not listed below must be under [`TARGET`] lines. A
//!   new topic gets a new module (`src/<topic>.rs`, `pub mod` + `pub use` from
//!   the crate root) — which is the pattern this crate already uses for
//!   `perf`, `icon` and `ratings`.
//! * **The files over the target do not grow.** The numbers in
//!   [`OVER_TARGET`] are what those files are today, and they are **ceilings**:
//!   a file may be edited, but it may not get bigger. To add to one, put the
//!   new code in a module of its own; to shrink one, split it and lower its
//!   number here.
//!
//! **A ratchet, not a wish.** A test that asserted "every file is under 1000
//! lines" would be red on the day it was written and would be deleted rather
//! than fixed. This one is green now and gets *harder* to keep green as the
//! files come down: the list only ever shortens, and a number that is raised
//! is a decision somebody wrote down (with a reason) rather than a default.
//!
//! The ceilings are exact, so a change that adds a line to `src/lib.rs` fails
//! here — which is the point. The fix is a new module or a split, not a bigger
//! number.

use std::path::{Path, PathBuf};

/// The size a source file should be under.
const TARGET: usize = 1000;

/// The files that are over [`TARGET`], and the size each may not exceed.
///
/// **To split one:** move a whole section into its own module
/// (`src/<topic>.rs` or `tests/<topic>.rs`), declare it where the old code
/// lived (`pub mod <topic>;` plus `pub use <topic>::*;` for the crate root, so
/// `image_viewer::name` keeps working), and lower the number here to the file's
/// new size. **To add to one without splitting it:** the new code goes in a
/// module of its own instead.
const OVER_TARGET: &[(&str, usize)] = &[
    ("src/main.rs", 10_713),
    ("tests/unit.rs", 7_648),
    ("tests/spreads.rs", 7_384),
    ("tests/rotate_save.rs", 1_921),
    ("tests/flip_through.rs", 1_647),
    ("tests/keybindings.rs", 1_386),
    ("tests/bench_browse.rs", 1_309),
    ("tests/thumb_cache.rs", 1_121),
];

/// The directories a source file can live in.
const SOURCE_DIRS: &[&str] = &["src", "tests", "examples"];

/// Every source file in the tree, by path relative to the crate root.
fn source_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for dir in SOURCE_DIRS {
        let dir = root.join(dir);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The lines in a file, counted the way the numbers above were counted.
fn line_count(path: &Path) -> usize {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .count()
}

/// The path of a file relative to the crate root, as the table spells it.
fn relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// **A file over the target does not grow.**
///
/// The ceilings are the sizes those files are today. A file that has been
/// split comes off this list with a lower number, and a file that is edited
/// without growing keeps its number.
#[test]
fn the_files_over_the_target_do_not_grow() {
    for (name, ceiling) in OVER_TARGET {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        assert!(
            path.exists(),
            "{name} is in the over-target list but does not exist: remove it here, or \
             put the file back"
        );
        let lines = line_count(&path);
        assert!(
            lines <= *ceiling,
            "{name} has grown from {ceiling} to {lines} lines, and it is already over \
             the {TARGET}-line target. Put the new code in a module of its own \
             (`src/<topic>.rs`, declared with `pub mod` and re-exported), or split \
             this file and lower its number here."
        );
    }
}

/// **A file that is not on the list is under the target.**
///
/// This is the rule that shapes the *next* file rather than the last one, and
/// it is the half that needs no exception: a new module is new, and it starts
/// small.
#[test]
fn a_new_source_file_starts_under_the_target() {
    for path in source_files() {
        let name = relative(&path);
        if OVER_TARGET.iter().any(|(n, _)| *n == name) {
            continue;
        }
        let lines = line_count(&path);
        assert!(
            lines < TARGET,
            "{name} is {lines} lines, over the {TARGET}-line target, and it is not on \
             the over-target list. Either split it, or list it with its size so the \
             ratchet has something to hold."
        );
    }
}

/// **The list only ever shortens.**
///
/// A file that has come down under the target must come off the list: leaving
/// it there would keep a ceiling on a file that no longer needs one, and the
/// list would become a list of everything rather than a list of what is still
/// too big.
#[test]
fn a_file_under_the_target_comes_off_the_list() {
    for (name, _) in OVER_TARGET {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(name);
        let lines = line_count(&path);
        assert!(
            lines >= TARGET,
            "{name} is {lines} lines, under the {TARGET}-line target: take it off the \
             over-target list."
        );
    }
}
