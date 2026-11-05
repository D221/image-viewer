//! Fit-to-window, decorations, the title bar, recursive scans and archive
//! listing.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// Fit window keeps the chrome it measures and clamps to the monitor.
#[test]
fn fit_window_hugs_content_and_clamps() {
    // 800x600 window, 700x500 central: 100x100 of chrome, kept.
    assert_eq!(
        image_viewer::fit_window_size((400.0, 300.0), (800.0, 600.0), (700.0, 500.0), None),
        (500.0, 400.0)
    );
    // An 8x spread is bigger than any screen: clamped.
    assert_eq!(
        image_viewer::fit_window_size(
            (6000.0, 4000.0),
            (800.0, 600.0),
            (700.0, 500.0),
            Some((1920.0, 1080.0))
        ),
        (1920.0, 1080.0)
    );
    // Degenerate content never collapses the window.
    assert_eq!(
        image_viewer::fit_window_size((0.0, 0.0), (800.0, 600.0), (800.0, 600.0), None),
        (200.0, 120.0)
    );
}

/// The title-bar toggle moves only the chrome: hiding puts the frame top on
/// the client's top (the strip is being reclaimed), showing puts it
/// `strip_after` above (the strip is about to sit there), and a window with
/// less headroom than the strip clamps at the virtual screen's top rather
/// than parking its re-added title bar off-screen.
#[test]
fn the_decoration_toggle_lands_the_frame_where_the_client_was() {
    // Hide: 100-point window, 31-point strip. The client sat at 100; the
    // frame goes straight there and no strip is re-asserted.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, 0.0), 100.0);
    // Show it back: the remembered strip is subtracted, so the client stays
    // at 100 and the frame's top climbs to 69 to make room for the strip.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, 31.0), 69.0);
    // A toggle with no remembered measurement (first show of a session that
    // started undecorated) still keeps the client where it was: the window
    // may grow downward by the strip instead of moving.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, 0.0), 100.0);
    // Flush against the screen top with a re-added strip: clamped at 0, the
    // content gives up its top rows instead of the title bar going off-screen.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(20.0, 31.0), 0.0);
    // A negative measurement is never a negative offset.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, -4.0), 100.0);
}

/// The hide-title-bar checkbox keeps what the user was looking at.
///
/// `main.rs` is unreachable from the tests, so the wiring is a scan — the
/// geometry half it pins: the client area is measured *before* any command is
/// sent (the commands are applied against this frame's own geometry, so a
/// read after them would measure a window that has not changed yet), the
/// outer frame is landed with the library rule above, the client size is
/// re-asserted so winit's `SWP_NOSIZE` never lets the strip into the client,
/// and maximized/fullscreen windows are left to the OS's clamping.
#[test]
fn the_title_bar_toggle_keeps_the_client_area() {
    let src = main_src();

    let at = src.find(".checkbox(&mut self.config.hide_title_bar,").expect("the checkbox exists");
    let after = &src[at..];
    let arm_end = after.find("\n                });").expect("the checkbox's arm closes");
    let arm = &after[..arm_end];

    let geometry =
        arm.find(".zip(v.outer_rect)").expect("the arm measures the client and outer rects");
    let first_cmd = arm.find("send_viewport_cmd").expect("the arm sends commands");
    assert!(geometry < first_cmd, "the client area must be measured before any command is sent");

    assert!(
        arm.contains("image_viewer::outer_top_for_decoration_toggle("),
        "the frame must be landed through the library rule, not by an arm-local \
         formula"
    );
    assert!(
        arm.contains("ViewportCommand::OuterPosition(")
            && arm.contains("ViewportCommand::InnerSize("),
        "the frame must move and the client size must be re-asserted: winit's \
         SWP_NOSIZE default is what lets the strip into the client area"
    );
    assert!(
        arm.contains("if maximized || fullscreen {"),
        "maximized and fullscreen windows must be left to the OS, which clamps \
         their client area to the work area either way"
    );
    assert!(
        arm.contains("ViewportCommand::Decorations(!hiding)"),
        "the toggle itself must still reach winit in every branch"
    );

    // The strip is only measurable while decorated: the hide arm is the one
    // place it is written, and it is persisted next to the setting so a
    // future show in a fresh session reads it back.
    let lib = lib_src();
    for want in ["pub title_strip: f32,", "pub fn outer_top_for_decoration_toggle"] {
        assert!(lib.contains(want), "lib.rs must carry {want}");
    }
}

