//! Where the zoom anchors.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! The zoom keeps the point under the cursor fixed, which needs one number: where
//! the content's centre is. `MyApp::central` used `anchor - panel.center()`, and
//! that is right **only when the content is centred on the panel's centre**. In a
//! spread it is not, on either axis:
//!
//! * **Horizontally**, `spread_rects` puts the *seam* on the panel's centre and
//!   extends each page outward, so the turn's centre is the seam plus half the
//!   difference of the two widths.
//! * **Vertically**, the turn is top-aligned, so its centre is the panel's top plus
//!   half the *tallest* page.
//! * **For a lone page** the content sits entirely on one side of the seam, so it
//!   is displaced by up to a quarter of the panel.
//!
//! So the anchor has to be the content's centre, **derived from `spread_rects`
//! itself** - a second statement of the seam rule is a second rule.

use image_viewer::{Rect4, spread_centre, spread_rects};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

/// A two-page turn's anchor is displaced from the panel centre by half the width
/// difference - and the number is pinned, because "not the panel centre" is
/// satisfied by any wrong displacement.
///
/// The pages are 400 and 700 wide at 0.5, so 200 and 350 on screen: the seam is at
/// `PANEL[0] + PANEL[2]/2` = 400 either way, and the content spans
/// `[400-350, 400+200]` under `rtl` and `[400-200, 400+350]` under `ltr`. Both are
/// 700 wide - the two halves together are always as wide as the seam plus both
/// pages - so the centre is the midpoint of that span and the displacement is
/// Ã‚Â±(350-200)/2 = Ã‚Â±75.
#[test]
fn the_zoom_anchor_is_the_content_centre_not_the_panel_centre() {
    let pages = [(400.0, 800.0), (700.0, 800.0)];
    let scale = 0.5;
    let seam = PANEL[0] + PANEL[2] * 0.5;
    let panel_centre = (PANEL[0] + PANEL[2] * 0.5, PANEL[1] + PANEL[3] * 0.5);
    let half_difference = (700.0 - 400.0) * scale / 2.0; // 75.0

    for (rtl, expected) in [(true, seam - half_difference), (false, seam + half_difference)] {
        let (cx, _) = spread_centre(PANEL, &pages, scale, rtl, false);
        assert_eq!(
            cx, expected,
            "rtl={rtl}: the content's centre is the seam plus half the width difference, \
             measured from the *wider* side ({expected}), not the panel centre"
        );
        assert_eq!(
            cx - panel_centre.0,
            if rtl { -half_difference } else { half_difference },
            "rtl={rtl}: the displacement itself, which is what `anchor - panel.center()` cannot see"
        );
        assert_ne!(
            (cx, spread_centre(PANEL, &pages, scale, rtl, false).1),
            panel_centre,
            "rtl={rtl}: and it is not the panel centre on either axis"
        );
    }
}

/// A lone page anchors on **its own slot**, on the side its reading direction puts
/// it in. A quarter of the panel, which is the largest displacement of all.
///
/// `spread_rects` is used in the test as the definition of "its own slot", and the
/// numbers are pinned as well: at 0.5 the page is 200 wide and 400 tall, so the
/// centre is the seam Ã‚Â± 100 horizontally and 200 vertically.
#[test]
fn a_lone_page_anchors_its_zoom_on_itself() {
    let pages = [(400.0, 800.0)];
    let scale = 0.5;
    for (rtl, expected_x) in [(true, 500.0), (false, 300.0)] {
        let rects = spread_rects(PANEL, &pages, scale, rtl, false);
        let slot = rects[0];
        assert_eq!(
            spread_centre(PANEL, &pages, scale, rtl, false),
            (slot[0] + slot[2] * 0.5, slot[1] + slot[3] * 0.5),
            "rtl={rtl}: the middle of the page's own slot, which is what the zoom measures from"
        );
        assert_eq!(
            spread_centre(PANEL, &pages, scale, rtl, false).0,
            expected_x,
            "rtl={rtl}: pinned"
        );
        assert_eq!(
            spread_centre(PANEL, &pages, scale, rtl, false).1,
            200.0,
            "rtl={rtl}: and vertically the page's own middle, not the panel's"
        );
        assert_ne!(
            spread_centre(PANEL, &pages, scale, rtl, false).0,
            PANEL[0] + PANEL[2] * 0.5,
            "rtl={rtl}: the whole point - a lone page is nowhere near the panel centre"
        );
    }
}

