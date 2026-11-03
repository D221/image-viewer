//! Where a turn may be panned.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! The zoom keeps the point under the cursor where it is, and then the offset is
//! clamped so the picture cannot be lost off-screen. **The clamp was the same bug
//! one line further down**: `central` clamped to
//! `(content * scale - panel.size()) / 2`, the range that holds a content *centred
//! on the panel's centre* and nothing else. A turn's content is displaced from the
//! panel's centre by exactly what `spread_centre` reports, so the bound is the
//! symmetric one **translated by minus that displacement**.

use image_viewer::{spread_centre, spread_fit, spread_pan_bounds, spread_rects, Rect4};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

/// The content's box at offset zero, as `[min_x, min_y, max_x, max_y]`.
///
/// Derived from [`spread_rects`] here on purpose: it is the *test's* definition of
/// "where the content is", taken from the same rule the bound is derived from, so the
/// two can be compared rather than the test restating the bound's own arithmetic.
fn content_box(panel: Rect4, pages: &[(f32, f32)], scale: f32, rtl: bool) -> [f32; 4] {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for r in spread_rects(panel, pages, scale, rtl, false) {
        b[0] = b[0].min(r[0]);
        b[1] = b[1].min(r[1]);
        b[2] = b[2].max(r[0] + r[2]);
        b[3] = b[3].max(r[1] + r[3]);
    }
    b
}

/// The footprint the caller folds from the turn's `spread_dims`: **widths sum, heights
/// max**, which is what `spread_rects` builds when it hangs two pages off one seam.
///
/// Deliberately *not* read off the rects, so that a test can compare the two and a
/// disagreement is visible. `the_pan_range_is_the_footprint_the_caller_folded` is that
/// test.
fn folded_footprint(pages: &[(f32, f32)]) -> (f32, f32) {
    pages
        .iter()
        .fold((0.0f32, 0.0f32), |(w, h), &(pw, ph)| (w + pw, h.max(ph)))
}