/// Recursive scan: the whole tree in pre-order, natural within each level,
/// symlinks skipped, non-images ignored, position preserved like the flat
/// loader.
#[test]
fn recursive_scan_walks_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (rel, bytes) in [
        ("c.jpg", &b"1"[..]),
        ("a/10.jpg", &b"2"[..]),
        ("a/2.jpg", &b"3"[..]),
        ("a/deep/b.png", &b"4"[..]),
        ("notes.txt", &b"nope"[..]),
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    let flat: Vec<String> = image_viewer::scan_folder(root)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(flat, vec!["a", "c.jpg"], "flat scan lists subdirectories too");
    let walked: Vec<String> = image_viewer::scan_folder_recursive(root)
        .iter()
        .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(
        walked,
        vec!["a/2.jpg", "a/10.jpg", "a/deep/b.png", "c.jpg"],
        "pre-order, natural within each level, txt ignored"
    );

    // Position-preserving load over the recursive walk.
    let mut g = image_viewer::Gallery::empty();
    g.load_all_from_folder_recursive(root);
    assert_eq!(g.len(), 4);
    assert_eq!(
        g.current().unwrap().file_name().unwrap().to_string_lossy(),
        "2.jpg",
        "empty gallery starts at the first file"
    );

    // A symlink loop must not hang the walk: skipped, not followed. Link
    // creation needs privileges this box may not grant, so a failed link
    // skips rather than fails.
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_dir;
        let link = root.join("a/loop");
        if symlink_dir(root, &link).is_ok() {
            let walked = image_viewer::scan_folder_recursive(root);
            assert_eq!(walked.len(), 4, "the loop link must not add files");
            let _ = std::fs::remove_dir(&link);
        }
    }
}

/// "Show archives" must actually hide them — from *both* scans, with images
/// untouched, and the getter must round-trip so a test can put it back.
///
/// The getter is public for the reason `thumbnail_cache_enabled` is: the flag is
/// process-wide (it is the house pattern for a setting the library has to see,
/// because one predicate serves the flat scan, the recursive walk and
/// `Gallery::new`), so a test that changes it has to be able to restore it. A
/// test that forgot would silently decide the answer for the ones after it.
#[test]
fn the_show_archives_setting_hides_archives_and_spares_images() {
    use image_viewer::{
        is_browsable, scan_folder, scan_folder_recursive, set_show_archives, show_archives_enabled,
    };
    // Restore whatever the rest of the suite expects, however this test ends.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            set_show_archives(true);
        }
    }
    let _restore = Restore;

    assert!(show_archives_enabled(), "on by default: the feature shipped on");
    // Round-trip both ways *before* the scans, and leave it on: the "off"
    // assertions below are a separate state, and a test that left the flag
    // flipped would decide the answer for everything after it.
    set_show_archives(false);
    assert!(!show_archives_enabled(), "the getter must round-trip the setter");
    set_show_archives(true);
    assert!(show_archives_enabled(), "...and back again");

    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("vol2");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(dir.path().join("a.jpg"), b"x").unwrap();
    // Only archives WITH images should be listed. Create valid ones.
    write_test_zip(&dir.path().join("v1.cbz"), &[("p1.jpg", archive_test_png())]);
    write_test_7z(&dir.path().join("v2.7z"), &[("p1.jpg", archive_test_png())]);
    write_test_zip(&dir.path().join("v4.zip"), &[("p1.jpg", archive_test_png())]);
    // v3.rar - we can't easily write a valid RAR, so it won't be listed
    write_test_zip(&sub.join("deep.cbz"), &[("p.jpg", archive_test_png())]);

    let names = |v: Vec<std::path::PathBuf>| -> Vec<String> {
        v.iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect()
    };
    assert_eq!(
        names(scan_folder(dir.path())),
        vec!["a.jpg", "v1.cbz", "v2.7z", "v4.zip", "vol2"],
        "on: only valid archives with images are listed beside the image, plus subdirectories"
    );
    assert_eq!(names(scan_folder_recursive(dir.path())).len(), 5); // a.jpg + 4 valid archives

    set_show_archives(false);
    assert_eq!(
        names(scan_folder(dir.path())),
        vec!["a.jpg", "vol2"],
        "off: the image and subdirectory are browsable, no archives appear"
    );
    assert_eq!(
        names(scan_folder_recursive(dir.path())),
        vec!["a.jpg"],
        "off: the recursive walk uses the same predicate, so it hides them too"
    );
    assert!(
        !is_browsable(std::path::Path::new("x/v1.cbz")),
        "the predicate itself is what changes, not a second list of extensions"
    );
    assert!(is_browsable(std::path::Path::new("x/a.jpg")));
}