/// The zero-displacement cases, so the single-image path is **pinned** rather than
/// assumed to be untouched by this.
///
/// Two of them, and they are not the same. Two pages of equal width have no
/// horizontal displacement - the content is symmetric about the seam - but still
/// have a vertical one, because the turn is top-aligned. A turn with **no** pages is
/// the single-image path, and that one is the panel centre on both axes; it is what
/// `central` falls back to, so it is worth having said out loud.
#[test]
fn a_single_page_turn_anchors_on_the_panel_centre() {
    let panel_centre = (PANEL[0] + PANEL[2] * 0.5, PANEL[1] + PANEL[3] * 0.5);
    let equal = [(600.0, 800.0), (600.0, 800.0)];
    for rtl in [false, true] {
        let (cx, cy) = spread_centre(PANEL, &equal, 0.5, rtl, false);
        assert_eq!(
            cx, panel_centre.0,
            "rtl={rtl}: two equal pages are symmetric about the seam, so this axis is the panel's"
        );
        assert_eq!(
            cy, 200.0,
            "rtl={rtl}: the vertical one is not - top-aligned, so it is the panel's top plus half \
             the tallest page"
        );
        assert_ne!(
            (cx, cy),
            panel_centre,
            "rtl={rtl}: which is why a clean pair hides the vertical half of the bug and an \
             unequal one hides neither"
        );
    }

    // The single-image path, byte for byte: `central` computes this for a turn with
    // no pages and offsets by it, so a different answer here moves one image.
    for rtl in [false, true] {
        assert_eq!(
            spread_centre(PANEL, &[], 0.5, rtl, false),
            panel_centre,
            "rtl={rtl}: no pages means the content is the panel, so the anchor is its centre"
        );
    }
}

/// Vertically the anchor is the panel's top plus half the **tallest** page at the
/// current scale - three claims in one, because any one of them alone is satisfied
/// by a wrong answer.
///
/// * *Top*-aligned, so it is the panel's top and not its middle.
/// * The **tallest** page, so the taller page being the *second* of the turn cannot
///   change it (a first-page-only rule passes the equal-height tests above).
/// * At the **current scale**, so the number moves with the zoom - a hard-coded
///   half-height would pass every other test here.
#[test]
fn the_zoom_anchor_is_top_aligned_over_the_tallest_page() {
    let panel: Rect4 = [0.0, 40.0, 800.0, 600.0];
    let tall_second = [(400.0, 800.0), (400.0, 1600.0)];
    let tall_first = [(400.0, 1600.0), (400.0, 800.0)];
    let panel_vertical_centre = panel[1] + panel[3] * 0.5;

    let cy = spread_centre(panel, &tall_second, 0.5, true, false).1;
    assert_eq!(cy, 440.0, "40 + 800/2: the panel's top plus half the tallest page at 0.5");
    assert_ne!(
        cy, panel_vertical_centre,
        "not the panel's vertical centre, which is the half of this that survives a clean pair"
    );
    assert_eq!(
        spread_centre(panel, &tall_first, 0.5, true, false).1,
        cy,
        "the tallest page decides it, whichever of the two it is"
    );
    assert_eq!(
        spread_centre(panel, &tall_second, 1.0, true, false).1,
        840.0,
        "40 + 1600/2: at 1.0 the tallest page is twice as tall, so the anchor moves with the scale"
    );
}

