//! Module `spread_layout`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use spread_layout::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Manga spreads: laying one turn out on screen
// ---------------------------------------------------------------------------
//
// Beside `fit_scale_no_upscale` rather than beside the pairing walk further down,
// because this is that function's sibling and its own claim about where pages go:
// the same `(panel.width() / w).min(panel.height() / h)` fit, with the panel's
// **half** width in place of its width, and the same `clamp(1e-4, 8.0)` the
// single-image view fits itself at. Plain floats on purpose — `Rect4` and no egui,
// so a whole turn's layout is a thing a test can check without a window.
//
// Three decisions. Only two of them are quiet — the shared scale is loud, because it
// shows the moment the two halves differ in shape:
//
// * **One shared scale.** Not a fit per page; see `spread_fit`. `pairable` pairs
//   pages whose aspect ratios are merely *close* (`PAIR_ASPECT_TOLERANCE`) rather
//   than equal, so a turn of two differently shaped pages is ordinary, not exotic.
// * **The seam is the panel's horizontal centre, always.** Zero gutter, so two
//   facing pages touch and a drawing across the seam is unbroken; and no special
//   case for a lone *portrait* page, because a page with nothing on the far side
//   of the seam *is* the lone-page case. Getting this wrong shows as a picture a
//   reader only notices pages later.
// * **Input order out, screen order never.** See `spread_rects` — a swap that is
//   wrong in one reading direction and right in the other.
//
// **A lone spread is the exception, and it is the exception to the *rule about lone
// pages*, not to the seam rule.** A page wider than [`SPREAD_ASPECT`] is a scan of two
// printed pages ([`shape_of`]) and takes a whole turn to itself *because* it is the
// whole spread — so hanging it off a seam with nothing on the far side draws half a
// spread at half size, in half the window. It gets the full width instead, which is
// the single-image layout, and it is still **centred on the seam**: the seam stays
// where it is, the page simply straddles it. **Four functions carry the decision, not
// two** — [`spread_fit`] and [`spread_rects`] because they *are* the layout, and
// [`spread_centre`] and [`spread_pan_bounds`] because they derive their numbers from
// those same rects, where a half-width box is a wrong zoom anchor and a pan bound that
// drags the page off the panel. Each takes the decision as `lone_spread_fills_turn`
// rather than measuring the page, because the caller's answer is the one that is right —
// see [`turn_is_a_lone_spread`] for why that is the shape and not the geometry.

/// Whether a turn of `page_count` pages, whose cursor page is judged `shape`, is
/// **one page that is itself a spread** and so fills the whole turn.
///
/// The discriminator is [`Shape`], and it has to be, because the geometry lies in both
/// directions:
///
/// * **A user override outranks the pixels.** `X` forces a page to `Double` whatever
///   it looks like, and "treat this as a spread" has to mean *give it the whole turn* —
///   which is the thing the key promises. A `w > h` test would decline to honour a
///   forced *portrait* page, so the key would re-pair the book and then draw the
///   result at half size: the override half working, silently.
/// * **A near-square page is not a spread.** 1.05 is wider than tall and is a single
///   page (`SPREAD_ASPECT` is 1.2, and above it not at it), so a `w > h` test would
///   hand a lone square-ish page the entire screen instead of the half a cover keeps.
///
/// And the whole point of asking the *caller* is that the layout cannot disagree with
/// the pairing: a turn holds one page either because that page is a spread or because
/// it is front matter ([`LEAD_LONE_DEFAULT`]), those are the only two ways, and they
/// are laid out differently. The pairing already made this exact decision — a `Double`
/// never pairs (`pairable`), so a lone `Double` **is** a lone spread by construction
/// rather than by inference.
pub fn turn_is_a_lone_spread(page_count: usize, shape: Shape) -> bool {
    page_count == 1 && shape == Shape::Double
}

