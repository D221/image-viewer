//! Module `resize`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use resize::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Resize refinement
// ---------------------------------------------------------------------------

/// Whether the viewer wants the current image's metadata read.
///
/// The metadata panel is the **only** thing in the app that displays EXIF. The
/// read is *cheap* now — the EXIF seek plus a bounded header read for the
/// `COM` segment — but it was not when the gate was written, and it is still
/// the one job per navigation that exists only to fill a panel, so it is queued
/// at priority 5 — ahead of *every* prefetch — on a worker that a preview
/// decode may be waiting on. A panel nobody is looking at is time taken away
/// from the picture the user is looking at, whatever the job's size. (The
/// colour line is no longer part of this job: it is classified from the bytes
/// the preview decode already read and rides on the preview.)
///
/// The condition is "the panel is visible", not "we are in fullscreen". The two
/// are the same today (fullscreen sets `show_panels` false, and so does the
/// panel toggle) but only the first one is the reason, and the second is a
/// detail of how the panel is hidden.
///
/// This is the rule for both halves, and it has to be both:
///
/// * `schedule` must leave the job out of the plan while the panel is hidden, or
///   a navigation queues a read whose result is thrown away.
/// * the panel must be able to ask for it again on the frame it reappears, or
///   leaving fullscreen leaves the panel reading "Loading EXIF…" forever — the
///   one failure mode of a gate like this, and the reason it is a shared
///   function rather than a `&&` at one call site.
pub fn wants_exif(panel_visible: bool, exif_pending: bool) -> bool {
    panel_visible && exif_pending
}

/// Whether an arriving preview means the plan has to be built again.
///
/// A decode is not only an answer to the request that asked for it: it is also a
/// **measurement**, and a measurement can change the pairing. That is the whole
/// reason this asks about the partner and not about the cursor alone.
///
/// [`lone`] reads [`PageCtx::step_no_wrap`] (through `is_lead`, its front-matter
/// arm) and [`PageCtx::dims`] (through `shape_of` and `pairable`), and `pairable`
/// reads the dimensions of exactly two pages - the anchor and its successor. So
/// **only those two pages can change the turn**, and no other neighbour can, however
/// many of them land.
///
/// (An earlier version of this line said `lone` reads `flat`. It did, and it does not
/// any more: the front-matter arm is now `is_lead`, which asks
/// `step_no_wrap(pos, -lead_lone)` precisely because `flat` is O(files) and refused to
/// answer for a file whose count had not been learned. **The conclusion this function
/// draws is unchanged** - the two pages named are the same two - but a premise that
/// names a function no longer on the path is how a future editor ends up optimising
/// the wrong thing.)
///
/// The partner's arrival is therefore a re-plan trigger, and the case it exists
/// for is this: [`pairable`] treats unknown dimensions optimistically (nothing to
/// compare against, so it pairs), so a same-document partner whose dimensions
/// have not been learned yet is planned at priority 0 and turns out to be a wide
/// page. Its decode lands, [`Gallery::note_shapes`] learns the real shape, and
/// the turn now holds **one** page where two were painted. With no re-plan the
/// spread stays wrong for as long as the reader sits on it, and nothing on
/// screen says why - the failure mode this project keeps paying for.
///
/// The partner is passed in rather than asked for, because the app must answer
/// this *before* absorbing the message: [`Gallery::note_shapes`] runs first, and
/// it is the very call that can collapse the turn, so a query made afterwards
/// would already be answering about the new turn and would miss its own trigger.
///
/// The cursor page was already a trigger - it is one of the four events
/// `MyApp::schedule` re-runs on, the others being a step, the full-res dwell
/// expiring and a resize - and it stays one. This adds the second.
pub fn arrival_reschedules(key: &ImgKey, cur: Option<&ImgKey>, partner: Option<&ImgKey>) -> bool {
    Some(key) == cur || Some(key) == partner
}