/// The pan range reaches the **content's** edges: dragged to one end of it, an edge of
/// the picture meets the panel's edge on that side - and no further.
///
/// Four fixtures in one test, because the claim has two independent halves and both
/// have to hold: an **unequal pair** (the case the symmetric bound gets wrong, since
/// its content is displaced) and a **lone page** (displaced by up to half its own
/// half), each in both reading directions.
///
/// An axis the content **fits** in has no such edge to reach - the picture is smaller
/// than the panel, so no drag can bring an edge anywhere it is not already - and the
/// bound there is the locked zero that "centre it on axes where it fits" means. Both
/// branches are asserted: without the second, a bound that answered a fixed range on
/// every axis would pass the first.
#[test]
fn a_spread_pans_to_both_of_its_own_edges() {
    let unequal = [(400.0, 800.0), (700.0, 800.0)];
    let lone = [(400.0, 800.0)];
    let scale = 1.0; // Comfortably past fit on every axis of both fixtures.

    for (label, pages, x_rtl) in [
        // rtl content at 1.0: the pair spans [-300, 800] and the lone page [400, 800].
        ("unequal pair", &unequal[..], [0.0, 300.0]),
        ("lone page", &lone[..], [0.0, 0.0]),
    ] {
        let foot = folded_footprint(pages);
        // The two reading directions are mirror images, so the expectation for `ltr` is
        // the negation of the one for `rtl` - which is also asserted, rather than assumed,
        // because a bound that ignored `rtl` would otherwise be checked on one axis only.
        for (rtl, want_x) in [(true, x_rtl), (false, [-x_rtl[1], -x_rtl[0]])] {
            let b = content_box(PANEL, pages, scale, rtl);
            let (low, high) = spread_pan_bounds(PANEL, foot, pages, scale, rtl, false);

            // Horizontal, pinned. At 1.0 the two slots are 400 and 700 wide, so the pair
            // spans 990 either way and its content is 190 wider than the panel: 150 of
            // that excess is the displacement and 150 the rest, and the old bound handed
            // back only half the excess, symmetric about the panel's centre.
            assert_eq!(
                (low[0], high[0]),
                (want_x[0], want_x[1]),
                "{label} rtl={rtl}: the x range is a property of the content's edges"
            );

            // Vertical, both fixtures: top-aligned, so 800 tall against a 600 panel.
            assert!(
                (low[1] + 200.0).abs() < 1e-3 && high[1].abs() < 1e-3,
                "{label} rtl={rtl}: y range ({}, {}), expected (-200, 0)",
                low[1],
                high[1]
            );

            // The property the numbers are an instance of, on both axes.
            for (axis, (c_lo, c_hi, p_lo, p_hi)) in [
                (0usize, (b[0], b[2], PANEL[0], PANEL[0] + PANEL[2])),
                (1, (b[1], b[3], PANEL[1], PANEL[1] + PANEL[3])),
            ] {
                if c_hi - c_lo > p_hi - p_lo {
                    assert!(
                        (c_hi + low[axis] - p_hi).abs() < 1e-3,
                        "{label} rtl={rtl} axis {axis}: at the low end the content's trailing \
                         edge must sit on the panel's, and it is {} off",
                        (c_hi + low[axis] - p_hi).abs()
                    );
                    assert!(
                        (c_lo + high[axis] - p_lo).abs() < 1e-3,
                        "{label} rtl={rtl} axis {axis}: at the high end the content's leading \
                         edge must sit on the panel's, and it is {} off",
                        (c_lo + high[axis] - p_lo).abs()
                    );
                } else {
                    assert_eq!(
                        (low[axis], high[axis]),
                        (0.0, 0.0),
                        "{label} rtl={rtl} axis {axis}: the content is {} and the panel {} - it \
                         fits, so it is not dragged at all",
                        c_hi - c_lo,
                        p_hi - p_lo
                    );
                }
            }
        }
    }

    // The lone page's own half, which is why its horizontal range is a single point:
    // at this scale the page is 400 wide, exactly half the panel, so its outer edge is
    // already the panel's edge and there is no drag left in that direction.
    for rtl in [false, true] {
        let b = content_box(PANEL, &lone, 1.0, rtl);
        let (low, high) = spread_pan_bounds(PANEL, folded_footprint(&lone), &lone, 1.0, rtl, false);
        assert_eq!((low[0], high[0]), (0.0, 0.0), "rtl={rtl}: a lone page never pans sideways");
        let (inner, outer) = if rtl { (b[0], b[2]) } else { (b[2], b[0]) };
        assert!((inner - 400.0).abs() < 1e-3, "rtl={rtl}: its inner edge is the seam");
        assert!(
            (outer - (if rtl { 800.0 } else { 0.0 })).abs() < 1e-3,
            "rtl={rtl}: and its outer edge is the panel's"
        );
    }
}

