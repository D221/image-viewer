//! Where a turn's pages go, and a lone spread filling the turn.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! Two pure functions and no egui: the panel is `[x, y, width, height]` in plain
//! floats and the rects come back the same way, so a whole turn's layout is
//! checked without a window. What is pinned here is the three decisions the
//! pairing walk cannot see:
//!
//! * **one shared scale** - two halves cannot be drawn at two different zoom
//!   levels, so the constrained page decides and the other gets a margin under it;
//! * **input order out** - `rtl` decides which *side* a page lands on, so a
//!   function returning screen order swaps which page is which in exactly one
//!   reading direction, and nowhere else;
//! * **the seam at the centre, always** - zero gutter, and no special case for a
//!   lone page, which is the case that reads as a bug.

use image_viewer::{
    ImgKey, PageCtx, PagePos, Rect4, Shape, shape_of, spread_fit, spread_rects,
    turn_is_a_lone_spread,
};

use Shape::{Double, Single};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;
// ---------------------------------------------------------------------------
// Where a turn's pages go
// ---------------------------------------------------------------------------
//
// Two pure functions and no egui: the panel is `[x, y, width, height]` in plain
// floats and the rects come back the same way, so a whole turn's layout is checked
// without a window. What is pinned here is the three decisions the pairing walk
// above cannot see:
//
// * **one shared scale** Ã¢â‚¬â€ two halves cannot be drawn at two different zoom
//   levels, so the constrained page decides and the other gets a margin under it;
// * **input order out** Ã¢â‚¬â€ `rtl` decides which *side* a page lands on, so a
//   function returning screen order swaps which page is which in exactly one
//   reading direction, and nowhere else;
// * **the seam at the centre, always** Ã¢â‚¬â€ zero gutter, and no special case for a
//   lone page, which is the case that reads as a bug.

/// A rect's right-hand edge: every assertion below is about which side of the seam
/// something landed on, so this names the line instead of spelling `x + width` out
/// at each one.
fn right(r: Rect4) -> f32 {
    r[0] + r[2]
}

/// How much two rects overlap horizontally, or 0.0 for rects that merely touch.
///
/// Used instead of comparing edges so "neither overlaps the other" is a claim
/// about the two rects together Ã¢â‚¬â€ two pages drawn at the same `x` overlap no matter
/// how their edges compare to each other.
fn overlap(a: Rect4, b: Rect4) -> f32 {
    right(a).min(right(b)) - a[0].max(b[0])
}

/// The scale is deliberately **not** 1.0. At 1.0 a rect's width and height equal the
/// page's own, so an implementation that ignored `scale` on either axis would satisfy
/// the assertions below just as well; where the seam falls does not depend on it.
#[test]
fn a_two_page_turn_meets_at_the_seam() {
    let pages = [(600.0, 800.0), (600.0, 800.0)];
    let rects = spread_rects(PANEL, &pages, 0.5, false, false);

    assert_eq!(rects.len(), 2);
    // Zero gutter: the two halves touch at exactly the panel's centre, so a drawing
    // that crosses the seam is unbroken.
    assert_eq!(right(rects[0]), 400.0, "the left page ends on the seam");
    assert_eq!(rects[1][0], 400.0, "the right page begins on the seam");
    assert_eq!(
        overlap(rects[0], rects[1]),
        0.0,
        "touching is not overlapping: both pages starting on the seam would satisfy the two \
         assertions above and stack one on top of the other"
    );
    // One factor on both axes, which is the difference between a scale and a width:
    // 600x800 at 0.5 is 300x400, and neither of those is the page's own size.
    assert_eq!((rects[0][2], rects[0][3]), (300.0, 400.0));
}

