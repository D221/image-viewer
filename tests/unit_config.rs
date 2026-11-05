//! Config defaults, forward-compat reads, and the settings the toggles wire
//! up.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// Worker-count override (Tier 1 #12): 0 means auto, anything else is clamped.
#[test]
fn worker_count_override_is_honoured_and_auto_stays_put() {
    // Auto (0) keeps the shipped behaviour: available parallelism clamped to 2..=4.
    assert_eq!(image_viewer::resolve_worker_count(8, 0), 4);
    assert_eq!(image_viewer::resolve_worker_count(2, 0), 2);
    assert_eq!(image_viewer::resolve_worker_count(1, 0), 2);
    // An explicit preference is honoured, clamped to a sane 1..=8.
    assert_eq!(image_viewer::resolve_worker_count(8, 1), 1);
    assert_eq!(image_viewer::resolve_worker_count(8, 6), 6);
    assert_eq!(image_viewer::resolve_worker_count(8, 64), 8);
}

/// An old config without the workers key must still load, and come up on auto.
#[test]
fn a_config_written_before_workers_still_loads_and_stays_auto() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert_eq!(cfg.workers, 0, "an old config must come up on auto (0), not on a fixed count");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert_eq!(fresh.workers, 0, "a first run must also come up on auto");
}

/// The worker-count setting must be **wired**, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_worker_count_setting_is_wired_to_pool_startup() {
    let src = main_src();
    assert!(
        src.contains("resolve_worker_count("),
        "pool startup must go through resolve_worker_count, not compute the clamp inline"
    );
    assert!(src.contains("config.workers"), "pool startup must read the setting");
    assert!(
        src.contains("&mut self.config.workers"),
        "the settings panel must have a control bound to it"
    );
}

/// Fill-with-crop fit (Tier 1 #10): scale so the image covers the area,
/// cropping rather than letterboxing. The larger ratio wins, unlike
/// `fit_scale_no_upscale` where the smaller wins and upscale is refused.
#[test]
fn fill_scale_covers_the_area() {
    // Wide image in a square area: height binds.
    let s = image_viewer::fill_scale((200.0, 100.0), (100.0, 100.0));
    assert!((s - 1.0).abs() < 1e-6, "height must bind: got {s}");
    // Tall image in a wide area: width binds.
    let s = image_viewer::fill_scale((100.0, 200.0), (200.0, 100.0));
    assert!((s - 2.0).abs() < 1e-6, "width must bind: got {s}");
    // Small image fills by upscaling (that is what crops instead of borders).
    let s = image_viewer::fill_scale((50.0, 50.0), (100.0, 100.0));
    assert!((s - 2.0).abs() < 1e-6, "fill may upscale: got {s}");
}

/// Fill-with-crop must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here.
#[test]
fn the_fill_key_is_wired_to_a_fill_scale() {
    let src = main_src();
    assert!(
        src.contains("fill_scale("),
        "the fill key must size from fill_scale, not from fit_scale"
    );
    assert!(
        src.contains("modifiers.shift"),
        "fill must sit on Shift+F so bare F keeps meaning fit"
    );
}

/// Sort direction (Tier 1 #7): `descending` reverses each mode`s shipped order,
/// and off is exactly today`s behaviour - no existing order changes.
#[test]
fn sort_descending_reverses_each_mode() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.jpg"), vec![0u8; 10]).ok();
    std::fs::write(tmp.path().join("b.jpg"), vec![0u8; 4000]).ok();
    std::fs::write(tmp.path().join("c.jpg"), vec![0u8; 500]).ok();

    // Name: ascending is natural, descending its exact reverse.
    let asc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Name, false);
    let desc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Name, true);
    let names = |fs: &[std::path::PathBuf]| {
        fs.iter().map(|p| p.file_name().unwrap().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(
        names(&asc).iter().map(|s| s.to_str().unwrap()).collect::<Vec<_>>(),
        ["a.jpg", "b.jpg", "c.jpg"]
    );
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Name must be ascending reversed");

    // Size: shipped order is largest-first; descending puts smallest first.
    let desc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Size, true);
    assert_eq!(desc[0].file_name().unwrap(), "a.jpg");
    let asc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Size, false);
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Size must be ascending reversed");

    // Date: order depends on mtime granularity, but descending must still be
    // the exact reverse of whatever ascending produced.
    let asc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Date, false);
    let desc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Date, true);
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Date must be ascending reversed");
}