/// Archives are **listed** in a folder scan, and are deliberately not in
/// `SUPPORTED_EXTENSIONS` — that constant gates decoding and also decides
/// which members get extracted, so a zip in it would make a container
/// something the image decoders are handed.
///
/// Every extension the app claims is asserted here, because the claim is what
/// makes a folder's contents visible at all: a typo in this list is a format
/// that silently does not appear in any folder, with no error anywhere.
#[test]
fn archives_are_listed_and_images_still_are() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.jpg"), b"x").unwrap();
    // Only archives WITH images should be listed. Create valid ones.
    write_test_zip(&root.join("vol1.cbz"), &[("p1.jpg", archive_test_png())]);
    write_test_7z(&root.join("vol1.7z"), &[("p1.jpg", archive_test_png())]);
    // RAR we can't easily write a valid one, so skip it for listing test
    write_test_zip(&root.join("vol1.zip"), &[("p1.jpg", archive_test_png())]);
    std::fs::write(root.join("notes.txt"), b"x").unwrap();
    std::fs::write(root.join("inner.cbz"), b"x").unwrap(); // empty - should NOT appear
    // A subdirectory: folders are listed too, so the grid can descend into them.
    std::fs::create_dir_all(root.join("subdir")).unwrap();

    let mut found: Vec<String> = image_viewer::scan_folder(root)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    found.sort();
    // Images, valid archives with images, and subdirectories are listed;
    // notes.txt (not an image) and inner.cbz (empty archive) are excluded.
    assert_eq!(
        found,
        vec!["a.jpg", "subdir", "vol1.7z", "vol1.cbz", "vol1.zip"],
        "images, valid archives, and subdirectories listed; notes.txt and empty inner.cbz excluded"
    );
}

/// All three containers must produce the *same* flattened gallery: that shared
/// contract is what makes one `open_archive` correct for every format, and a
/// reader that quietly diverges (kept a subdirectory, kept a `.txt`, wrote two
/// members to one name) would show a gallery unlike every other archive's.
#[test]
fn every_archive_format_yields_the_same_flattened_gallery() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let png = archive_test_png();

    // Two members that flatten to the same file name, one nested and one with a
    // different case: without the case-insensitive claim, one silently wins and
    // the gallery is a page short with no error anywhere.
    let members: Vec<(&str, Vec<u8>)> = vec![
        ("ch1/01.png", png.clone()),
        ("ch2/01.PNG", png.clone()),
        ("ch2/02.png", png.clone()),
        ("notes.txt", b"ignore me".to_vec()),
        ("inner.cbz", b"ignore me too".to_vec()),
    ];

    write_test_zip(&root.join("v.cbz"), &members);
    write_test_7z(&root.join("v.7z"), &members);

    let mut results = Vec::new();
    for name in ["v.cbz", "v.7z"] {
        let dest = root.join(format!("out-{name}"));
        let (extracted, skipped) = image_viewer::extract_archive(&root.join(name), &dest)
            .unwrap_or_else(|e| {
                panic!("{name} extracted nothing: {e}");
            });
        let mut got: Vec<String> = image_viewer::scan_folder(&dest)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_lowercase())
            .collect();
        got.sort();
        results.push((name.to_string(), extracted, skipped, got));
    }

    for (name, extracted, skipped, got) in &results {
        assert_eq!(*extracted, 2, "{name}: two distinct pages");
        assert_eq!(*skipped, 0, "{name}: nothing was unreadable");
        assert_eq!(
            *got,
            vec!["01.png".to_string(), "02.png".to_string()],
            "{name}: flat, images only, the colliding member did not overwrite"
        );
    }
    assert_eq!(
        results[0].3, results[1].3,
        "a cbz and a 7z of the same members must show the same gallery"
    );

    // The count is a header read, and it agrees with what extraction produced —
    // it is what the status line and the grid cell draw, so a disagreement is a
    // number on screen that is wrong.
    assert_eq!(image_viewer::archive_image_count(&root.join("v.cbz")), Some(2));
    assert_eq!(image_viewer::archive_image_count(&root.join("v.7z")), Some(2));
    assert_eq!(
        image_viewer::archive_image_count(&root.join("nope.7z")),
        None,
        "a missing archive has no count, rather than a zero that reads as 'empty'"
    );
    assert_eq!(
        image_viewer::archive_image_count(&root.join("v.jpg")),
        None,
        "a non-archive has no count"
    );

    // `extract_archive` dispatches on the extension and refuses anything else
    // by name, so an unhandled container is an error rather than an empty
    // gallery that looks like an archive with no images in it.
    let err =
        image_viewer::extract_archive(std::path::Path::new("x.tar.zst"), &root.join("out-none"))
            .unwrap_err();
    assert!(err.contains("zst"), "the error names the format: {err}");
}

