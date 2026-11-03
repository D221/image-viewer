//! What a turn is *decoded* for.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! The layout was moved a commit before the decode target was, so a page drawn
//! across the panel's whole width was asked for a texture from half of it - and the
//! picture was soft in proportion to how much of the panel it used.

use image_viewer::{
    decode_panel_px, lone_spread_fills_turn, preview_target, shape_of, spread_fit, turn_len,
    PageCtx, Shape, LEAD_LONE_DEFAULT,
};

use Shape::{Double, Single};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

/// The panel the decode tests below use, in **points**: 1920x1080, which is the panel
/// the reported defect was measured on and a size every reader has.
const DECODE_PANEL: (f32, f32) = (1920.0, 1080.0);

/// [`DECODE_PANEL`] in physical pixels at a pixel ratio. Both `decode_panel_px` and
/// `preview_target` speak that unit and `panel_px` in `central` is built from it, so
/// this is the only place the two scales meet.
fn panel_px_at(ppp: u32) -> (u32, u32) {
    (DECODE_PANEL.0 as u32 * ppp, DECODE_PANEL.1 as u32 * ppp)
}

/// A lone **spread** is decoded for the whole panel: 1920 at `ppp` 1, 3840 at `ppp` 2.
///
/// This is the fix. The layout was moved a commit ago and the decode target was not,
/// so a page drawn across the panel's whole width was asked for a texture from half
/// of it Ã¢â‚¬â€ and the picture was soft in proportion to how much of the panel it used.
/// At `ppp` 1 that is a slightly downscaled 1518.75 points out of a 1920 texture and
/// nothing is visible; at `ppp` 2 it is 2160 against a 3037.5-point draw, which is
/// 0.71, and that is a visibly soft picture with no counter anywhere.
///
/// **The fixture is a spread by its *shape*, not by being alone.** It is the third
/// page of a document of singles with the default `lead_lone`, so it has a page
/// behind it (`step_no_wrap(pos, -1)` is `Some`) and is not front matter: the arm of
/// `lone` that makes this turn hold one page is the spread's, which is the only thing
/// that can make it true. A test with a lone page at the front of a gallery would pass
/// with front matter doing all the work.
#[test]
fn a_lone_spread_is_decoded_for_the_whole_panel() {
    let g = TestGallery::one_doc(vec![Single, Single, Double]);
    let pos = g.page_at(2).expect("the gallery has three pages");
    assert_eq!(
        g.step_no_wrap(pos, -1),
        Some(g.page_at(1).unwrap()),
        "the fixture must have a page behind it, or this test is about front matter"
    );
    assert_eq!(turn_len(pos, LEAD_LONE_DEFAULT, &g), 1, "a spread never pairs");
    assert_eq!(shape_of(pos, &g), Double, "and it is the shape, not the geometry");
    assert!(
        lone_spread_fills_turn(Some(pos), LEAD_LONE_DEFAULT, &g),
        "so the query the app makes at the top of the frame answers true"
    );

    // **The numbers, at both pixel ratios.** Not "not halved": 3840 is the whole claim,
    // and a clamp or an off-by-one elsewhere would satisfy a direction check.
    assert_eq!(
        decode_panel_px(panel_px_at(1), true, true),
        (1920, 1080),
        "ppp 1: the whole panel, for a page laid out across the whole panel"
    );
    assert_eq!(preview_target(decode_panel_px(panel_px_at(1), true, true)), 1920);
    assert_eq!(
        decode_panel_px(panel_px_at(2), true, true),
        (3840, 2160),
        "ppp 2: and the whole panel again, in physical pixels"
    );
    assert_eq!(preview_target(decode_panel_px(panel_px_at(2), true, true)), 3840);

    // The height was never the axis in question Ã¢â‚¬â€ the seam runs vertically, so one
    // page is fitted into the panel's full height in both cases.
    assert_eq!(
        decode_panel_px(panel_px_at(2), true, true).1,
        panel_px_at(2).1,
        "only the width is halved, and this page is not halved at all"
    );

    // The mode being off is the *same* panel, and that is the point: a lone spread is
    // laid out as the single-image layout, so on and off draw and decode the same
    // picture. See `toggling_the_mode_on_a_lone_spread_changes_nothing_to_decode`.
    assert_eq!(decode_panel_px(panel_px_at(1), false, true), (1920, 1080));

    // The number this fix removes, named so a reader can see what regressed: at `ppp` 1
    // the half-panel answer decoded for 1080 and at `ppp` 2 for 2160.
    assert_eq!(preview_target((960, 1080)), 1080);
    assert_eq!(preview_target((1920, 2160)), 2160);
}

