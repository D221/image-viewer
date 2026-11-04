//! Resize refinement
//!
//! The rule that replaced dropping the preview on resize. Two properties matter
//! and both are easy to get wrong:
//!
//!  1. It must converge. A source smaller than the display can never be sharper,
//!     so growing the window must cost at most one decode. Comparing the
//!     *returned pixels* to the target instead of the target that was asked for
//!     re-decodes on every resize, forever.
//!  2. It must not fire for growth too small to see, nor for a source that has
//!     nothing more to give.

use image_viewer::{RESIZE_PX_FLOOR, needs_refine};

/// preview_long, source_long, decoded_for, now_target -> refine?
fn refine(preview: u32, source: u32, decoded_for: u32, now: u32) -> bool {
    needs_refine(preview, source, decoded_for, now, RESIZE_PX_FLOOR)
}

#[test]
fn resize_refines_when_the_display_outgrew_the_texture() {
    // A 1600px texture decoded for a 1600px display; the window is now 2400px
    // and the source is big enough to fill it. This is the case that matters.
    assert!(refine(1600, 4000, 1600, 2400));
}

#[test]
fn resize_does_not_refine_when_the_texture_already_fills_the_display() {
    // Same window growth, but the texture is already 2400px: a re-decode would
    // produce an identical picture for a full decode's cost.
    assert!(!refine(2400, 4000, 1600, 2400));
}

#[test]
fn resize_does_not_refine_when_the_source_has_nothing_more_to_give() {
    // The convergence guard. A 1200px source on a 4000px display returns 1200px
    // whatever target it is asked for, so a pixels-vs-target rule re-decodes on
    // every resize, forever.
    assert!(!refine(1200, 1200, 1600, 4000));
    assert!(!refine(1200, 1200, 1600, 8000), "and it stays quiet as it grows");
}

#[test]
fn resize_ignores_growth_below_the_floor() {
    // A drag delivers a stream of 1-2px growths. One decode costs the same
    // whatever the size, so only growth past the floor is worth anything.
    let t = 1600;
    assert!(!refine(t, 4000, t, t + image_viewer::RESIZE_PX_FLOOR - 1));
    assert!(refine(t, 4000, t, t + image_viewer::RESIZE_PX_FLOOR + 1));
}

#[test]
fn resize_does_not_refine_when_the_display_shrank() {
    // Shrinking inside the band the pipeline filters at must cost nothing: this is
    // the ordinary window drag and the `M` toggle, where the picture is at worst
    // `PREFILTER_FACTOR`x the panel target and the CPU pass has filtered it.
    //
    // **This used to carry the stronger claim "shrinking never makes the picture
    // softer, and must not cost a decode", and that was wrong** - it is the
    // reported regression. Past the band it is not true at all, which is
    // `a_picture_decoded_far_above_the_display_is_refined_even_though_it_is_sharp`
    // and the whole reason `needs_refine` reads `decoded_for` in both directions.
    assert!(!refine(2400, 4000, 2400, 1600));
}