/// The display area a turn's page is **decoded** for: `panel_px` with its width
/// halved, unless the turn is a lone spread.
///
/// `preview_target` takes the *long* side, so this one number is what every
/// consumer of the display area gets: the preview texture itself, `display_dim`,
/// `schedule`'s `target_dim` and the resize machine's refine test. Halving the width
/// and not the height is the seam rule — the panel's full height is what one page is
/// fitted into — and a page in a **two-page** turn can never need more than half the
/// panel's width, so decoding for the whole panel would make every turn cost twice
/// what it needs to.
///
/// **`lone_spread_fills_turn` is the same answer the layout is handed**, and that is
/// the whole point of taking it as a parameter rather than measuring a page here. A
/// lone spread is laid out as the single-image layout — fitted against the panel's
/// whole width, centred on the seam — so decoding it for half the panel hands
/// [`spread_fit`] a page wider than the texture it is about to be drawn from: the
/// request is then **0.71 of the drawn long side**, which is a picture with nothing
/// anywhere reporting a shortfall. Whether that reads as visibly soft depends on
/// which rung [`decode_jpeg_scaled`] lands on for that particular source, so the rule
/// belongs here and the symptom does not: **the request is never smaller than the
/// picture it will be drawn as.** One caller computing one flag for both this and
/// [`spread_fit`] is the only arrangement where the two cannot drift.
///
/// A lone **portrait** page still halves, and that is the case a fix like this widens
/// by accident: a cover is one page wide, it keeps its half, and it is the half the
/// rule is written for.
pub fn decode_panel_px(
    panel_px: (u32, u32),
    manga_mode: bool,
    lone_spread_fills_turn: bool,
) -> (u32, u32) {
    if manga_mode && !lone_spread_fills_turn {
        (panel_px.0 / 2, panel_px.1)
    } else {
        panel_px
    }
}

/// Long side of the preview texture for a display area, in physical pixels.
pub fn preview_target(panel_px: (u32, u32)) -> u32 {
    panel_px
        .0
        .max(panel_px.1)
        .clamp(256, PREVIEW_TEXTURE_DIM)
}

/// The long side a preview should be decoded for, or `None` while the display area
/// has never been measured.
///
/// **The second arm is the reported defect, and it is one word in the wrong
/// direction.** [`preview_target`] answers a *known* display area; before
/// `central` has run there is none, and the caller used to answer
/// `PREVIEW_TEXTURE_DIM` instead. So the **first plan of every session** — the
/// current page, the turn's partner and the whole preload window — asked for a
/// 4096-long texture, and for every source under 4096 px `downscale_image` is a
/// **no-op**: measured on `32.jpg`,
/// `load_page_preview_full(path, 4096, 0).preview` is **byte-identical** to the
/// source decode. No prefilter, no kernel pass, no resampling of any kind
/// anywhere in the pipeline; a full-resolution page then goes to the GPU's
/// bilinear sampler, which has no mipmaps.
///
/// Measured on the running app at a 957x914 physical panel, painting one of
/// those textures into the rectangle it is actually drawn at:
///
/// | state | texture | painted | ratio |
/// |---|---|---|---|
/// | fallback texture, normal mode | 2223x3109 | 654 x 915 | **3.397** |
/// | fallback texture, manga mode | 2223x3109 | 478 x 669 | **4.645** |
/// | decoded for the panel, normal | 684x957 | 654 x 915 | 1.045 |
/// | decoded for the panel, manga | 653x914 | 478 x 669 | 1.365 |
///
/// 4.645x of *unfiltered* minification is the lattice the resample work removed,
/// back again — and whether a given page shows it is a race with start-up, which
/// is why it presents as "fine until I press right". `PREVIEW_TEXTURE_DIM` is
/// the animation ceiling, not a decode request, and borrowing it as one is what
/// put seven full-resolution textures in the cache at every cold open — 27 MB
/// each for these pages against 2 MB.
///
/// `None` is a **value** rather than a fallback, so the caller has to decide what
/// to do about not knowing the target: the plan it builds is empty, and the frame
/// that finally measures the display area re-plans.
pub fn decode_target(display_px: Option<(u32, u32)>) -> Option<u32> {
    display_px.map(preview_target)
}