/// An old config without the key must still load, and keep today`s orders.
#[test]
fn a_config_written_before_sort_descending_still_loads_and_stays_off() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.sort_descending, "an old config must keep the shipped orders");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.sort_descending, "a first run must also keep them");
}

/// The sort-direction setting must be **wired**, and `main.rs` is unreachable
/// from every test, so this is a source scan - the established pattern here.
#[test]
fn the_sort_direction_setting_is_wired_to_rescan() {
    let src = main_src();
    assert!(
        src.contains("scan_folder_sorted_dir("),
        "folder scans must go through the directed sort, not the old wrapper"
    );
    assert!(
        src.contains("&mut self.config.sort_descending"),
        "the settings panel must have a control bound to it"
    );
}

/// Copy-path (Tier 1 #4): Ctrl+Shift+C arrives as `Event::Copy` - egui-winit`s
/// `is_copy_command` is `modifiers.command && key == C` with no shift
/// exclusion, and it returns before pushing the Key event - so the shift state
/// must disambiguate path from pixels inside the Copy arm.
#[test]
fn shift_copy_puts_the_path_on_the_clipboard() {
    let src = main_src();
    assert!(
        src.contains("copy_path_to_clipboard"),
        "there must be a path-copy action distinct from copy_image_to_clipboard"
    );
    // The call site, not the definition: the arm around it must branch on the
    // shift state, since both chords arrive as the same event.
    let call = src.find("self.copy_path_to_clipboard(ctx)").expect("the Copy arm must call it");
    let arm = src[..call].rfind("clip_img").expect("the call must sit in the Copy arm");
    assert!(
        src[arm..call].contains("clip_shift"),
        "the Copy arm must read the shift state to tell Ctrl+Shift+C apart \
         from Ctrl+C (both arrive as Event::Copy)"
    );
}

/// JPEG comments (Tier 1 #8): a COM segment in the header must be found.
#[test]
fn jpeg_comment_finds_the_com_segment() {
    // SOI + COM("hello") + EOI.
    let jpg = vec![0xFF, 0xD8, 0xFF, 0xFE, 0x00, 0x07, b'h', b'e', b'l', b'l', b'o', 0xFF, 0xD9];
    assert_eq!(
        image_viewer::jpeg_comment(&jpg).as_deref(),
        Some("hello"),
        "the COM payload must come back as text"
    );
    // No COM at all.
    let bare = vec![0xFF, 0xD8, 0xFF, 0xD9];
    assert_eq!(image_viewer::jpeg_comment(&bare), None);
    // Truncated mid-length must not panic and must not invent a comment.
    let cut = vec![0xFF, 0xD8, 0xFF, 0xFE, 0x00];
    assert_eq!(image_viewer::jpeg_comment(&cut), None);
    // Not a JPEG at all.
    assert_eq!(image_viewer::jpeg_comment(b"definitely not jpeg"), None);
}

/// EXIF text comments end to end: description wins over UserComment, read
/// from a real file so the tag lookup is exercised and not just the helpers.
///
/// `test-images/` is gitignored, so this skips when absent — the same pattern
/// as `test_gps_decode`. Where it runs, it runs for real.
#[test]
fn exif_comment_reads_description_from_a_real_file() {
    let path = std::path::Path::new("test-images/color-p3-anon.jpg");
    if !path.exists() {
        return;
    }
    let exif = nom_exif::read_exif(path).expect("fixture must parse");
    assert_eq!(
        image_viewer::exif_comment(&exif).as_deref(),
        Some("Screenshot"),
        "ImageDescription must come back as text"
    );
    // The file also carries an 18-byte UserComment; description still wins,
    // and the payload itself must decode to something non-empty.
    let payload = exif
        .get(nom_exif::ExifTag::UserComment)
        .and_then(|v| v.as_undefined())
        .expect("fixture carries a UserComment");
    assert_eq!(payload.len(), 18);
    assert!(
        image_viewer::decode_user_comment(payload).is_some(),
        "the real UserComment payload must decode"
    );

    // Synthetic payloads without a file: ASCII prefix + text, and an
    // all-NUL body is no comment rather than an empty line.
    assert_eq!(image_viewer::decode_user_comment(b"ASCII\0\0\0hello").as_deref(), Some("hello"));
    assert_eq!(image_viewer::decode_user_comment(b"ASCII\0\0\0\0\0\0"), None);
}