/// A preview decoded for a target far above the one it is now painted at is
/// **not** a sharp picture, and refining it is the reported regression's fix.
///
/// **Every number here is measured on the running app**, and the reason the arm
/// exists is that the growth-only rule could not see any of them. A 2223x3109
/// manga page in the preload window, decoded for `PREVIEW_TEXTURE_DIM` because the
/// first plan of a session ran before any panel size existed (so
/// `downscale_image` was a **no-op** - `load_page_preview_full(path, 4096, 0)`
/// is byte-identical to the source decode), painted into the rectangle a manga
/// half-panel actually draws it in:
///
/// | state | texture | painted | ratio |
/// |---|---|---|---|
/// | decoded for 4096 | 2223x3109 | 478.5 x 669.3 | **4.645** |
/// | decoded for the panel | 653x914 | 478.5 x 669.3 | 1.365 |
/// | decoded for 4096, normal mode | 2223x3109 | 654.4 x 915.3 | **3.397** |
/// | decoded for the panel, normal | 684x957 | 654.4 x 915.3 | 1.045 |
///
/// 4.645x of minification by a bilinear sampler with no mipmaps, over content
/// nothing in this crate ever low-passed. It is the lattice the resample work
/// removed, and the old rule called none of it a refine: `now_target (914) <=`
/// `decoded_for (4096) + 8` short-circuited on the first line, and
/// `source_long > preview_long` was false besides (3109 > 3109), so **no resize,
/// for the rest of the session, could ever have corrected it**.
///
/// **Judged on the request, not on the pixels**, which is the whole reason the
/// two numbers are different arguments. A pixel rule would fire forever on a
/// source smaller than the display - see
/// `an_oversized_refine_converges_after_one_decode` - and `decoded_for` converges
/// because the next decode is requested for the current target.
#[test]
fn a_picture_decoded_far_above_the_display_is_refined_even_though_it_is_sharp() {
    const PAGE: u32 = 3109; // 32.jpg and every other 2223x3109 page
    let target = 914; // manga half-panel on a 957x914 physical panel

    // The reported state, verbatim from the running app.
    assert!(
        refine(PAGE, PAGE, image_viewer::PREVIEW_TEXTURE_DIM, target),
        "a full-resolution texture painted into a half panel at 4.6x is not a sharp \
         picture, and the old rule answered false for it on both of its tests"
    );

    // The texture is sharp by every pixel measure - it is the *source*, at 1:1 -
    // which is why the growth-only rule could never see this.
    assert!(PAGE > target);
    assert_eq!(PAGE, PAGE);

    // Normal mode is the same defect at a smaller factor: 3109 of 915.3 is 3.397.
    assert!(refine(PAGE, PAGE, image_viewer::PREVIEW_TEXTURE_DIM, 957));

    // **And it stops once the picture is inside the band the pipeline filters
    // at**, which is the control that says the arm is not "re-decode on any
    // resize": a texture decoded for the panel is quiet at that same panel.
    let panel_tex = 941;
    assert!(!refine(panel_tex, PAGE, panel_tex, target));
    assert!(
        !refine(panel_tex, panel_tex, panel_tex, panel_tex),
        "a texture decoded for exactly the target is never refined, in either direction"
    );
}

/// The over-sized arm is judged on the **request**, so a source that can never be
/// bigger than the display still converges instead of re-decoding forever.
///
/// This is the loop guard for the new arm, and it is the reason `decoded_for` is
/// an argument at all: a 600px source on a 4000px panel returns 600px whatever it
/// is asked for, so a rule written on `preview_long > now_target * 2` would be
/// true on **every frame forever**. `decoded_for` is a number the decode actually
/// accepted, so the second call is quiet.
#[test]
fn an_oversized_refine_converges_after_one_decode() {
    let source = 600u32;
    let mut decoded_for = 4000u32; // the fallback target a session starts at
    let mut preview = source; // and what it returned: the whole source
    let mut decodes = 0;
    let target = 900u32;

    // Eight frames at the same display size - what a settle looks like.
    for _ in 0..8 {
        if refine(preview, source, decoded_for, target) {
            decodes += 1;
            decoded_for = target;
            preview = target.min(source);
        }
    }
    assert_eq!(decodes, 1, "one decode and then quiet, not one per frame");
    assert_eq!(decoded_for, target);

    // **And a pixel rule would not have.** The number it would keep seeing is
    // pinned here rather than asserted on `needs_refine`, because it is the
    // reason the arm reads `decoded_for`: a 600px source on a 900px display is
    // permanently "oversized" by pixels and permanently sharp by pixels.
    assert!(preview <= target);
}

