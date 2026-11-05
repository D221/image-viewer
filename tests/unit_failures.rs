//! Decode failures, animation timing and wallpaper: nothing may show
//! "Loading" forever, and the last press wins.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// A grid cell whose thumbnail failed every attempt says so instead of
/// loading forever: after `THUMB_TRIES` the ellipsis stopped meaning
/// "decoding" and reads as a hang, so the cell gets the same "?" an
/// unreadable container does.
#[test]
fn a_failed_grid_cell_says_so_instead_of_loading_forever() {
    let src = main_src();
    let cell = fn_body(&src, "fn grid_cell(").expect("grid_cell");
    // Anchored on `>=`, not the bare name: the request guard in the same
    // function also mentions `THUMB_TRIES` (with `<`), so a bare `contains`
    // passes with the exhausted arm deleted — which is exactly the mutation
    // that was tried, and exactly what this assertion has to catch.
    assert!(
        cell.contains(">= THUMB_TRIES"),
        "the fallback must branch on exhausted attempts: without it a failed \
         cell is indistinguishable from a decoding one, forever"
    );
    // The "?" has to sit behind that branch, not beside it — an unconditional
    // "?" would replace the loading mark for cells that are genuinely still
    // decoding.
    let tries_at = cell.find(">= THUMB_TRIES").expect("tries check");
    assert!(
        cell[tries_at..].contains("\"?\""),
        "an exhausted cell must show the failed mark after the budget check"
    );
}

/// An undecodable file names its container instead of parroting the
/// guesser — but only when no decoder was even attempted. A corrupt file
/// of a supported kind keeps the decode error, or every broken JPEG would
/// gain a wrong explanation.
#[test]
fn the_decode_failure_names_unclaimed_bytes_only() {
    let src = lib_src();
    let decode = fn_body(&src, "fn decode_bytes(").expect("decode_bytes");
    assert!(
        decode.contains("looks_like_supported_image(bytes)"),
        "the remap must be gated on the magic: bytes a decoder claims are \
         a corrupt file, not an unsupported one"
    );
    assert!(
        decode.contains("unsupported_error(bytes)"),
        "unclaimed bytes must go through the naming function rather than \
         the guesser's shrug"
    );
}

/// A failed image must never show "Loading…": its decode will never be
/// re-issued, so the overlay is a promise nothing keeps — and navigating
/// away and back must re-state the stored error rather than blank it, since
/// the revisit produces no new message of its own.
#[test]
fn a_failed_image_shows_its_error_instead_of_loading_forever() {
    let src = main_src();
    // Anchored on the overlay's own condition, not on the first "Loading…"
    // in the function: that literal also starts the shown-None branch and
    // appears in a comment, and both read before the overlay without
    // guarding it. `panel.left_top()` is the paint call itself, unique in
    // the file.
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    let at = central.find("self.cur != self.shown").expect("the overlay condition");
    let tail = &central[at..];
    let gate = tail.find("Kind::Preview))").expect("the failed check");
    let paint = tail.find("panel.left_top()").expect("the overlay paint");
    // The gate must come *before* the paint it guards: a check after the
    // text is painted is a comment, not a condition.
    assert!(
        gate < paint,
        "the Loading overlay must be gated on the failed set: without the \
         gate it paints forever over an image that will never arrive"
    );
    // ...and `goto` re-states the stored message, because the revisit
    // produces no new decode and therefore no new error of its own.
    let goto = fn_body(&src, "fn goto(&mut self").expect("goto");
    assert!(
        goto.contains("load_errors.get("),
        "goto must re-state a stored failure message: clearing the banner \
         without this leaves the revisit blank with no error and no spinner \
         activity"
    );
}

/// The stored message lives and dies with the `failed` entry: written on
/// the same arm, cleared at every site that clears it. A message that
/// outlives its failure is shown for an image that now decodes.
#[test]
fn the_stored_failure_message_cannot_outlive_its_failure() {
    let src = main_src();
    assert!(
        src.contains("self.load_errors.insert(key.clone(), msg.clone())"),
        "the message must be stored on the failure arm, beside the failed \
         entry it describes"
    );
    for site in ["self.load_errors.retain(|k, _| k.path != p)", "self.load_errors.clear()"] {
        assert!(
            src.contains(site),
            "every site that clears `failed` must clear the message too, or \
             a fixed file keeps reporting its old failure"
        );
    }
}