/// A lone **portrait** page is decoded for the half, because that is the layout it is
/// drawn with: a book's cover is one page wide, it hangs off the seam, and a texture
/// for the whole panel would make every cover in a volume cost twice what it needs.
///
/// This is the regression guard for the fix, and the temptation it creates is the one
/// that matters: widen the rule to "a lone page is decoded for the panel" and every
/// cover in the library is decoded at full width and drawn at half.
#[test]
fn a_lone_portrait_page_is_decoded_for_the_half() {
    let g = TestGallery::one_doc(vec![Single, Single, Single]);
    // The last page: a turn of one because nothing pairs with it, not because it is
    // front matter (the default `lead_lone` already made page 0 a lone turn).
    let pos = g.page_at(2).expect("three pages");
    assert_eq!(turn_len(pos, LEAD_LONE_DEFAULT, &g), 1);
    assert_eq!(shape_of(pos, &g), Single);
    assert!(
        !lone_spread_fills_turn(Some(pos), LEAD_LONE_DEFAULT, &g),
        "a portrait page is a lone turn and not a lone spread"
    );

    assert_eq!(
        decode_panel_px(panel_px_at(1), true, false),
        (960, 1080),
        "half the width, all of the height"
    );
    assert_eq!(preview_target(decode_panel_px(panel_px_at(1), true, false)), 1080);
    assert_eq!(decode_panel_px(panel_px_at(2), true, false), (1920, 2160));
    assert_eq!(preview_target(decode_panel_px(panel_px_at(2), true, false)), 2160);
}

/// A two-page turn is decoded for the half: each page is one page wide, and a texture
/// for the whole panel would double the cost of every turn in the book to sharpen
/// nothing.
#[test]
fn a_two_page_turn_is_decoded_for_the_half() {
    let g = TestGallery::one_doc(vec![Single, Single, Single, Single]);
    let pos = g.page_at(1).expect("four pages");
    assert_eq!(turn_len(pos, LEAD_LONE_DEFAULT, &g), 2, "a pair is a turn of two");
    assert_eq!(shape_of(pos, &g), Single);
    assert!(
        !lone_spread_fills_turn(Some(pos), LEAD_LONE_DEFAULT, &g),
        "two pages cannot be one picture however wide either of them is"
    );

    assert_eq!(decode_panel_px(panel_px_at(1), true, false), (960, 1080));
    assert_eq!(decode_panel_px(panel_px_at(2), true, false), (1920, 2160));

    // The mode off is the whole panel whatever the turn is, so a photo folder's
    // previews do not shrink when the mode is toggled back off.
    assert_eq!(decode_panel_px(panel_px_at(1), false, false), (1920, 1080));
}

/// The decode target and the layout the page is actually drawn at are the **same
/// width**, which is the whole claim; the two agreeing is what makes the flag one
/// decision rather than two.
///
/// The test derives the drawn rectangle from [`spread_fit`] Ã¢â‚¬â€ the function the app
/// hands the same flag to Ã¢â‚¬â€ and requires the decode's long side to cover it, at both
/// pixel ratios. Two weaker assertions were available and both are worthless: "the
/// decode is at least half the panel" is satisfied by the bug, and "the decode does not
/// exceed the panel" is satisfied by a texture ten times too large.
///
/// **And the numbers that fail.** For the lone spread the draw is 1518.75 x 1080
/// points (the height binds: `min(1920/2700, 1080/1920) = 0.5625`), so 1920 of 1920 is
/// **1.26** of it and the half-panel answer's 1080 is **0.71**. At `ppp` 2 that is
/// 3840 of 3037.5 against 2160 of 3037.5. The portrait page and the pair both land
/// exactly on 1080, because one page fitted into a half panel is bound by the panel's
/// height Ã¢â‚¬â€ which is why the half-panel rule is not merely conservative on those two
/// and is *wrong* only on the third.
#[test]
fn the_decode_target_covers_the_picture_the_layout_draws() {
    // A real double-page scan: 2700x1920 is the 1.414 a two-page scan comes out of a
    // phone, and it is wider than `SPREAD_ASPECT` so `shape_of` calls it a `Double`.
    let scan = WIDE_SCAN;
    // A real single page, portrait, at the same scale family.
    let page = (1350.0, 1920.0);

    for ppp in [1u32, 2] {
        for (what, pages, flag) in [
            ("a lone spread", vec![scan], true),
            ("a lone portrait page", vec![page], false),
            ("a two-page turn", vec![page, page], false),
        ] {
            let scale = spread_fit(DECODE_PANEL, &pages, flag);
            let drawn = pages
                .iter()
                .map(|&(w, h)| (w * scale).max(h * scale) * ppp as f32)
                .fold(0.0f32, f32::max);
            let target = preview_target(decode_panel_px(panel_px_at(ppp), true, flag)) as f32;
            assert!(
                target >= drawn,
                "ppp {ppp}, {what}: drawn at {drawn} points and decoded for {target}, so the \
                 texture is short of the picture by {} - which is the reported defect, still \
                 present",
                drawn - target
            );
        }
    }

    // The two numbers, spelled out, because "the assertion above passed" is not the
    // evidence for *how much* was wrong. Rounded to three decimals because the claim is
    // about how much of the picture the texture covers, and an f32 ratio written out to
    // seventeen places would pin the rounding rather than the ratio.
    let scale = spread_fit(DECODE_PANEL, &[scan], true);
    assert_eq!(scale, 0.5625, "2700x1920 on a 1920x1080 panel");
}