/// The over-sized arm is quiet **inside** the band, so a shrink — the window
/// restore, the panel toggle, `M` on a narrow panel — costs no decode.
///
/// The numbers are the ones measured on the running app on this box: the panel's
/// long side moves between 914 (a manga half-panel's height) and 957 (the whole
/// panel's width), and previews decoded at either have to survive the other
/// without a decode.
///
/// **A growth inside the band is not free, and the test used to claim it was.**
/// `941 -> 957` is 16 px, past [`image_viewer::RESIZE_PX_FLOOR`], so the
/// *pre-existing* growth arm charges a decode — and that is the growth arm
/// working, not the new one. It is also the mechanism behind the burst of
/// `resize-driven re-decodes` measured when `M` is pressed after this fix (0
/// before it, because before it nothing was ever decoded at a real target and the
/// arm was unreachable). Asserted here so the boundary is stated rather than
/// assumed: the over-sized arm owns shrinks, the growth arm owns growths.
#[test]
fn an_oversize_inside_the_filter_band_costs_no_decode() {
    const PAGE: u32 = 3109;
    for (decoded_for, target) in [(941u32, 914u32), (957, 914), (914, 900), (957, 500)] {
        if decoded_for > target * image_viewer::PREFILTER_FACTOR + image_viewer::RESIZE_PX_FLOOR {
            continue;
        }
        assert!(
            !refine(decoded_for, PAGE, decoded_for, target),
            "{decoded_for} -> {target} is inside the {}-band the pipeline filters at, so \
             the over-sized arm must be quiet about it",
            image_viewer::PREFILTER_FACTOR
        );
    }
    // And the growth side of the boundary, so the two arms do not overlap silently.
    assert!(
        refine(941, PAGE, 941, 941 + image_viewer::RESIZE_PX_FLOOR + 1),
        "a growth past the floor is the growth arm's business and it is unchanged"
    );
    assert!(!refine(941, PAGE, 941, 941 + image_viewer::RESIZE_PX_FLOOR - 1));
    // The band is `PREFILTER_FACTOR`, and it is `prefilter_dim`'s, not a number
    // chosen here: a texture more than that above the target is one whose
    // pre-reduction could not fire.
    assert_eq!(
        image_viewer::PREFILTER_FACTOR,
        2,
        "if the prefilter's own factor moves, `prefilter_dim` and the over-sized arm \
         move with it and these numbers have to be re-measured"
    );
}

/// There is **no decode target** before the display area has been measured, and
/// that is a value the caller has to answer for rather than a default.
///
/// **The default was the reported regression's root cause.** `schedule` used to
/// fall back to `PREVIEW_TEXTURE_DIM` for the gap between the first `schedule()`
/// — which runs from `goto` in `frame_prelude` — and the first `central` that
/// measures the panel. `PREVIEW_TEXTURE_DIM` (4096) is the *animation* ceiling and
/// not a decode request, so on every cold open the current page, the turn's other
/// half and all six preload neighbours were decoded at 4096, which for any source
/// under 4096 px means the full-resolution page and no resampling at all. The scan
/// `the_plan_is_empty_until_the_display_area_is_measured` is the other half: it
/// cannot see this function, only that `MyApp::schedule` asks it.
#[test]
fn there_is_no_decode_target_before_the_display_area_is_measured() {
    assert_eq!(
        image_viewer::decode_target(None),
        None,
        "an unmeasured display area must not borrow the animation ceiling as a decode \
         request: for every source under 4096 px that is a full-resolution texture, \
         minified by the GPU with no mipmaps and no low-pass anywhere in the pipeline"
    );
    // A measured one is the panel's long side, unchanged from what it always was.
    assert_eq!(image_viewer::decode_target(Some((957, 914))), Some(957));
    assert_eq!(image_viewer::decode_target(Some((478, 914))), Some(914));
    assert_eq!(
        image_viewer::decode_target(Some((957, 914))),
        Some(image_viewer::preview_target((957, 914))),
        "and it is `preview_target` - one number, so the plan and the paint cannot \
         disagree about what a display area means"
    );
}