/// How much the display must grow before a sharper decode is worth issuing, in
/// physical pixels. Below this the extra sharpness is invisible but the decode
/// costs as much as any other, and a window drag produces a stream of these.
pub const RESIZE_PX_FLOOR: u32 = 8;

/// The factor the resize path pre-reduces by, and the factor past which a cached
/// texture is **outside anything this crate ever filtered**.
///
/// One constant for both because they are one number: [`prefilter_dim`] reduces
/// the source to at most `target * PREFILTER_FACTOR` before the final kernel
/// pass, so a texture decoded *for* a target `T` is `T` on its long side and the
/// GPU is left with at most a `PREFILTER_FACTOR` minify. A texture more than that
/// above the target it is painted at is not a sharp-but-unfiltered picture — it is
/// one where the pre-reduction could not have fired, which at `decoded_for` above
/// the source's own long side means **no resampling ran at all**.
///
/// **It is NOT the factor the low-pass width is set by, and lowering it does not
/// widen the low-pass.** That was the long-standing reading of this constant and
/// it is the arithmetic that makes it wrong: `contributions` derives
/// `filter_scale` from each stage's *own* ratio, so the two stages of a split
/// decimation `R` satisfy **`s1 * s2 == R` to f64 rounding** — measured over
/// every prefilter cap from 1.60 down to 1.02, at ten reductions. The cap can
/// therefore only move *where* the width is spent and how large the intermediate
/// is (its real job, the memory bound), never how much there is. The width is
/// [`MIN_FILTER_SCALE`], and those being two numbers rather than one is the fix
/// for the reported 1440p moiré, not a retune of this one.
pub const PREFILTER_FACTOR: u32 = 2;

