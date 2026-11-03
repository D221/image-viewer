//! Gallery filtering and grid paging.
//!
//! The filter is the part most likely to be subtly wrong — a wrong index here
//! shows the wrong image, or an out-of-range one — so it is tested through the
//! real `Gallery::set_filtered` rather than a reimplementation.

use image_viewer::{Gallery, file_name_of};
use std::path::{Path, PathBuf};

fn p(s: &str) -> PathBuf {
    PathBuf::from("/photos").join(s)
}

fn gallery(names: &[&str]) -> Gallery {
    let mut g = Gallery::from_file(p(names[0]));
    g.files = names.iter().map(|n| p(n)).collect();
    g
}

#[test]
fn test_loop_back_on_by_default_wraps_at_both_ends() {
    // The historical behaviour, and what an existing config with no
    // `loop_back` key must keep getting.
    let mut g = gallery(&["a.jpg", "b.jpg", "c.jpg"]);
    assert!(g.loop_back, "looping must default to on");

    assert!(g.next());
    assert!(g.next());
    assert_eq!(g.index, 2);
    assert!(g.next(), "past the end it wraps to the first");
    assert_eq!(g.index, 0);
    assert!(g.prev(), "before the start it wraps to the last");
    assert_eq!(g.index, 2);
}

#[test]
fn test_loop_back_off_stops_at_both_ends() {
    let mut g = gallery(&["a.jpg", "b.jpg", "c.jpg"]);
    g.loop_back = false;

    assert!(g.next());
    assert!(g.next());
    assert_eq!(g.index, 2);
    assert!(!g.next(), "stepping past the last image must not move");
    assert_eq!(g.index, 2, "and must stay on the last image");
    // Repeatedly pressing must stay put, not creep or wrap.
    for _ in 0..5 {
        assert!(!g.next());
    }
    assert_eq!(g.index, 2);

    assert!(g.prev());
    assert!(g.prev());
    assert_eq!(g.index, 0);
    assert!(!g.prev(), "stepping before the first image must not move");
    assert_eq!(g.index, 0, "and must stay on the first image");
}

#[test]
fn test_loop_back_off_is_safe_on_an_empty_gallery() {
    // No files at all: nothing to move to, and nothing may panic.
    let mut g = Gallery::from_file(p("only.jpg"));
    g.files.clear();
    g.loop_back = false;
    assert!(!g.next());
    assert!(!g.prev());
    assert!(g.is_empty());
}

#[test]
fn test_loop_back_off_with_a_single_image_never_moves() {
    let mut g = gallery(&["only.jpg"]);
    g.loop_back = false;
    assert!(!g.next());
    assert!(!g.prev());
    assert_eq!(g.index, 0);
    assert_eq!(file_name_of(g.current().unwrap()), "only.jpg");
}

#[test]
fn test_filter_is_a_case_insensitive_name_substring() {
    let mut g = gallery(&["Beach.jpg", "beach2.JPG", "Mountain.png", "portrait.jpeg"]);
    g.set_filtered(&g.files.clone(), "beach");
    let names: Vec<String> = g.files.iter().map(|f| file_name_of(f)).collect();
    assert_eq!(names, vec!["Beach.jpg", "beach2.JPG"]);

    // Extension is part of the name, so it matches too.
    g.set_filtered(
        &[p("Beach.jpg"), p("beach2.JPG"), p("Mountain.png"), p("portrait.jpeg")],
        ".jpeg",
    );
    let names: Vec<String> = g.files.iter().map(|f| file_name_of(f)).collect();
    assert_eq!(names, vec!["portrait.jpeg"]);
}

#[test]
fn test_empty_and_whitespace_filters_show_everything() {
    let all = vec![p("a.jpg"), p("b.jpg"), p("c.png")];
    for needle in ["", "   ", "\t"] {
        let mut g = gallery(&["a.jpg"]);
        g.set_filtered(&all, needle);
        assert_eq!(g.len(), 3, "needle {needle:?} must not filter anything out");
    }
}

#[test]
fn test_filter_that_matches_nothing_leaves_the_index_valid() {
    // The dangerous case: `files` is empty, so `index` must be clamped rather
    // than left pointing past the end (which `current()` would hide but every
    // `next()` would misbehave on).
    let all = vec![p("a.jpg"), p("b.jpg")];
    let mut g = gallery(&["a.jpg", "b.jpg"]);
    g.index = 1;
    let changed = g.set_filtered(&all, "zzzz-no-such-file");
    assert!(g.is_empty());
    assert_eq!(g.index, 0);
    assert!(g.current().is_none());
    assert!(changed, "the current image did not survive, so the caller must re-goto");
    // And navigating an empty gallery must not panic.
    g.next();
    g.prev();
    assert!(g.is_empty());
}