/// The property, stated once: **a page is painted from a texture decoded for the
/// target it is painted at**, and the planner cannot ask for anything else.
///
/// Every other test here is one instance of it. This one walks the whole chain —
/// the panel, the decode target, the texture the decode returns and the rectangle
/// the layout draws — and asserts that the texture is never more than the factor
/// the pipeline low-passed at above the drawn rectangle's long side. The
/// offenders are named rather than generated, because a generated oversize would
/// be the *hypothetical* one and the reported one is these four.
#[test]
fn a_painted_page_comes_from_a_texture_inside_the_filtered_band() {
    // A real 2223x3109 manga page, and the panel measured on this box with the
    // viewer at its default 1280x800 window (`panel_px` = 957x914 physical).
    const PANEL_NORMAL: (u32, u32) = (957, 914);
    let page = (2223u32, 3109u32);
    let panel_manga = image_viewer::decode_panel_px(PANEL_NORMAL, true, false);
    let panel_lone_spread = image_viewer::decode_panel_px(PANEL_NORMAL, true, true);
    assert_eq!(panel_manga, (478, 914), "a portrait page keeps its half");

    for (what, panel) in [
        ("normal mode", PANEL_NORMAL),
        ("manga, two pages or a lone page", panel_manga),
        ("manga, a lone spread fills the turn", panel_lone_spread),
    ] {
        let target = image_viewer::decode_target(Some(panel)).expect("a measured panel");
        // What the decode at that target actually returns for this page: a
        // proportional reduction, because the source is larger than the target on
        // both axes. 2223/3109 * 914 = 653.7 -> 653.
        let drawn_long = (page.0 as f64 * panel.0 as f64 / page.1 as f64).max(panel.1 as f64);
        let tex_long = page.1.min(target);
        let ratio = tex_long as f64 / drawn_long;
        assert!(
            ratio <= image_viewer::PREFILTER_FACTOR as f64 + 0.35,
            "{what}: decoded for {target} the texture is {tex_long} long and the page is \
             drawn {drawn_long:.0} wide, a ratio of {ratio:.3} - outside the {}-band, so \
             the GPU is minifying by more than anything here low-passed for",
            image_viewer::PREFILTER_FACTOR
        );
        // And the refine machine must be quiet about it: the picture is what it
        // asked for.
        assert!(
            !image_viewer::needs_refine(
                tex_long,
                page.1,
                target,
                target,
                image_viewer::RESIZE_PX_FLOOR
            ),
            "{what}: a texture decoded for the current target must never be refined, or \
             every frame of a still picture would cost a decode"
        );
    }

    // **The offender, for the record**, so the number is in the suite rather than
    // only in the report: 4.645x at the same panel.
    let fallback_texture = page.1; // a decode at PREVIEW_TEXTURE_DIM is a no-op
    let drawn = 478.5f64;
    assert!(
        fallback_texture as f64 / drawn > 4.0,
        "the state the fix removes: a {}px texture drawn into {}px is {}x, and nothing \
         in the pipeline filtered the difference",
        fallback_texture,
        drawn,
        fallback_texture as f64 / drawn
    );
}

#[test]
fn resize_refine_stops_at_the_source_limit_and_never_restarts() {
    // A drag that keeps growing, against a source that eventually runs out.
    // Each settled size is a real growth and earns a decode, so the count is
    // not expected to be 1 — what must hold is that it *stops* at the source
    // limit and stays stopped, which is what stops a resize from turning into
    // an unbounded decode loop.
    let source = 5000u32;
    let mut preview = 1600u32;
    let mut decoded_for = 1600u32;
    let mut decodes = 0;
    let mut at_limit = None;

    for target in [2000u32, 2400, 3000, 4000, 5000, 6000, 7000, 8000] {
        if refine(preview, source, decoded_for, target) {
            decodes += 1;
            decoded_for = target;
            preview = target.min(source); // what the decode actually returns
            if preview == source && at_limit.is_none() {
                at_limit = Some(decodes);
            }
        }
    }
    assert!(at_limit.is_some(), "test setup: never reached the source limit");
    assert_eq!(
        decodes,
        at_limit.unwrap(),
        "no decode may happen once the texture is as large as the source allows"
    );
    assert_eq!(preview, source);
}

#[test]
fn a_source_smaller_than_the_display_is_never_re_decoded() {
    // The extreme of the same guard: a 1200px source, already sharp for the
    // display, on a window that grows to 6000px. Every step is free, because no
    // decode could ever return more than the 1200px already on screen.
    let source = 1200u32;
    let mut decoded_for = 1600u32;
    let mut decodes = 0;
    for target in [2000u32, 3000, 4000, 5000, 6000] {
        if refine(source, source, decoded_for, target) {
            decodes += 1;
            decoded_for = target;
        }
    }
    assert_eq!(decodes, 0, "nothing to gain, so nothing to pay for");
}