/// The bound is the old symmetric one **translated by minus the content's displacement**,
/// and it is only symmetric where the content is centred.
///
/// This is the whole rule in one sentence, and the equal-width pair is pinned on both
/// sides of it:
///
/// * **Zero displacement** - two equal pages are symmetric about the seam - and the
///   bound is `Ã‚Â±(footÃ‚Â·scale Ã¢Ë†â€™ panel.size()) / 2` **bit for bit**. That is the case the
///   old expression was accidentally right about, and it is pinned so this change
///   cannot silently move it.
/// * **Non-zero displacement** and the bound is translated. Horizontally by half the
///   width difference, and vertically for **every** turn, equal pair included: the turn
///   is top-aligned, so its vertical centre is the panel's top plus half the tallest
///   page, which is the panel's vertical centre only in the one case where the turn is
///   exactly as tall as the panel.
///
/// The single-image path is the third case: no pages at all, the content *is* the
/// panel, and the bound is the symmetric one on both axes - unchanged, which is why it
/// is asserted with `assert_eq!` and not a tolerance.
#[test]
fn the_pan_bound_is_symmetric_only_when_the_content_is_centred() {
    let panel_centre = (PANEL[0] + PANEL[2] * 0.5, PANEL[1] + PANEL[3] * 0.5);
    let equal = [(600.0, 800.0), (600.0, 800.0)];

    for rtl in [false, true] {
        // --- the zero-displacement case, bit for bit.
        //
        // 1.5 is not arbitrary: 600 x 1.5 and 1200 x 1.5 are exact, so this equality is
        // a comparison of bit patterns rather than of two roundings that happen to
        // agree, and a bound computed by a different route (a hand-restated seam, say)
        // would fail it rather than pass within a tolerance.
        let foot = folded_footprint(&equal);
        let (low, high) = spread_pan_bounds(PANEL, foot, &equal, 1.5, rtl, false);
        let half = [
            ((foot.0 * 1.5 - PANEL[2]) * 0.5).max(0.0),
            ((foot.1 * 1.5 - PANEL[3]) * 0.5).max(0.0),
        ];
        assert_eq!((low[0], high[0]), (-half[0], half[0]), "rtl={rtl}: x is unchanged");
        // ...and the vertical one is not, which is the half of this a clean pair hides.
        assert!(
            (low[1] + 2.0 * half[1]).abs() < 1e-3 && high[1].abs() < 1e-3,
            "rtl={rtl}: y is ({}, {}), and the top-aligned content's own edges want \
             (-{:.0}, 0): the turn's top edge IS the panel's top edge, so it can be dragged \
             down by exactly its excess over the panel and no further, and never below zero",
            low[1],
            high[1],
            2.0 * half[1]
        );

        // --- the general law, at several scales and both turn shapes.
        for pages in [&equal[..], &[(400.0, 800.0), (700.0, 800.0)], &[(400.0, 800.0)]] {
            let foot = folded_footprint(pages);
            for scale in [0.6f32, 0.9, 1.1, 1.5] {
                let (low, high) = spread_pan_bounds(PANEL, foot, pages, scale, rtl, false);
                let (cx, cy) = spread_centre(PANEL, pages, scale, rtl, false);
                let b = content_box(PANEL, pages, scale, rtl);
                for axis in 0..2 {
                    let mid = (low[axis] + high[axis]) * 0.5;
                    let width = (high[axis] - low[axis]) * 0.5;
                    let disp = [cx - panel_centre.0, cy - panel_centre.1][axis];
                    let content = [b[2] - b[0], b[3] - b[1]][axis];
                    let panel = [PANEL[2], PANEL[3]][axis];
                    // The law is about a range that exists. Where the content **fits**,
                    // the range is the locked zero and there is no centre to place -
                    // which is a case of its own and is asserted as one.
                    if content <= panel {
                        assert_eq!(
                            (low[axis], high[axis]),
                            (0.0, 0.0),
                            "pages {pages:?} rtl={rtl} s={scale} axis {axis}: {} fits in {}, so \
                             it is not panned at all",
                            content,
                            panel
                        );
                        continue;
                    }
                    assert!(
                        (mid + disp).abs() < 1e-3,
                        "pages {pages:?} rtl={rtl} s={scale} axis {axis}: the range is centred on \
                         {mid} and the content is displaced by {disp} - panning moves content the \
                         other way, so the centre of the range is minus the displacement"
                    );
                    let excess = content - panel;
                    assert!(
                        (width - excess * 0.5).abs() < 1e-3,
                        "pages {pages:?} rtl={rtl} s={scale} axis {axis}: and it is as wide as the \
                         old one ({width} against {}), because translating a range does not \
                         resize it",
                        excess * 0.5
                    );
                    // The name of the test, stated where it is measured: a non-zero
                    // displacement cannot leave the bound symmetric about zero, and a zero
                    // one cannot move it.
                    assert_eq!(
                        low[axis] == -high[axis],
                        disp.abs() < 1e-6,
                        "pages {pages:?} rtl={rtl} s={scale} axis {axis}: the bound ({}, {}) is \
                         symmetric while the content is displaced by {disp}",
                        low[axis],
                        high[axis]
                    );
                }
            }
        }
    }

    // --- the single-image path, unchanged and not approximately unchanged.
    for rtl in [false, true] {
        for (foot, scale) in [((1600.0, 1200.0), 1.0f32), ((1600.0, 1200.0), 0.4), ((400.0, 300.0), 2.0)] {
            let (low, high) = spread_pan_bounds(PANEL, foot, &[], scale, rtl, false);
            let half = [
                ((foot.0 * scale - PANEL[2]) * 0.5).max(0.0),
                ((foot.1 * scale - PANEL[3]) * 0.5).max(0.0),
            ];
            assert_eq!(
                (low, high),
                ([-half[0], -half[1]], half),
                "rtl={rtl}: one image is centred on the panel, which is the only case the old \
                 expression was written for, so it has to be the same f32"
            );
        }
    }
}