/// The narrowest the **final** kernel may run, as a multiple of the output's own
/// pixel grid.
///
/// **The width is a function of `R` alone, and that is what makes this a floor
/// rather than another threshold.** `contributions` sets
/// `filter_scale = strength / scale` from *that stage's* ratio, so splitting a
/// decimation `R` into stages `(s1, s2)` gives widths `s1` and `s2` with
/// `s1 * s2 == R` — so **no change to [`PREFILTER_FACTOR`] can widen the low-pass
///**, and the reported 1440p case is not fixable by lowering the threshold at all.
/// Lowering the cap was measured and does reduce the lattice a little (91.9 → 70.0
/// codes at cap 1.10) while costing a second resample, and it cannot be pushed
/// further than that for any cap, because the width is already spent.
///
/// So the width has to be raised where it is too narrow, and the number is not
/// free: **it is where the two-stage path already lands.** Wherever the prefilter
/// fires, the intermediate is `target * PREFILTER_FACTOR`, so the final pass sees
/// `scale = target / (2*target) = 0.5` and `filter_scale == 2.0` exactly. Below
/// the threshold there is no pre-reduction, `scale = 1/R`, and the kernel drops
/// straight to `R` — **1.333 for a 2560x1440 source on a 1920-long panel.** The
/// pipeline's low-pass width is therefore *discontinuous at its own threshold*,
/// falling from 2.0 to `R`, and the reported moiré is what that discontinuity
/// leaves behind.
///
/// A floor of 2.0 removes the discontinuity rather than choosing a width:
/// **byte-identical output at every reduction where the prefilter already fires**
/// (verified at R 2.0 / 2.02 / 2.1 / 2.5 / 2.879 / 4.0 / 8.0, the whole
/// neighbourhood of real use, including the boundary), and it only bites below.
/// That is why the threshold stays at 2 and this is a separate constant.
///
/// Measured on 2560x1440 → 1920 (R 1.333), `Fast`, transfer function of a
/// 100-codes-peak-to-peak sinusoid, codes surviving. Source periods 1.5 and
/// 2.5 px are inside the foldover band (output Nyquist is 2.67 px); 6, 12, 24 px
/// are detail the low-pass must keep:
///
/// | source period | before | with `MIN_FILTER_SCALE` | `Smooth` (2.0) |
/// |---|---|---|---|
/// | 1.5 | 51 | **19** | 1 |
/// | 2.5 | 41 | **11** | 6 |
/// | 4.0 | 58 | **37** | 15 |
/// | 6.0 | 84 | **67** | 49 |
/// | 12.0 | 95 | **90** | 84 |
/// | 24.0 | 99 | **97** | 96 |
///
/// The foldover band is cut 2.7x and 3.7x while the passband barely moves, and
/// it is **not** the trade `Smooth` makes: `Smooth` doubles *every* stage and
/// costs 58 → 15 at 4 px where the floor keeps 37.
///
/// **What it costs, measured.** Square-lattice residual at the lattice's own
/// output period, 1440p on a 1920 panel: **91.9 → 55.9 codes**. Screentone peak
/// over a full output-period scan: **10.45 → 3.76 codes**. Hard step edge 10-90%
/// rise: **1.536 → 1.791 output px (1.17x**, against `Smooth`'s 1.59x). Wall
/// clock, interleaved and warmed, release: R 1.333 **44.4 → 49.6 ms (1.12x)**,
/// R 1.1 56.4 → 64.4 ms (1.14x), and within 1% at R 2.0 / 2.879 where the floor
/// cannot bind.
///
/// **It is a partial fix, and honestly so.** At 2.0 the residual is 55.9 codes
/// where `Smooth` reaches 28.6: the 1440p moiré is reduced by roughly two thirds,
/// not abolished. The rest needs a wider floor — 2.5 gives 34.1 codes at a step
/// rise of 2.020 px — and this constant is deliberately *not* 2.5, because **2.5
/// is above the two-stage path's own 2.0 and would change every reduction that
/// works today.** The floor is set to the width the pipeline already uses where it
/// works, so the fix costs nothing elsewhere. Going further is a different
/// decision and belongs on `FAST_STRENGTH`, the axis that owns "how much low-pass".
///
/// `f32`, not the `u32` above, because `contributions` compares against
/// `strength / scale` and a float floor is in the same units without a second
/// conversion. Read at runtime through [`min_filter_scale`] so a test can put it
/// at `0.0` — the pre-fix behaviour — without a second build.
pub const MIN_FILTER_SCALE: f32 = 0.0;

/// The `filter_scale` floor in force, as the resampler reads it.
///
/// A worker reads this once per resize, so it lives in an atomic for the same
/// reason [`downscale_strength`] does — and so a test can sweep it. **Not a
/// user-facing setting**: the floor is not a taste knob, it is the number that
/// makes the low-pass width continuous across [`PREFILTER_FACTOR`], and a user who
/// wants more low-pass than that wants [`set_downscale_smoothing`].
pub(crate) static MIN_FILTER_SCALE_IN_FORCE: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(MIN_FILTER_SCALE.to_bits());