/// The zoom block asks for the content's centre Ã¢â‚¬â€ **twice, at two scales** Ã¢â‚¬â€ and
/// never for the panel's.
///
/// **A source scan, and the four tests above are not it.** They pin
/// [`spread_centre`]; nothing pins the call, because `MyApp::central` is in a binary
/// no integration test can reach (`AGENTS.md`, "Tests"), and re-introducing
/// `anchor - panel.center()` there leaves **all 84 tests in this file green** Ã¢â‚¬â€ that
/// was run, and it is why this exists.
///
/// It is weak in the way [`invalidate_path_drops_the_cached_dimensions`] is weak in:
/// it cannot tell whether the anchor maths *works*, only that the centre asked for
/// is the one the library computes. What it catches is the regression that actually
/// happens Ã¢â‚¬â€ the call being reverted, or a later edit reusing one centre where the
/// two scales differ. The behaviour was verified separately, by instrumenting the
/// running app and checking its own per-frame offsets against the formula (see the
/// task report); that evidence is not a test and does not survive here.
#[test]
fn the_zoom_measures_from_the_content_centre_at_both_scales() {
    let src = main_src();
    let body = fn_body(&src, "fn central(");
    // **Comments are stripped first**, and that is not tidiness: the block quotes the
    // expression it replaced ("`anchor - panel.center()` is right only when..."), so
    // scanning the raw text would pass on the comment alone and mean nothing.
    let code =
        body.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");

    assert!(
        !code.contains("anchor - panel.center()"),
        "the zoom is anchoring on the panel's centre again, which is displaced from the\n\
         content's on BOTH axes in a spread: the seam is on the panel centre, so a\n\
         turn of unequal pages is off by half the width difference, and being top\n\
         aligned it is off vertically by half the tallest page.\nBody was:\n{body}"
    );

    // The centre_at maths moved into `central_zoom_input` (its own module: central.rs
    // passed the 1000-line target). It must still bracket the content point between the
    // centre at the current scale and the centre at the target one.
    let zoom = fn_body(&src, "fn central_zoom_input(");
    let zcode =
        zoom.lines().filter(|l| !l.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
    assert!(
        zcode.contains("centre_at(scale)") && zcode.contains("centre_at(target_scale)"),
        "the zoom must bracket the content point between the centre at the current\n\
         scale and the centre at the target one. Reusing one of them is the old line\n\
         again with a different centre, and it is exact only when the content's centre\n\
         does not move with the zoom - true for one image, false for every turn.\nBody was:\n{zoom}"
    );
}

/// Both halves of the full-resolution fix are wiring in the binary, so both are
/// pinned as source scans and **neither is claimed as behaviour**.
///
/// Two different wirings, two different ways of being wrong:
///
/// * The tile loop must range over the turn's **pages**, not over the cursor: the
///   partner's full-resolution buffer is already in memory (the single-pass decode
///   hands every cached preview one) and the loop used to filter it away.
/// * A `Kind::Full` result that is not the cursor's must be **kept**, on the page it
///   belongs to. Dropping it is what made the partner's half soft for the whole
///   session rather than for one decode Ã¢â‚¬â€ and because the plan asks again every
///   schedule, it was a re-decode of a 30 MB page on every navigation.
///
/// Scoped to their own functions so a `pv.full` or a `Kind::Full` elsewhere in the
/// file cannot stand in for either. The tile loop is inside `central`, so the scan
/// cannot tell which page it was reading Ã¢â‚¬â€ that was verified by instrumentation
/// instead, and the numbers are in the task report.
#[test]
fn both_halves_of_a_turn_are_cut_from_their_own_full_resolution_buffers() {
    let src = main_src();
    // The tile loop moved into `central_tiles` (its own module: central.rs passed the
    // 1000-line target). It must still range over the turn's pages.
    let tiles = fn_body(&src, "fn central_tiles(");
    assert!(
        tiles.contains("tile_pages")
            && tiles.contains("page_rects[i]")
            && tiles.contains("rects[i]"),
        "the tile loop is not ranging over the pages of the turn any more. It has to cut\n\
         each page from that page's own buffer at that page's own rect, or a zoomed\n\
         spread is sharp on one half and soft on the other - with the expensive decode\n\
         already paid for and sitting in memory.\nBody was:\n{tiles}"
    );

    let full = fn_body(&src, "Msg::Full { key, result } => match result {");
    assert!(
        full.contains("pv.full = Some(data)"),
        "a full decode of the turn's other half is being thrown away again. The result\n\
         arrives, is not the cursor's, and is dropped - so that half stays at preview\n\
         resolution for the whole session, and the plan asks for the same decode again\n\
         on every navigation.\nBody was:\n{full}"
    );
}