/// The one answer the app needs about the turn under the cursor, from the position
/// alone: **is that turn a lone spread**, so the whole turn is one picture?
///
/// It exists because the app needs this *twice*, in two places that cannot be allowed
/// to disagree — to decide what to **decode** for ([`decode_panel_px`]) and to decide
/// how to **lay out** ([`spread_fit`] and the three functions that follow it). Asking
/// it once is the fix for a picture that was drawn at one size and decoded at another,
/// and the caller cannot get it wrong by accident: the page count comes from
/// [`turn_len`] and the shape from [`shape_of`], so neither `spread_dims.len()` nor
/// `shown_partner.is_none()` is available to be substituted for the rule.
///
/// **Both halves are the same rule as the layout's.** `turn_len` is what
/// `turn_partner` is built from, so the count and the partner cannot diverge; `shape_of`
/// is the shape the fit is judged by. That equivalence is why this can be asked at the
/// top of a frame, before the pages exist, and be the same answer the layout reaches
/// later: nothing here reads the *paint*, so nothing here can be a frame out of date
/// with it.
///
/// `None` for no position at all — nothing on screen yet — answers **false**, which
/// halves the panel exactly as the unfixed code did. A turn cannot be a lone spread
/// before its first frame exists, so this is the conservative answer and not a
/// special case.
pub fn lone_spread_fills_turn(pos: Option<PagePos>, lead_lone: u32, ctx: &dyn PageCtx) -> bool {
    pos.is_some_and(|pos| {
        turn_is_a_lone_spread(turn_len(pos, lead_lone, ctx), shape_of(pos, ctx))
    })
}

/// The scale a whole turn is drawn at: the more constrained fit of every page
/// present into *its half* of the panel, so every page fits at one shared factor.
///
/// **"Its half", except for a lone spread**, which is fitted against the panel's whole
/// width — `lone_spread_fills_turn`, the caller's [`turn_is_a_lone_spread`]. That is
/// not a second fit rule bolted on: it is the same single-image fit, on the one turn
/// that is one picture rather than two, and both pages of a two-page turn get exactly
/// the number they got before.
///
/// A `min` over the pages rather than a fit per page, and that is the load-bearing
/// part. The reference reader does the same thing — `webgpuviewer`'s `ImagePage.kt`
/// takes a `minOf` over its pages while its own comment describes the fit as
/// happening "independently" — and **the code is the honest reading of the two**,
/// because two halves drawn at two scales cannot share one zoom level: there is one
/// `scale` and one `view`, so the constrained page sets the factor and the other one
/// keeps a margin under it. Fitting each page independently would mean fitting each
/// page independently on *paper* and nowhere else.
///
/// It asks for no [`PageCtx`] and knows nothing about pairing: a turn is a list of
/// page sizes and that is the whole of what a layout needs to know.
///
/// Unlike [`fit_scale_no_upscale`] this **may exceed 1.0**, as the single-image
/// view's own fit does: a page is drawn to fill the area it is given. The clamp is the
/// same `(1e-4, 8.0)` that view uses.
///
/// A panel with no area, or a turn with no pages, answers **1.0**, exactly as
/// [`fit_scale_no_upscale`] does for its own degenerate cases. Not a `NaN` guard —
/// `w.max(1.0)` is what stops a division by zero here — but a *value* guard: without
/// the early return the clamp answers `1e-4` for an empty panel, which draws a page a
/// fraction of a point across, and `8.0` for a turn with no pages, where no rect is wrong
/// either way. `1.0` is the one answer that means "as drawn".
pub fn spread_fit(panel: (f32, f32), pages: &[(f32, f32)], lone_spread_fills_turn: bool) -> f32 {
    // **The width a page is fitted into is the panel's *half*, or its whole width when
    // the turn is a lone spread** — `turn_is_a_lone_spread`'s decision, handed in.
    // The `pages.len() == 1` half of it is re-checked here rather than trusted, because
    // this is the function that turns the answer into a width: a flag true on a turn of
    // two is a caller's bug, and honouring it would fit each page against the whole
    // panel and put them on top of each other.
    let fit_w = if lone_spread_fills_turn && pages.len() == 1 {
        panel.0
    } else {
        panel.0 / 2.0
    };
    if fit_w <= 0.0 || panel.1 <= 0.0 || pages.is_empty() {
        return 1.0;
    }
    pages.iter()
        .map(|&(w, h)| (fit_w / w.max(1.0)).min(panel.1 / h.max(1.0)))
        .fold(f32::INFINITY, f32::min)
        .clamp(1e-4, 8.0)
}