/// A lone page cannot be dragged off the seam at any zoom **at which it still fits the
/// panel**, and past that it is the ordinary bound.
///
/// The rule is `a_lone_page_rests_with_the_seam_on_the_centre`'s - a lone page hangs off
/// the seam rather than being centred - and this is its consequence under a pan. A
/// bound that let the page sideways would slide it across the seam and turn a cover page
/// into a two-page scan, which is the one reading Mihon's note rules out.
///
/// **Over a range of scales, not one**, because the interesting case is the zoomed-in
/// one: at fit the page is smaller than the panel on both axes and nothing could have
/// panned it anyway. Past fit the vertical range opens (the page is taller than the
/// panel) and it is exactly then that a horizontal range computed from the panel centre
/// would start letting the page slide.
///
/// **And the limit of the claim is asserted too**, because "never pans sideways" is easy
/// to over-apply: past the panel's own width a lone page is wider than the half it hangs
/// off, it has no half left to stay in, and refusing to pan it at all would put a fifth of
/// the picture permanently out of reach. That case has its own assertion, and the controls
/// at the bottom - an unequal pair which does pan at the same scales, and a lone page whose
/// vertical range does open - are what stop "the range is always zero" from being what
/// makes this pass.
#[test]
fn panning_does_not_move_a_lone_page_off_its_seam() {
    let lone = [(400.0, 800.0)];
    let foot = folded_footprint(&lone);
    let mut pair_had_room = false;
    let mut lone_vertical_opened = false;

    for rtl in [false, true] {
        // 0.75 is this page's **fit** on this panel (`min(400/400, 600/800)`), and is in
        // the sweep for that reason: at fit nothing may be panned on either axis, and a
        // bound that opened one there would be caught. 2.0 is the last scale at which a
        // 400-wide page still fits an 800 panel - at 2.0 it is exactly as wide as the
        // panel - and it is where the sweep stops being about the seam at all (below).
        for scale in [0.75f32, 1.0, 1.5, 2.0] {
            let (low, high) = spread_pan_bounds(PANEL, foot, &lone, scale, rtl, false);
            assert_eq!(
                (low[0], high[0]),
                (0.0, 0.0),
                "rtl={rtl} s={scale}: a lone page is at most half the panel wide while it fits \
                 the panel at all, so it is never dragged sideways - and {} left would put it \
                 across the seam",
                high[0]
            );
            // Stated as the property rather than as the number: for every offset the
            // bound permits, the page's inner edge is still the seam.
            for o in [low[0], 0.0, high[0]] {
                let rects = spread_rects(
                    [PANEL[0] + o, PANEL[1], PANEL[2], PANEL[3]],
                    &lone,
                    scale,
                    rtl,
                    false,
                );
                let inner = if rtl {
                    rects[0][0]
                } else {
                    rects[0][0] + rects[0][2]
                };
                assert!(
                    (inner - 400.0).abs() < 1e-3,
                    "rtl={rtl} s={scale}: at offset {o} the page's inner edge is at {inner}, not \
                     on the seam"
                );
            }
            // The vertical is the other half of the claim, and it is the axis that opens
            // as the zoom grows: the page's own top edge is the panel's top edge, so it
            // can be dragged **up** by its excess and never down.
            assert!(
                high[1] <= 0.0 && low[1] <= 0.0,
                "rtl={rtl} s={scale}: the vertical range ({}, {}) must never allow a positive \
                 offset - a top-aligned content has nothing to give at the panel's top edge",
                low[1],
                high[1]
            );
            lone_vertical_opened |= low[1] < 0.0;
        }

        // Past the panel's own width the page has no half left to stay in - it is 1200
        // wide here, against a panel of 800 - and the ordinary bound takes over. Pinned
        // because the tempting "fix" for the assertions above is to lock a lone page
        // sideways **forever**, which would make the leftmost fifth of a zoomed page
        // unreachable.
        let (low, high) = spread_pan_bounds(PANEL, foot, &lone, 3.0, rtl, false);
        assert_eq!(
            (low[0], high[0]),
            if rtl { (-800.0, -400.0) } else { (400.0, 800.0) },
            "rtl={rtl}: a page wider than the panel pans by its excess, towards its own side"
        );

        let pair = [(400.0, 800.0), (700.0, 800.0)];
        for scale in [0.75f32, 1.0, 1.5] {
            let (low, high) = spread_pan_bounds(PANEL, folded_footprint(&pair), &pair, scale, rtl, false);
            pair_had_room |= low[0] != 0.0 || high[0] != 0.0;
        }
    }
    assert!(
        lone_vertical_opened,
        "the control: the vertical range does open somewhere in the sweep, so the horizontal \
         (0, 0) above is the layout and not a bound that never opens at all"
    );
    assert!(
        pair_had_room,
        "the control: an unequal pair does pan horizontally at these scales, so a lone page's \
         (0, 0) is the layout and not a bound that never opens"
    );
}