/// The frame clock survives its move to lib with speed: file timing pins
/// the old behavior, and 2x halves both the index math and the repaint wait
/// (which is real time — scheduling an effective-time wait would repaint
/// late by the speed factor).
#[test]
fn anim_frame_state_keeps_time_and_scales() {
    use image_viewer::ImageData;
    use std::time::Duration;
    let frame =
        || ImageData { width: 1, height: 1, rgba: vec![0, 0, 0, 255], load_time: Duration::ZERO };
    let anim = image_viewer::Animation {
        frames: vec![frame(), frame()],
        delays: vec![Duration::from_millis(100), Duration::from_millis(200)],
    };
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::ZERO, 1.0),
        (0, Duration::from_millis(100))
    );
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::from_millis(150), 1.0),
        (1, Duration::from_millis(150))
    );
    // Loops: total 300 ms, so 300 lands back on frame 0.
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::from_millis(300), 1.0),
        (0, Duration::from_millis(100))
    );
    // 2x: 50 ms in means 100 ms along — frame 1, with 200 ms of effective
    // time left reading as 100 ms on the wall clock.
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::from_millis(50), 2.0),
        (1, Duration::from_millis(100))
    );
    // Clamped, not frozen: 0x behaves as the 0.1x floor.
    let (idx, wait) = image_viewer::anim_frame_state(&anim, Duration::from_millis(1000), 0.0);
    assert!(idx < 2 && wait > Duration::ZERO);
    assert_eq!(image_viewer::clamp_anim_speed(0.0), 0.1);
    assert_eq!(image_viewer::clamp_anim_speed(99.0), 8.0);
}

/// Playback speed must be **wired**: offered in Settings, and read by the
/// frame clock (paint and resume agree through the same clamp).
#[test]
fn animation_speed_is_wired() {
    let src = main_src();
    assert!(
        src.contains("self.config.anim_speed"),
        "Settings must offer the speed and the clock must read it"
    );
    assert!(
        src.contains("clamp_anim_speed(self.config.anim_speed)"),
        "resume must scale through the same clamp as the clock"
    );
}

/// Wallpaper-as-displayed must be **wired**: crop and turn baked into a PNG
/// beside the config (the shell reads files, never buffers), refusing the
/// cases the path-keyed edit cannot express.
#[test]
fn wallpaper_shown_is_wired() {
    let src = main_src();
    assert!(
        src.contains("fn set_wallpaper_shown(&mut self"),
        "there must be a processed-wallpaper action"
    );
    let at = src.find("fn set_wallpaper_shown(&mut self)").expect("the action must exist");
    let body = &src[at..(at + 3500).min(src.len())];
    assert!(
        body.contains("region_for_save(") && body.contains("wallpaper.png"),
        "it must bake through region_for_save into a config-side PNG"
    );
    assert!(
        body.contains("Cannot wallpaper a transformed animation")
            && body.contains("multi-page document"),
        "animations and later pages must be refused, like save and copy refuse them"
    );
    assert!(
        src.contains("binding_down(i, &kb, \"wall_shown\")"),
        "Shift+W must reach it through the bindings map"
    );
}

/// **A wallpaper request must not decode on the paint thread**, and the last
/// press must win. Both wallpaper paths do a whole-file read and (converted or
/// Shift+W) a full decode + re-encode; the "the UI thread never decodes" rule
/// does not weigh the *reason*, so the work lives on a worker thread like the
/// drop copy's and the rating read's. `main.rs` is unreachable from every test,
/// so this is the source-scan pattern, asserting the arrangement by byte order:
/// the spawn must come **before** every heavy call, and the stale-answer drop
/// must exist in `pump`.
///
/// This lifetime rule is the `drops.rs` shape: presence of a spawn says nothing
/// about which side of it the decode is on, and the defect this guards was that
/// arrangement until this moved.
#[test]
fn wallpaper_work_is_on_a_worker_and_the_last_press_wins() {
    let src = main_src();

    for name in ["fn set_wallpaper(", "fn set_wallpaper_shown("] {
        let at = src.find(name).unwrap_or_else(|| panic!("{name} must exist"));
        // Brace-match the body: the functions are adjacent to other methods,
        // and a substring window drifted past this once and still passed.
        let body = fn_body(&src[at..], name).expect("balanced braces");
        let spawn =
            body.find("std::thread::spawn").unwrap_or_else(|| panic!("{name} must spawn a worker"));
        // The heavy half: read, decode, transform, encode, and the shell call
        // itself — every one of them after the spawn, or the rule is not kept.
        for heavy in
            ["load_image(", "load_image_full(", "region_for_save(", "wallpaper::set_from_path("]
        {
            if let Some(h) = body.find(heavy) {
                assert!(spawn < h, "{name}: {heavy} must run on the worker, after thread::spawn");
            }
        }
        // The label: an in-flight wallpaper request is otherwise invisible
        // until the desktop changes, which reads as a dropped keystroke.
        assert!(body.contains("\"Setting wallpaper…\""), "{name} must show its in-flight notice");
        // The token: the worker must abort at an early check and again at the
        // set, so a superseded press never points the desktop at a picture the
        // user's last action replaced.
        let loads = body.matches("issued.load(").count();
        assert!(
            loads >= 2,
            "{name} must check its issuance token before the work and again at the set, found {loads}"
        );
    }

    // The result's stale drop: a superseded answer must be recognised by its
    // order and silently discarded, not shown over the last press's text.
    let pump = src.find("fn pump(").expect("pump must exist");
    // Brace-match from the signature; pump is long and full of closures.
    let pb = fn_body(&src[pump..], "fn pump(").expect("pump braces balance");
    assert!(
        pb.contains("Msg::Wallpaper { order, result }"),
        "pump must absorb the wallpaper result"
    );
    assert!(
        pb.contains("self.wallpaper_order.load("),
        "pump must drop a result whose order is not the counter's now"
    );
}
