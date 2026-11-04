//! Module `manga`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use manga::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// A screen rectangle as `[x, y, width, height]`. Plain floats rather than
/// `egui::Rect` so it can be tested: this module has no egui dependency on
/// purpose.
pub type Rect4 = [f32; 4];

/// The aspect ratio above which an untagged page is taken to be a scan of two
/// printed pages, and so takes a whole turn to itself.
///
/// 1.2 and not 1.0, which is what most readers use: a real page is about 0.707
/// and a real spread about 1.414, so a threshold of 1.0 sits on the midpoint and
/// would call a square-ish single page a spread. Above the threshold, not at or
/// below it - a page exactly at 1.2 is a page.
pub const SPREAD_ASPECT: f32 = 1.2;

/// How far two pages' aspect ratios may differ and still be one turn.
///
/// Mihon's `pairAspectTolerance`. This is the check that keeps a landscape page
/// out of a pair rather than squashing it into a half, and 0.1 is loose enough
/// that two scans of the same book - slightly cropped, slightly rotated -
/// still pair.
pub const PAIR_ASPECT_TOLERANCE: f32 = 0.1;

/// How many pages at the front of a gallery are shown one to a turn: the cover
/// alone, then pairs.
///
/// 1 because that is how a book opens, and because the most-reported pairing
/// failure in every mature reader is the opposite case - a scan whose blank
/// front-matter page was dropped, which wants 0. It is a setting because which
/// one is right depends on the book and not on the format.
pub const LEAD_LONE_DEFAULT: u32 = 1;

/// The most leading pages [`LEAD_LONE_DEFAULT`] can be raised to.
///
/// A bound rather than a free number because the setting is nudged with a key,
/// and a gallery is only ever asked about its first few pages - past that the
/// question "is this page front matter?" has stopped having an answer.
pub const LEAD_LONE_MAX: u32 = 4;

/// What a page looks like, which decides whether it takes a whole turn.
///
/// Serialised, because the user's override for one page is written to disk (see
/// [`GallerySpreads`]) into a file meant to be read and edited by a person: the
/// format is chosen so that a shape can be typed by hand. `rename_all =
/// "lowercase"` is what makes the documented spelling (`"double"`) the one that
/// *loads*; without it serde writes `"Double"` and refuses `"double"`, so a
/// hand-edited file silently becomes no overrides at all — the same trap an
/// enum with a `rename_all` mismatch falls into, which `AGENTS.md` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Single,
    Double,
}

/// One page of the flat sequence a gallery walks.
///
/// A position rather than a key, because the walk is arithmetic: `step` and
/// `flat` are what it needs, and both are cheaper over an index than over a
/// `HashMap` lookup. The same pair plus a path is what every pixel cache is
/// keyed on, so nothing here has to invent an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagePos {
    /// Which file in the gallery.
    pub index: usize,
    /// Which page within a multi-page document, 0-based. Always 0 for an
    /// ordinary image.
    pub page: u32,
}

/// The four facts the pairing walk needs, and nothing else.
///
/// A trait rather than a parameter struct because the real implementation is a
/// `Gallery` plus a shape cache and the test implementation is a `Vec<Shape>`,
/// and neither can be built from the other. Every method is a read: the walk
/// makes no decision that cannot be recomputed from these, which is what keeps
/// a wrong pairing debuggable.
///
/// No default bodies on purpose - a default would be a guess about a gallery
/// this section cannot see, and the guess would be silently wrong for archives
/// and for a `page_counts` that has not been learned yet.
pub trait PageCtx {
    /// Step the flat page cursor by `by` pages, honouring `loop_back`. `None`
    /// past either end when looping is off. **The only primitive that moves.**
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos>;

    /// Step the flat page cursor without wrapping: `None` past either end even
    /// when `loop_back` is on.
    ///
    /// **Both this and [`Self::step`] exist because two questions were being
    /// asked with one method**, and they have different right answers:
    ///
    /// * *"Does this page have a partner within this gallery?"* - **must not see
    ///   across the wrap.** A turn is a spread on screen, and a spread that
    ///   pairs the last page of a volume with its own first is not a spread of
    ///   that book: turning forward off the last spread would show page 4
    ///   beside page 1. So with looping on the **last turn truncates to one
    ///   page**, and the tiling is exactly the tiling the same gallery would
    ///   have with looping off.
    /// * *"Where does the cursor go next?"* - **must wrap**, or `loop_back` does
    ///   nothing at all.
    ///
    /// `step` answers the second and this answers the first, so [`lone`] - which
    /// asks whether a partner exists - uses this one, and only
    /// [`next_anchor`] uses `step`.
    ///
    /// It also removes a self-comparison that had no meaning: with wrapping, a
    /// one-page gallery's "next page" was page 0 again, and asking whether it
    /// pairs with itself said yes.
    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos>;