/// The two halves compose: a zoom about a point on the **partner's** half leaves that
/// point under the pointer once the offset is clamped.
///
/// This is the regression as reported, and it is asserted as a composition rather than
/// as a bound in isolation, because either half alone was already right. The zoom
/// computes the offset that would hold the anchored point still
/// (`zoom_offset_about`, below - a copy of the expression in the binary, which
/// `the_zoom_measures_from_the_content_centre_at_both_scales` pins is still there), and
/// then the clamp is applied to it exactly as a frame does. The claim is about the
/// **point**, not the offset: `offset` may legitimately be truncated when the zoom wants
/// a pan past the content's own edge, and what must survive is the picture under the
/// cursor.
///
/// The fixture is built so the symmetric bound truncates these steps and the content's
/// bound does not. Panel 800x600, an unequal pair of tall pages (400x1200 and
/// 700x1200), fitting at exactly 0.5, and a zoom to 1.0 - a factor of two, so the wanted
/// offsets are large. The bounds at 1.0 are `x (0, 300)` and `y (Ã¢Ë†â€™600, 0)` against the
/// symmetric `Ã‚Â±150` and `Ã‚Â±300`.
///
/// **Two anchors, because one of them leaves the clamp idle.** At x=150 the zoom asks
/// for `+250`, comfortably inside `(0, 300)`, so "the point did not move" would be
/// satisfied by a bound that did nothing at all. At x=100 it asks for `+300` - exactly
/// the limit, the content's trailing edge on the panel's - so the clamp is *at its
/// edge* and the assertion is about the geometry rather than about a no-op. Both are
/// asserted, and the residual is **bit-exact zero** in each: the point lands on the
/// pointer's coordinates, not within a tolerance of them.
///
/// The control is in both: the symmetric bound moves the point by 315 points at the
/// first anchor and 150 at the second, at a scale of 1.0 where a point **is** a source
/// pixel.
#[test]
fn zooming_about_the_partner_half_leaves_that_point_under_the_pointer() {
    let pages = [(400.0, 1200.0), (700.0, 1200.0)];
    let foot = folded_footprint(&pages);
    assert_eq!(
        spread_fit((PANEL[2], PANEL[3]), &pages, false),
        0.5,
        "the fixture starts at fit, so the step below starts from offset zero"
    );
    let (from, to) = (0.5f32, 1.0f32);

    // Anchors on the *partner's* half - the left half under rtl, the right one under
    // ltr - and near the panel's bottom edge, where a top-aligned turn's content has the
    // most room to want a pan. `x = 100` is the one that lands on the bound's limit.
    for (rtl, anchors) in [
        (true, [(150.0f32, 250.0f32), (100.0, 300.0)]),
        (false, [(650.0f32, -250.0f32), (700.0, -300.0)]),
    ] {
        for (anchor_x, want_x) in anchors {
            let anchor = (anchor_x, 599.0f32);
            let partner = spread_rects(PANEL, &pages, from, rtl, false)[1];
            assert!(
                anchor.0 >= partner[0] && anchor.0 <= partner[0] + partner[2],
                "rtl={rtl}: the anchor at x={} is on the partner's half [{:.1}, {:.1}], not \
                 the cursor's - a test that zoomed about the cursor's own side would not be \
                 the reported regression",
                anchor.0,
                partner[0],
                partner[0] + partner[2]
            );

            let wanted = zoom_offset_about(PANEL, &pages, from, to, rtl, anchor, (0.0, 0.0));
            let (low, high) = spread_pan_bounds(PANEL, foot, &pages, to, rtl, false);
            let clamped = (
                wanted.0.clamp(low[0], high[0]),
                wanted.1.clamp(low[1], high[1]),
            );

            // The content point under the pointer, and where it lands after the clamp.
            let (cf, ct) = (
                spread_centre(PANEL, &pages, from, rtl, false),
                spread_centre(PANEL, &pages, to, rtl, false),
            );
            let q = (
                (anchor.0 - cf.0 - 0.0) / from,
                (anchor.1 - cf.1 - 0.0) / from,
            );
            let moved = |o: (f32, f32)| {
                let (x, y) = (ct.0 + o.0 + q.0 * to, ct.1 + o.1 + q.1 * to);
                ((x - anchor.0).powi(2) + (y - anchor.1).powi(2)).sqrt()
            };

            // The wanted offsets, pinned, so a change in `zoom_offset_about` cannot
            // quietly make this test agree with itself. The numbers are the fixture's.
            assert_eq!(
                wanted,
                (want_x, -599.0),
                "rtl={rtl}: what the zoom asks for, before any clamp"
            );
            assert_eq!(
                clamped, wanted,
                "rtl={rtl}: so a correct bound truncates nothing here, and the two offsets are \
                 one number - which is what makes the point's position below a check on the \
                 bound and not on the zoom"
            );
            if want_x.abs() > 250.0 {
                // What makes the second anchor worth having: its wanted offset is exactly
                // *on* the bound, so the clamp is at its limit rather than idle, and the
                // assertion below is about the geometry and not about a no-op.
                assert!(
                    wanted.0 == low[0] || wanted.0 == high[0],
                    "rtl={rtl} x={}: this anchor is supposed to land exactly on the bound, and \
                     {} is not on {}..{}",
                    anchor.0,
                    wanted.0,
                    low[0],
                    high[0]
                );
            }
            assert!(
                moved(clamped) == 0.0,
                "rtl={rtl} x={}: the point under the pointer moved {} points. The wanted \
                 offset ({:?}) is inside the bound ({:?}..{:?}), so nothing should move it at \
                 all - not 'not much'",
                anchor.0,
                moved(clamped),
                wanted,
                low,
                high
            );

            // The control, and the reason the numbers above are the ones they are: the
            // old symmetric bound truncates this very step.
            let old = [
                ((foot.0 * to - PANEL[2]) * 0.5).max(0.0),
                ((foot.1 * to - PANEL[3]) * 0.5).max(0.0),
            ];
            let old_clamped = (
                wanted.0.clamp(-old[0], old[0]),
                wanted.1.clamp(-old[1], old[1]),
            );
            assert_eq!(
                old_clamped,
                (want_x.signum() * old[0], -old[1]),
                "rtl={rtl} x={}: the old bound's answer",
                anchor.0
            );
            assert!(
                moved(old_clamped) >= 100.0,
                "rtl={rtl} x={}: the control. The symmetric bound is supposed to be the one \
                 that fails here - it truncates {wanted:?} to {old_clamped:?} - and it moved \
                 the point by {}, which would mean this test is not testing anything",
                anchor.0,
                moved(old_clamped)
            );
        }
    }
}