#[test]
fn test_filter_keeps_the_current_image_when_it_survives() {
    // All three names contain "c", so the list is unchanged and the index must
    // not move.
    let all = vec![p("1c.jpg"), p("2c.jpg"), p("3c.jpg")];
    let mut g = gallery(&["1c.jpg", "2c.jpg", "3c.jpg"]);
    g.index = 2; // 3c.jpg
    let changed = g.set_filtered(&all, "c");
    assert_eq!(file_name_of(g.current().unwrap()), "3c.jpg");
    assert!(!changed, "3c.jpg survived, so no re-goto is needed");
    assert_eq!(g.index, 2, "a surviving image must keep its position");
}

#[test]
fn test_filter_falls_back_to_the_nearest_surviving_index() {
    let all = vec![p("a1.jpg"), p("a2.jpg"), p("a3.jpg"), p("b1.jpg")];
    let mut g = gallery(&["a1.jpg", "a2.jpg", "a3.jpg", "b1.jpg"]);
    g.index = 3; // b1.jpg, which the filter removes
    let changed = g.set_filtered(&all, "a");
    assert!(changed, "the current image did not survive");
    assert_eq!(g.len(), 3);
    // Index 3 is clamped to the last survivor; it must be in range and must
    // actually be one of the files that survived.
    assert!(g.index < g.len(), "index must be clamped, never out of range");
    let names: Vec<String> = g.files.iter().map(|f| file_name_of(f)).collect();
    assert!(
        names.contains(&file_name_of(g.current().unwrap())),
        "current must be a surviving file"
    );
    assert_eq!(file_name_of(g.current().unwrap()), "a3.jpg");
}

#[test]
fn test_clearing_the_filter_restores_the_whole_folder() {
    let all = vec![p("a.jpg"), p("b.jpg"), p("c.jpg"), p("d.png")];
    let mut g = gallery(&["a.jpg", "b.jpg", "c.jpg", "d.png"]);
    g.set_filtered(&all, "b");
    assert_eq!(g.len(), 1);
    g.set_filtered(&all, "");
    assert_eq!(g.len(), 4, "clearing the filter must restore every file");
    // Order is the unfiltered order, not the filtered one.
    let names: Vec<String> = g.files.iter().map(|f| file_name_of(f)).collect();
    assert_eq!(names, vec!["a.jpg", "b.jpg", "c.jpg", "d.png"]);
}

#[test]
fn test_filter_survives_a_growing_folder() {
    // The watcher rebuilds the unfiltered list and re-applies the filter, so a
    // new file that matches must appear without the user retyping.
    let mut all = vec![p("a1.jpg"), p("a2.jpg")];
    let mut g = gallery(&["a1.jpg", "a2.jpg"]);
    g.set_filtered(&all, "a");
    assert_eq!(g.len(), 2);
    all.push(p("a3.jpg"));
    g.set_filtered(&all, "a");
    assert_eq!(g.len(), 3);
    all.push(p("zzz.jpg"));
    g.set_filtered(&all, "a");
    assert_eq!(g.len(), 3, "a non-matching new file stays hidden");
}

// --- grid paging -----------------------------------------------------------
//
// Mirrors the arithmetic in `MyApp::update`, as the sibling grid-scroll test
// does, because the grid selection lives on the app rather than in the library.

/// Grid selection movement, in index space.
fn grid_move(sel: usize, last: usize, cols: usize, page_rows: usize, d: Key) -> usize {
    let page = page_rows.max(1) * cols;
    match d {
        Key::PageDown => (sel + page).min(last),
        Key::PageUp => sel.saturating_sub(page),
        Key::Next => (sel + 1).min(last),
        Key::Prev => sel.saturating_sub(1),
        Key::Down => (sel + cols).min(last),
        Key::Up => sel.saturating_sub(cols),
        Key::Home => 0,
        Key::End => last,
    }
}

#[derive(Clone, Copy)]
enum Key {
    PageUp,
    PageDown,
    Next,
    Prev,
    Up,
    Down,
    Home,
    End,
}

#[test]
fn test_grid_paging_moves_a_page_and_clamps_at_both_ends() {
    // 10 columns, 4 rows visible, 137 images.
    let (cols, rows, total) = (10usize, 4usize, 137usize);
    let last = total - 1;
    let page = rows * cols; // 40

    // A page down from the top lands exactly one viewport later.
    assert_eq!(grid_move(0, last, cols, rows, Key::PageDown), page);
    // And back up returns to the top.
    assert_eq!(grid_move(page, last, cols, rows, Key::PageUp), 0);

    // Repeated page-downs must saturate, never run off the end.
    let mut sel = 0;
    for _ in 0..20 {
        sel = grid_move(sel, last, cols, rows, Key::PageDown);
    }
    assert_eq!(sel, last);
    let mut sel = last;
    for _ in 0..20 {
        sel = grid_move(sel, last, cols, rows, Key::PageUp);
    }
    assert_eq!(sel, 0);

    // Home/End still work and are not confused by paging.
    assert_eq!(grid_move(57, last, cols, rows, Key::Home), 0);
    assert_eq!(grid_move(57, last, cols, rows, Key::End), last);

    // A single step still steps one, so Page did not swallow arrow behaviour.
    assert_eq!(grid_move(57, last, cols, rows, Key::Next), 58);
    assert_eq!(grid_move(57, last, cols, rows, Key::Prev), 56);
    assert_eq!(grid_move(57, last, cols, rows, Key::Down), 67);
    assert_eq!(grid_move(57, last, cols, rows, Key::Up), 47);
}