    /// The key for a position, for the shape cache and the loader. `None` for a
    /// position that does not exist.
    fn key(&self, pos: PagePos) -> Option<ImgKey>;

    /// Cached display dimensions. `None` = not learned yet, and that is a state
    /// the pairing is built to survive rather than an error.
    fn dims(&self, pos: PagePos) -> Option<(u32, u32)>;

    /// The user's override for this page, if any. Beats the detector outright.
    fn override_shape(&self, pos: PagePos) -> Option<Shape>;

    /// The page's ordinal in the whole gallery, so it can be compared with
    /// `lead_lone`. `None` when a preceding file's page count is not yet
    /// learned - only ever asked about the first few pages of a gallery, where
    /// the cursor's having arrived there guarantees the counts are known.
    fn flat(&self, pos: PagePos) -> Option<usize>;
}

/// What a page looks like: the user's override if there is one, otherwise
/// whether it is wider than a real page is.
///
/// **The dimensions must be the ones the page _displays_ at.** A phone scan
/// stored 1600x900 with EXIF orientation 6 displays as 900x1600, and the stored
/// pair's ratio is 1.78 - above [`SPREAD_ASPECT`] - so *every* page in the
/// volume is judged a spread, every page takes a whole turn to itself, and
/// nothing ever pairs. That failure is silent and it lands on the largest
/// category of real manga library there is, which is why whatever fills
/// [`PageCtx::dims`] applies the orientation tag instead of reporting the
/// stored size.
///
/// Unknown dimensions are [`Shape::Single`], the optimistic reading: an
/// unmeasured page is not a wide one.
///
/// A zero height is never a division by zero - `h > 0` is checked before the
/// ratio is formed, not after.
pub fn shape_of(pos: PagePos, ctx: &dyn PageCtx) -> Shape {
    if let Some(s) = ctx.override_shape(pos) {
        return s;
    }
    match ctx.dims(pos) {
        Some((w, h)) if h > 0 && (w as f32 / h as f32) > SPREAD_ASPECT => Shape::Double,
        _ => Shape::Single,
    }
}

/// Whether two adjacent pages can share a turn.
///
/// Three conditions, and each one is a way the obvious alternative is wrong:
///
/// * **Neither page is a spread.** A scan of two printed pages is a whole turn
///   by itself; pairing it would put two real pages and one scan in one
///   viewport, all three squashed.
/// * **Their ratios agree to within [`PAIR_ASPECT_TOLERANCE`].** This is what
///   keeps a landscape page out of a pair - Paperback #251's "the double page
///   always start on the left panel" and Aidoku #939's "forced into double-page
///   layout, resulting in black screen" - without any content-based detection,
///   which is out of scope because it would need a decode before the pairing is
///   known.
/// * **An unknown dimension never blocks the pair.** Optimism here costs a
///   pairing that the shared scale makes visible as a gap, and strictness costs
///   a turn that never opens.
///
/// The two ways a ratio can be unavailable are therefore treated differently,
/// and the asymmetry is deliberate:
///
/// * **No dimensions at all** pairs. There is nothing to compare against, and
///   refusing would strand a turn for as long as the dimension read takes.
/// * **A height of zero** does not pair. The page was measured and still has no
///   ratio, so it is not missing data - it is a page that cannot be sized.
///
/// Both directions of that are the safe ones: a lone turn is visible on screen,
/// a wrong partner is not. `a_shape_at_the_threshold_is_not_a_spread` pins the
/// zero-height case for both `shape_of` and this.
///
/// What this deliberately does **not** answer is whether `a` is the first half
/// of its turn. That is [`lone`]'s question, and `lone` may only be asked at an
/// anchor.
///
/// **Four [`PageCtx::dims`] calls per invocation, down from six.** Two are the
/// hoisted locals below and two are inside the two [`shape_of`] calls; the old
/// spelling of the three conditions called `dims` six times for the same two
/// pages. It is not tidiness: `dims` is a `HashMap` lookup keyed on a `PathBuf`,
/// and [`prev_anchor`] calls this once per page of an O(n) walk.
///
/// **The remaining two are the obvious follow-up** - `shape_of` re-reads what
/// the locals already hold, so passing the dimensions in would make this two
/// calls. Not done here because `shape_of` is public and takes a `PagePos`, and
/// changing its signature for this is Task 2's call rather than a local tidy.
pub fn pairable(a: PagePos, b: PagePos, ctx: &dyn PageCtx) -> bool {
    let (da, db) = (ctx.dims(a), ctx.dims(b));
    if shape_of(a, ctx) == Shape::Double || shape_of(b, ctx) == Shape::Double {
        return false;
    }
    // An unknown dimension never blocks the pair, so the tolerance check is only
    // reached when both are known.
    match (da, db) {
        (Some((aw, ah)), Some((bw, bh))) if ah > 0 && bh > 0 => {
            (aw as f32 / ah as f32 - bw as f32 / bh as f32).abs() <= PAIR_ASPECT_TOLERANCE
        }
        (Some((_, 0)), _) | (_, Some((_, 0))) => false,
        _ => true,
    }
}