/// The offset a zoom about `anchor` asks for, copied from `MyApp::central`.
///
/// The exact two-scale form, and **not** the single-centre version the brief suggested:
/// `c_to = anchor Ã¢Ë†â€™ C(target)` and `c_from = anchor Ã¢Ë†â€™ C(current)` bracket the same
/// content point, and reusing one centre for both is exact only when the content's
/// centre does not move with the zoom. That is true for a single image and false for
/// every turn, so the copy below is the one the binary has. The binary's own copy is
/// pinned by `the_zoom_measures_from_the_content_centre_at_both_scales`; this is a copy
/// so that a test can *use* the expression, not so that it can check it.
fn zoom_offset_about(
    panel: Rect4,
    pages: &[(f32, f32)],
    scale: f32,
    target: f32,
    rtl: bool,
    anchor: (f32, f32),
    offset: (f32, f32),
) -> (f32, f32) {
    let centre_at = |s: f32| spread_centre(panel, pages, s, rtl, false);
    let (cf, ct) = (centre_at(scale), centre_at(target));
    let c_from = (anchor.0 - cf.0, anchor.1 - cf.1);
    let c_to = (anchor.0 - ct.0, anchor.1 - ct.1);
    (
        c_to.0 - (c_from.0 - offset.0) * (target / scale),
        c_to.1 - (c_from.1 - offset.1) * (target / scale),
    )
}