/// Which page is the right-hand one is `rtl`'s decision, and it is the only thing
/// `rtl` decides: the seam and the fit are the same either way.
#[test]
fn a_right_to_left_turn_opens_on_the_right() {
    let pages = [(600.0, 800.0), (600.0, 800.0)];
    let rects = spread_rects(PANEL, &pages, 1.0, true, false);

    assert_eq!(rects[0][0], 400.0, "page 0 starts at the seam");
    assert_eq!(
        right(rects[1]),
        400.0,
        "page 1 ends on it, so page 0 is the right-hand page and the reader opens on it"
    );
    assert_eq!(overlap(rects[0], rects[1]), 0.0, "still no gutter and still no overlap");
}

/// A lone page keeps the half its reading direction puts it in.
///
/// Mihon's note on why a lone page is **not** centred: centring the span would pull
/// it to the middle, indistinguishable from a page with no partner. So the
/// assertions are that it hangs off the seam and stops there Ã¢â‚¬â€ and the control
/// below, because the centred version is the change somebody would make to "fix"
/// this.
#[test]
fn a_lone_page_rests_with_the_seam_on_the_centre() {
    let pages = [(400.0, 800.0)];
    let scale = spread_fit((PANEL[2], PANEL[3]), &pages, false);
    let rects = spread_rects(PANEL, &pages, scale, true, false);

    assert_eq!(rects.len(), 1);
    assert_eq!(rects[0][0], 400.0, "the lone page's inner edge is the seam");
    assert!(
        rects[0][2] <= 400.0,
        "and it keeps to its own half of the panel: {} wide",
        rects[0][2]
    );
    assert_ne!(
        rects[0][0] + rects[0][2] / 2.0,
        400.0,
        "the control: a centred lone page is indistinguishable from a page with no partner"
    );
}

/// The same rule reading the other way, so "the lone page rests somewhere" is not
/// satisfied by a lone page that is simply always on one side.
#[test]
fn a_lone_page_rests_on_the_left_when_reading_left_to_right() {
    let pages = [(400.0, 800.0)];
    let scale = spread_fit((PANEL[2], PANEL[3]), &pages, false);
    let rects = spread_rects(PANEL, &pages, scale, false, false);

    assert_eq!(rects.len(), 1);
    assert_eq!(
        right(rects[0]),
        400.0,
        "reading left to right the lone page is the left-hand one, so its inner edge is the seam"
    );
    assert_ne!(rects[0][0] + rects[0][2] / 2.0, 400.0, "still not centred");
}

/// Both halves' top edges are the panel's top edge, so a page shorter than its
/// partner leaves a gap beneath rather than being centred.
///
/// The panel's `y` is non-zero on purpose: a layout that placed pages at the window
/// origin rather than in the panel would agree with everything else here.
#[test]
fn the_whole_turn_is_top_aligned() {
    let panel: Rect4 = [0.0, 40.0, 800.0, 600.0];
    let pages = [(400.0, 800.0), (400.0, 200.0)];
    let rects = spread_rects(panel, &pages, 0.5, false, false);

    assert_eq!(rects.len(), 2);
    for (i, r) in rects.iter().enumerate() {
        assert_eq!(r[1], panel[1], "page {i} starts at the panel's top edge, not at the window's");
    }
    // Their own heights, not one shared height: stretching the short page would
    // distort it, and centring it would float it in the middle of its half.
    assert_eq!(rects[0][3], 400.0);
    assert_eq!(rects[1][3], 100.0);
    assert_ne!(
        rects[1][1] + rects[1][3] / 2.0,
        panel[1] + panel[3] / 2.0,
        "the short page is top aligned, not centred"
    );
}

