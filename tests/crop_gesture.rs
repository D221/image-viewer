//! The crop-frame drag, as a state machine.
//!
//! Crop mode decides once, on the press, whether the gesture draws a new frame
//! or moves the existing one, and then answers from that decision every frame
//! the button is held. The decision cannot be per-frame: egui's
//! `primary_pressed` is true for a single frame, so a per-frame test starts the
//! move and then falls through to the draw arm, which paints a new frame from
//! the press point instead. That is exactly what happened, and the whole point
//! of the type under test is that it cannot happen again.
//!
//! The rest is the move's arithmetic: the frame must translate from the rect it
//! had at press (never from last frame's result), keep its size, and stop at the
//! picture edge instead of shrinking against it.

use image_viewer::{CropDrag, CropFrame, translate_clamped};

const PICTURE: [f32; 4] = [0.0, 0.0, 400.0, 300.0];

fn rect(frame: CropFrame) -> [f32; 4] {
    match frame {
        CropFrame::Move(r) => r,
        CropFrame::Draw { .. } => panic!("expected a move, got a draw from {frame:?}"),
    }
}

/// A press inside the frame starts a **move**, and every later frame is a move
/// too — not a new frame drawn from the press point.
#[test]
fn a_press_inside_the_frame_keeps_moving_it_for_the_whole_drag() {
    let base = [100.0, 100.0, 200.0, 180.0];
    let drag = CropDrag::at_press(Some(base), (150.0, 140.0));
    assert_eq!(drag, CropDrag::Move { grab: (150.0, 140.0), base });
    // Three frames of one held drag, drifting right and down.
    for pointer in [(152.0, 141.0), (170.0, 150.0), (190.0, 170.0)] {
        let moved = rect(drag.frame(pointer, PICTURE));
        assert_eq!(moved, translate_clamped(base, (pointer.0 - 150.0, pointer.1 - 140.0), PICTURE));
    }
}

/// The frame is measured from where the button went down, not accumulated from
/// the previous frame. Getting this wrong doubles the delta every frame and the
/// frame outruns the pointer — which is why the latch stores `base` and `grab`.
#[test]
fn the_move_is_measured_from_the_press_not_from_the_last_frame() {
    let base = [100.0, 100.0, 200.0, 180.0];
    let drag = CropDrag::at_press(Some(base), (150.0, 140.0));
    let at = rect(drag.frame((160.0, 150.0), PICTURE));
    // The same pointer position twice is the same rectangle: no accumulation.
    assert_eq!(at, [110.0, 110.0, 210.0, 190.0]);
    assert_eq!(rect(drag.frame((160.0, 150.0), PICTURE)), at);
}

/// A press that missed the frame draws, and keeps the press point as its anchor
/// however far the pointer goes.
#[test]
fn a_press_outside_the_frame_draws_from_the_press_point() {
    let drag = CropDrag::at_press(None, (10.0, 20.0));
    assert_eq!(drag, CropDrag::Draw { anchor: (10.0, 20.0) });
    assert_eq!(
        drag.frame((210.0, 120.0), PICTURE),
        CropFrame::Draw { anchor: (10.0, 20.0), corner: (210.0, 120.0) }
    );
}

/// Pushing the frame past the picture edge slides it to the edge and leaves its
/// size alone. Intersecting instead — what the first version did — shaved a
/// strip off the crop on every overshoot, invisibly until the save.
#[test]
fn a_frame_pushed_past_the_edge_stops_at_it_and_keeps_its_size() {
    let base = [100.0, 100.0, 200.0, 180.0];
    let drag = CropDrag::at_press(Some(base), (150.0, 140.0));
    let far = rect(drag.frame((10_000.0, 10_000.0), PICTURE));
    assert_eq!(far, [300.0, 220.0, 400.0, 300.0]);
    assert_eq!((far[2] - far[0], far[3] - far[1]), (100.0, 80.0));

    let back = rect(drag.frame((-10_000.0, -10_000.0), PICTURE));
    assert_eq!(back, [0.0, 0.0, 100.0, 80.0]);
    assert_eq!((back[2] - back[0], back[3] - back[1]), (100.0, 80.0));
}