/// The pan range's **width** is the content's excess over the panel, so it cannot be
/// computed from the panel's centre by accident - and it agrees with the footprint the
/// caller folded from `spread_dims`, which is what says the bound and the picture are
/// one geometry rather than two that agree today.
///
/// Widths sum and heights max, so the box `spread_rects` returns is exactly
/// `foot * scale` on both axes: for any two pages, the sum of the two widths and the
/// larger of the two heights. Asserting it over a spread of shapes is what pins "the
/// clamp's geometry is the layout's geometry".
#[test]
fn the_pan_range_is_the_footprint_the_caller_folded() {
    for pages in [
        &[(400.0, 800.0), (700.0, 800.0)][..],
        &[(600.0, 800.0), (600.0, 800.0)],
        &[(400.0, 800.0)],
        &[(1200.0, 800.0)],
        &[(400.0, 800.0), (300.0, 1600.0)],
    ] {
        let foot = folded_footprint(pages);
        for scale in [0.3f32, 0.75, 1.0, 2.0] {
            for rtl in [false, true] {
                let b = content_box(PANEL, pages, scale, rtl);
                let (low, high) = spread_pan_bounds(PANEL, foot, pages, scale, rtl, false);
                for axis in 0..2 {
                    let content = [b[2] - b[0], b[3] - b[1]][axis];
                    let panel = [PANEL[2], PANEL[3]][axis];
                    let folded = [foot.0 * scale, foot.1 * scale][axis];
                    assert!(
                        (content - folded).abs() < 1e-3,
                        "pages {pages:?} s={scale} rtl={rtl} axis {axis}: the rects span {content} \
                         and the caller's folded footprint is {folded} - two statements of one \
                         footprint, which is the drift this is not allowed to have"
                    );
                    assert!(
                        (high[axis] - low[axis] - (content - panel).max(0.0)).abs() < 1e-3,
                        "pages {pages:?} s={scale} rtl={rtl} axis {axis}: the range is {} wide and \
                         the content's excess over the panel is {}",
                        high[axis] - low[axis],
                        (content - panel).max(0.0)
                    );
                }
            }
        }
    }
}

