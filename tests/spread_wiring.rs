//! Source-scan wiring tests for manga mode: `central` threads the lone-spread
//! flag through `decode_panel_px` and all four layout calls exactly once, and
//! `schedule` asks the library for a decode target rather than inventing one.
//!
//! These are source scans — `MyApp::central` and `MyApp::schedule` live in the
//! binary, which no integration test can reach, so the calls are asserted
//! against `src/main.rs` as text. The same pattern as
//! `tests/renderer.rs`, `tests/keybindings.rs` and the other scans here.
//!
//! The helpers below are twins of `tests/common/spreads_fixtures.rs`' ones: one test
//! target cannot import from another, so `main_src`, `collect_src`, `fn_body` and
//! `block_after` exist once per file. `call_args` is defined here because it is
//! only used by this file's scans.

use std::path::Path;

use image_viewer::{
    decode_panel_px, decode_target, lone_spread_fills_turn, preview_target, spread_centre,
    spread_fit, spread_pan_bounds, spread_rects,
};

/// Every library source file, concatenated - the whole crate except the binary.
fn main_src() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut texts: Vec<String> = Vec::new();
    collect_src(&root, &mut texts);
    texts.sort();
    texts.join("\n")
}

fn collect_src(dir: &Path, texts: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_src(&path, texts);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            texts.push(std::fs::read_to_string(&path).expect("read"));
        }
    }
}

/// The body of `fn <sig>` in `src/main.rs`, found by **brace matching**.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src.find(sig).unwrap_or_else(|| panic!("{sig} is gone; update this test to match"))
        + sig.len();
    let mut depth = 0i32;
    let mut end = None;
    for (i, b) in src.as_bytes()[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.expect(
        "could not find the end of the function: its braces do not \
                         balance, or the signature changed shape. Update this test.",
    );
    &src[start..end]
}