/// The rects come back in **input** order, whatever side each page landed on.
///
/// This is the footgun guard. `rtl` puts page 0's rect to the *right* of page 1's, so
/// screen order is the reverse of input order under `rtl` and identical to it under
/// `ltr`: a layout that collected its rects by side is therefore right in one reading
/// direction and silently swapped in the other, and a caller zipping the result
/// against the turn's page list paints the wrong half of the spread with nothing on
/// screen to say which. The pages here are different widths so that "rect 0" and "the
/// rect of page 0" cannot be the same thing by accident.
#[test]
fn rects_come_back_in_input_order() {
    // Asymmetric, so "the first one" and "the narrow one" cannot be confused.
    let pages = [(400.0, 800.0), (700.0, 800.0)];
    for rtl in [false, true] {
        let rects = spread_rects(PANEL, &pages, 1.0, rtl, false);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0][2], 400.0, "rtl={rtl}: rect 0 is the 400-wide page");
        assert_eq!(rects[1][2], 700.0, "rtl={rtl}: rect 1 is the 700-wide page");
    }

    // ...and the two orders really are opposite under `rtl`, which is what makes the
    // assertions above mean something. Screen order fails them there.
    let ltr = spread_rects(PANEL, &pages, 1.0, false, false);
    let rtl = spread_rects(PANEL, &pages, 1.0, true, false);
    assert!(ltr[0][0] < ltr[1][0], "ltr: page 0 is leftmost, so input order is screen order");
    assert!(
        rtl[0][0] > rtl[1][0],
        "rtl: page 0 is rightmost, so screen order is the reverse of input order - and the \
         return value is still input order"
    );
}

/// One scale for the whole turn: the **constrained** page decides it.
///
/// A per-page fit would give the tall page 0.375 and the short one 0.75, and two
/// halves at two scales is not a layout Ã¢â‚¬â€ it is one zoom level for the reader.
#[test]
fn spread_fit_is_the_more_constrained_of_the_pages() {
    let panel = (800.0, 600.0);
    let pages = [(400.0, 800.0), (400.0, 1600.0)];

    assert_eq!(
        spread_fit(panel, &pages, false),
        0.375,
        "600.0 / 1600.0: the tall page's own fit decides the turn"
    );
    assert_eq!(
        spread_fit(panel, &pages[..1], false),
        0.75,
        "the short page alone would be drawn larger, which is exactly the per-page fit this is not"
    );
    assert!(
        spread_fit(panel, &pages, false) < spread_fit(panel, &pages[..1], false),
        "so the turn's scale is the constrained page's and the short page keeps a margin"
    );
}

/// An **unflagged** turn Ã¢â‚¬â€ a lone portrait page, which is what a lone page in practice
/// is now Ã¢â‚¬â€ is fitted into its half and laid out there.
///
/// The case a naive fit gets wrong: fitting it into the *whole* panel gives
/// `800 / 1200 = 0.667` and hands it the entire screen.
///
/// **This page is 1200x800, which is a `Shape::Double`**, so the caller does *not*
/// hand it to the layout unflagged: `X` on it, or the detector on its own, puts it
/// on the other side of `lone_spread_fills_turn` and it fills the turn instead
/// ([`a_lone_spread_fills_the_whole_turn`]). What is pinned here is therefore the
/// **unchanged half rule**, and the last assertion is the guard: the flag is what
/// moves a page off its half, so a layout that ignored the flag entirely would leave
/// every one of these numbers right and still be the bug that was reported.
#[test]
fn an_unflagged_lone_page_lays_out_at_the_half_the_seam_is_on() {
    let panel: (f32, f32) = (800.0, 600.0);
    let pages = [(1200.0, 800.0)];
    let fit = spread_fit(panel, &pages, false);
    let naive = (panel.0 / 1200.0).min(panel.1 / 800.0);
    assert!(
        (fit - naive / 2.0).abs() < 1e-6,
        "fitted into its half, not into the panel: {fit} against {naive} for the whole panel"
    );

    let rtl = spread_rects(PANEL, &pages, fit, true, false);
    assert_eq!(rtl.len(), 1, "a page comes back, not an empty vector");
    assert_eq!(rtl[0][0], 400.0, "its inner edge is the seam");
    assert!((rtl[0][2] - 400.0).abs() < 1e-3, "as wide as the half it is on");
    assert!(rtl[0][3] <= PANEL[3], "and no taller than the panel");

    let ltr = spread_rects(PANEL, &pages, fit, false, false);
    assert_eq!(right(ltr[0]), 400.0, "the seam side under ltr is the left half");

    // The guard: same panel, same page, same scale - the flag alone moves it.
    let filled = spread_rects(PANEL, &pages, spread_fit(panel, &pages, true), true, true);
    assert!(
        (filled[0][2] - 400.0).abs() > 1e-3,
        "a flagged lone spread is NOT as wide as the half it is on: {} wide",
        filled[0][2]
    );
}
// ---------------------------------------------------------------------------
// A lone spread fills the turn
// ---------------------------------------------------------------------------
//
// A page above `SPREAD_ASPECT` is a scan of two printed pages, and it takes a whole
// turn to itself *because* it is the whole spread. Hanging it off a seam with nothing
// on the far side drew half a spread at half size in half the window: confirmed on a
// real volume where the pages are 1350x1920 (0.70) and exactly one is 2700x1920
// (1.41), and that one alone rendered in the right half.
//
// The fix passes the decision in rather than measuring it, because two of the three
// ways to measure it are wrong on their own:
//
// * **A forced portrait page** (`X` -> `Double`) is wider than nothing and still wants
//   the whole turn, because "treat this as a spread" promises the whole turn.
// * **A near-square page** (1.05) is wider than tall, is a `Single`, and a lone one
//   keeps its half like a cover.
//
// So the discriminator is [`Shape`] and the caller decides it: [`turn_is_a_lone_spread`].