/// The comment lines must reach the panel, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_comment_lines_are_wired_into_extract_exif() {
    let src = main_src();
    assert!(
        src.contains("image_viewer::exif_comment("),
        "extract_exif must show the EXIF description/UserComment"
    );
    assert!(
        src.contains("image_viewer::jpeg_comment("),
        "extract_exif must also show a JPEG COM segment"
    );
}

/// The colour line must come from the preview, not from the metadata job, and
/// `main.rs` is unreachable from every test, so this is a source scan - the
/// established pattern here.
/// Colour is classified from the bytes the preview decode already read. The
/// whole point is that `extract_exif` runs **no** second read of the file — the
/// old shape called `color_info(path)` there, an 8-12 ms whole-file
/// `fs::read` on a real photograph, queued ahead of every prefetch. The panel
/// draws the line from the cached preview, which answers with no read of any
/// kind.
#[test]
fn the_colour_line_rides_on_the_preview_not_the_metadata_read() {
    let src = main_src();
    let body = fn_body(&src, "fn extract_exif(").expect("extract_exif");
    assert!(
        !body.contains("color_info"),
        "extract_exif must not classify colour: that is the decode's answer now, \
         and calling color_info here is a second whole-file read of every image"
    );

    // The panel draws it from the shown image's cached preview.
    let coloured = src.contains("preview.colour.panel_line()");
    assert!(coloured, "the metadata panel must draw the colour line from the cached preview");
    assert!(
        src.contains("self.shown.as_ref().and_then(|k| self.previews.get(k))"),
        "the colour line must be read off `shown`'s cached preview, not re-derived \
         per frame"
    );
}

/// Screen-size save must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here.
#[test]
fn the_screen_save_key_is_wired_to_a_scaled_save() {
    let src = main_src();
    assert!(
        src.contains("save_transformed_scaled("),
        "Ctrl+Shift+S must go through the scaled save, not the full-res one"
    );
    assert!(src.contains("save_scaled_copy()"), "there must be a screen-size save action");
}

/// Toasts (Tier 1 #11): a resort must say what order it produced. Notices are
/// status-line toasts; the sort change was the state change that stayed silent.
#[test]
fn resort_names_the_order_it_produced() {
    let src = main_src();
    let at = src.find("if resort {").expect("the settings panel must have a resort arm");
    let arm = &src[at..(at + 600).min(src.len())];
    assert!(
        arm.contains("self.notice = Some("),
        "resorting must leave a notice naming the order, or a flipped folder \
         reads as nothing happening"
    );
    assert!(
        arm.contains("self.config.sort"),
        "the notice must read the sort the rescan is about to apply"
    );
}

/// Single-instance toggle must be **wired**, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_single_instance_toggle_is_wired() {
    let src = main_src();
    assert!(
        src.contains("self.config.single_instance"),
        "settings panel must offer the single-instance toggle"
    );
    assert!(
        src.contains("hold_single_instance()"),
        "startup must gate on the single-instance mutex"
    );
}

/// The mutex itself, claimed twice in one process: the first claim wins and
/// the second loses, which is the whole semantic the gate relies on.
#[test]
fn second_claim_of_the_same_name_loses() {
    let name = format!("Local\\image-viewer-claim-test-{}", std::process::id());
    assert!(image_viewer::claim_single_instance(&name), "first claim of a fresh name must win");
    assert!(!image_viewer::claim_single_instance(&name), "second claim of a held name must lose");
}