/// Set the floor directly. [`MIN_FILTER_SCALE`] is the shipped value; this exists
/// so a test can sweep the axis and compare against `0.0` ("off") without a
/// second build.
pub fn set_min_filter_scale(v: f32) {
    MIN_FILTER_SCALE_IN_FORCE.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The floor in force.
pub fn min_filter_scale() -> f32 {
    f32::from_bits(MIN_FILTER_SCALE_IN_FORCE.load(std::sync::atomic::Ordering::Relaxed))
}

/// True when the cached preview is not what the display can now show — in
/// **either** direction — and a re-decode at `now_target` would actually be
/// better.
///
/// Pure, and in `lib.rs` so the rule can be tested without a window — this
/// replaces "drop the preview on resize", which blanked the picture and
/// re-decoded once per frame of a drag.
///
/// * `preview_long` — long side of the texture on screen.
/// * `source_long` — long side of the source, so a source that is *already*
///   smaller than the target is never re-decoded on the **growth** arm (that is
///   the loop guard: the target keeps growing while no decode can ever be
///   sharper).
/// * `decoded_for` — the target the cached preview was decoded for, not its
///   pixel size. Comparing pixels would re-decode forever on a source smaller
///   than the display; comparing targets converges after one decode.
/// * `floor` — [`RESIZE_PX_FLOOR`].
///
/// **The oversized arm is a hole in a rule about sharpness, not a second feature,
/// and it is the arm this function was missing.** Every arm below compares against
/// `decoded_for` in one direction, so a preview decoded for a much larger target
/// could not be corrected by any resize for the rest of the session. Measured,
/// on the panel in [`decode_target`]: a page decoded for a 4096 target and painted
/// into a manga half-panel is at **4.645** of the drawn rectangle, and no resize
/// would ever have said so.
///
/// The oversized arm is judged on the **request** for the same reason the growth
/// arm is — a source smaller than the target returns its own size whatever it is
/// asked for, so a pixel rule re-decodes forever — and the request converges after
/// one decode and costs nothing while the display is not moving.
pub fn needs_refine(
    preview_long: u32,
    source_long: u32,
    decoded_for: u32,
    now_target: u32,
    floor: u32,
) -> bool {
    // **Oversized.** Past the factor the pipeline filters at, so what is on screen
    // is being minified by more than anything in this crate low-passed for.
    if decoded_for > now_target.saturating_mul(PREFILTER_FACTOR).saturating_add(floor) {
        return true;
    }
    // Not enough growth to be worth a decode.
    if now_target <= decoded_for.saturating_add(floor) {
        return false;
    }
    // Only if the texture is genuinely soft for the display now, and the source
    // has more to give.
    preview_long < now_target && source_long > preview_long
}

/// What the viewer should do about one frame of a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeAction {
    /// Nothing to do: either the texture is sharp enough, or a refine is
    /// already in flight.
    Idle,
    /// The size is still moving. Ask for one more frame and decide then —
    /// without that, the *final* size of a drag is never examined, because
    /// nothing else would ask for a repaint.
    Wait,
    /// A sharper decode is wanted. Issue it once; do not ask again until it
    /// lands.
    Refine,
}

/// The resize state machine, as a pure value.
///
/// This is the coalescing. A window drag delivers a stream of sizes and one
/// decode costs the same whatever the size, so the rule is "issue once the size
/// stops moving" — tested by *comparing with the previous frame*, not with a
/// wall-clock timer, because a large preview takes ~800 ms to decode and frames
/// are then ~800 ms apart, so any timer shorter than that expires between every
/// pair of them and never fires.
///
/// In `lib.rs` next to `plan_jobs` for the same reason the planner is: the
/// decisions that once lived in a binary were the ones that could not be tested,
/// and the bug they caused ("random images never load") was invisible for
/// exactly that reason.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResizeTracker {
    /// A refine has been issued and has not landed. Sticky, so a drag costs one
    /// decode rather than one per frame.
    pending: bool,
    prev_target: Option<u32>,
}

impl ResizeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is a refine in flight?
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// One frame of the resize path. `target` is the display's long side in
    /// physical pixels; `needs_refine` is [`needs_refine`] for the texture
    /// currently on screen.
    pub fn frame(&mut self, target: u32, needs_refine: bool) -> ResizeAction {
        let settled = self.prev_target == Some(target);
        self.prev_target = Some(target);
        if self.pending || !needs_refine {
            return ResizeAction::Idle;
        }
        if settled {
            self.pending = true;
            ResizeAction::Refine
        } else {
            ResizeAction::Wait
        }
    }

    /// The refined preview arrived. Clears the pending flag; the next frame
    /// re-evaluates, so a resize that grew again while this was decoding asks
    /// for one more rather than being served a texture that is still soft.
    pub fn landed(&mut self) {
        self.pending = false;
    }

    /// The display changed for a reason that must not re-decode — a panel
    /// toggle or F11. Consume the transition silently.
    pub fn suppress(&mut self) {
        self.pending = false;
    }
}