/// The fixture panel's seam is 400 and its half is 400 wide; both appear in the
/// assertions as named numbers so "the full panel" cannot be satisfied by "a half".
const HALF: f32 = 400.0;
const SEAM: f32 = 400.0;

/// A real two-page scan's proportions, from the volume this was reported on: 1.41,
/// comfortably above [`image_viewer::SPREAD_ASPECT`] so the detector agrees it is a
/// spread. 2700x1920 rather than a round pair so the fit is not an exact division and
/// a rect cannot land on a nice number by accident.
const WIDE_SCAN: (f32, f32) = (2700.0, 1920.0);

/// A lone spread fills the whole panel, centred, in **both** reading directions.
///
/// `scale` is the real one from [`spread_fit`] rather than a round number, so the rect
/// is what the app would draw and not an idealisation of it. Under rtl a 2700x1920 page
/// in an 800x600 panel fits at `800 / 2700 = 0.296` on width and `600 / 1920 = 0.3125`
/// on height, so it is 800 wide by 568.9 high and its horizontal centre is the seam.
#[test]
fn a_lone_spread_fills_the_whole_turn() {
    let pages = [WIDE_SCAN];
    let scale = spread_fit((PANEL[2], PANEL[3]), &pages, true);

    for rtl in [false, true] {
        let rects = spread_rects(PANEL, &pages, scale, rtl, true);
        assert_eq!(rects.len(), 1, "one page, one rect");
        let r = rects[0];

        assert!(
            (r[0] - PANEL[0]).abs() < 1e-3 && (right(r) - right(PANEL)).abs() < 1e-3,
            "rtl={rtl}: a lone spread spans the whole panel, [{r:?}] against the panel {PANEL:?}"
        );
        assert!(
            (r[0] + r[2] * 0.5 - SEAM).abs() < 1e-3,
            "rtl={rtl}: and is centred on the seam, not hung off it: centre {}",
            r[0] + r[2] * 0.5
        );
        // **The guard that matters: a full-width page has no side to be on.** Both
        // readings must agree exactly, and neither may satisfy the assertion by being
        // the seam-adjacent half that this defect shipped as.
        assert!(
            r[2] > HALF,
            "rtl={rtl}: {} wide, which is the half a lone *portrait* page keeps - so this is \
             the reported defect, unchanged",
            r[2]
        );
        assert!(
            r[3] <= PANEL[3] + 1e-3,
            "rtl={rtl}: fitted, so it is no taller than the panel either"
        );
    }

    // The two directions really are identical, rather than the loop above comparing a
    // rect against itself: a lone spread is the one case where `rtl` decides nothing.
    assert_eq!(
        spread_rects(PANEL, &pages, scale, false, true),
        spread_rects(PANEL, &pages, scale, true, true),
        "a page on both sides of the seam has no reading direction of its own"
    );
}