/// The startup brick: a `\\.\`-prefixed device path is not an object name,
/// so creation fails on every launch — and that failure used to read as
/// "already running". A guard that refuses to start the app is worse than no
/// guard, so an uncreatable name starts anyway.
#[test]
fn an_uncreatable_mutex_name_does_not_block_startup() {
    assert!(
        image_viewer::claim_single_instance("\\\\.\\Global\\image-viewer-instance"),
        "a mutex that cannot be created must fail open, not exit the app"
    );
}

/// Rebindable keys (new system): overrides win, blanks fall back, unknown
/// actions resolve to "" rather than a guessed chord. Canonicalization
/// makes modifier order and case irrelevant.
#[test]
fn key_overrides_win_and_blanks_fall_back() {
    let mut keys = std::collections::HashMap::new();
    assert_eq!(image_viewer::resolve_binding(&keys, "next"), "ArrowRight");
    keys.insert("next".into(), "PageDown".into());
    assert_eq!(image_viewer::resolve_binding(&keys, "next"), "PageDown");
    keys.insert("next".into(), "   ".into());
    assert_eq!(image_viewer::resolve_binding(&keys, "next"), "ArrowRight");
    assert_eq!(image_viewer::resolve_binding(&keys, "nope"), "");
    assert_eq!(image_viewer::canonical_chord("shift+ctrl+s"), Some("Ctrl+Shift+S".into()));
    assert_eq!(image_viewer::canonical_chord("Space"), Some("Space".into()));
    assert_eq!(image_viewer::canonical_chord("Ctrl++"), None);
    assert_eq!(
        image_viewer::split_chord("Alt+shift+Delete"),
        Some((false, true, true, "Delete".into()))
    );
    assert_eq!(image_viewer::split_chord("Ctrl"), None);
}

/// Every default chord's key must be a known name: otherwise it can never
/// match, and the action ships dead. The one exception is a *deliberately*
/// blank default (`next_alt` since Space became the zoom toggle): an
/// unparseable shipped chord is still a shipped-dead action, so a second
/// blank row must add itself here, not slip through.
#[test]
fn every_default_key_is_a_known_name() {
    let mut blank: Vec<&str> = Vec::new();
    for (id, chord, _) in image_viewer::KEY_ACTIONS {
        let Some((_, _, _, name)) = image_viewer::split_chord(chord) else {
            assert_eq!(
                *chord, "",
                "default chord of `{id}` (`{chord}`) names no key: it must be blank on \
                 purpose like `next_alt`, or a parseable name like `Space`"
            );
            blank.push(id);
            continue;
        };
        assert!(
            image_viewer::KEY_NAMES.contains(&name.as_str()),
            "default chord of `{id}` names unknown key `{name}`"
        );
    }
    assert_eq!(
        blank,
        vec!["next_alt"],
        "the deliberately-unbound actions are exactly `next_alt`; if another row is \
         blank on purpose, name it here so a typo'd chord never ships unbound"
    );
}

/// Capture, totality, and live labels must be wired: the capture state and
/// its commit path, both directions of the key-name table for every known
/// name, and buttons built through the live-label helper.
#[test]
fn capture_and_labels_are_wired() {
    let src = main_src();
    assert!(
        src.contains("self.capturing = Some(") && src.contains("press keys… (Esc cancels)"),
        "Settings must offer click-to-capture with an Esc out"
    );
    // Duplicates land with a warning naming the sharers (never refused),
    // and every row marks its own conflicts in red.
    assert!(
        src.contains("(also: {sharers})") && src.contains("\"also: {}\""),
        "conflicts must show, not block"
    );
    for name in image_viewer::KEY_NAMES {
        // Single characters ride the char-match branches, not string arms.
        if name.len() == 1 {
            continue;
        }
        assert!(src.contains(&format!("\"{name}\" =>")), "key name `{name}` has no parse arm");
        assert!(
            src.contains(&format!("Key::{name} => \"{name}\"")),
            "key name `{name}` has no reverse arm"
        );
    }
    assert!(
        src.contains("is_ascii_alphabetic") && src.contains("is_ascii_digit"),
        "single characters must route through the char-match branches"
    );
    assert!(
        src.contains("fn action_button_text("),
        "buttons must go through the live-label helper"
    );
    // Capture waits for a non-modifier key (a bare Ctrl press must not
    // commit as "Ctrl+?") and refuses names the matcher cannot parse.
    assert!(
        src.contains("is_modifier_key(*key)"),
        "capture must skip modifier keypresses, not commit them"
    );
    assert!(
        src.contains("is not a bindable key"),
        "an unparseable capture must say so instead of storing a dead chord"
    );
}