#[test]
fn test_grid_paging_handles_a_partial_last_page_and_a_tiny_folder() {
    // 10 columns, 4 rows visible, 23 images: the last page is partial.
    let (cols, rows, total) = (10usize, 4usize, 23usize);
    let last = total - 1;
    let sel = grid_move(0, last, cols, rows, Key::PageDown);
    assert_eq!(sel, last, "a partial final page must clamp to the last image");

    // Fewer images than one page: every key lands on the only/last image.
    for total in 1..=3usize {
        let last = total - 1;
        for k in [
            Key::PageDown,
            Key::PageUp,
            Key::Next,
            Key::Prev,
            Key::Up,
            Key::Down,
            Key::Home,
            Key::End,
        ] {
            assert!(grid_move(0, last, cols, rows, k) <= last);
        }
    }
}

/// The image files in `dir` (sidecars excluded) whose names match `needle`,
/// through the real `Gallery::set_filtered` rather than a reimplementation.
fn names_matching(dir: &Path, needle: &str) -> Vec<String> {
    let mut all: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().map(|e| e != "xmp").unwrap_or(true))
        .collect();
    all.sort();
    let mut g = Gallery::empty();
    g.set_filtered(&all, needle);
    g.files.iter().map(|f| file_name_of(f)).collect()
}

/// The point of rating at all: being able to navigate by it. A rating you cannot
/// filter on is a number you have to remember.
#[test]
fn the_filter_can_narrow_by_rating_flag_and_tag() {
    let dir = tempfile::tempdir().unwrap();
    for (name, stars, flag, tags) in [
        ("a.jpg", 5u8, true, vec!["holiday"]),
        ("b.jpg", 3, false, vec![]),
        ("c.jpg", 0, false, vec!["holiday", "family"]),
    ] {
        let p = dir.path().join(name);
        std::fs::write(&p, b"\xff\xd8\xff\xe0jpeg").unwrap();
        let r = image_viewer::ratings::Ratings {
            rating: image_viewer::ratings::Rating { stars, flag },
            tags: tags.into_iter().map(String::from).collect(),
            caption: String::new(),
            artist: String::new(),
        };
        r.write(&p).unwrap();
    }

    let all: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.ends_with(".xmp"))
        .collect();
    assert_eq!(all.len(), 3);

    assert_eq!(names_matching(dir.path(), "stars:4"), vec!["a.jpg"]);
    assert_eq!(names_matching(dir.path(), "stars:3+"), vec!["a.jpg", "b.jpg"]);
    assert_eq!(names_matching(dir.path(), "flagged"), vec!["a.jpg"]);
    assert_eq!(names_matching(dir.path(), "tag:family"), vec!["c.jpg"]);
    assert_eq!(names_matching(dir.path(), "tag:HOLIDAY").len(), 2, "case-insensitive");
}

/// Files without a sidecar — and files with a damaged one — degrade to
/// unrated rather than erroring or vanishing: `unflagged` and `stars:0`
/// include them, and a damaged sidecar never hides its image.
#[test]
fn the_filter_treats_missing_and_damaged_sidecars_as_unrated() {
    let dir = tempfile::tempdir().unwrap();
    // Rated and flagged, for contrast.
    let rated = dir.path().join("rated.jpg");
    std::fs::write(&rated, b"\xff\xd8\xff\xe0jpeg").unwrap();
    image_viewer::ratings::Ratings {
        rating: image_viewer::ratings::Rating { stars: 4, flag: true },
        tags: vec![],
        caption: String::new(),
        artist: String::new(),
    }
    .write(&rated)
    .unwrap();
    // No sidecar at all.
    let plain = dir.path().join("plain.jpg");
    std::fs::write(&plain, b"\xff\xd8\xff\xe0jpeg").unwrap();
    // A damaged sidecar: an error when read directly...
    let broken = dir.path().join("broken.jpg");
    std::fs::write(&broken, b"\xff\xd8\xff\xe0jpeg").unwrap();
    let side = image_viewer::ratings::sidecar_path(&broken).unwrap();
    std::fs::write(&side, "this is not xml at all").unwrap();
    assert!(image_viewer::ratings::Ratings::read(&broken).is_err());

    // ...but unrated for filtering, so the image stays reachable.
    assert_eq!(names_matching(dir.path(), "unflagged"), vec!["broken.jpg", "plain.jpg"]);
    assert_eq!(names_matching(dir.path(), "stars:0"), vec!["broken.jpg", "plain.jpg", "rated.jpg"]);
    assert_eq!(names_matching(dir.path(), "flagged"), vec!["rated.jpg"]);
    assert_eq!(names_matching(dir.path(), "stars:4"), vec!["rated.jpg"]);
}