/// A RAR this build cannot read must **say so**, not open as an empty gallery.
///
/// **What is and is not covered here, precisely.** Nothing in the dependency set
/// can write a RAR, and the 7-Zip on this box is read-only for the format
/// ("Not implemented" on write), so there is no RAR fixture to test against — a
/// hand-built one would be this crate's guess at the format rather than the
/// format. So the real archive path is untested here and this test covers only
/// what is checkable:
///
///  * a RAR4 file - the format most `.rar` files in the wild actually are, and
///    one this crate does not read - produces an error naming RAR, rather than
///    an empty gallery, which reads as "an archive with no images in it";
///  * the guard that catches an undecompressed member is content-based, so it
///    can be tested directly on bytes.
///
/// **Not claimed:** that a real RAR5 with stored PNGs opens and yields the same
/// flattened gallery as a cbz. That needs a RAR file this repo does not have,
/// and saying otherwise would be the kind of claim that survives only because
/// nobody can run the test.
#[test]
fn a_rar_this_build_cannot_read_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // RAR4's signature block: `Rar!\x1a\x07\x00`. Recognisably a RAR, and a
    // version the `rar` crate does not parse.
    let rar4 = root.join("vol.rar");
    std::fs::write(&rar4, b"Rar!\x1a\x07\x00\x00\x00").unwrap();

    let err = image_viewer::extract_archive(&rar4, &root.join("out")).unwrap_err();
    assert!(
        err.contains("RAR") || err.contains("Cannot read"),
        "the error says the format could not be read: {err}"
    );
    assert!(
        !root.join("out").exists() || image_viewer::scan_folder(&root.join("out")).is_empty(),
        "an unreadable archive must not leave gallery entries behind"
    );

    // The member guard is content, so it is testable without an archive: bytes
    // that are not a picture must be refused whatever the name says, because
    // that is the shape of an undecompressed member.
    assert!(image_viewer::looks_like_supported_image(&archive_test_png()));
    assert!(!image_viewer::looks_like_supported_image(b"\x00\x01\x02not an image at all"));
    assert!(!image_viewer::looks_like_supported_image(&[]));
    assert!(
        !image_viewer::looks_like_supported_image(b"RIFFxxxxWAVEfmt "),
        "a RIFF that is a WAV is not a WebP, and the check says so"
    );
}

/// T mode is always absolute: the shrink helper is deleted, so there is no
/// shrink behavior left to test — its absence is asserted in the wiring scan.
#[test]
fn the_fit_window_key_is_wired_to_a_live_resize() {
    let src = main_src();
    assert!(src.contains("self.fit_window_to_image()"), "`T` must arm the fit");
    assert!(
        src.contains("ViewportCommand::InnerSize("),
        "central must send a live resize with the drawn size"
    );
    assert!(
        src.contains("fit_window_size("),
        "the resize must go through the chrome-measuring helper"
    );
    assert!(
        src.contains("self.view.fit = false") && src.contains("self.view.scale = 1.0 / ppp"),
        "`T` must reset to absolute 100%, never a shrink-to-fit"
    );
    // Same frame: the reset must land before `scale` is read from the view,
    // or the window is sized for a zoom the picture draws a frame later.
    // T mode is always absolute: no shrink path exists, and `fit_zoom` is
    // gone entirely rather than merely uncalled.
    assert!(!src.contains("fit_zoom("), "the shrink path is deleted, not bypassed");
    let lib = lib_src();
    assert!(!lib.contains("fn fit_zoom("), "the shrink helper must go with its only caller");
    // Converge on the symptom (drawn vs panel), and re-arm while it
    // disagrees: a one-shot loop that clears its flags up front runs once
    // no matter what `fit_iters` allows, which is how the bars survived a
    // "bounded iteration" that never iterated.
    assert!(
        src.contains("(drawn.0 - panel.width()).abs()"),
        "convergence must compare drawn against panel, not window against target"
    );
}