/// Does the turn anchored at `s` hold only that page?
///
/// **VALID ONLY AT AN ANCHOR** - a page some turn opens on - which is the
/// cursor's only ever position, by the invariant this section is built on. At a
/// non-anchor this function is *wrong*, and the example is worth more than the
/// rule:
///
/// ```text
/// page 4 is portrait, page 5 is a landscape insert
///   -> 4 "cannot pair with 5", so this answers "4 is alone"
///   -> but 4 is the second half of {3, 4}
/// ```
///
/// Asking whether `j` pairs with `j+1` cannot tell "the first half of a pair"
/// from "alone", because the answer depends on what happened *behind* `j`. At
/// an anchor nothing paired from behind, so the same local test is a complete
/// answer. **This is the one thing here that must not be "simplified"** into a
/// predicate evaluated wherever the cursor happens to be: every such
/// formulation passes a forward-only test, because forward pairing never looks
/// back, and is wrong from the first landscape page onwards - a wrong pairing
/// being invisible until you are three pages into the wrong book.
///
/// The same asymmetry is why [`prev_anchor`] cannot ask this question either,
/// and does not try: it rebuilds the tiling forward from page 0 instead of
/// testing the page before the cursor, because every local test of that page has
/// already been shown wrong. Its doc comment lists the three that were tried.
///
/// Three reasons to be alone, in the order they are asked:
///
/// * **Front matter.** The first `lead_lone` pages of the gallery, because a book
///   opens on its cover alone. `0` for a scan whose blank first page was dropped,
///   which is the most-reported pairing complaint in every mature reader (Komga
///   #123, #2132; OpenComic #456; Paperback #251). The test is [`is_lead`]'s one
///   step, not a count of the pages before it - same rule, and it is the rule's
///   cost and its behaviour under an unlearned page count that make it that way.
/// * **A spread**, which takes a whole turn and re-pairs everything after it.
///   **This arm is redundant with `pairable`'s own `shape_of(a) != Double` check,
///   and is kept for legibility.** Deleting it was tried and changes no answer on
///   any input, because the third arm's `!pairable` is true for every wide page
///   anyway. It stays because it is the only statement of the re-pairing rule
///   visible at the point where re-pairing is decided, and because someone will
///   otherwise try to "remove the duplication". The arm that is genuinely
///   load-bearing is the first one: deleting that breaks the trace,
///   `leading_pages_can_be_lone`, `backwards_agrees_with_forwards` and the
///   round-trip property at once.
/// * **Nothing to pair with**: the next page is a spread, it is a different
///   shape of page, or there is no next page at all.
///
/// The partner test asks [`PageCtx::step_no_wrap`], not `step`, and that one
/// choice is what stops a turn straddling the wrap: with `loop_back` on, the
/// last anchor has no partner, so the last turn of a volume is one page and the
/// next press moves to page 1. A reader expects the last spread to be the last
/// page alone, not that page beside the first one.
pub fn lone(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> bool {
    is_lead(anchor, lead_lone, ctx)
        || shape_of(anchor, ctx) == Shape::Double
        || match ctx.step_no_wrap(anchor, 1) {
            Some(next) => !pairable(anchor, next, ctx),
            None => true,
        }
}

/// Whether `pos` is one of the gallery's first `n` pages - the front matter a book
/// opens alone.
///
/// **One step, not a count.** "`pos` has fewer than `n` pages before it" and "stepping
/// back `n` pages runs off the front" are the same statement, so this asks
/// [`PageCtx::step_no_wrap`] rather than [`PageCtx::flat`]. Both reasons for that were
/// found by measurement and by reading what `flat` does with an unknown count:
///
/// * **O(1), where `flat` is O(files).** [`Gallery::flat`] sums the page counts of
///   every file before `pos`, with a `PathBuf`-keyed lookup per element, and `lone` is
///   called once per anchor of every walk - so one backward step over a folder of 2000
///   loose images spent **117-385 ms** in it, measured in release on the development
///   box. `n` is at most [`LEAD_LONE_MAX`] (4), so this is a single `step_no_wrap` call
///   and not a loop of them.
/// * **It cannot refuse to answer.** `flat` is `None` whenever a *preceding* file's
///   page count has not been learned, and the spelling this replaces guarded that with
///   `is_some_and`, which then answered "not front matter". So a front page that
///   happened to sit behind an unlearned file lost its front-matter arm and paired with
///   the page after it - a wrong pairing, silent, and the kind this whole section is
///   built to avoid. `step_no_wrap` asks the position arithmetic, which is always
///   answerable, so the page is judged by where it is rather than by what is known
///   about the files ahead of it.
///
/// **Two boundaries, and neither earns a guard.** `n == 0` asks for
/// `step_no_wrap(pos, 0)`, which is `Some(pos)` for any valid position, so the answer
/// is "no" - right, since nothing can be among the first zero pages. And a position
/// that does not exist also answers `None` and would be called front matter: `lone` is
/// only ever asked at an anchor, which is a position a walk produced and so exists, so
/// that cannot arise - and a guard there would be a branch that cannot change an output.
pub(crate) fn is_lead(pos: PagePos, n: u32, ctx: &dyn PageCtx) -> bool {
    ctx.step_no_wrap(pos, -(n as isize)).is_none()
}

/// How many pages the turn anchored at `s` holds: one, or two.
///
/// There is no `Turn` type on purpose. A turn is `[anchor] + maybe [anchor+1]`,
/// which every caller can already express, and a struct over it would only add
/// a conversion to get wrong.
pub fn turn_len(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> usize {
    if lone(anchor, lead_lone, ctx) { 1 } else { 2 }
}

/// The other page of the turn anchored at `anchor`, or `None` when the turn holds
/// one page. `anchor` itself is never returned: it is the cursor's job.
///
/// **This is the whole of the turn's second half in one expression**, and it lives
/// here rather than in the app because every piece of it is a rule and every rule
/// in this section is testable from a test. The app's version is the mode gate
/// plus one call.
///
/// Three decisions, each of which has a wrong answer that looks right:
///
/// * **`turn_len` decides whether there is one at all**, and it is the same
///   question [`lone`] answers, so there is no second opinion to reconcile. A
///   lone turn - front matter, a spread, or a last page with nothing after it -
///   has no other half and must not borrow one.
/// * **`step_no_wrap(anchor, 1)`, not [`next_anchor`].** A turn is
///   `[anchor] + maybe [anchor + 1]`, so its second page is the anchor's
///   *successor*; `next_anchor` is the turn **after** this one, a whole
///   `turn_len` away, which is two pages forward whenever the answer is `Some`.
///   It is also documented as the only caller of the *wrapping* step, and a turn
///   must not straddle the wrap - so using it here would both paint the wrong
///   page and contradict [`lone`]'s own reason for asking
///   `step_no_wrap`. (The wrapping step taken *on its own* is a different matter,
///   and equivalent: see the last paragraph.)
/// * **`anchor` can never come back**, and that is a property of the step rather
///   than a check: `step_no_wrap` does not wrap, so one page forward is either
///   the next page of this document (`page + 1`) or the first page of the next
///   file (`index + 1`), and neither can be the position already named. A
///   guard would be a branch that cannot change an output.
///
/// **`None` for a one-page gallery is the same property, not a special case.**
/// `step_no_wrap` runs off the end and answers `None`, so a lone page cannot be
/// reported as its own partner - which is the artefact that made the two
/// questions ("does this page have a partner" and "where does the cursor go")
/// need separate steps in the first place.
///
/// **`step` would give the same answer here, and that is not an unpinned
/// constraint - it is an equivalence.** There is no test that can tell this
/// function's step from the wrapping one, and looking for one is a waste of a
/// review round: the two differ only at the two ends of a gallery, and this
/// function never reaches an end.
///
/// * It steps at all only when `turn_len == 2`, that is when [`lone`] is false.
/// * [`lone`]'s front-matter arm and its `Double` arm each make it true on their
///   own, so when it is false it must be the third arm that failed - which means
///   `step_no_wrap(anchor, 1)` did return a position, and [`pairable`] accepted
///   it.
/// * And `step_no_wrap` answers `None` only at the end of the gallery, where
///   `lone` is true and `turn_len` is 1, so the function has already returned.
///
/// So an interior anchor is the only one that gets here, and at an interior anchor
/// `step` and `step_no_wrap` are the same walk. `step_no_wrap` is the spelling to
/// keep anyway, for two reasons that are about the code rather than the answer: it
/// says out loud that a turn must not straddle the wrap, which is the property
/// [`lone`] is built on; and it keeps this function asking `lone`'s own question,
/// so the two cannot drift apart.
pub fn turn_partner(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<ImgKey> {
    if turn_len(anchor, lead_lone, ctx) == 1 {
        return None;
    }
    ctx.step_no_wrap(anchor, 1).and_then(|pos| ctx.key(pos))
}

/// The anchor after `a`, or `None` past the end with looping off.
///
/// Forward is one addition **from an anchor**, and that is the reason this design
/// needs no pairing table: the next anchor is this turn's length away, and a turn's
/// length is a local test.
///
/// **What it costs the app, which is not this.** `MyApp::turn_forward` calls
/// [`next_turn`], not this, and that snaps to the anchor first — so the navigation a
/// press performs is one `anchor_of` walk plus this one addition. See `anchor_of`'s
/// doc for the corrected cost and `TODO.md` for the deferred table.
///
/// **The only caller of the wrapping [`PageCtx::step`].** The wrap belongs here
/// and nowhere else: [`lone`] asks for a partner with `step_no_wrap`, so a turn
/// never straddles the seam, and this is where a looping gallery's cursor
/// actually gets back to page 0. That split is what makes a looping gallery's
/// tiling identical to the same gallery's tiling with looping off, so the
/// partition property holds for both.
pub fn next_anchor(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    ctx.step(a, turn_len(a, lead_lone, ctx) as isize)
}

/// How many anchors a tiling walk will consider before giving up.
///
/// **A bound on a walk that terminates on its own, not the walk's cost.** Every
/// walk bounded by this exits as soon as [`PageCtx::step_no_wrap`] returns `None`
/// at the front or the back of the gallery, whatever `loop_back` says, so this
/// only ever bites on a gallery larger than it names. Its value is deliberately far
/// above any plausible gallery rather than tuned: being generous costs nothing,
/// because the loop cannot reach the number without the answer already being known.
pub const PREV_SCAN_LIMIT: usize = 1 << 20;

/// The anchor of the turn before `a`'s turn, or `None` when there is none.
///
/// **`None` has two causes, and they are different questions.** It answers `None` at
/// the first turn - there is no turn before it - **and** for a position that is not
/// an anchor at all: the walk compares successors, so a second half is never the `a`
/// it is looking for, the walk runs off the end, and the answer is the same `None`.
/// So `prev_anchor(a).is_none()` does **not** mean "`a` is the first anchor", and a
/// caller that needs that has to ask [`anchor_of`] first and compare:
///
/// ```text
/// 8 single-page files, lead_lone = 1  ->  turns {0} {1,2} {3,4} {5,6} {7}
///   a = (2, 0): the second half of {1,2}, and there are two turns behind it.
///     prev_anchor walks 0 -> 1 -> 3 -> 5 -> 7 -> off the end, and answers None.
///   a = (0, 0): the first anchor, and there is nothing behind it.
///     prev_anchor answers None for the same reason and the same value.
/// ```
///
/// **A cursor can be on a second half without this section's help**, which is why
/// that distinction is load-bearing rather than theoretical: three ways into the app
/// assign the cursor by file index rather than by turn - a grid pick, a start-up or
/// resume, and a `Del` that keeps the freed index - and at the default
/// `lead_lone` of 1 an ordinary folder of single pages has a lone cover, so "file 3"
/// is page 2 of the turn `{1,2}`. Reading `None` as "at the front" is how a backward
/// step from such a position jumps to the last spread of the volume, which is what
/// [`prev_turn`] exists to prevent.
///
/// **Backward is O(distance from the front), and it has to be.** Forward is one
/// addition, because an anchor knows its own turn length without asking anything.
/// Going backwards there is no such thing to ask, and the reason is the same
/// asymmetry this whole section is built on:
///
/// ```text
/// pages [Single, Single, Single, Double], lead_lone = 0
///   forward:  {0,1} paired | {2} alone (3 is wide) | {3} wide
///   so page 2 is an ANCHOR - it went alone because nothing was ahead of it.
///   but pairable(1, 2) is true (both portrait), so "could 2 pair with the page
///   behind it?" cannot tell that apart from "2 is the second half of {1,2}".
/// ```
///
/// So this **rebuilds the tiling forward from page 0** and returns the anchor
/// that led to `a`. Every step is in-memory arithmetic over dimensions that are
/// already cached, and the walk is only ever as long as the distance back to the
/// first page - which for a manga volume is a few hundred cheap reads against a
/// decode measured in hundreds of milliseconds.
///
/// **Do not "optimise" this back into a two-candidate local test.** There is no
/// such rule. Three were tried and all three are wrong, each on
/// `[Single, Single, Single, Double]`:
///
/// * `lone(b)` - wrong at `lead_lone = 1`, returns 2 where the answer is 1.
/// * `shape(b) == Double || !pairable(b-1, b) || front(b)` - wrong at
///   `lead_lone = 0`, returns 1 where the answer is 2.
/// * "is `b` an anchor, i.e. does its own `turn_len` land on `a`?" - wrong at
///   `lead_lone = 1`, returns 2 where the answer is 1.
///
/// Anchor-ness of `b` depends on `b-1`'s turn length, which depends on `b-2`'s,
/// so it is only settled by walking back to the front. The forward direction
/// escapes this because it always starts from a known anchor; the backward
/// direction has no such starting point, which is why it walks instead.
///
/// **Returns `None` at the first anchor even when `loop_back` is on.** There is
/// no turn before the first one; whether the *app* should jump back to the last
/// turn of a looping gallery is the navigation layer's decision, and
/// [`prev_turn`] makes it. This function only ever reports the tiling - and, as
/// its summary says, it reports `None` for a non-anchor as well, so it is
/// [`anchor_of`] that decides whether a `None` means the front of the gallery.
pub fn prev_anchor(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    let mut anchor = PagePos { index: 0, page: 0 };
    for _ in 0..PREV_SCAN_LIMIT {
        // `step_no_wrap`, so the walk ends at the front of the gallery rather
        // than cycling: there is no previous turn before the first one, and
        // whether the *app* wraps to the last turn on a looping gallery is the
        // navigation layer's decision, not this function's.
        let next = ctx.step_no_wrap(anchor, turn_len(anchor, lead_lone, ctx) as isize)?;
        // Compared against the SUCCESSOR, and that is the whole contract: the
        // answer to "the anchor before `a`" is the anchor whose turn *ends* by
        // handing over to `a`.
        //
        // **Two edits that look similar are not the same, and only one of them
        // changes behaviour.** Adding `if anchor == a { return None }` at the top
        // of this loop, and leaving the check below, is *equivalent*: this loop
        // only ever tests `anchor` values the walk has already passed, so the top
        // check can match only the first anchor - whose answer is `None` anyway,
        // which is what the `None` arm above already returned. That edit was run
        // and nothing failed. **Replacing** the check below with a top-of-loop
        // `if anchor == a { return Some(anchor) }` is a different edit and is
        // *not* equivalent: it returns `Some(first_anchor)` where the answer is
        // `None`, stranding the backward walk on a one-turn gallery.
        //
        // Kept as written because it is the one that says what it means, and
        // because the guard is on the *property* rather than the spelling:
        // `every_anchor_is_reached_from_the_anchor_before_it` asserts that
        // whatever this returns is an anchor whose successor is `a`.
        if next == a {
            return Some(anchor);
        }
        anchor = next;
    }
    None
}

/// The turn anchor at or before `pos`: the anchor of the turn `pos` is on.
///
/// **Total, because the turns partition the gallery.** Every page is in exactly one
/// turn and the walk from the first anchor visits every anchor, so the answer always
/// exists for a position that does - which is what makes it usable as the thing to
/// call *before* asking anything else about a cursor.
///
/// **It walks forward, for the same reason [`prev_anchor`] walks forward**, and the
/// two together are the whole of anchor arithmetic: this one answers "which turn is
/// this page on", and `prev_anchor` answers "which turn is before that one". Asking
/// them of a position that is not an anchor is exactly what [`prev_anchor`] cannot
/// do, and the failure it causes is worse than a wrong page number: see its summary.
///
/// **Total does not mean correct for a position that does not exist.** A `pos` past
/// the end of the gallery is answered with the last turn's anchor, because that is
/// the walk's final answer and inventing a second failure mode for it would only give
/// callers something else to get wrong. Every caller in this crate passes a position
/// the walk produced or a cursor, and [`Gallery::goto_page`] refuses an index past the
/// end of the list, so nothing can observe the difference.
///
/// **O(distance from the *front*), not from `pos` — and the distinction is load
/// bearing, so it is worth stating plainly.** The walk starts at `(0, 0)`, so this
/// costs one iteration per *turn before the answer*, not one per page between the
/// front and `pos`. It therefore reads the same cached dimensions the rest of the
/// section does and nothing else: no I/O, no decode, and **it stops at the turn `pos`
/// is on rather than reading to the end of the volume**.
///
/// **An earlier version of this doc said "O(distance from `pos`), like every other
/// walk here", and it was wrong.** So was "forward is O(1)", which
/// [`next_anchor`]'s own doc repeats and which is the claim that lets a reader
/// believe no pairing table is needed: it is true of `next_anchor` *from an anchor*,
/// and the navigation the app actually performs is [`next_turn`], which **calls this
/// function first**. At the far end of a 2000-file folder a forward step is therefore
/// ~1000 iterations of `turn_len`, not one addition — still microseconds of in-memory
/// arithmetic against a decode measured in hundreds of milliseconds, and still not a
/// hang, but it is not the bound it was documented as. The deferred fix (a per-gallery
/// anchor list) and the corrected cost are recorded in `TODO.md`; do not "optimise"
/// this into a local test, for [`prev_anchor`]'s reasons.
pub fn anchor_of(pos: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> PagePos {
    let mut anchor = PagePos { index: 0, page: 0 };
    for _ in 0..PREV_SCAN_LIMIT {
        if anchor == pos {
            return anchor;
        }
        // The turn holds two pages when `lone` says so, and its second one is
        // `step_no_wrap(anchor, 1)` - the same spelling [`turn_partner`] uses, and
        // for the same reason: the wrapping step would answer across the seam.
        if turn_len(anchor, lead_lone, ctx) == 2 && ctx.step_no_wrap(anchor, 1) == Some(pos) {
            return anchor;
        }
        // `turn_len` is asked twice for the same anchor, which is one wasted local
        // test per iteration - not hoisted because hoisting it would only move the
        // cost inside the loop, and the loop's cost is the walk, not this call.
        let Some(next) = ctx.step_no_wrap(anchor, turn_len(anchor, lead_lone, ctx) as isize) else {
            return anchor;
        };
        anchor = next;
    }
    anchor
}

/// The anchor of the gallery's final turn.
///
/// **Forward, not backward, and for [`prev_anchor`]'s reason read from the other
/// end.** Which turn is last is a fact about the whole tiling, and nothing asked
/// *at* the gallery's last page can answer it. `lone` there always says "alone" -
/// it looks for a successor and there is none - so it says the same thing whether
/// the last page opens the final turn or closes it. `prev_anchor` there answers
/// `None` when the last page is a second half and the turn *before* the last one
/// when it is not, and neither is the answer. So the walk starts at the front and
/// reads forwards, the same way [`prev_anchor`] does.
///
/// **It does not see the wrap, and that is what makes a looping gallery's tiling
/// the tiling it would have with looping off** - the property the round-trip test
/// in `tests/spreads_nav.rs` asserts. It walks with [`PageCtx::step_no_wrap`] for the
/// same reason [`prev_anchor`] does: a turn must not name a page twice, and across
/// the seam it would. It is also what makes the walk **end by itself**: a wrapping
/// step on a looping gallery never answers `None`, so the loop would run to
/// [`PREV_SCAN_LIMIT`] instead - measured at ~25 s before the test fails, which is
/// the cost of getting this line wrong rather than of getting it right.
///
/// `None` for a gallery with nothing in it, checked rather than assumed: a gallery
/// with no files has no last turn, and `(0, 0)` is not a page of it.
pub fn last_anchor(lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    // `by` of 0 runs no iterations, so this is the "is there a first page at all"
    // question answered by the same bounds check every step does. Not a formality: a
    // gallery with no files has no page 0, and a walk that started at `(0, 0)` without
    // asking would answer `Some` for it.
    let mut anchor = ctx.step_no_wrap(PagePos { index: 0, page: 0 }, 0)?;
    for _ in 0..PREV_SCAN_LIMIT {
        let Some(next) = ctx.step_no_wrap(anchor, turn_len(anchor, lead_lone, ctx) as isize) else {
            return Some(anchor);
        };
        anchor = next;
    }
    None
}

/// Where one forward *step* lands: the anchor of the next turn.
///
/// **[`next_anchor`] snapped to the anchor first**, and that is the whole difference
/// between the two. `next_anchor` is a question about the tiling asked of a position
/// the caller believes to be an anchor; this is the navigation layer's step, asked of
/// a cursor that may not be one - and at a second half, `turn_len` is the second half's
/// own answer, so `next_anchor` returns a position one page past the turn on screen and
/// every step after that is off by the same page.
///
/// The snap costs one walk from the front ([`anchor_of`]); `next_anchor` itself is one
/// `turn_len`, so a forward step is that walk and nothing else. It is the same walk
/// [`prev_anchor`] makes for a backward step, and the same price in both directions —
/// paid to be callable with any cursor, rather than the alternative: a navigation layer
/// that silently mis-navigates on every second half, which at the default `lead_lone`
/// of 1 is every other page of an ordinary folder.
pub fn next_turn(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    next_anchor(anchor_of(a, lead_lone, ctx), lead_lone, ctx)
}

/// Where one backward *step* lands: the anchor of the turn before the one `a` is on,
/// or - when that is the gallery's first turn and this context's step would wrap -
/// the gallery's last turn.
///
/// **`prev_anchor` is asked first, and its `Some` is a proof rather than a lucky
/// answer.** The walk compares successors, so it can only return `Some` for a position
/// that *is* an anchor - a second half is never the successor it is looking for. So
/// when it answers, there is nothing to disambiguate and no second walk to pay for,
/// which is the case every step after the first is in (the walks always land on an
/// anchor, and `MyApp::goto` snaps whatever did not).
///
/// **`None` is the ambiguous one, and that is the bug this function was corrected
/// for.** `None` means "the first turn" *or* "`a` is not an anchor at all", and three
/// ways into the app put the cursor in the second case without asking: a grid pick
/// leaves `page` alone, a start-up or resume lands on page 0, and a `Del` keeps the
/// freed index. At the default `lead_lone` of 1 an ordinary folder of single pages
/// tiles `{0} {1,2} {3,4} {5,6} {7}`, so "the third file" is page 2 - the second half
/// of `{1,2}` - and reading its `None` as "at the front" sent a backward step to the
/// last spread of the volume, with `loop_back` on **and** off. [`anchor_of`] is what
/// separates the two, and it is only asked when there is a `None` to interpret.
///
/// **A second half of the *first* turn still reaches the wrap arm**, because its own
/// anchor is the first anchor and there is nothing before that either. Written as a
/// fall-through rather than an early return, and the case is real rather than
/// theoretical: on a two-file gallery with `loop_back` on, page 1 is the first turn's
/// second half, and the answer is the only turn in the volume.
///
/// **The wrap is decided by asking [`PageCtx::step`], not by a `looping` argument**,
/// so `loop_back` stays the single setting that decides it and this cannot disagree
/// with [`Gallery::prev`] about whether the cursor wraps at all. **The question is
/// asked of the anchor, not of `a`**, because `a` may be a second half: one page back
/// from page 1 of `{0,1}` is page 0 of the same gallery, not past its front, so asking
/// `a` would wrap (or refuse) where the answer is "nowhere". A backward step at the
/// front with looping off still stops, which is the whole of what `loop_back` has ever
/// meant.
pub fn prev_turn(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    if let Some(prev) = prev_anchor(a, lead_lone, ctx) {
        return Some(prev);
    }
    let at = anchor_of(a, lead_lone, ctx);
    if at != a {
        // A second half: the answer is the turn before the one *it* is on, which is
        // a second walk and the price of accepting a cursor that is not on an anchor.
        if let Some(prev) = prev_anchor(at, lead_lone, ctx) {
            return Some(prev);
        }
        // `at` is the first anchor and `a` its turn's second half: still the front.
    }
    // If a one-page step backwards from the first turn would wrap, the cursor's next
    // position backwards is the last turn; if it would not, nothing moves.
    ctx.step(at, -1)?;
    last_anchor(lead_lone, ctx)
}