/// The cover case, unchanged: a lone **portrait** page keeps its half, on the seam, in
/// both directions.
///
/// This is the regression guard for the fix. Everything above it is about a page that
/// should have been bigger; this is about the page that must not have been, and the
/// temptation a fix like that creates is to widen the rule to "a lone page fills the
/// turn" Ã¢â‚¬â€ which would give a cover the whole window and lose the seam it shares with
/// every other turn.
#[test]
fn a_lone_portrait_page_still_keeps_its_half() {
    let pages = [(400.0, 800.0)];
    let scale = spread_fit((PANEL[2], PANEL[3]), &pages, false);

    for rtl in [false, true] {
        let rects = spread_rects(PANEL, &pages, scale, rtl, false);
        assert_eq!(rects.len(), 1);
        let r = rects[0];
        let inner = if rtl { r[0] } else { right(r) };
        assert_eq!(
            inner, SEAM,
            "rtl={rtl}: a lone portrait page's inner edge is still the seam, on its own half"
        );
        assert!(r[2] <= HALF, "rtl={rtl}: and it still keeps to that half: {} wide", r[2]);
        // **Flush against the seam, with the leftover at its _outer_ edge**, which is the
        // other half of the rule and is what makes this the cover layout rather than the
        // lone-spread one. This page fits at `min(400/400, 600/800) = 0.75`, so it is
        // 300 wide in a 400 half: it touches the seam and stops 100 short of the panel's
        // edge. Centring it in its half instead would put 50 points of bare panel
        // *between it and the seam*, which is a hairline down the middle of every turn
        // this rule is for.
        let half_centre = if rtl { SEAM + HALF / 2.0 } else { SEAM - HALF / 2.0 };
        assert_ne!(
            r[0] + r[2] * 0.5,
            half_centre,
            "rtl={rtl}: a cover is flush with the seam, not centred in its half - 50 points \
             of bare panel beside the seam is a hairline, and it is what a layout that \
             centred this page would draw"
        );
        assert!(
            r[0] >= PANEL[0] && right(r) <= PANEL[0] + PANEL[2],
            "rtl={rtl}: and it never overhangs the panel: {r:?}"
        );
    }
}

/// The flag moves the **fit**, not only the placement: a lone spread is fitted against
/// the panel's whole width, and the rect that comes out is larger than the same page's
/// unflagged rect in both directions.
///
/// The height half alone is not enough to catch this: 2700x1920 at
/// `min(400/2700, 600/1920)` is 148x106 in a half, at `min(800/2700, 600/1920)` it is
/// 296x178 in the panel Ã¢â‚¬â€ so a fix that only centred the rect would leave the page at
/// half size in the middle of the window, which is a different wrong picture and just
/// as wrong.
#[test]
fn a_lone_spread_is_fitted_to_the_whole_panel() {
    let panel = (PANEL[2], PANEL[3]);
    let pages = [WIDE_SCAN];
    let wide = (PANEL[2] / WIDE_SCAN.0).min(PANEL[3] / WIDE_SCAN.1);
    let half = (PANEL[2] / 2.0 / WIDE_SCAN.0).min(PANEL[3] / WIDE_SCAN.1);

    let flagged = spread_fit(panel, &pages, true);
    assert!(
        (flagged - wide).abs() < 1e-6,
        "fitted into the whole panel: {flagged} against {wide} for the panel and {half} for \
         its half"
    );
    assert!(
        flagged > half * 1.5,
        "and the half's fit is {half}: a flag that did not reach `spread_fit` would answer \
         that, giving half the picture"
    );

    // "At least as large", on the rect and not on the scale, because that is the
    // user-visible claim: the same page, flagged, is never drawn smaller.
    for rtl in [false, true] {
        let flagged_rect = spread_rects(PANEL, &pages, flagged, rtl, true)[0];
        let half_rect =
            spread_rects(PANEL, &pages, spread_fit(panel, &pages, false), rtl, false)[0];
        assert!(
            flagged_rect[2] >= half_rect[2] && flagged_rect[3] >= half_rect[3],
            "rtl={rtl}: the flagged rect {flagged_rect:?} is smaller than the unflagged \
             {half_rect:?}"
        );
        assert!(
            (flagged_rect[2] - PANEL[2]).abs() < 1e-3,
            "rtl={rtl}: and this fixture's width is the binding constraint, so it is exactly \
             the panel's width: {} against {}",
            flagged_rect[2],
            PANEL[2]
        );
    }
}