/// Where each page of a turn goes, **in input order**, not screen order.
///
/// One shared scale, zero gutter, top-aligned. The seam is always at the panel's
/// horizontal centre: page 0 extends from the seam outward in `rtl`'s direction and
/// page 1 the other way, so a lone **portrait** page needs no special case — it simply
/// has nothing on the far side of the seam, and renders at no more than half the
/// panel's width, hanging off the seam, which is what a book's cover does. Centring a
/// lone page instead is the tempting "fix" and it is wrong twice over: it stops sharing
/// the seam that every other turn's pages meet on, and a centred lone page is
/// indistinguishable from a page with no partner.
///
/// **A lone spread is the one page that does get a special case**, and it is a
/// correction rather than an exception to taste — see `lone_spread_fills_turn`'s
/// discussion at the section head. It straddles the seam (so `rtl` decides nothing
/// about it) and [`spread_fit`] gave it the panel's whole width, which together make
/// it the single-image layout. It is **not** centred in the sense the paragraph above
/// warns about: its inner edge is the seam's mirror, not a margin beside it, and the
/// partner slide further down cannot fire on it because there is no second rect.
///
/// Top-aligned rather than centred, so a page shorter than its partner leaves its
/// gap beneath instead of floating in the middle of its half. A page narrower than
/// its half is not stretched to fill it either — it keeps its own width and leaves
/// the difference at its outer edge.
///
/// `scale` is the single factor [`spread_fit`] chose, applied to both axes, so a
/// fitted turn cannot overflow the panel. At most two pages, and **this does not
/// enforce it**: a turn is one page or two by construction ([`turn_len`]), so a third
/// page takes page 1's side and lands on top of it. That is the honest answer to an
/// input that cannot happen — the alternative is a `take(2)` that silently drops a
/// page — and the rule stays in `turn_len` rather than restated here where the two
/// copies could drift apart.
pub fn spread_rects(
    panel: Rect4,
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> Vec<Rect4> {
    let seam = panel[0] + panel[2] * 0.5;
    // **A lone spread straddles the seam instead of hanging off it**, so `rtl` decides
    // nothing about it: a page with the whole turn to itself has no side to be on, and
    // the `if let [cursor, partner]` seam slide below cannot fire either (it needs two
    // rects), so its edges are the panel's own.
    //
    // The seam is still where it was — the page is *centred* on it rather than moved,
    // so a zoom anchored here and a pan clamped to this box are the same geometry the
    // single-image path already uses. Re-checked `len() == 1` here as in `spread_fit`,
    // because a flag true on a turn of two would stack both pages on the centre.
    let lone_spread = lone_spread_fills_turn && pages.len() == 1;
    pages.iter().enumerate().map(|(i, &(pw, ph))| {
        let w = pw * scale;
        let on_right = if i == 0 { rtl } else { !rtl };
        let x = if lone_spread { seam - w * 0.5 } else if on_right { seam } else { seam - w };
        [x, panel[1], w, ph * scale]
    }).collect()
}

/// Where a turn's content is centred when the view offset is zero, which is what a
/// zoom measures from to keep the point under the cursor where it is.
///
/// **Not the panel centre in a spread, and that is the whole of it.** `spread_rects`
/// puts the seam on the panel's horizontal centre and extends each page outward, so
/// the content's centre is the seam plus half the difference of the two widths - zero
/// for two pages of equal width, which is why the displacement is invisible on a
/// clean pair and shows up the moment pages differ. Vertically it is not the panel's
/// centre either: the turn is **top-aligned**, so the centre is the panel's top plus
/// half the *tallest* page. For a lone **portrait** page the content sits entirely on
/// one side of the seam, displaced by up to half its own half. Both offsets are zero
/// for a single image, which is the case the single-image view's own zoom maths was
/// written for and the assumption this replaces — **and for a lone spread**, which is
/// centred on the seam and top-aligned like everything else, so its horizontal
/// displacement is zero where the lone portrait page's is the largest here. That is
/// worth saying because it is a *third* case that behaves like the first two on screen:
/// `lone_spread_fills_turn` has to be threaded here as well, or a full-panel page is
/// zoomed about a centre taken from a half-width box.
///
/// ## Derived from `spread_rects`, not restated
///
/// The bounding box of the rects [`spread_rects`] actually returned. A second
/// statement of the seam rule would be a second rule: `rtl` decides which page is
/// which side, and a "centre" computed from the seam and the
/// widths by hand agrees with the picture only until one of those changes. So the
/// cost is one two-element `Vec` per zoom frame - nothing beside a decode - and
/// the two cannot drift apart.
///
/// **The paint path's sub-pixel seam slide does move this centre, by up to half a
/// physical pixel, and it is harmless.** The slide is a rigid translation of the
/// *partner's* rect by a sub-pixel amount `d` - the two pages share one edge, so
/// snapping the cursor page's `x` independently would leave a hairline gap of `d` at
/// the seam - so the content's box moves by `d` on one side and the centre with it by
/// `d / 2`. An earlier version of this doc claimed the slide "does not move any rect's
/// centre away from what this returns", and that was false in the exact direction the
/// number is used: this is the anchor a zoom measures from, so the error is a point on
/// the point under the cursor. It is a fraction of a device pixel, it is one-sided (the
/// cursor page does not move, so the slide can only *close* a gap, never open one), and
/// it is not worth a second rule. **Stated here rather than fixed**, so the next reader
/// does not derive a seam rule from this box and then find the hairlines it cannot
/// close.
///
/// **A turn with no pages answers the panel centre**, exactly what `central` computed
/// for itself before a turn existed, and bit for bit: `panel[0] + panel[2] * 0.5` is
/// `Rect::center().x` spelled out with the same operations in the same order. That is
/// the single-image path, so it must not move.
///
/// Same arguments as [`spread_rects`], and the same deliberate non-checks: a turn is
/// one page or two by construction ([`turn_len`]), and a third page would land on top
/// of the second rather than widening the box.
pub fn spread_centre(
    panel: Rect4,
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> (f32, f32) {
    match spread_box(panel, pages, scale, rtl, lone_spread_fills_turn) {
        Some(b) => ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5),
        None => (panel[0] + panel[2] * 0.5, panel[1] + panel[3] * 0.5),
    }
}

/// The box a turn's pages occupy, as `[min_x, min_y, max_x, max_y]`, or `None` for a
/// turn with no pages.
///
/// The one statement of "where the content is", shared by [`spread_centre`] and
/// [`spread_pan_bounds`] so the two cannot disagree about it - for the same reason
/// `spread_centre` derives itself from [`spread_rects`] rather than restating the
/// seam rule. `spread_rects` is called once per caller rather than passed in: it
/// allocates a `Vec` either way and borrowing it would invite a caller to hand over
/// rects from somewhere other than this rule, which is the drift being avoided.
pub(crate) fn spread_box(
    panel: Rect4,
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> Option<[f32; 4]> {
    let rects = spread_rects(panel, pages, scale, rtl, lone_spread_fills_turn);
    let first = rects.first()?;
    let (mut min_x, mut max_x, mut min_y, mut max_y) =
        (first[0], first[0] + first[2], first[1], first[1] + first[3]);
    for r in &rects[1..] {
        min_x = min_x.min(r[0]);
        max_x = max_x.max(r[0] + r[2]);
        min_y = min_y.min(r[1]);
        max_y = max_y.max(r[1] + r[3]);
    }
    Some([min_x, min_y, max_x, max_y])
}

/// The range the view offset may take, as `(low, high)`: the furthest the content may
/// be dragged up/left and down/right before a gap opens between it and the panel.
///
/// **Not symmetric, and that is the whole of it.** `central` used to clamp to
/// `±(content·scale − panel.size()) / 2`, which is the range that holds a content
/// **centred on the panel's centre** and nothing else. A turn's content is not centred
/// on it, by the same displacement [`spread_centre`] exists to report: horizontally by
/// half the difference of the two widths, vertically because the turn is top-aligned.
/// Panning moves content the *other* way, so the range is the symmetric one **translated
/// by minus that displacement** - not narrowed, moved - and what that is worth is a
/// measured number: on a two-page spread (a 2000-wide and a 2800-wide page) over 41 zoom
/// steps, the symmetric bound truncated **36 of them**, all of them steps that still had
/// room to pan, by up to 67 points. (The other 5 are truncations both bounds share, at
/// scales where the content still fits the panel - refusing to pan there is the point.)
///
/// **Derived from the content, not from a second statement of it.** The bound is read
/// off the same rectangles [`spread_rects`] returns (via [`spread_box`], shared with
/// [`spread_centre`]): the content may be dragged until **its own edge** meets the
/// panel's, so `low` is `panel.max − content.max` and `high` is `panel.min − content.min`,
/// and the width of that range is `content − panel` on both accounts. A hand-written
/// "where is the content" in this function is the exact drift this fix exists to remove -
/// the clamp and the picture have to be one geometry, not two that agree today.
///
/// **An axis the content fits in is locked at zero**, which is what "centre it on axes
/// where it fits" means and what `.max(ZERO)` meant in the old expression. A lone
/// **portrait** page is the case that makes this visible: [`spread_fit`] draws it at no
/// more than half the panel's width, so for every zoom at which it fits the panel it
/// cannot be dragged sideways at all - it stays on its own side of the seam, which is
/// `a_lone_page_rests_with_the_seam_on_the_centre`'s rule and this function's
/// consequence. Past `2 × fit` the page is wider than the panel, has no half left to stay
/// in, and the ordinary bound takes over; the part of it that a zoom pushes past the
/// panel's edge before that does not pan into view, which is the old behaviour too and
/// unchanged by this.
///
/// **A lone spread is the counter-case, and it needs the flag threaded here too.**
/// `spread_fit` gave it the panel's whole width, so at fit it exactly fills the panel
/// on any axis the width bound and this returns `(0, 0)` on it - the same answer as the
/// lone portrait page, reached for the **opposite** reason, and the two are one edit
/// apart. One zoom step past fit separates them. On a 2400x1200 page (ratio 2.0, wider
/// than this 1920x1080 panel, so width-bound) at `1.01 x fit` the flagged bound is
/// `x (-9.6, 9.6)` - symmetric, because the lone spread is centred on the seam, which
/// is the single-image behaviour - and a bound derived from the **half-width** box the
/// flag forgot about answers `x (-979.2, -960.0)`: it would drag a page that is only
/// 1% too wide to nine hundred points off the panel, and the zoom's wanted offset
/// would be silently truncated to that. A 2700x1920 page does not show this at all
/// (it is height-bound, so `x` never opens), which is why the claim is stated for the
/// width-bound case and the number is the one that was measured.
///
/// **`pages` empty is the single-image path, and it is bit for bit what it always was**:
/// the same `((foot·scale − panel.size()) * 0.5).max(0)` on each axis, the same
/// operations in the same order, from the same `foot`. That is the whole of the gate -
/// `foot` is read on that branch and on no other - and it is what
/// `the_pan_bound_is_symmetric_only_when_the_content_is_centred` pins, since a content
/// centred on the panel's centre is exactly the case the old expression is right about.
/// `foot` is the caller's own fold of the turn's pages (widths sum, heights max) or the
/// single image's footprint; the turn's branch deliberately does not read it, because the
/// box it derives from `spread_rects` is the same number and reading it twice is the
/// second statement this file keeps refusing to write.
///
/// Both bounds are finite and `low <= high`, which the caller relies on: it clamps with
/// `f32::clamp`, and that panics on an inverted range rather than returning a picture
/// that is subtly in the wrong place. It cannot be inverted - the range is
/// `content − panel` wide, so a content that fits gives `(0, 0)` and a content that does
/// not has `panel.min − content.min > panel.max − content.max` strictly - and
/// `a_pan_range_is_always_ordered_and_finite` pins that over the degenerate inputs.
pub fn spread_pan_bounds(
    panel: Rect4,
    foot: (f32, f32),
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> ([f32; 2], [f32; 2]) {
    // The single-image path, byte for byte: the same `((foot·scale − panel.size()) * 0.5)`
    // the binary computed inline, the same operations in the same order, and the same
    // `.max(0.0)` per axis. `Vec2::clamp(−m, m)` and `f32::clamp(m_lo, m_hi)` are the same
    // function for finite inputs, so the caller's switch to per-axis `f32::clamp` does not
    // move one image either.
    if pages.is_empty() {
        let half = [
            ((foot.0 * scale - panel[2]) * 0.5).max(0.0),
            ((foot.1 * scale - panel[3]) * 0.5).max(0.0),
        ];
        return ([-half[0], -half[1]], half);
    }

    // A turn: the content's own edges against the panel's, which is the same arithmetic
    // read the other way round. `low` puts the content's trailing edge on the panel's
    // trailing edge and `high` puts its leading edge on the panel's leading edge; the
    // range is `content − panel` wide, so a content that fits leaves it empty.
    //
    // `spread_box` cannot answer `None` here - `spread_rects` returns one rect per page -
    // so the fallback is the unreachable one, and it is the *locked* answer rather than
    // the symmetric one, which is what a caller wanting to clamp needs.
    let Some([min_x, min_y, max_x, max_y]) =
        spread_box(panel, pages, scale, rtl, lone_spread_fills_turn)
    else {
        return ([0.0, 0.0], [0.0, 0.0]);
    };
    let axis = |c_lo: f32, c_hi: f32, p_lo: f32, p_hi: f32| {
        if c_hi - c_lo <= p_hi - p_lo {
            // Fits: no edge to reach and no gap to open, so it does not pan at all.
            (0.0f32, 0.0f32)
        } else {
            (p_hi - c_hi, p_lo - c_lo)
        }
    };
    let (lo_x, hi_x) = axis(min_x, max_x, panel[0], panel[0] + panel[2]);
    let (lo_y, hi_y) = axis(min_y, max_y, panel[1], panel[1] + panel[3]);
    ([lo_x, lo_y], [hi_x, hi_y])
}

/// Footprint `(width, height)` an image of `w x h` occupies once `xform` has been
/// applied to it.
///
/// Only the four axis-swapping transforms change the shape; a mirror does not,
/// which is why this is asked of the transform rather than derived from a turn
/// count. All eight keep the result axis-aligned, so 1:1 stays pixel-sharp.
pub fn xformed_footprint(w: f32, h: f32, xform: Xform) -> (f32, f32) {
    if xform.swaps_axes() {
        (h, w)
    } else {
        (w, h)
    }
}