/// The bound is always ordered and finite, which is what lets the caller use
/// `f32::clamp` on it.
///
/// That clamp **panics** on an inverted range (`assert!(min <= max)`), so an
/// implementation that returned `high` before `low` on an axis where the content fits -
/// where the true range is empty - would take the window down rather than draw
/// something. The degenerate inputs are the ones that can do it: no pages, a
/// zero-sized page, a zero-area panel, and a panel whose origin is not the window's.
///
/// The containment is arithmetic rather than a runtime guard: the range is
/// `content - panel` wide, so a content that fits gives `(0, 0)` and a content that does
/// not has `panel.min - content.min > panel.max - content.max` strictly. This is that
/// claim over the inputs, so it does not have to be argued about every time the bound
/// changes.
#[test]
fn a_pan_range_is_always_ordered_and_finite() {
    for (panel, pages) in [
        (PANEL, vec![]),
        (PANEL, vec![(0.0, 0.0)]),
        ([0.0, 0.0, 0.0, 0.0], vec![(400.0, 800.0)]),
        ([10.0, 20.0, 0.0, 0.0], vec![(400.0, 800.0), (400.0, 800.0)]),
        (PANEL, vec![(400.0, 800.0), (700.0, 800.0), (500.0, 900.0)]),
    ] {
        let foot = folded_footprint(&pages);
        for scale in [0.0f32, 1e-4, 0.5, 1.0, 8.0] {
            for rtl in [false, true] {
                let (low, high) = spread_pan_bounds(panel, foot, &pages, scale, rtl, false);
                for axis in 0..2 {
                    assert!(
                        low[axis].is_finite() && high[axis].is_finite(),
                        "panel {panel:?} pages {pages:?} s={scale} rtl={rtl} axis {axis}: \
                         ({}, {}) is not a number a clamp can take",
                        low[axis],
                        high[axis]
                    );
                    assert!(
                        low[axis] <= high[axis],
                        "panel {panel:?} pages {pages:?} s={scale} rtl={rtl} axis {axis}: \
                         ({}, {}) is inverted, and `f32::clamp` panics on that rather than \
                         returning a picture in the wrong place",
                        low[axis],
                        high[axis]
                    );
                }
            }
        }
    }
}

/// The clamp in `central` has to be the one the library computed, and **this cannot see
/// whether the maths works** - only which function is asked.
///
/// A source scan, for the reason `AGENTS.md` ("Tests") gives: `MyApp::central` is in a
/// binary no integration test can reach, and reverting the wiring leaves every
/// behavioural test above green. That was run as a guard and is recorded in the task
/// report as a **no-op**, which is exactly why this exists.
///
/// Comments are stripped first, because the block quotes the expression it replaced
/// (`(clamp_px * scale - panel.size()) * 0.5`) and scanning the raw text would pass on
/// the comment alone.
///
/// What it *does* catch is the regression that actually happens: the call being reverted,
/// or a later edit clamping with one bound on both axes - the shape a reader of a fix
/// for "the bound is wrong in a spread" is most likely to reintroduce, since the
/// symmetric form is one line shorter.
#[test]
fn the_pan_clamp_asks_the_library_for_the_range() {
    let src = main_src();
    let body = fn_body(&src, "fn central(");
    let code = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("spread_pan_bounds"),
        "the clamp is not asking the library for the range any more. `central` is in a binary\n\
         no test can reach, so nothing else in this file notices - and the expression it is\n\
         likely to have gone back to, `((clamp_px * scale - panel.size()) * 0.5)`, is the one\n\
         that assumes the content is centred on the panel, which a turn's is not on either\n\
         axis. `spread_pan_bounds` reads the bound off the rectangles the same frame draws.\n\
         Body was:\n{body}"
    );
    assert!(
        !code.contains("clamp_px * scale - panel.size()") && !code.contains("let max_off"),
        "the symmetric bound is being computed in the binary again, which is the defect this\n\
         section fixes: it is `Ã‚Â±(content - panel) / 2` about the PANEL's centre, so in a spread\n\
         it truncates the zoom's anchor as soon as the zoom wants a pan the content's own\n\
         edges do not allow - 36 of 41 zoom steps on the running app, by up to 67 points.\n\
         Body was:\n{body}"
    );
}