/// A two-page turn ignores the flag entirely: it is not a lone spread, and both halves
/// are already on the seam.
///
/// Asserted over both flags and both directions rather than one, because the flag is a
/// new parameter on a shared function and the failure it invites is a `len() == 1` check
/// that is too wide Ã¢â‚¬â€ which would hand a real spread two half-width slots.
#[test]
fn a_two_page_turn_is_unaffected() {
    let panel = (PANEL[2], PANEL[3]);
    let pages = [(400.0, 800.0), (700.0, 800.0)];
    let scale = spread_fit(panel, &pages, false);

    assert_eq!(
        spread_fit(panel, &pages, true),
        scale,
        "the fit of a two-page turn does not depend on the flag at all"
    );
    for rtl in [false, true] {
        assert_eq!(
            spread_rects(PANEL, &pages, scale, rtl, true),
            spread_rects(PANEL, &pages, scale, rtl, false),
            "rtl={rtl}: the flag changes nothing about a pair's layout"
        );
    }

    // The control: a pair whose pages *are* wide still pairs into halves, because
    // `pairable` is what keeps a `Double` out of a pair Ã¢â‚¬â€ not the layout's flag. Without
    // this, "a two-page turn is unaffected" is satisfiable by a `spread_rects` that lays
    // every turn out as a lone spread whenever the flag happens to be true, on a page
    // count nobody checked.
    let wide_pair = spread_rects(PANEL, &[WIDE_SCAN, WIDE_SCAN], scale, true, true);
    assert_eq!(
        wide_pair[0][0], SEAM,
        "two wide pages still meet at the seam: a flag true on a turn of two is not a \
         request to fill the panel twice over"
    );
    assert!(overlap(wide_pair[0], wide_pair[1]) == 0.0, "and do not overlap while doing it");
}