/// Clamping each axis on its own: a frame already against the right edge still
/// moves freely up and down.
#[test]
fn the_edges_are_independent() {
    let base = [300.0, 100.0, 400.0, 180.0];
    let moved = rect(CropDrag::at_press(Some(base), (350.0, 140.0)).frame((500.0, 160.0), PICTURE));
    assert_eq!(moved, [300.0, 120.0, 400.0, 200.0]);
}

/// A frame larger than the picture cannot be made to fit by moving it, and
/// must not be collapsed onto an edge either.
#[test]
fn a_frame_bigger_than_the_picture_does_not_jump() {
    let base = [-50.0, -20.0, 450.0, 320.0];
    let moved = rect(CropDrag::at_press(Some(base), (10.0, 10.0)).frame((210.0, 110.0), PICTURE));
    assert_eq!(moved, base);
}

/// A translation of zero is the identity, on every edge case that matters.
#[test]
fn a_click_without_movement_changes_nothing() {
    let frame = [12.5, 7.25, 199.0, 180.75];
    assert_eq!(translate_clamped(frame, (0.0, 0.0), PICTURE), frame);
    assert_eq!(translate_clamped(frame, (0.0, 0.0), frame), frame);
}

/// The press is the only place the gesture is chosen. Everything after it runs
/// on every held frame, where `pressed` is false — so the latch must be what
/// answers, and `pressed` must not appear again. The shape of the bug was
/// `pressed && inside`, re-decided per frame, which fell through to the draw arm
/// and painted a new frame from the press point.
#[test]
fn the_crop_gesture_is_latched_on_press_in_the_app() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    // The crop gesture moved into `central_crop` (its own module: central.rs passed the
    // 1000-line target).
    let src = std::fs::read_to_string(root.join("src/app/central/crop.rs")).expect("crop.rs");
    let body = strip_line_comments(fn_body(&src, "fn central_crop(").expect("fn central_crop"));
    let (press_at, press_end) = block_after(&body, "if pressed {");
    let press = &body[press_at..=press_end];
    assert!(
        press.contains("CropDragState {"),
        "the press must latch the gesture instead of re-deciding it every frame"
    );
    let after = &body[press_end..];
    assert!(
        !after.contains("pressed"),
        "the drag must come from the latch after the press: `pressed` is false on \
         every held frame, so testing it again is what redrew the frame instead of \
         moving it"
    );
    assert!(
        after.contains(".frame((p.x, p.y), screen(footprint))"),
        "every held frame must ask the latched gesture where the frame is"
    );
    // And the move must not go through the *draw* conversion, which applies an
    // aspect to an anchor and would reshape a frame that is only being moved.
    assert!(
        after.contains("crop_from_screen_rect(frame, footprint, display_px, pv)"),
        "a moved frame is converted as a finished rectangle"
    );
}

/// The `{}` block whose opening brace is the first `{` at or after `open`,
/// as an inclusive byte range.
fn block_after(src: &str, open: &str) -> (usize, usize) {
    let at = src.find(open).unwrap_or_else(|| panic!("no `{open}`"));
    let brace = at + src[at..].find('{').expect("opening brace");
    let mut depth = 0i32;
    for (i, b) in src.as_bytes()[brace..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (brace, brace + i);
                }
            }
            _ => {}
        }
    }
    panic!("unterminated `{open}` block");
}

/// `src` without its `//` line comments.
///
/// The prose in this file quotes the very calls these scans look for — a comment
/// explaining why `pressed` must not be consulted would otherwise fail the test
/// that says so.
fn strip_line_comments(src: &str) -> String {
    src.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n")
}

/// The body of the function whose signature is `sig`, by brace counting.
fn fn_body<'a>(src: &'a str, sig: &str) -> Option<&'a str> {
    let start = src.find(sig)? + sig.len();
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
    let end = end?;
    Some(&src[start..end])
}