/// Sticky mode: `T` toggles persistence (not a one-shot), arrivals refit via
/// the cursor key, user zooms re-hug without touching the zoom, and every
/// resize centers on the old window.
#[test]
fn the_fit_window_mode_is_sticky_centered_and_follows() {
    let src = main_src();
    assert!(
        src.contains("self.config.fit_window_mode = !self.config.fit_window_mode"),
        "`T` must toggle the mode, not fire once"
    );
    let goto_at = src.find("fn goto(&mut self").expect("goto must exist");
    // 9400 and not 8000: the cursor-change block also carries the per-image
    // remember wiring (`view_memory_is_wired_...`), which grew goto past
    // this scan's old heuristic window.
    let goto = &src[goto_at..(goto_at + 9400).min(src.len())];
    assert!(
        goto.contains("self.fit_window_key") && goto.contains("self.fit_arrive_pending = true"),
        "arrivals must refit through the cursor key, so toggles and rescans do not"
    );
    assert!(
        src.contains("let reset = self.fit_window_pending || self.fit_arrive_pending"),
        "arrival must carry the 100% reset too, or a zoomed-out view survives navigation"
    );
    assert!(
        src.contains("self.fit_arrive_pending") && src.contains("self.shown == self.cur"),
        "arrival fits must wait until the new image is displayed, or every fit reads one image behind"
    );
    assert!(
        src.contains("self.fit_wrap_pending = true"),
        "user zooms in the mode must re-hug the new size"
    );
    assert!(
        src.contains("ViewportCommand::OuterPosition(") && src.contains("((m.x - want.0) / 2.0"),
        "resizes must center on the monitor"
    );
}

/// Adopting a preview follows the view modes: fit-window owns the zoom
/// absolutely (its arrival reset), 1:1 pins it below, fit-view fits it here
/// — anything else leaves the zoom alone, so photos open at 100% instead of
/// maximized and an absolute zoom survives navigation. (An unconditional snap
/// here once compounded per visit into a shrinking spiral while every reset
/// correctly computed 1.0.)
#[test]
fn adoption_follows_the_view_modes() {
    let src = main_src();
    let at = src.find("fn sync_shown(&mut self").expect("sync_shown must exist");
    let body = &src[at..(at + 2500).min(src.len())];
    assert!(
        body.contains("self.config.fit_mode") && body.contains("self.view.fit = true"),
        "adoption must fit only in fit-view mode, never unconditionally"
    );
}

/// The two view modes exclude each other: turning fit-view on disables
/// fit-window, and vice versa.
#[test]
fn view_modes_are_exclusive() {
    let src = main_src();
    assert!(
        src.contains("fn toggle_fit_mode(&mut self)")
            && src.contains("fn toggle_fit_mode(&mut self"),
        "toggle_fit_mode must exist"
    );
    let at =
        src.find("self.config.fit_mode = !self.config.fit_mode").expect("toggle arm must exist");
    let body = &src[at..(at + 1200).min(src.len())];
    assert!(
        body.contains("self.config.fit_window_mode = false"),
        "fit-view switched on must switch fit-window off"
    );
}

/// Panning belongs to its picture: arriving at a new image zeroes the
/// offset, or the old drag shifts the new picture (black one side, clipped
/// the other) with a perfectly innocent zoom readout. Keyed on the cursor,
/// so toggles and rescans re-`goto`ing the same image keep the pan.
#[test]
fn arriving_resets_the_pan_but_stays_do_not() {
    let src = main_src();
    let goto_at = src.find("fn goto(&mut self").expect("goto must exist");
    // 9400 and not 8000: the cursor-change block also carries the per-image
    // remember wiring (`view_memory_is_wired_...`), which grew goto past
    // this scan's old heuristic window.
    let goto = &src[goto_at..(goto_at + 9400).min(src.len())];
    assert!(
        goto.contains("self.pan_key") && goto.contains("self.view.offset = Vec2::ZERO"),
        "goto must zero the pan when the cursor changed"
    );
}