/// The discriminator is the **shape**, and these are the two cases where a geometric
/// `w > h` would answer differently.
///
/// A lone near-square page is 1.05 Ã¢â‚¬â€ wider than tall, so a geometry test calls it a
/// spread Ã¢â‚¬â€ and it is a `Single` (`SPREAD_ASPECT` is 1.2), so it keeps its half like a
/// cover. The forced portrait page is the mirror: 0.70, so a geometry test calls it a
/// page, and `X` has called it a `Double`, so it fills the turn.
#[test]
fn a_lone_near_square_page_is_not_a_spread() {
    struct One(u32, u32, Option<Shape>);
    impl PageCtx for One {
        fn step(&self, _from: PagePos, _by: isize) -> Option<PagePos> {
            None
        }
        fn step_no_wrap(&self, _from: PagePos, _by: isize) -> Option<PagePos> {
            None
        }
        fn key(&self, _pos: PagePos) -> Option<ImgKey> {
            None
        }
        fn dims(&self, _pos: PagePos) -> Option<(u32, u32)> {
            Some((self.0, self.1))
        }
        fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
            self.2
        }
        fn flat(&self, _pos: PagePos) -> Option<usize> {
            Some(0)
        }
    }
    let page = PagePos { index: 0, page: 0 };

    // 1050x1000: 1.05, wider than tall, and a `Single`.
    let squareish = One(1050, 1000, None);
    assert_eq!(shape_of(page, &squareish), Single, "1.05 is a page, not a spread");
    assert!(
        !turn_is_a_lone_spread(1, shape_of(page, &squareish)),
        "so a lone one keeps its half, which a `w > h` test would have taken away from it"
    );
    let fit = spread_fit((PANEL[2], PANEL[3]), &[(1050.0, 1000.0)], false);
    let rects = spread_rects(PANEL, &[(1050.0, 1000.0)], fit, true, false);
    assert_eq!(rects[0][0], SEAM, "and its inner edge is the seam");
    assert!(rects[0][2] <= HALF, "{} wide", rects[0][2]);

    // 1000x1400 forced to `Double` by `X`: taller than wide, and a spread.
    let forced = One(1000, 1400, Some(Double));
    assert_eq!(shape_of(page, &forced), Double, "an override beats the geometry");
    assert!(
        turn_is_a_lone_spread(1, shape_of(page, &forced)),
        "and a lone one therefore fills the turn, which a `w > h` test would have refused"
    );
    let fit = spread_fit((PANEL[2], PANEL[3]), &[(1000.0, 1400.0)], true);
    let rects = spread_rects(PANEL, &[(1000.0, 1400.0)], fit, true, true);
    assert!(
        (rects[0][0] + rects[0][2] * 0.5 - SEAM).abs() < 1e-3,
        "centred on the seam: the user asked for a spread and got the whole turn"
    );

    // The count is the other half of the rule, and it is checked here rather than in the
    // layout tests so the decision has one place it is pinned: a `Double` is only a lone
    // spread when the turn really holds one page.
    assert!(
        !turn_is_a_lone_spread(2, Double),
        "a turn of two pages is not a lone spread, whatever the shape says"
    );
    assert!(
        !turn_is_a_lone_spread(0, Double),
        "and neither is the empty turn, which is the single-image path"
    );
}

/// No degenerate input may produce a scale or a rect that cannot be painted, and a
/// panel or a turn with nothing in it answers the one value that means "as drawn".
///
/// The finiteness half is the property the layout depends on Ã¢â‚¬â€ a `NaN` scale reaches
/// every rect and leaves an unpaintable page with no error to point at Ã¢â‚¬â€ and the two
/// `1.0`s are what keep the guard in `spread_fit` from being mistaken for decoration:
/// the clamp alone would answer `1e-4` for an empty panel and `8.0` for no pages, both
/// finite, and `1e-4` draws a page a fraction of a point across.
#[test]
fn spread_fit_survives_an_empty_panel_and_a_zero_sized_page() {
    for (panel, pages) in [
        ((0.0, 0.0), vec![(0.0, 0.0)]),
        ((0.0, 0.0), vec![]),
        ((800.0, 600.0), vec![(0.0, 0.0)]),
        ((400.0, 800.0), vec![(400.0, 0.0)]),
        ((800.0, 0.0), vec![(400.0, 800.0)]),
    ] {
        let scale = spread_fit(panel, &pages, false);
        assert!(
            scale.is_finite() && scale > 0.0,
            "panel {panel:?} pages {pages:?} gave the scale {scale}"
        );
        let rects = spread_rects([0.0, 0.0, panel.0, panel.1], &pages, scale, false, false);
        assert_eq!(rects.len(), pages.len(), "one rect per page, at any size");
        for r in &rects {
            assert!(
                r.iter().all(|v| v.is_finite()),
                "panel {panel:?} pages {pages:?} laid out at {scale} gave {r:?}, which is a rect \
                 nothing can paint"
            );
        }
    }

    // The value itself, which the loop above cannot see: the clamp would hand back
    // 1e-4 for an empty panel and 8.0 for no pages, and both are finite, so only
    // these two assertions tell the early return apart from its absence.
    assert_eq!(spread_fit((0.0, 0.0), &[(400.0, 800.0)], false), 1.0, "no panel, no fit");
    assert_eq!(spread_fit((800.0, 600.0), &[], false), 1.0, "no pages, no fit");
}