/// Recursive browsing must be **wired**: one funnel for every rebuild and a
/// recursive variant where position is preserved.
#[test]
fn recursive_browsing_is_wired() {
    let src = main_src();
    assert!(
        src.contains("fn sorted_scan(") && src.contains("fn load_folder_into("),
        "every rebuild needs a funnel for each half (sorted list, position-preserving load)"
    );
    // The flag is *decided* twice, not once per call site: a `recursive_folders`
    // read anywhere else is a gallery that misses the tree.
    assert_eq!(
        src.matches("config.recursive_folders && ").count(),
        6,
        "exactly the six read sites: startup scan, open_folder, open_file, scan_current \
         (rescan), the settings reload, and leave_archive. A seventh means a rebuild was \
         added without asking whether it sees the tree — leave_archive is the one that \
         has to agree with the open it is going back from, or backing out lands in a \
         flat listing of a folder the reader was browsing as a tree."
    );
    assert!(
        src.contains("fn scan_current(&self)") && src.contains("rx_rescan"),
        "rescan scans off the paint thread via rx_rescan, keeping the funnel"
    );
    assert!(
        src.contains("load_all_from_folder_recursive("),
        "the position-preserving funnel must have a recursive arm"
    );
}

/// Crop is the one site where "record before the change" is not adjacent to the
/// change: `set_crop` runs **per drag frame**, so recording at the mutation would
/// push ~200 entries for one gesture and make undo useless after a single drag.
///
/// So the record goes on **press**, and this asserts that it is on the press path
/// rather than merely present somewhere in the function — the same lesson as
/// `the_turns_second_half_is_derived_not_latched`.
#[test]
fn a_crop_gesture_records_once_on_press() {
    let src = main_src();
    let body = fn_body(&src, "fn crop_from_screen(").expect("crop_from_screen");
    // `record` must NOT be inside crop_from_screen (it is pure geometry).
    assert!(
        !body.contains("history.record("),
        "crop_from_screen is pure geometry; recording inside it would record \
         once per drag frame"
    );
    // It must be on the press handler, which runs once per gesture. The arm is
    // entered every frame the button is held, so the record must also sit after
    // the `primary_pressed()` test that makes it a press.
    let press = fn_body(&src, "fn central(").expect("central");
    let pressed =
        press.find("primary_pressed()").expect("the crop press must test for the press frame");
    let record = press.find("self.history.record(").expect("the crop press must record");
    let set_crop = press.find("self.edits.set_crop(").expect("the crop drag must set");
    assert!(
        pressed < record && record < set_crop,
        "the record must be on the press path, after the press test and before \
         the drag sets the crop"
    );
    // Every clear must record immediately before it, so a third `clear_crop`
    // site cannot appear unrecorded. The count is not compared with the number
    // of records: `apply_to_current` records too, and it is not a clear.
    let clear = "self.edits.clear_crop(";
    let sites: Vec<usize> = src.match_indices(clear).map(|(i, _)| i).collect();
    assert!(!sites.is_empty(), "expected clear_crop sites in main.rs");
    for at in sites {
        let before = &src[at.saturating_sub(120)..at];
        assert!(
            before.contains("self.history.record("),
            "every clear_crop must record just before it; unrecorded site at byte {at}"
        );
    }
}