/// The text from `marker` through its matching closing brace.
fn block_after<'a>(src: &'a str, marker: &str) -> &'a str {
    let at =
        src.find(marker).unwrap_or_else(|| panic!("{marker} is gone; update this test to match"))
            + marker.len();
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    for (i, b) in bytes[at..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &src[at..at + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("{marker} has no closing brace");
}

/// The text between a call's `(` and its matching `)`, starting from the call's own name.
fn call_args<'a>(src: &'a str, at: usize) -> &'a str {
    let open = src[at..].find('(').map(|i| at + i).expect("no `(` after the call name");
    let mut depth = 0i32;
    for (i, b) in src.as_bytes()[open..].iter().enumerate() {
        match b {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return &src[open + 1..open + i];
                }
            }
            _ => {}
        }
    }
    panic!("the call at {at} has no closing parenthesis; its parens do not balance");
}
/// The four layout calls and the decode target are handed **one** answer, and
/// `central` reaches it through the library twice rather than through two readings of
/// the rule.
///
/// **This is a source scan, and it is the only thing covering the wiring.** The
/// behavioural tests above drive `lone_spread_fills_turn` and `decode_panel_px`
/// directly, so every one of them stays green if `MyApp::central` never asks: the
/// library would be correct and the app would still draw the reported defect, because
/// the call sites would default to "halve". `central` is in the binary, which no
/// integration test can reach, so the call is asserted against the source for the same
/// reason `tests/renderer.rs`, `tests/keybindings.rs` and the other scans here do. What
/// a scan cannot tell is whether any of it *works*.
///
/// **The negative assertions are the load-bearing half.** An earlier version of this
/// wiring asked `turn_is_a_lone_spread(spread_dims.len(), shape_of(pos, ..))` where the
/// pages were already laid out, which is right; moving the question earlier is only
/// safe if there is exactly one place it is asked. A `central` that computed it twice
/// Ã¢â‚¬â€ once for the decode and once for the fit Ã¢â‚¬â€ would satisfy every positive assertion
/// in this test and could disagree with itself on the frame a shape is learned.
#[test]
fn central_decides_and_threads_the_lone_spread_flag() {
    let src = main_src();
    let central = fn_body(&src, "fn central(");

    assert!(
        central.contains("image_viewer::lone_spread_fills_turn("),
        "`central` no longer asks the library whether the turn is a lone spread, so the flag is\n\
         always false and every lone spread is fitted into a half and decoded for half a panel -\n\
         which is the reported defect, unchanged.\nBody was:\n{central}"
    );
    assert!(
        !central.contains("image_viewer::turn_is_a_lone_spread("),
        "`central` asks the lone-spread question a second time, from `spread_dims.len()`. Two\n\
         sites answering one rule is free to disagree - and the second one would be a reading of\n\
         the paint, so it can be a frame out of date with the first. The query at the top of the\n\
         frame is the one that is asked once.\nBody was:\n{central}"
    );
    assert_eq!(
        central.matches("let lone_spread_fills_turn =").count(),
        1,
        "`central` must bind `lone_spread_fills_turn` exactly once and read it everywhere, so the\n\
         decode target and the four layout calls cannot come from two different answers.\n\
         Body was:\n{central}"
    );

    // **The decode target, and it must be the same flag.** A call that passed a literal
    // `false`, or recomputed the shape, would leave the picture decoded for a half panel
    // while it is drawn across the whole one - which is the defect, and it is the *only*
    // thing in this fix.
    let at = central
        .find("decode_panel_px(")
        .expect("`central` no longer halves the panel through the library");
    let args = call_args(central, at);
    assert!(
        args.contains("lone_spread_fills_turn") && args.contains("self.config.manga_mode"),
        "`decode_panel_px` must be handed the one flag and the mode, so the decode target is the\n\
         layout's own answer rather than a second opinion about it.\nArguments were:\n{args}"
    );
    // ...and it must come **before** the first thing that consumes `panel_px`, which
    // is what "asked early" means: `preview_target` is the resize machine's target and
    // `display_dim` is what `schedule` reads, so a `panel_px` that is still being
    // halved below those two lines is the old bug with extra steps. Comparing against
    // the layout calls would be checking nothing Ã¢â‚¬â€ `central` fits after the picture is
    // known, so an ask placed anywhere above `spread_fit` would satisfy it.
    let preview_target_at =
        central.find("preview_target(").expect("`central` no longer asks for the preview target");
    assert!(
        at < preview_target_at,
        "the decode target is decided after `preview_target` and `display_dim` have already \
         read `panel_px`, so the resize machine and the loader queue are planning against a \
         half-panel answer whatever the layout then does with it"
    );

    // Every layout call, in both spellings, with the flag as an argument. `spread_fit`
    // and `spread_rects` on one line in the common case and spread over several in this
    // one, so the window is taken from the call's name to the end of its line.
    for (call, what) in [
        ("spread_fit(", "the fit: a lone spread drawn at half size in the middle of the window"),
        ("spread_rects(", "the rects: the reported defect itself"),
        ("spread_centre(", "the zoom anchor: a wheel notch would creep the page under the cursor"),
        (
            "spread_pan_bounds(",
            "the pan clamp: the zoom's wanted offset would be silently truncated",
        ),
    ] {
        let at = central
            .find(call)
            .unwrap_or_else(|| panic!("`central` no longer calls `{call}`, so {what}"));
        let args = call_args(central, at);
        assert!(
            args.contains("lone_spread_fills_turn"),
            "`central`'s `{call}` no longer receives `lone_spread_fills_turn`, so {what}, while \
             the library's own tests stay green because they pass the flag directly.\n\
             Arguments were:\n{args}"
        );
    }
}

/// The **wiring** half of the decode-target rule: `schedule` must ask the library
/// for a target rather than invent one, and `central` must re-plan from inside the
/// block that records the display area.
///
/// **This is a source scan, and it is here rather than in `flip_through.rs`
/// because it is the third scan in this suite of the same defect class** Ã¢â‚¬â€ the
/// block, not the call, being the property Ã¢â‚¬â€ and because it wants the
/// brace-matching [`fn_body`] and [`block_after`] this file already owns.
/// `central` and `MyApp::schedule` are in the binary, which no integration test
/// can reach; `flip_through.rs`'s
/// `there_is_no_decode_target_before_the_display_area_is_measured` covers the rule
/// the library applies and cannot see either of these two call sites.
///
/// **The load-bearing assertion is the negative one, and it is about position.**
/// `display_dim` is what `schedule` reads, so a `self.schedule()` one line *above*
/// the assignment inside `central` would be a correct-looking call in the correct
/// function re-planning against the same empty `display_dim` it is trying to fill Ã¢â‚¬â€
/// the exact defect shape `the_turns_second_half_is_derived_not_latched` records,
/// where the assignment was present, in the right function, in the wrong block.
/// So this brace-matches the `if` that assigns `display_dim` and requires the call
/// to be inside it, which a `contains` on the whole body could not tell.
#[test]
fn the_plan_is_empty_until_the_display_area_is_measured() {
    let src = main_src();
    let schedule = fn_body(&src, "fn schedule(");

    assert!(
        schedule.contains("image_viewer::decode_target(self.display_dim)"),
        "`MyApp::schedule` no longer asks the library for a decode target, so it is back \
         to inventing one - and the fallback was `PREVIEW_TEXTURE_DIM`, which is the \
         animation ceiling. On every cold open the first plan (current page, turn \
         partner and all six preload neighbours) is then decoded at 4096, which for any \
         source under 4096 px is the full-resolution page with `downscale_image` a \
         no-op: measured byte-identical to the source decode on 32.jpg, and painted at \
         3.397x (normal) / 4.645x (manga) the rectangle it is drawn in.\n\
         Body was:\n{schedule}"
    );
    // **The old spelling, by its code and not by the constant it mentioned.** The
    // removed line was
    // `self.display_dim.map(image_viewer::preview_target).unwrap_or(PREVIEW_TEXTURE_DIM)`,
    // so pinning the *call* catches it whatever the fallback is called - and a scan
    // that trips on a comment is a scan that gets satisfied by rewording, which is
    // the wrong pressure on the next editor.
    assert!(
        !schedule.contains("image_viewer::preview_target"),
        "`MyApp::schedule` computes the target itself again. `display_dim.map(preview_target)` \
         is the expression that had to answer for the `None`, and it must go through \
         `decode_target`, which is the version that can say it does not know.\n\
         Body was:\n{schedule}"
    );
    assert!(
        schedule.contains("let Some(target_dim) ="),
        "`MyApp::schedule` no longer takes the missing target as a case. There is no \
         measured display area before `central`'s first frame, and that has to be a \
         handled state rather than a default.\nBody was:\n{schedule}"
    );
    assert!(
        schedule.contains("self.loader.replace(Vec::new())"),
        "`MyApp::schedule` has no answer for a target it does not have: the plan it builds \
         must be empty, so the frame that finally measures the display area is the one that \
         queues the work.\nBody was:\n{schedule}"
    );

    // `central`: the call has to be *inside* the block that assigns `display_dim`.
    let marker = "if self.display_dim != Some(panel_px)";
    let block = block_after(&src, marker);
    assert!(
        block.contains("self.schedule()"),
        "`central` assigns `display_dim` without re-planning, so the plan `frame_prelude` \
         built - empty, because no target existed yet - stays empty and the first frame \
         queues nothing at all: the viewer opens on a permanent \"Loading...\". The call \
         belongs inside this block; brace-matched, because one line above the assignment \
         it would read the same `display_dim` it is trying to fill.\nBlock was:\n{block}"
    );
    let assign = block
        .find("self.display_dim = Some(panel_px)")
        .unwrap_or_else(|| panic!("the block no longer assigns display_dim:\n{block}"));
    let call = block
        .find("self.schedule()")
        .unwrap_or_else(|| panic!("the block has no schedule() call:\n{block}"));
    assert!(
        call > assign,
        "`self.schedule()` sits before `display_dim` is assigned, so it builds a plan \
         against the value it is meant to replace. This is the whole reason the assertion \
         above is about position and not presence.\nBlock was:\n{block}"
    );
    // **And only on the first measurement.** Re-planning on every change of the
    // display area is what a window drag must not do, so the call is gated on the
    // transition and the gate has to be visible here rather than assumed.
    assert!(
        block.contains("is_none()"),
        "`central` re-plans on every change of the display area rather than on the first \
         measurement. That is a decode per frame of a window drag, which is the storm \
         `ResizeTracker` exists to coalesce - `ResizeAction::Wait` is not `Refine` for \
         exactly this reason.\nBlock was:\n{block}"
    );
    // One call in the block. (`central` has a second, unrelated `schedule()` in
    // its `ResizeAction::Refine` arm, so the count is the block's - which is the
    // unit the property is about.)
    assert_eq!(
        block.matches("self.schedule()").count(),
        1,
        "the `display_dim` block must re-plan exactly once: twice would be a second \
         reading of the same transition, and a `ResizeAction::Refine` in the same block \
         would re-plan for a target the block has only just replaced.\nBlock was:\n{block}"
    );
}
