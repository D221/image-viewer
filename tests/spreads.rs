//! Manga mode's pairing core: shapes, the anchor invariant, and the walk — plus
//! the overrides the user makes on top of them, and the file they are written to.
//!
//! Most of this runs against [`TestGallery`] rather than a filesystem, which is
//! the point of `PageCtx`: the pairing is arithmetic over a flat page cursor, so
//! random galleries of every shape mix can be driven through a property test
//! without decoding anything. The property tests are **random, not exhaustive** -
//! see `any_gallery` for what 256 cases do and do not cover - so where a
//! specific shape matters it is spelled out in a named test.
//!
//! ## The one thing here that is not behavioural
//!
//! `invalidate_path_drops_the_cached_dimensions` reads `src/main.rs` as text, for
//! the same reason `tests/renderer.rs` and `tests/keybindings.rs` do: the wiring
//! it protects lives in the binary, which no integration test can reach, and
//! adding a `#[cfg(test)]` module to a binary for this purpose would break a
//! documented property of the project (`AGENTS.md`, "Tests"). The *behaviour* it
//! calls - that dropping a file's remembered dimensions makes the next read
//! report the new ones - is asserted against the real `Gallery` in the same
//! test. What the source scan adds is only the call, which nothing else can see.

use image_viewer::{
    anchor_of, decode_panel_px, exif_orientation, last_anchor, load_image, load_spread_overrides,
    lone, lone_spread_fills_turn, needs_refine, next_anchor, next_turn, page_dims, pairable,
    prev_anchor, prev_turn, preview_target, save_spread_overrides, shape_of, spread_centre,
    spread_fit, spread_pan_bounds, spread_rects, spread_root_key, spreads_root, spreads_tmp,
    thumb_cache_root, turn_is_a_lone_spread, turn_len, turn_partner, Gallery, GallerySpreads,
    ImgKey, PageCtx, PagePos, Rect4, ResizeAction, ResizeTracker, Shape, LEAD_LONE_DEFAULT,
    LEAD_LONE_MAX, PROBE_PREFIX_BYTES, RESIZE_PX_FLOOR, SPREADS_MAX_BYTES, SPREADS_MAX_PAGES,
};
use proptest::prelude::*;

use Shape::{Double, Single};

/// A gallery of documents, each a list of page shapes. One entry per file, so a
/// multi-page TIFF is a document with more than one page - which is what the
/// straddle tests need.
#[derive(Debug, Clone)]
struct TestGallery {
    docs: Vec<Vec<Shape>>,
    loop_back: bool,
}

impl TestGallery {
    /// One file whose pages have these shapes. Most tests are about the walk, and
    /// a file boundary is not one of the things they are testing.
    fn one_doc(shapes: Vec<Shape>) -> Self {
        Self::docs(vec![shapes])
    }

    /// One entry per file.
    fn docs(docs: Vec<Vec<Shape>>) -> Self {
        TestGallery {
            docs,
            loop_back: false,
        }
    }

    /// How many pages the whole gallery has.
    fn total_pages(&self) -> usize {
        self.docs.iter().map(|d| d.len()).sum()
    }

    /// The shape of one page, or `None` for a position that does not exist.
    fn shape_at(&self, pos: PagePos) -> Option<Shape> {
        self.docs.get(pos.index)?.get(pos.page as usize).copied()
    }

    /// The `n`th page of the flat list, so a test can name a position without
    /// knowing which document it falls in. This is the same ordering the walk
    /// sees: every page of a document, then the next document's first page.
    fn page_at(&self, n: usize) -> Option<PagePos> {
        let mut seen = 0usize;
        for (index, doc) in self.docs.iter().enumerate() {
            if n < seen + doc.len() {
                return Some(PagePos {
                    index,
                    page: (n - seen) as u32,
                });
            }
            seen += doc.len();
        }
        None
    }

    /// The flat ordinals of a list of positions, dropping any that do not exist.
    /// Tests assert on ordinals rather than on positions because an ordinal is
    /// what a reader can count, and `PagePos` makes them spell out the file
    /// layout every time.
    fn flat_of(&self, positions: &[PagePos]) -> Vec<usize> {
        positions.iter().filter_map(|p| self.flat(*p)).collect()
    }

    /// The whole anchor list, by repeated `next_anchor`. Stops when the walk
    /// stops moving: with `loop_back` off it runs out of pages, and the visited
    /// check is what keeps a looping gallery from spinning here forever.
    ///
    /// The stop condition is "a position we have already seen". With looping on,
    /// `next_anchor` from the last anchor wraps to page 0, so a walk that only
    /// stopped on `None` would spin forever; and it cannot stop merely on
    /// returning to the start, because a one-page looping gallery returns to the
    /// start on the first step. A helper that assumed either would hang rather
    /// than fail, which is the worst way for a test to end.
    fn anchors(&self, lead_lone: u32) -> Vec<PagePos> {
        let Some(mut cur) = self.page_at(0) else {
            return Vec::new();
        };
        let mut out = vec![cur];
        while let Some(next) = next_anchor(cur, lead_lone, self) {
            if out.contains(&next) {
                break;
            }
            out.push(next);
            cur = next;
        }
        out
    }

    /// The turn anchored at `a`: one position, or the anchor and the page after
    /// it. There is deliberately no `Turn` type in the library for this to
    /// return - a turn is `[anchor] + maybe [anchor+1]`, which is already
    /// expressible, and a struct would only add a conversion.
    ///
    /// `step_no_wrap`, because `turn_len` looks for a partner the same way: a
    /// turn must not name a page twice, and across the wrap seam it would.
    fn turn_at(&self, a: PagePos, lead_lone: u32) -> Vec<PagePos> {
        let mut turn = vec![a];
        if turn_len(a, lead_lone, self) == 2 {
            if let Some(partner) = self.step_no_wrap(a, 1) {
                turn.push(partner);
            }
        }
        turn
    }

    /// Every turn in the gallery, as flat ordinals, in reading order.
    fn turns(&self, lead_lone: u32) -> Vec<Vec<usize>> {
        self.anchors(lead_lone)
            .iter()
            .map(|&a| self.flat_of(&self.turn_at(a, lead_lone)))
            .collect()
    }
}

impl PageCtx for TestGallery {
    /// Step the flat page list by `by` pages. `None` past either end with
    /// `loop_back` off, wrapped otherwise - the same contract `Gallery::next`
    /// and `Gallery::prev` honour, including `prev` from page 0 landing on the
    /// *last* page of the previous document rather than its first.
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
        let total = self.total_pages() as isize;
        if total == 0 {
            return None;
        }
        let target = self.flat(from)? as isize + by;
        let flat = if self.loop_back {
            target.rem_euclid(total) as usize
        } else if target < 0 || target >= total {
            return None;
        } else {
            target as usize
        };
        self.page_at(flat)
    }

    /// The same walk with the wrap taken out, so a gallery's last turn cannot
    /// straddle the seam however `loop_back` is set.
    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
        let total = self.total_pages() as isize;
        if total == 0 {
            return None;
        }
        let target = self.flat(from)? as isize + by;
        if target < 0 || target >= total {
            return None;
        }
        self.page_at(target as usize)
    }

    /// A real key per position, because [`turn_partner`] hands one back and the
    /// planner cannot be shown right without it. `path` names the document and
    /// `page` is the position's own page, which is what makes page 2
    /// distinguishable from page 0 of the same file.
    ///
    /// Nothing else in the pairing reads this: `shape_of`, `pairable` and `lone`
    /// go through `dims`, `override_shape` and `flat`, so giving it a real answer
    /// cannot perturb a test that never asks for a key. `None` for a position
    /// that does not exist, like every other `PageCtx` here.
    fn key(&self, pos: PagePos) -> Option<ImgKey> {
        self.shape_at(pos)?;
        Some(key_of(pos.index, pos.page))
    }

    /// The dimensions a detected shape implies: portrait for a single page, and
    /// a real spread's 1.414 for a double, so the tolerance check in `pairable`
    /// has two genuinely different ratios to disagree about.
    fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
        match self.shape_at(pos)? {
            Single => Some((1000, 1400)),
            Double => Some((2000, 1400)),
        }
    }

    fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
        None
    }

    /// The page's ordinal in the whole gallery. Everything here is known, so this
    /// is `Some` for every position that exists; `Gallery` returns `None` when a
    /// preceding file's page count has not been learned yet.
    fn flat(&self, pos: PagePos) -> Option<usize> {
        let doc = self.docs.get(pos.index)?;
        let page = pos.page as usize;
        if page >= doc.len() {
            return None;
        }
        Some(self.docs[..pos.index].iter().map(|d| d.len()).sum::<usize>() + page)
    }
}

/// The design's own worked trace, asserted turn by turn and then walked
/// backwards: pages 0-7, all portrait except page 5, which is landscape.
///
/// Four spreads over six pages is Komga's own assertion for this shape
/// (`book-spreads.spec.ts`), and the *reshift* is the point - the wide page takes
/// a whole turn and page 6 pairs with 7 where it would otherwise have paired
/// with 4.
///
/// The wide page is at index **5**, as the spec's trace has it. An earlier
/// version of this list put the `Double` at index 4, which cannot produce these
/// anchors by any implementation: a wide page never shares a turn, so `{3,4}`
/// and `{5}` are only both true of the page being wide being 5.
#[test]
fn a_turn_runs_cover_pairs_a_wide_page_then_pairs_again() {
    let g = TestGallery::one_doc(vec![Single, Single, Single, Single, Single, Double, Single, Single]);
    let lead = 1;

    assert_eq!(g.flat_of(&g.anchors(lead)), vec![0, 1, 3, 5, 6]);
    assert_eq!(g.turns(lead), vec![vec![0], vec![1, 2], vec![3, 4], vec![5], vec![6, 7]]);

    // The backward walk, from the spec: back from anchor 6 the wide page is
    // `shape(5) == Double` so it took a whole turn, and back from anchor 5 page 4
    // turns out to have been the second half of {3,4}.
    assert_eq!(prev_anchor(g.page_at(6).unwrap(), lead, &g), g.page_at(5));
    assert_eq!(prev_anchor(g.page_at(5).unwrap(), lead, &g), g.page_at(3));
    assert_eq!(prev_anchor(g.page_at(3).unwrap(), lead, &g), g.page_at(1));
    assert_eq!(prev_anchor(g.page_at(1).unwrap(), lead, &g), g.page_at(0));
    assert_eq!(prev_anchor(g.page_at(0).unwrap(), lead, &g), None);
}

/// Random galleries: a length in 1..=12, a leading-lone offset in 0..=2, either
/// `loop_back` setting, and each page independently `Single` or `Double`.
///
/// **Random, not exhaustive.** 256 cases cannot cover 2^12 shape patterns across
/// twelve lengths, three offsets and two loop settings - that is orders of
/// magnitude short, and any wording here implying otherwise is the kind of claim
/// a future editor trusts when deciding a case can be skipped.
///
/// It is enough for what it is used for. Random booleans put wide pages at every
/// position including the ends at a good rate, and the minimal counterexample
/// that broke every local formulation of `prev_anchor`
/// (`[Single, Single, Single, Double]`) turns up readily - this strategy is what
/// found it. Where a specific shape matters, a named test states it outright.
///
/// Both `loop_back` settings are in here, and the partition property below holds
/// for both, because `lone` asks for a partner with `step_no_wrap`: a turn never
/// straddles the wrap, so a looping gallery's tiling is exactly the tiling of the
/// same gallery with looping off. Only `next_anchor` wraps. Before that rule a
/// looping gallery tiled cyclically - four all-`Single` pages gave
/// `{0}{1,2}{3,0}` - and no linear partition could hold.
fn any_gallery() -> impl Strategy<Value = (TestGallery, u32)> {
    (1usize..=12, 0u32..=2, any::<bool>()).prop_flat_map(|(len, lead, looping)| {
        proptest::collection::vec(any::<bool>(), len).prop_map(move |wide| {
            let shapes = wide
                .into_iter()
                .map(|d| if d { Double } else { Single })
                .collect();
            (
                TestGallery {
                    docs: vec![shapes],
                    loop_back: looping,
                },
                lead,
            )
        })
    })
}

/// A gallery of `len` pages in one document, for the tests that only care about
/// length. `Single` throughout, so the only thing that decides the turns is the
/// leading-lone offset and the end of the gallery.
fn singles(len: usize) -> TestGallery {
    TestGallery::one_doc(vec![Single; len])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// The tiling property: the turns **partition** the gallery. Every page is in
    /// exactly one turn - none in two, none missing - and the anchors strictly
    /// increase.
    ///
    /// Counted **per page** rather than compared as a sorted multiset, so the
    /// two failure modes are separable: a count of 2 is a page in two turns, and
    /// a count of 0 is a page in none. Comparing multisets would report both as
    /// one "not equal to `0..n`", and would make any in-range check below
    /// strictly implied rather than a check of its own.
    ///
    /// **Holds for looping galleries too**, and that is the point of the
    /// no-wrap rule: a turn must never straddle the seam, so a looping gallery
    /// tiles exactly as a non-looping one does. The per-turn no-duplicate check
    /// is what would catch a seam-straddling turn on its own - `{3,0}` names no
    /// page twice, but it does put the gallery's last page beside its first.
    ///
    /// This is the check that catches an off-by-one in `turn_len` - a walk that
    /// stepped by 2 unconditionally would strand the last page of an odd-length
    /// gallery, and one that stepped by 1 would pair nothing at all.
    #[test]
    fn the_turns_partition_the_gallery((g, lead) in any_gallery()) {
        let anchors = g.anchors(lead);
        prop_assert!(!anchors.is_empty(), "a non-empty gallery has at least one anchor");

        let mut times_seen = vec![0usize; g.total_pages()];
        let mut previous: Option<usize> = None;
        for &anchor in &anchors {
            let flat = g.flat(anchor).unwrap();
            if let Some(prev) = previous {
                prop_assert!(prev < flat, "anchors must strictly increase: {} then {}", prev, flat);
            }
            previous = Some(flat);

            let turn = g.flat_of(&g.turn_at(anchor, lead));
            let mut in_turn = turn.clone();
            in_turn.sort_unstable();
            in_turn.dedup();
            prop_assert_eq!(in_turn.len(), turn.len(),
                "a turn names no page twice: {:?}", turn);
            for f in turn {
                prop_assert!(f < g.total_pages(),
                    "turn names page {} of a {}-page gallery", f, g.total_pages());
                times_seen[f] += 1;
            }
        }

        for (f, n) in times_seen.iter().enumerate() {
            prop_assert_eq!(*n, 1, "page {} is in {} turns, not exactly one", f, n);
        }
    }

    /// Forward to the far end and then back the same number of times returns to
    /// the start - the round trip, over the random galleries `any_gallery` makes.
    ///
    /// Split out from the partition property because it is the only thing that
    /// exercises `prev_anchor`, and it is the property every local formulation of
    /// `prev_anchor` fails: they cannot tell "b is a lone anchor" from "b is a
    /// second half whose forward neighbour happens to be unpairable". The case
    /// that pins it is `[Single, Single, Single, Double]`, at `lead_lone` 0 and 1
    /// alike.
    #[test]
    fn walking_forwards_then_backwards_returns_to_the_start((g, lead) in any_gallery()) {
        let anchors = g.anchors(lead);
        prop_assert!(!anchors.is_empty());

        let mut cur = *anchors.last().unwrap();
        for &expected in anchors[..anchors.len() - 1].iter().rev() {
            cur = prev_anchor(cur, lead, &g).expect("every anchor but the first has one before it");
            prop_assert_eq!(
                g.flat(cur),
                g.flat(expected),
                "round trip over {:?} at lead {}",
                g,
                lead
            );
        }
        prop_assert!(prev_anchor(anchors[0], lead, &g).is_none(),
            "nothing comes before the first anchor");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    /// The two walks are two implementations of one rule, so they must produce the
    /// same anchor list.
    ///
    /// A proptest rather than four hand-picked galleries, because a disagreement
    /// only needs one awkward shape and a hand-picked fixture will not find it -
    /// which is exactly what happened: the four cases this replaced all passed
    /// against a `prev_anchor` that was wrong.
    #[test]
    fn backwards_agrees_with_forwards((g, lead) in any_gallery()) {
        let anchors = g.anchors(lead);
        let forwards = g.flat_of(&anchors);
        prop_assert!(!forwards.is_empty());

        // Seeded with the last anchor: the backward walk *starts* there, and a
        // gallery of one turn has no predecessors to collect.
        //
        // Bounded by the anchor count, and the bound is load-bearing rather than
        // defensive: a `prev_anchor` that returns `Some` for a position that is
        // its own predecessor walks this loop forever, so a mutation that made it
        // return the cursor page would hang the suite instead of failing it. A
        // test that can only end by hanging reports nothing.
        let mut backwards = vec![g.flat(*anchors.last().unwrap()).unwrap()];
        let mut cur = *anchors.last().unwrap();
        for _ in 0..anchors.len() {
            let Some(prev) = prev_anchor(cur, lead, &g) else { break };
            backwards.push(g.flat(prev).unwrap());
            cur = prev;
        }
        backwards.reverse();
        prop_assert_eq!(backwards, forwards, "lead {} over {:?}", lead, g);
    }

    /// One backward turn-step lands on the turn immediately before the turn the
    /// position is on — from **every** position, anchor or not.
    ///
    /// **This is the guard for the bug `prev_anchor`'s ambiguous `None` caused.** Its
    /// first version read `prev_anchor(a).is_none()` as "`a` is the first turn", so a
    /// backward step from a *second half* — which `prev_anchor` also answers `None`
    /// for — wrapped to `last_anchor` and put the reader on the last spread of the
    /// volume, with `loop_back` on and off. Stated as a property rather than as one
    /// fixture, because the property is the thing the navigation rests on: a step goes
    /// to the previous turn or nowhere, never forwards.
    ///
    /// Two assertions, because "lands on an anchor" alone would be satisfied by the
    /// first anchor of the gallery — which is what the bug returned.
    ///
    /// * `anchor_of(prev) == prev`: the position handed back is an anchor, so the
    ///   cursor is somewhere the pairing's rules are valid at.
    /// * `next_anchor(prev) == anchor_of(at)`: the turn *after* the one it landed on
    ///   is the turn the position was on, so it is exactly one turn back. Under
    ///   `loop_back` this also covers the wrap: stepping forward from the last turn
    ///   arrives at the first, which is the right answer when the step came from the
    ///   first turn.
    ///
    /// Every position of every random gallery is checked, not a sample, because the
    /// bug needs a second half and a hand-picked fixture does not reliably contain one
    /// at every `lead_lone`. Positions that do not move are skipped — with `loop_back`
    /// off that is the first turn, and there is nothing to assert about it.
    ///
    /// **Verified to fail against the version this replaced**: `prev_turn` without the
    /// snap, run over the same 256 cases, fails on the first second half it meets.
    #[test]
    fn a_backward_turn_step_lands_on_the_turn_before_the_one_it_came_from((g, lead) in any_gallery()) {
        let mut checked = 0usize;
        for n in 0..g.total_pages() {
            let at = g.page_at(n).unwrap();
            let Some(prev) = prev_turn(at, lead, &g) else { continue };
            checked += 1;
            prop_assert_eq!(
                anchor_of(prev, lead, &g),
                prev,
                "a backward step from page {} (lead {}) landed on {:?}, which is not an anchor, over {:?}",
                n,
                lead,
                prev,
                g
            );
            prop_assert_eq!(
                next_anchor(prev, lead, &g),
                Some(anchor_of(at, lead, &g)),
                "a backward step from page {} (lead {}) landed on {:?}, whose next turn is not the one it is on, over {:?}",
                n,
                lead,
                prev,
                g
            );
            // The same trap forwards, and it is quieter: at a second half `turn_len`
            // is that page's *own* answer, so an unsnapped `next_anchor` steps a page
            // too far and lands on an anchor of the wrong turn. Asserted as agreement
            // with the step from the anchor rather than against a spelled-out list,
            // because the list is what the pairing is for and this is the navigation.
            prop_assert_eq!(
                next_turn(at, lead, &g),
                next_turn(anchor_of(at, lead, &g), lead, &g),
                "a forward step from page {} (lead {}) disagrees with the same step from its anchor, over {:?}",
                n,
                lead,
                g
            );
        }
        // So a case whose positions all refuse still asserts something: without it a
        // gallery of one page with `loop_back` off would run no assertion at all.
        prop_assert!(checked <= g.total_pages(), "one landing per position at most");
    }
}

/// Review Focus 2: a splash page, a fold-out insert or a landscape layout page
/// between two portrait ones. It takes a whole turn and never shares one -
/// Paperback #251 and Aidoku #939, both of which shipped the squash.
///
/// `lone` is asserted directly as well as through the walk, because the two
/// answer different questions and this is the one case where they must agree.
#[test]
fn a_wide_page_never_shares_a_turn() {
    let g = TestGallery::one_doc(vec![Single, Double, Single]);
    let wide = g.page_at(1).unwrap();

    assert_eq!(shape_of(wide, &g), Double);
    assert_eq!(g.flat_of(&g.anchors(0)), vec![0, 1, 2]);
    assert_eq!(turn_len(wide, 0, &g), 1);
    assert!(lone(wide, 0, &g), "a wide page takes a whole turn");
    assert!(!pairable(g.page_at(0).unwrap(), wide, &g));
    assert!(!pairable(wide, g.page_at(2).unwrap(), &g));
}

/// Review Focus 3: a turn that straddles a page boundary *and* a file boundary,
/// which is the multi-page TIFF in manga mode. Page 1 of the first file is wide,
/// so it re-pairs what follows it - the turn `{2,3}` is the first two pages of
/// the second file, reached by crossing a file boundary.
///
/// Every turn must read forwards: no turn may hold a page and then a page of an
/// earlier file, because that is what "the page is a cursor, not a list" is
/// protecting.
#[test]
fn a_turn_straddles_a_page_boundary() {
    let g = TestGallery::docs(vec![
        vec![Single, Double],
        vec![Single, Single, Single],
    ]);
    assert_eq!(g.flat_of(&g.anchors(0)), vec![0, 1, 2, 4]);
    assert_eq!(g.turns(0), vec![vec![0], vec![1], vec![2, 3], vec![4]]);

    for &anchor in &g.anchors(0) {
        for pair in g.turn_at(anchor, 0).windows(2) {
            let (a, b) = (g.flat(pair[0]).unwrap(), g.flat(pair[1]).unwrap());
            assert!(a < b, "a turn reads forwards, got {a} then {b}");
            assert!(pair[0].index <= pair[1].index, "a turn never goes back to an earlier file");
        }
    }
}

/// Two ordinary files, and the first turn pairs the last page of one with the
/// first page of the next. Review Focus 3's second half: with the page as a
/// cursor rather than a list, a file boundary is not a turn boundary.
///
/// Both ordinals are asserted because the two readings differ here and only one
/// of them is the interesting one. The flat sequence is `[0, 2]` - three pages
/// make two turns - while the anchors sit in **files** 0 and 1. The straddle
/// itself is `{0, 1}`: file 0's only page beside file 1's first, in that order.
#[test]
fn a_turn_straddles_two_files() {
    let g = TestGallery::docs(vec![vec![Single], vec![Single, Single]]);
    assert_eq!(g.flat_of(&g.anchors(0)), vec![0, 2]);
    assert_eq!(
        g.anchors(0),
        vec![PagePos { index: 0, page: 0 }, PagePos { index: 1, page: 1 }],
        "one anchor per file"
    );
    assert_eq!(g.turns(0), vec![vec![0, 1], vec![2]]);
    assert_eq!(
        g.turn_at(g.page_at(0).unwrap(), 0),
        vec![PagePos { index: 0, page: 0 }, PagePos { index: 1, page: 0 }],
        "the first turn crosses the file boundary"
    );
}

/// The front-matter offset is the fix for the most-reported pairing complaint in
/// every mature reader (Komga #123 and #2132, OpenComic #456, Paperback #251): a
/// scan whose blank first page was dropped wants `lead_lone` 0, and one with its
/// cover intact wants 1.
///
/// `lead_lone` 2 is included because the setting is clamped to `LEAD_LONE_MAX` 4
/// and a lead beyond the cover is a real (if unusual) book - a two-page title
/// sequence - so the walk must not assume one.
#[test]
fn leading_pages_can_be_lone() {
    let g = singles(8);
    assert_eq!(g.turns(0), vec![vec![0, 1], vec![2, 3], vec![4, 5], vec![6, 7]]);
    assert_eq!(g.turns(1), vec![vec![0], vec![1, 2], vec![3, 4], vec![5, 6], vec![7]]);
    assert_eq!(g.turns(2), vec![vec![0], vec![1], vec![2, 3], vec![4, 5], vec![6, 7]]);

    // And every leading page is a turn of one at the offset, not a special case
    // that only happens to line up on a whole number of pairs.
    for lead in 0..=3u32 {
        for flat in 0..lead as usize {
            let pos = g.page_at(flat).unwrap();
            assert_eq!(turn_len(pos, lead, &g), 1, "page {flat} at lead_lone {lead}");
        }
    }
}

/// Review Focus 5: with looping off, neither walk runs off either end. A
/// phantom partner past the last page would be a blank half in a spread, and a
/// wrap to the first page would be pressing left on the cover and landing
/// somewhere else entirely.
#[test]
fn nothing_moves_past_either_end_when_looping_is_off() {
    for g in [singles(1), singles(2), singles(8), TestGallery::one_doc(vec![Double, Single])] {
        let anchors = g.anchors(0);
        assert_eq!(next_anchor(*anchors.last().unwrap(), 0, &g), None, "{g:?}");
        assert_eq!(prev_anchor(anchors[0], 0, &g), None, "{g:?}");

        // The last page is still its own turn rather than half a spread.
        let last = g.page_at(g.total_pages() - 1).unwrap();
        assert_eq!(turn_len(last, 0, &g), 1, "{g:?}");
    }
}

/// `Gallery::step_no_wrap` is the real page arithmetic with the wrap taken out,
/// and Task 2's `PageCtx for Gallery` is a one-liner over it - so it is pinned
/// here rather than left for Task 2 to discover.
///
/// The case that matters is the one the cursor is not on: `pages_in_current`
/// answers about `self.index`, so an implementation that reached for it would
/// report the *cursor's* file's page count for any other position, and a
/// three-page document's last page would look like an ordinary file's. Asking
/// about file 0's pages while the cursor sits on file 1 is what separates the
/// two.
#[test]
fn a_gallery_steps_without_wrapping_whatever_loop_back_says() {
    use image_viewer::{Gallery, PagePos as P};
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<std::path::PathBuf> = ["a.jpg", "b.jpg", "c.jpg"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();
    for p in &paths {
        std::fs::write(p, b"not really a jpeg").unwrap();
    }
    let mut gal = Gallery::from_file(paths[0].clone());
    gal.files = paths;
    gal.set_page_counts([3u32, 1, 1]);
    gal.set_loop_back(true);

    // Inside a three-page document, with the cursor parked on file 1.
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 0 }, 1), Some(P { index: 0, page: 1 }));
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 1 }, 1), Some(P { index: 0, page: 2 }));
    // Its last page has no partner, and does not wrap to the next document's
    // first page from *within* the step - it crosses into it, which is `next`.
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 2 }, 1), Some(P { index: 1, page: 0 }));

    // The end of the gallery is `None` whatever `loop_back` says.
    assert_eq!(gal.step_no_wrap(P { index: 2, page: 0 }, 1), None, "no wrap past the last file");
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 0 }, -1), None, "no wrap before the first");
    // Backwards within a document.
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 2 }, -1), Some(P { index: 0, page: 1 }));
    // And an index that does not exist is not a panic.
    assert_eq!(gal.step_no_wrap(P { index: 9, page: 0 }, 1), None);

    // **`by` may be more than one page**, because `prev_anchor`'s walk steps by a
    // whole turn and a turn is 1 or 2. An implementation that only understood
    // single steps returns `None` for every turn of two, which is correct for the
    // first anchor and wrong for every one after it - and invisible to the
    // property tests, whose test double does handle arbitrary `by`.
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 0 }, 2), Some(P { index: 0, page: 2 }));
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 0 }, 3), Some(P { index: 1, page: 0 }));
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 2 }, -2), Some(P { index: 0, page: 0 }));
    // A two-page step that would run off the end is `None`, not a partial answer.
    assert_eq!(gal.step_no_wrap(P { index: 2, page: 0 }, 2), None);
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 1 }, -2), None, "past the first file");
    assert_eq!(gal.step_no_wrap(P { index: 0, page: 0 }, -1), None);
}

/// A looping gallery's **last turn is one page**, and `next_anchor` from it wraps
/// to page 0.
///
/// This is the wrap seam, and it is the case that made the partition property
/// false for looping galleries. Four all-`Single` pages used to tile
/// `{0} {1,2} {3,0}`, so turning forward off the last spread showed page 4
/// beside page 1 - a spread of the wrong two pages, silently. `lone` now asks
/// for a partner with `step_no_wrap`, so page 3 finds no partner inside the
/// gallery, takes a turn alone, and `next_anchor` wraps from there.
///
/// So the tiling is `{0,1} {2,3}` with the cursor cycling 0 -> 2 -> 0, which is
/// exactly what the same gallery gives with looping off, and the wrap lives in
/// one place instead of in the pairing.
#[test]
fn a_looping_gallery_truncates_its_last_turn_and_still_wraps() {
    let g = TestGallery {
        docs: vec![vec![Single, Single, Single, Single]],
        loop_back: true,
    };
    assert_eq!(g.turns(0), vec![vec![0, 1], vec![2, 3]], "no turn straddles the seam");
    assert_eq!(g.flat_of(&g.anchors(0)), vec![0, 2]);

    let last = g.page_at(2).unwrap();
    assert_eq!(turn_len(last, 0, &g), 2);
    let end = g.page_at(3).unwrap();
    assert_eq!(turn_len(end, 0, &g), 1, "the last anchor has no partner inside the gallery");
    assert_eq!(next_anchor(end, 0, &g), g.page_at(0), "and the cursor still wraps");

    // Backwards stops at the front even though the cursor wraps forwards: there
    // is no previous turn before the first one, and whether the app should jump
    // to the last turn is the navigation layer's decision, not this function's.
    assert_eq!(prev_anchor(g.page_at(0).unwrap(), 0, &g), None, "{g:?}");
    assert_eq!(prev_anchor(g.page_at(2).unwrap(), 0, &g), g.page_at(0));
}

/// A one-page gallery: the only turn is `{0}`, so `turn_len` is 1 whatever
/// `loop_back` says.
///
/// `step_no_wrap` is what makes that true with looping on. Before it, the
/// partner test saw the wrap, so a one-page gallery's "next page" was page 0
/// again and `pairable(0, 0)` compared the page with itself and said yes -
/// pinning a turn of two for a gallery that has one page in it.
///
/// Pinned separately because nothing else covers it: every other gallery here
/// has a second page, and a self-comparison is the only way to get this wrong.
#[test]
fn a_one_page_gallery_is_a_turn_of_one_however_it_loops() {
    for looping in [false, true] {
        for shapes in [vec![Single], vec![Double]] {
            let g = TestGallery {
                docs: vec![shapes],
                loop_back: looping,
            };
            let only = g.page_at(0).unwrap();
            assert_eq!(turn_len(only, 0, &g), 1, "looping {looping} over {g:?}");
            assert_eq!(g.turns(0), vec![vec![0]], "looping {looping} over {g:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// The turn's second half
// ---------------------------------------------------------------------------
//
// `turn_partner` is the one function here that hands back a *key* rather than a
// position, because a key is what the loader and every pixel cache are keyed on.
// That makes it the function the rest of the pipeline can get wrong silently: a
// key built as `ImgKey::first` of the right path is the wrong picture of a
// multi-page document, and `Job::path()` — which drops the page — cannot see the
// difference. So the key is asserted whole, and never through `path()`.

/// A key the way `turn_partner` must build it: the document's path and the
/// position's **own page number**, which is the half `ImgKey::first` throws away.
fn key_of(index: usize, page: u32) -> ImgKey {
    ImgKey {
        path: std::path::PathBuf::from(format!("/vol/doc{index}.tiff")),
        page,
    }
}

/// A lone turn has no other half, and there are three reasons to be lone, so all
/// three are asserted rather than only the easiest. `Double` is the interesting
/// one: it is the only reason that is not visible in `lead_lone`.
#[test]
fn a_lone_turn_has_no_partner() {
    let partner = |g: &TestGallery, index: usize, page: u32, lead_lone: u32| {
        turn_partner(PagePos { index, page }, lead_lone, g)
    };

    // One all-`Single` document covers two of the three reasons: page 0 is front
    // matter at `lead_lone` 1 and page 2 is the last page. Both are alone for
    // different reasons, and both would otherwise be the easy assertion.
    let doc = TestGallery::one_doc(vec![Single, Single, Single]);
    assert_eq!(partner(&doc, 0, 0, 1), None, "front matter stands alone");
    assert_eq!(
        partner(&doc, 0, 2, 0),
        None,
        "the last page has nothing after it, so it is not its own partner"
    );

    let wide = TestGallery::one_doc(vec![Double, Single, Single]);
    assert_eq!(
        partner(&wide, 0, 0, 0),
        None,
        "a spread takes a whole turn to itself, so it has no partner to name"
    );

    // A one-page gallery, both shapes, both loop settings. With looping on this is
    // the case where the *wrapping* step would answer "page 0 again" and
    // `pairable(0, 0)` would compare the page with itself and say yes — the
    // artefact that made "does this page have a partner" and "where does the
    // cursor go" two different questions in the first place.
    for looping in [false, true] {
        for shapes in [vec![Single], vec![Double]] {
            let g = TestGallery {
                docs: vec![shapes],
                loop_back: looping,
            };
            assert_eq!(
                partner(&g, 0, 0, 0),
                None,
                "one page must not be its own partner (looping {looping}, {g:?})"
            );
        }
    }
}

/// A turn of two names its **successor**, with the successor's real page number.
///
/// Page **2** and not page 1, deliberately: a key assembled as `ImgKey::first`
/// would satisfy an assertion about page 1 and mean nothing at all. The extra
/// `assert_ne` is the point — it says the fixture cannot pass by accident, so a
/// future edit that flattens every key to page 0 breaks here instead of quietly
/// making the test weaker.
#[test]
fn the_partner_is_the_anchor_s_successor_with_its_own_page_number() {
    let doc = TestGallery::one_doc(vec![Single, Single, Single, Single]);

    assert_eq!(
        turn_partner(PagePos { index: 0, page: 1 }, 0, &doc),
        Some(key_of(0, 2)),
        "page 1's partner is page 2 of the same document"
    );
    assert_ne!(
        Some(key_of(0, 2)),
        Some(ImgKey::first(std::path::Path::new("/vol/doc0.tiff"))),
        "test setup: a page-2 key must not equal `ImgKey::first` of the same path"
    );

    // Across a file boundary, because a turn is not a file boundary: the last page
    // of one document is the first page of a turn with the next one's first.
    let two = TestGallery::docs(vec![vec![Single], vec![Single]]);
    assert_eq!(
        turn_partner(PagePos { index: 0, page: 0 }, 0, &two),
        Some(key_of(1, 0)),
        "the last page of one file pairs with the first page of the next"
    );
}

/// The plan's interface notes specified `next_anchor` for this function, and
/// `next_anchor(anchor)` is `step(anchor, turn_len(anchor))` — on any turn of two
/// that is **two** pages forward, the start of the next turn. So the wrong answer
/// is pinned here rather than left to review.
///
/// Each case is one where the two answers name different keys, and the last is
/// where they differ most: `next_anchor` off the end of a document is `None`, so
/// a function delegating to it would report *no partner at all* for the very turn
/// that has one.
#[test]
fn the_partner_is_not_the_next_anchor() {
    let doc = TestGallery::one_doc(vec![Single, Single, Single, Single]);
    // Both are compared as page numbers because that is the only part that can
    // differ: both keys name the same document, so a `path()`-only assertion would
    // pass for either answer.
    let next_turn_page = |anchor| {
        next_anchor(anchor, 0, &doc).and_then(|p| doc.key(p)).map(|k| k.page)
    };
    let partner_page = |anchor| turn_partner(anchor, 0, &doc).map(|k| k.page);

    let anchor = PagePos { index: 0, page: 0 };
    assert_eq!(
        next_turn_page(anchor),
        Some(2),
        "test setup: `next_anchor` really is two pages forward"
    );
    assert_eq!(
        partner_page(anchor),
        Some(1),
        "the turn's second page is one"
    );
    assert_ne!(
        next_turn_page(anchor),
        partner_page(anchor),
        "the two must differ, or this test proves nothing"
    );

    let last = PagePos { index: 0, page: 2 };
    assert_eq!(
        partner_page(last),
        Some(3),
        "the last turn of a document still has its second page"
    );
    assert_eq!(
        next_turn_page(last),
        None,
        "test setup: `next_anchor` off the end of a document is `None`"
    );
}

/// A decode is a measurement, and a measurement can change the pairing — so
/// `arrival_reschedules` fires for the turn's *second* page, not only the cursor's.
///
/// The case it exists for is the optimistic half of `pairable`: a partner whose
/// dimensions are unknown pairs, so it is planned at priority 0, and it turns out
/// to be a wide page. Its decode lands, `note_shapes` learns the real shape, and
/// the turn now holds **one** page where two were painted. If nothing re-plans,
/// the wrongly paired spread stands with nothing on screen to say why.
#[test]
fn a_partners_arrival_re_plans_because_it_can_change_the_turn() {
    let cur = key_of(0, 0);
    let partner = key_of(0, 1);
    let neighbour = key_of(0, 3);

    assert!(
        image_viewer::arrival_reschedules(&cur, Some(&cur), Some(&partner)),
        "the cursor page was already a trigger"
    );
    assert!(
        image_viewer::arrival_reschedules(&partner, Some(&cur), Some(&partner)),
        "a partner's decode is the measurement that can collapse the turn, so it \
         must re-plan or a wrongly paired spread stands with nothing to notice it"
    );
    assert!(
        !image_viewer::arrival_reschedules(&neighbour, Some(&cur), Some(&partner)),
        "`pairable` reads only the anchor's and its successor's dimensions, so no \
         other page can change the turn and none may cost a re-plan"
    );
    assert!(
        !image_viewer::arrival_reschedules(&partner, Some(&cur), None),
        "single-image browsing has no partner, so a preview is not a re-plan trigger"
    );
}

/// An unmeasured page is judged `Single` and **pairs** optimistically, so a turn
/// is never stranded waiting for a dimension read. The spec's §5 rule 3: refusing
/// to pair on missing data would strand a turn, and a stranded turn is worse than
/// a pair that turns out to be wrong.
///
/// The turn is of *two*, not one, and **that depends on `lead_lone` being 0**,
/// which is why it is passed explicitly. The anchor under test is page 0, so at
/// `lead_lone = 1` it is front matter and `lone` answers yes whatever its
/// dimensions said - `turn_len` is then 1 and the assertion below would fail,
/// and the test would have stopped being about unknown dimensions. At
/// `lead_lone = 0` the only thing that can make it a turn of one is the
/// dimensions themselves.
///
/// `shape_of` and `pairable` are asserted against page **1** instead, and both
/// are insensitive to `lead_lone`: neither one reads `flat`, so front matter can
/// reach them only through `lone`.
#[test]
fn an_unknown_page_pairs_and_is_a_turn_of_two_until_it_is_measured() {
    struct Unknown;
    impl PageCtx for Unknown {
        fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
            let to = from.page as isize + by;
            (0..2).contains(&to).then_some(PagePos { index: 0, page: to as u32 })
        }
        fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
            self.step(from, by)
        }
        fn key(&self, _pos: PagePos) -> Option<image_viewer::ImgKey> {
            None
        }
        fn dims(&self, _pos: PagePos) -> Option<(u32, u32)> {
            None
        }
        fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
            None
        }
        fn flat(&self, pos: PagePos) -> Option<usize> {
            Some(pos.page as usize)
        }
    }

    let first = PagePos { index: 0, page: 0 };
    let second = PagePos { index: 0, page: 1 };
    assert_eq!(shape_of(second, &Unknown), Single, "unknown is not wide");
    assert!(pairable(first, second, &Unknown), "an unknown ratio never blocks a pair");
    assert_eq!(turn_len(first, 0, &Unknown), 2, "so it is half of a spread, not alone");

    // The tolerance check is skipped, not skipped *by accident*: it is the
    // `dims().is_none()` arm that admits the pair. Two pages that both *are*
    // measured and disagree are still refused.
    let mixed = TestGallery::one_doc(vec![Single, Double]);
    assert!(!pairable(mixed.page_at(0).unwrap(), mixed.page_at(1).unwrap(), &mixed));
}

/// The threshold is strict: a page exactly at `SPREAD_ASPECT` is a page, and the
/// first ratio above it is a spread. 1.2 rather than 1.0 because a real page is
/// about 0.707 and a real spread about 1.414 - a threshold of 1.0 sits on the
/// midpoint and would call a square-ish single page a spread.
#[test]
fn a_shape_at_the_threshold_is_not_a_spread() {
    struct Dims(u32, u32);
    impl PageCtx for Dims {
        fn step(&self, _from: PagePos, _by: isize) -> Option<PagePos> {
            None
        }
        fn step_no_wrap(&self, _from: PagePos, _by: isize) -> Option<PagePos> {
            None
        }
        fn key(&self, _pos: PagePos) -> Option<image_viewer::ImgKey> {
            None
        }
        fn dims(&self, _pos: PagePos) -> Option<(u32, u32)> {
            Some((self.0, self.1))
        }
        fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
            None
        }
        fn flat(&self, pos: PagePos) -> Option<usize> {
            Some(pos.index)
        }
    }

    let page = PagePos { index: 0, page: 0 };
    // Ratios written as w/h chosen so the f32 division lands on or beside the
    // threshold rather than near it, since the comparison is in f32.
    assert_eq!(shape_of(page, &Dims(1200, 1000)), Single, "exactly 1.2 is a page");
    assert_eq!(shape_of(page, &Dims(1199, 1000)), Single, "1.199 is a page");
    assert_eq!(shape_of(page, &Dims(1210, 1000)), Double, "1.21 is a spread");

    // A zero height is a page rather than a division by zero, and it does **not**
    // pair - unlike a page with no dimensions at all, which does. That asymmetry
    // is in `pairable`'s doc and is load-bearing: "unmeasured" is optimistic,
    // "measured and unsizable" is not.
    let flat = Dims(2000, 0);
    assert_eq!(shape_of(page, &flat), Single);
    assert!(!pairable(page, page, &flat), "a zero height has no ratio to agree on");
    // And the unknown case really is the opposite, so the two are not the same
    // arm. `Dims(1000, 1400)` is the control: measured, sizeable, and it pairs.
    assert!(pairable(page, page, &Dims(1000, 1400)), "two identical pages pair");

    // **One side zero-height, the other unknown** - the cell that changed when
    // `pairable`'s conditions were hoisted, and the one a symmetric-looking test
    // never reaches. The old spelling short-circuited on `dims(b).is_none()`
    // before the `NAN` comparison and answered *pairable*; the current one
    // answers *not pairable*, which is what `pairable`'s doc says a zero height
    // does. Both pages' identities matter: `flat`/`unknown` is the pair being
    // tested, and `unknown`/`flat` is the mirror, so neither arm can be winning
    // by accident of position.
    struct Mixed(Option<(u32, u32)>, Option<(u32, u32)>);
    impl PageCtx for Mixed {
        fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
            let to = from.page as isize + by;
            (0..2).contains(&to).then_some(PagePos { index: 0, page: to as u32 })
        }
        fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
            self.step(from, by)
        }
        fn key(&self, _pos: PagePos) -> Option<image_viewer::ImgKey> {
            None
        }
        fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
            match pos.page {
                0 => self.0,
                _ => self.1,
            }
        }
        fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
            None
        }
        fn flat(&self, pos: PagePos) -> Option<usize> {
            Some(pos.page as usize)
        }
    }

    let other = PagePos { index: 0, page: 1 };
    assert!(
        !pairable(page, other, &Mixed(Some((2000, 0)), None)),
        "a measured-but-unsizable page does not pair, even beside an unknown one"
    );
    assert!(
        !pairable(other, page, &Mixed(Some((2000, 0)), None)),
        "and the same whichever side it is on"
    );
    // Controls, so the two assertions above are not passing for an unrelated
    // reason: unknown beside unknown pairs, and a zero height beside a *sized*
    // page does not either.
    assert!(pairable(page, other, &Mixed(None, None)), "two unknowns pair");
    assert!(
        !pairable(page, other, &Mixed(Some((2000, 0)), Some((1000, 1400)))),
        "zero height beside a sized page is the same refusal"
    );
    assert!(
        pairable(page, other, &Mixed(None, Some((1000, 1400)))),
        "unknown beside a sized page pairs - the optimistic arm"
    );

    // Cheap to change later: there is no `impl PageCtx` in `src/` yet, only here,
    // so this cell is not reachable from the app until Task 2 writes the real
    // implementation. If the semantics move again, this is the assertion to move
    // with them.
}

/// The property ruling 1's `next == a` line exists to state: whatever
/// `prev_anchor` returns is an anchor **whose successor is `a`**.
///
/// This is the guard on the load-bearing decision, and it is deliberately
/// stated as a property rather than as a comparison against a particular
/// spelling of the walk.
///
/// Two edits to `prev_anchor` look like the same mistake and are not, so both are
/// named here rather than left to be rediscovered:
///
/// * **Adding** `if anchor == a { return None }` at the top of the loop, leaving
///   the successor check alone, is **behaviourally equivalent** - verified by
///   running it. The walk only tests `anchor` values it has already passed, so
///   the added check can match only the first anchor, whose answer is `None`
///   anyway. No test here can fail on that edit, and inventing one that could
///   would be the decoration this file exists to avoid.
/// * **Replacing** the successor check with a top-of-loop
///   `if anchor == a { return Some(anchor) }` is **not** equivalent: it returns
///   `Some(first_anchor)` where the answer is `None`. The first anchor's `None`
///   is already pinned by `backwards_agrees_with_forwards` and by this test, so
///   that edit fails both without anything new being added.
///
/// What this test pins is the property an edit would break by changing behaviour
/// rather than spelling: whatever `prev_anchor` returns is an anchor **whose
/// successor is `a`** - not `a` itself, and not an anchor where the answer is
/// `None`. Both of those were checked by mutation: returning `Some(a)` fails five
/// tests including this one.
#[test]
fn every_anchor_is_reached_from_the_anchor_before_it() {
    for lead in 0..=2u32 {
        for g in [
            singles(1),
            singles(2),
            singles(7),
            TestGallery::one_doc(vec![Single, Single, Single, Double]),
            TestGallery::one_doc(vec![Double, Single, Single]),
        ] {
            let anchors = g.anchors(lead);
            for (i, &a) in anchors.iter().enumerate().skip(1) {
                let prev = prev_anchor(a, lead, &g)
                    .unwrap_or_else(|| panic!("no anchor before {a:?} in {g:?} at lead {lead}"));
                // The answer is an anchor the forward walk actually visits.
                assert!(
                    anchors.contains(&prev),
                    "{prev:?} is not an anchor of {g:?} at lead {lead}"
                );
                // And it is the one *immediately* before, by position.
                assert_eq!(prev, anchors[i - 1], "wrong predecessor for {a:?} in {g:?}");
                // The load-bearing half: advancing from it by its own turn length
                // lands exactly on `a`. This is what "the anchor that led to a"
                // means, and it is what an edit to the `next == a` line breaks.
                assert_eq!(
                    g.step_no_wrap(prev, turn_len(prev, lead, &g) as isize),
                    Some(a),
                    "{prev:?} does not lead to {a:?} in {g:?} at lead {lead}"
                );
            }
            // And the first anchor has none, whatever `loop_back` says.
            assert_eq!(prev_anchor(anchors[0], lead, &g), None, "{g:?} at lead {lead}");
        }
    }
}

// ---------------------------------------------------------------------------
// Task 2: reading a page's dimensions, and the cache that remembers them.
// ---------------------------------------------------------------------------

/// A file of `w` x `h` grey pixels, written as a PNG.
///
/// `image` is the crate the whole tree already decodes PNGs with, so the fixture
/// is built the same way a real file is rather than hand-assembled.
fn write_png(path: &std::path::Path, w: u32, h: u32) {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(w, h, image::Luma([128])))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// A JPEG of `w` x `h` pixels carrying a real APP1/Exif block whose IFD0 has the
/// single Orientation entry `tag`.
///
/// Hand-built for the same reason as everywhere else in this tree
/// (`tests/rotate_save.rs`, `tests/unit.rs`): `image`'s encoder writes no EXIF, so
/// there is nothing to configure and the only way to get a tagged JPEG is to
/// splice the segment in. Big-endian so the bytes are readable.
fn write_jpeg_with_orientation(path: &std::path::Path, w: u32, h: u32, tag: u16) {
    let mut jpeg = Vec::new();
    image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(w, h, image::Luma([128])))
        .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .unwrap();

    // APP1 payload: "Exif\0\0", then a TIFF header, then IFD0 with one SHORT.
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\x00\x2a"); // big-endian, version 42
    tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 at offset 8
    tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
    tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_be_bytes()); // one value
    // A SHORT's value lives in the first 2 bytes of its 4-byte field.
    tiff.extend_from_slice(&tag.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes()); // no IFD1

    let mut app1 = Vec::new();
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);
    let mut seg = Vec::new();
    seg.extend_from_slice(&[0xFF, 0xE1]);
    seg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    seg.extend_from_slice(&app1);

    // Straight after SOI, where a camera puts it.
    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[0..2]);
    out.extend_from_slice(&seg);
    out.extend_from_slice(&jpeg[2..]);
    std::fs::write(path, out).unwrap();
}

/// The pages of `paths`, as a `Gallery` with `counts` pages each. The files on
/// disk are deliberately not real images: these tests are about the arithmetic,
/// and `set_page_counts` exists so a navigation test does not also depend on the
/// IFD walker being right.
fn gallery_of(paths: &[std::path::PathBuf], counts: &[u32]) -> Gallery {
    let mut gal = Gallery::from_file(paths[0].clone());
    gal.files = paths.to_vec();
    gal.set_page_counts(counts.iter().copied());
    gal.goto_index(0);
    gal
}

/// `page_dims` reads a header, not pixels.
///
/// The control matters as much as the assertion: the file is **provably
/// undecodable** (its compressed image data is scrambled) and `image::open`
/// refuses it, so an answer here cannot have come from decoding it. `image`'s
/// `into_dimensions` stops at the header, which is the whole point - the pairing
/// query has to be cheap enough to run on arrival rather than on arrival *plus*
/// a decode.
///
/// An aspect of the header, so the value is not a shape the reader could get by
/// luck: 800x1200 is portrait and 0.667, well under [`SPREAD_ASPECT`], so it
/// would also be judged `Single` - which is what makes the *rotation* test's
/// failure mode below legible rather than a coincidence.
///
/// **The fixture is shorter than the 64 KB prefix, so this also pins the prefix
/// read's tolerance of a short file** - and the test checks that premise rather
/// than asserting it in a comment. `read_exact` into the preallocated buffer
/// would fail here; `read_prefix` truncates to what actually arrived. A uniform
/// 800x1200 grey PNG compresses to about 8 KB, which is not an accident of the
/// fixture: it is what a *small* image is, and a small image is the common case.
#[test]
fn page_dims_reads_stored_dimensions_without_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 800, 1200);

    // The premise of the short-file half of this test, checked rather than
    // asserted in the doc comment.
    let len = std::fs::metadata(&p).unwrap().len() as usize;
    assert!(
        len < PROBE_PREFIX_BYTES,
        "this fixture must be shorter than the prefix ({len} bytes), or the \
         short-file tolerance is not being exercised"
    );
    assert_eq!(page_dims(&p, 0), Some((800, 1200)));

    // Now break the pixels. IHDR is at a fixed offset and stays intact; the
    // entropy-coded data after it does not.
    let mut bytes = std::fs::read(&p).unwrap();
    let idat = bytes
        .windows(4)
        .position(|w| w == b"IDAT")
        .expect("the fixture has no IDAT chunk");
    for b in bytes[idat + 4..].iter_mut() {
        *b ^= 0xA5;
    }
    std::fs::write(&p, &bytes).unwrap();

    assert!(image::open(&p).is_err(), "the control: this file must not decode");
    assert_eq!(
        page_dims(&p, 0),
        Some((800, 1200)),
        "the header is still readable from a file whose pixels are nonsense"
    );
}

/// Unreadable is `None`, not a guess. A shape judged from dimensions nobody could
/// read is a shape nobody can correct, and the pairing is built to survive
/// `dims` being `None` (see `an_unknown_page_pairs_and_is_a_turn_of_two_until_it_is_measured`),
/// so there is nothing to gain by inventing a pair.
#[test]
fn page_dims_returns_none_for_a_file_that_is_not_an_image() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("notes.txt");
    std::fs::write(&p, b"this folder is not an image folder, it is a folder").unwrap();
    assert_eq!(page_dims(&p, 0), None);

    // And a file that is not there at all is the same answer, not a panic: this
    // is called on arrival, and a file can be deleted between being listed and
    // being opened.
    assert_eq!(page_dims(&dir.path().join("gone.png"), 0), None);

    // And so is a file too short to hold any header, which is the short-file case
    // the prefix read has to tolerate rather than treat as an error.
    let stub = dir.path().join("stub.png");
    std::fs::write(&stub, b"\x89PNG").unwrap();
    assert_eq!(page_dims(&stub, 0), None);
}

/// A `PageCtx` that reports one fixed pair for every position.
///
/// Exists so a test can ask *what the detector would say about a different set of
/// dimensions* - which is how the orientation test below states its own bug as an
/// executable assertion rather than a comment.
struct FixedDims((u32, u32));

impl PageCtx for FixedDims {
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
        Some(self.0)
    }
    fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
        None
    }
    fn flat(&self, pos: PagePos) -> Option<usize> {
        Some(pos.index)
    }
}

/// Review Focus 1, and the highest-consequence line in the whole plan: a
/// phone-scanned page is stored **landscape** and tagged to display **portrait**.
///
/// Stored 1600x900 with orientation 6, the stored pair's ratio is 1.78 - above
/// [`SPREAD_ASPECT`](image_viewer::SPREAD_ASPECT) - so judging the stored size
/// calls *every* page in the volume a spread. Every page then takes a whole turn
/// to itself, nothing ever pairs, and nothing on screen says why. It is silent,
/// and it lands on the largest category of real manga library there is.
///
/// So `page_dims` deliberately reports the **stored** size and `learn_shapes`
/// applies the tag, and this test pins both halves plus the converse: feeding the
/// stored pair to the detector is the bug, asserted directly rather than
/// described.
#[test]
fn a_rotated_phone_scan_is_judged_as_it_displays() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("scan.jpg");
    write_jpeg_with_orientation(&p, 1600, 900, 6);
    assert_eq!(exif_orientation(&p), 6, "the fixture must really carry the tag");

    assert_eq!(
        page_dims(&p, 0),
        Some((1600, 900)),
        "`page_dims` reports the *stored* size; applying the tag is a separate step"
    );

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let page = gal.page_pos();
    // Behaviour first, mechanism second: what a dropped orientation tag actually
    // breaks is the *verdict*, and the dimension is only how it gets there.
    assert_eq!(shape_of(page, &gal), Single, "a portrait page is not a spread");
    assert_eq!(
        PageCtx::dims(&gal, page),
        Some((900, 1600)),
        "the cache holds the size the page *displays* at"
    );

    // The converse, and the reason the assertion above is worth anything: the
    // stored pair is exactly what a dropped orientation tag feeds the detector,
    // and it says the opposite.
    assert_eq!(
        shape_of(page, &FixedDims((1600, 900))),
        Double,
        "the stored size is the bug this cache exists to prevent"
    );

    // The whole tag table, so the swap is pinned as a property of the tags rather
    // than of the one example. 5-8 rotate by a quarter turn and swap; 2-4 do not,
    // and a 180 rotation keeps the aspect ratio, so a landscape stored page is
    // still a spread under tag 3.
    for tag in 1u16..=8 {
        let p = dir.path().join(format!("tag{tag}.jpg"));
        write_jpeg_with_orientation(&p, 1600, 900, tag);
        let mut gal = Gallery::from_file(p.clone());
        gal.learn_shapes(PagePos { index: 0, page: 0 });
        let pos = gal.page_pos();
        let swaps = (5..=8).contains(&tag);
        assert_eq!(
            PageCtx::dims(&gal, pos),
            Some(if swaps { (900, 1600) } else { (1600, 900) }),
            "tag {tag}"
        );
        assert_eq!(shape_of(pos, &gal), if swaps { Single } else { Double }, "tag {tag}");
    }
}

/// A multi-page TIFF whose pages are **different sizes**, which is legal and which
/// `tiff_page_dim` exists to see. The pages differ in aspect on purpose, so a
/// walker that answered every page with the first page's size would report a shape
/// the pairing then trusts for the session.
///
/// `write_image` is the whole page in one call and it *chains* into the IFD
/// sequence, so calling it in a loop is what produces a multi-page file.
/// `new_directory()` per page would not: an unlinked IFD is invisible to
/// `more_images()`, so the file comes out single-page however many times the loop
/// runs (`tests/multiframe.rs` records that the hard way).
#[test]
fn a_later_page_of_a_document_is_measured_as_its_own_page() {
    use tiff::encoder::{colortype, TiffEncoder};
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("doc.tiff");

    let mut bytes = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut bytes)).unwrap();
        for &(w, h) in &[(900u32, 1200u32), (1600, 900)] {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [9u8, 9, 9, 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px)
                .expect("one chained page");
        }
    }
    std::fs::write(&p, &bytes).unwrap();

    assert_eq!(page_dims(&p, 0), Some((900, 1200)), "page 0 is portrait");
    assert_eq!(
        page_dims(&p, 1),
        Some((1600, 900)),
        "page 1 is walked to its own directory, not page 0's"
    );
    // A page that does not exist is `None` rather than the last one that does: the
    // walk is bounded by the chain, so this also says the walk terminates.
    assert_eq!(page_dims(&p, 2), None);
    assert_eq!(page_dims(&p, 99), None);

    // The cache is keyed per *page*, so page 1 keeps its own dimensions rather than
    // page 0's. This is the consequence RULING 11 exists for, and
    // `a_page_of_a_document_is_judged_by_its_own_dimensions` is the test that
    // fails if the keying ever reverts; the assertion here is the mechanism behind
    // it, one level down.
    let mut gal = Gallery::from_file(p.clone());
    gal.learn_pages(&p);
    assert_eq!(gal.pages_in(0), 2, "the fixture is really two pages");
    // **The second call is redundant since RULING 13** — learning page 0 of a
    // two-page document already learns page 1 — and it is kept anyway, as a cache
    // hit, so this test's subject stays unambiguously "the IFD walk reaches page 1"
    // rather than "the next-page learn filled it".
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    gal.learn_shapes(PagePos { index: 0, page: 1 });
    assert_eq!(
        PageCtx::dims(&gal, PagePos { index: 0, page: 0 }),
        Some((900, 1200))
    );
    assert_eq!(
        PageCtx::dims(&gal, PagePos { index: 0, page: 1 }),
        Some((1600, 900)),
        "page 1 keeps its own entry rather than page 0's"
    );

    // **And `forget_shapes` drops *every* page of the file, not just page 0.** The
    // entries are keyed by `ImgKey`, so a path-keyed `remove` would leave page 1's
    // old shape behind and only page 0 would ever be re-measured — the per-page
    // failure that a per-file `remove` cannot even express. `known_shapes` is the
    // only place the difference shows: a single-page query answers identically
    // either way.
    assert_eq!(gal.known_shapes(), 2, "both pages are remembered");
    gal.forget_shapes(&p);
    assert_eq!(gal.known_shapes(), 0, "not one page of the old document survives");
    assert_eq!(PageCtx::dims(&gal, PagePos { index: 0, page: 0 }), None);
    assert_eq!(PageCtx::dims(&gal, PagePos { index: 0, page: 1 }), None);
}

/// RULING 11: **the pages of one document are judged separately**, because a
/// document can hold a landscape double-page spread among portrait singles.
///
/// This is the shape the feature exists for — a manga volume scanned as one
/// multi-page TIFF, with page 3 a two-page spread — and it is exactly what a
/// path-keyed cache cannot express: one entry for the file would judge page 3 by
/// page 0, call it a portrait page, and pair it with page 4. Every spread after the
/// fold-out would then be wrong, and nothing on screen would say why.
///
/// Reverting `shapes` to a path key makes this fail and nothing else, which is why
/// it is a separate test from the `page_dims` walk above: that one pins the reader,
/// this one pins what the cache does with the answer.
///
/// Both pages are measured deliberately. Learning only page 0 would leave page 3
/// `None`, and `shape_of` reads `None` as `Single` — the *same wrong verdict* by a
/// different route, so a test that asserted only "page 3 is not Double" would pass
/// against the more likely half-regression as well.
#[test]
fn a_page_of_a_document_is_judged_by_its_own_dimensions() {
    use tiff::encoder::{colortype, TiffEncoder};
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("volume.tiff");

    // Pages 0-3: portrait singles. Page 3: a landscape two-page spread.
    let mut bytes = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut bytes)).unwrap();
        for &(w, h) in &[(900u32, 1200u32), (900, 1200), (900, 1200), (1600, 900)] {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [9u8, 9, 9, 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px)
                .expect("one chained page");
        }
    }
    std::fs::write(&p, &bytes).unwrap();

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_pages(&p);
    assert_eq!(gal.pages_in(0), 4, "the fixture is really four pages");

    // Arrive at each page in turn, the way `goto` does. Since RULING 13 the first call
    // already learns pages 0 and 1, so the loop is mostly cache hits — kept because
    // this test's subject is "every page is judged on its own", and that should not
    // depend on how many pages the next-page learn happens to cover.
    for page in 0..4u32 {
        gal.learn_shapes(PagePos { index: 0, page });
    }

    for page in 0..3u32 {
        assert_eq!(
            shape_of(PagePos { index: 0, page }, &gal),
            Single,
            "page {page} is a portrait page"
        );
    }
    assert_eq!(
        shape_of(PagePos { index: 0, page: 3 }, &gal),
        Double,
        "the spread is judged by its own dimensions, not page 0's"
    );

    // The turn walk agrees, which is the consequence that matters: the spread takes
    // a whole turn and the pages around it pair from where they really are. A
    // path-keyed cache judges all four `Single` and tiles them as pages 0+1 then
    // 2+3, putting the spread beside a portrait page.
    //
    // One document, so a flat ordinal *is* the page number and no mapping is needed.
    // An anchor is the *first* page of its turn, so the pair is `{1,2}` and its
    // anchor is page 1.
    let page = |n: u32| PagePos { index: 0, page: n };
    let mut anchors = vec![page(0)];
    let mut cur = page(0);
    while let Some(next) = next_anchor(cur, 1, &gal) {
        if anchors.contains(&next) {
            break;
        }
        anchors.push(next);
        cur = next;
    }
    assert_eq!(
        anchors,
        vec![page(0), page(1), page(3)],
        "the cover alone, then pages 1+2 paired, then the spread alone"
    );
    assert_eq!(turn_len(page(3), 1, &gal), 1, "the spread takes a whole turn");
    assert_eq!(turn_len(page(1), 1, &gal), 2, "and page 1 pairs with page 2");
}

/// A **two-page** document whose *second* page is a landscape spread. Page 1 is the
/// shape the cursor has not reached yet, and it has to be right anyway.
fn two_page_tiff_with_a_spread_on_page_1(dir: &std::path::Path) -> std::path::PathBuf {
    use tiff::encoder::{colortype, TiffEncoder};
    let p = dir.join("volume.tiff");
    let mut bytes = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut bytes)).unwrap();
        for &(w, h) in &[(900u32, 1200u32), (1600, 900)] {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [9u8, 9, 9, 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px)
                .expect("one chained page");
        }
    }
    std::fs::write(&p, &bytes).unwrap();
    p
}

/// RULING 13: **the page after the cursor is already measured**, because the pairing
/// query reads one page ahead.
///
/// `lone(anchor)` asks about `anchor + 1`, and an unknown `+1` reads as `Single`. So
/// learning only the page under the cursor gets the turn wrong in the worst way
/// available: arriving at page 1 changes page 1's shape, which changes `lone`, which
/// **re-flows the turn the reader is already looking at**. A spread that changes
/// under the reader is worse than one that was wrong from the start.
///
/// The cursor is asserted to be on page 0 throughout, because that is the whole
/// claim: page 1 is judged without having been visited.
///
/// This is the regression the next-page learn exists to prevent, and nothing else
/// pins it: removing the learn leaves page 1 at `None`, `shape_of` calls that
/// `Single`, and the turn silently pairs the spread with the page after it.
#[test]
fn a_spread_on_the_next_page_is_known_before_the_cursor_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let p = two_page_tiff_with_a_spread_on_page_1(dir.path());
    let at = |page: u32| PagePos { index: 0, page };

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_pages(&p);
    assert_eq!(gal.pages_in(0), 2, "the fixture is really two pages");

    // The cursor arrives at page 0 and stops there.
    gal.goto_index(0);
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(gal.page_pos(), at(0), "the cursor has not moved to page 1");

    assert_eq!(
        PageCtx::dims(&gal, at(1)),
        Some((1600, 900)),
        "page 1 was measured without being visited"
    );
    assert_eq!(
        shape_of(at(1), &gal),
        Double,
        "so the spread is detected before the reader reaches it"
    );

    // The consequence, in turns: page 0 does not pair with the spread, because a
    // spread never shares a turn. Learning page 1 late would have made page 0 pair
    // with it and then, on arrival, break that pair.
    assert!(!pairable(at(0), at(1), &gal), "a wide page never shares a turn");
    assert_eq!(turn_len(at(0), 0, &gal), 1, "so page 0 is alone in its turn");
    assert_eq!(turn_len(at(1), 0, &gal), 1, "and so is the spread");
}

/// The next-page learn is bounded, and both bounds are pinned here.
///
/// **A one-page file pays nothing extra**, which is the ordinary case and the whole
/// of a photo folder: the learn is guarded on the page count being greater than one,
/// so a single-page image produces exactly one entry and no read of a page that does
/// not exist. Dropping the guard is invisible in every other test here, because the
/// fixtures that *have* a next page legitimately learn it.
///
/// **And the hop stays inside the document.** On the last page of a multi-page file,
/// `step_no_wrap` points at the *next file's* first page; that is not this function's
/// business, and nothing is lost by skipping it — the next file's first page is
/// reported by its own preview landing, or measured when the cursor gets there.
///
/// `known_shapes` is what makes the second half visible. `dims` cannot: a page nobody
/// learned answers `None` either way, which is exactly what an unmeasured page is
/// supposed to look like.
///
/// **What this test does *not* establish, stated plainly:** the two bounds are not
/// independently load-bearing, and only one of them can be caught here. Removing the
/// page-count bound is a **no-op mutation** — a one-page file's next page is by
/// definition in a *different* file, so the same-document bound always catches it too.
/// Both mutations were run; that one leaves all 28 tests green. The page-count bound
/// is kept because it states the read-ahead's scope at the point where it is decided
/// and skips one in-memory hop, not because anything depends on it.
#[test]
fn the_next_page_learn_stops_at_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let doc = two_page_tiff_with_a_spread_on_page_1(dir.path());
    let after = dir.path().join("after.tiff");
    write_png(&after, 700, 1000);

    // Two files: the document, then an ordinary image.
    let mut gal = Gallery::from_file(doc.clone());
    gal.files = vec![doc.clone(), after.clone()];
    gal.learn_pages(&doc);
    gal.learn_pages(&after);
    assert_eq!(gal.pages_in(0), 2);

    // **Order matters here, and getting it wrong hides the cross-file hop.** The
    // document's last page is learned *first*, while the next file's page is still
    // unknown. Learn it second instead and the cache-first check absorbs the hop —
    // the entry is already there, so `known_shapes` cannot tell "did not learn it"
    // from "learned it and it was a no-op", and a guard that was doing nothing looks
    // exactly like a guard that is working. That is a no-op mutation wearing a green
    // tick, and it is the reason this ordering is spelled out.
    gal.learn_shapes(PagePos { index: 0, page: 1 });
    assert_eq!(
        gal.known_shapes(),
        1,
        "the document's last page was learned and it did not reach into the next file"
    );
    assert_eq!(
        PageCtx::dims(&gal, PagePos { index: 1, page: 0 }),
        None,
        "the next file's first page is still unmeasured"
    );

    // Now the ordinary file. It has one page, so there is no next page of it to
    // learn, and it is the last file, so there is no next file either.
    gal.goto_index(1);
    gal.learn_shapes(PagePos { index: 1, page: 0 });
    assert_eq!(gal.known_shapes(), 2);
    assert_eq!(
        PageCtx::dims(&gal, PagePos { index: 1, page: 0 }),
        Some((700, 1000)),
        "learned directly this time"
    );

    // And a one-page file has no page 1 to invent.
    gal.learn_shapes(PagePos { index: 1, page: 1 });
    assert_eq!(gal.known_shapes(), 2, "a one-page file has no page 1 to learn");
    assert_eq!(PageCtx::dims(&gal, PagePos { index: 1, page: 1 }), None);
}

/// `note_shapes` **fills a gap and never replaces a measurement**, so a decode
/// cannot displace what `learn_shapes` read out of the source header.
///
/// The two numbers here are ratio-equivalent — 900x1200 and 450x600 — so no verdict
/// is at stake either way, and that is exactly the point: the guard is not there to
/// stop a wrong shape, it is there to stop a *source measurement* being replaced by a
/// *downscaled approximation*. `learn_shapes` short-circuits on the entry existing, so
/// once the approximation landed the better number could never come back.
///
/// Both halves are asserted. The first says the learned value survives; the second
/// says `note_shapes` is not merely inert — a key with no entry is still filled, or
/// this test would pass against a `note_shapes` that did nothing at all.
#[test]
fn a_decode_never_displaces_a_learned_dimension() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 900, 1200);

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let at = |page: u32| PagePos { index: 0, page };
    assert_eq!(gal.known_shapes(), 1, "the control: one page is known");
    assert_eq!(
        PageCtx::dims(&gal, at(0)),
        Some((900, 1200)),
        "learned from the header"
    );

    // A decode of the same page reports a half-size version - what an animation over
    // 4096, or a TIFF page past the first, would arrive with.
    gal.note_shapes(ImgKey::first(&p), (450, 600));
    assert_eq!(
        PageCtx::dims(&gal, at(0)),
        Some((900, 1200)),
        "the source measurement survives the decode"
    );
    assert_eq!(gal.known_shapes(), 1, "and the decode added no second entry");

    // And a page with no entry is still filled, so the guard above is a guard and
    // not a no-op.
    gal.note_shapes(
        ImgKey {
            path: p.clone(),
            page: 4,
        },
        (300, 400),
    );
    assert_eq!(PageCtx::dims(&gal, at(4)), Some((300, 400)), "a gap is still filled");
    assert_eq!(gal.known_shapes(), 2);
}

/// A learned dimension is **frozen** until it is dropped.
///
/// This is the same rule the on-disk thumbnail cache follows and for the same
/// reason: correctness must not depend on something remembering to invalidate.
/// Here the thing that would have to remember is the 2 s folder watcher, and a
/// pairing that reads a stale shape mis-pairs every turn *after* the changed
/// file with nothing on screen to say so.
#[test]
fn a_learned_dimension_never_changes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 800, 1200);

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let pos = gal.page_pos();
    assert_eq!(PageCtx::dims(&gal, pos), Some((800, 1200)));

    // Same path, different picture. `learn_shapes` runs on *every* navigation, so
    // re-reading here would be a file open on the critical path of every
    // keypress to re-learn something already known.
    write_png(&p, 300, 200);
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(
        PageCtx::dims(&gal, pos),
        Some((800, 1200)),
        "a learned dimension is not re-read behind its own back"
    );
}

/// ...and `forget_shapes` is what makes it move again. The file's *contents*
/// changed, which is the only signal anything has: `invalidate_path` sees a
/// changed stamp and knows nothing about which dimension it implies.
#[test]
fn forget_shapes_makes_the_dimension_change_again() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 800, 1200);

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let pos = gal.page_pos();
    assert_eq!(PageCtx::dims(&gal, pos), Some((800, 1200)));

    write_png(&p, 300, 200);
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(PageCtx::dims(&gal, pos), Some((800, 1200)), "still the old one");

    gal.forget_shapes(&p);
    assert_eq!(PageCtx::dims(&gal, pos), None, "forgotten means unknown, not stale");
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(PageCtx::dims(&gal, pos), Some((300, 200)), "re-learned from the new bytes");
}

/// `PageCtx::step` on a real `Gallery` is the same walk `Gallery::next` and
/// `Gallery::prev` do, so the two cannot disagree about where a page is.
///
/// It is `next`/`prev` on a copy rather than a reimplementation, because the rules
/// that are easy to get wrong are all in them: from page 0 `prev` lands on the
/// **last** page of the *previous* file, not its first; the file boundary is not
/// a turn boundary; and `loop_back` decides what happens at either end. Page
/// counts are deliberately uneven (2, 1, 3) so a step that crossed a file
/// boundary and a step that stayed inside a document are both exercised, and the
/// cursor is parked *after* the step in each direction.
///
/// Asserted in both directions over the whole gallery rather than at one position:
/// `prev`'s page-before-file rule only shows up somewhere in the middle, and the
/// second assertion is what proves `step` is not a `next` that happens to look
/// right forwards.
#[test]
fn a_gallery_walks_pages_the_same_way_the_planner_sees_them() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<std::path::PathBuf> = ["a.jpg", "b.jpg", "c.jpg"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();
    for p in &paths {
        std::fs::write(p, b"not really a jpeg").unwrap();
    }
    let mut gal = gallery_of(&paths, &[2, 1, 3]);
    gal.set_loop_back(false);

    let start = gal.page_pos();
    assert_eq!(start, PagePos { index: 0, page: 0 });
    let mut forwards = vec![start];
    while gal.next() {
        forwards.push(gal.page_pos());
    }
    // 2 + 1 + 3 pages, in reading order, with no page named twice.
    assert_eq!(
        forwards,
        vec![
            PagePos { index: 0, page: 0 },
            PagePos { index: 0, page: 1 },
            PagePos { index: 1, page: 0 },
            PagePos { index: 2, page: 0 },
            PagePos { index: 2, page: 1 },
            PagePos { index: 2, page: 2 },
        ]
    );

    for pair in forwards.windows(2) {
        assert_eq!(
            PageCtx::step(&gal, pair[0], 1),
            Some(pair[1]),
            "step forwards from {pair:?}"
        );
        assert_eq!(
            PageCtx::step(&gal, pair[1], -1),
            Some(pair[0]),
            "step back from {pair:?}"
        );
    }

    // And `prev` retraces the entire walk, which is the half `step` could have
    // been wrong about in a way the loop above would not show: from page 0 of
    // file 1 it goes to the *last* page of file 0, not to file 0's first.
    for &expected in forwards[..forwards.len() - 1].iter().rev() {
        assert!(gal.prev(), "there is a previous page before {expected:?}");
        assert_eq!(gal.page_pos(), expected);
    }
    assert!(!gal.prev(), "nothing before the first page");
    assert_eq!(gal.page_pos(), start, "a refused `prev` does not move the cursor");

    // Multi-page steps, because `prev_anchor`'s walk steps by a whole turn and a
    // turn is 1 or 2. Same reason `step_no_wrap` is a loop rather than a `match`.
    assert_eq!(PageCtx::step(&gal, PagePos { index: 0, page: 0 }, 2), Some(PagePos { index: 1, page: 0 }));
    assert_eq!(PageCtx::step(&gal, PagePos { index: 2, page: 2 }, -2), Some(PagePos { index: 2, page: 0 }));
    // Off either end is `None`, not a wrap, with looping off.
    assert_eq!(PageCtx::step(&gal, PagePos { index: 2, page: 2 }, 1), None);
    assert_eq!(PageCtx::step(&gal, PagePos { index: 0, page: 0 }, -1), None);
    // An index that does not exist is `None` rather than a wrap to page 0, which
    // is what the raw `next`/`prev` arithmetic would do on its own: `next` from
    // an out-of-range index finds no next file and loops.
    assert_eq!(PageCtx::step(&gal, PagePos { index: 9, page: 0 }, 1), None);

    // With `loop_back` on, `step` wraps where `step_no_wrap` does not - the whole
    // reason there are two methods, so both are asserted rather than one.
    gal.set_loop_back(true);
    let last = *forwards.last().unwrap();
    assert_eq!(PageCtx::step(&gal, last, 1), Some(start), "forward wraps");
    assert_eq!(
        PageCtx::step(&gal, start, -1),
        Some(last),
        "and backwards wraps to the last page, not to file 0's first"
    );
    assert_eq!(
        PageCtx::step_no_wrap(&gal, last, 1),
        None,
        "while the partner test must not see across the seam"
    );
}

/// `flat` is the page's ordinal in the whole gallery, so it can be compared with
/// `lead_lone` - and a *sum* of page counts is only a sum if they are known.
/// A file whose count has not been learned contributes an unknown number of
/// pages, so the answer is `None` rather than a number that quietly assumes one.
///
/// Both halves are asserted, and the difference between them is the point: the
/// same position goes `None` -> `Some` as the missing count is supplied, so the
/// test cannot be passing because the position was always unanswerable.
///
/// `None` for the first page is the optimisation the walk depends on: it is only
/// ever asked about the leading pages, where nothing precedes the position, and
/// there is nothing to be uncertain about.
#[test]
fn an_unknown_preceding_page_count_makes_flat_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<std::path::PathBuf> = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();
    for p in &paths {
        std::fs::write(p, b"not really a jpeg").unwrap();
    }
    let mut gal = gallery_of(&paths, &[2, 1, 1, 1]);
    let at = |i: usize, page: u32| PagePos { index: i, page };

    // Every count known: the sum is real, and it counts pages rather than files.
    assert_eq!(PageCtx::flat(&gal, at(0, 0)), Some(0));
    assert_eq!(PageCtx::flat(&gal, at(0, 1)), Some(1));
    assert_eq!(PageCtx::flat(&gal, at(1, 0)), Some(2), "file 0 has two pages");
    assert_eq!(PageCtx::flat(&gal, at(3, 0)), Some(2 + 1 + 1));

    // Forget file 0's count. Nothing about file 1 changes, but its ordinal now
    // depends on a number nobody has.
    gal.forget_page_count(&paths[0]);
    assert_eq!(PageCtx::flat(&gal, at(0, 0)), Some(0), "nothing precedes the first page");
    assert_eq!(PageCtx::flat(&gal, at(0, 1)), Some(1));
    assert_eq!(PageCtx::flat(&gal, at(1, 0)), None, "file 0's count is unknown");
    assert_eq!(PageCtx::flat(&gal, at(3, 0)), None);

    // Supplying it again resolves the same positions. `learn_pages` reads four
    // bytes and this file is not a TIFF, so it learns 1 - which is why the
    // assertion is `Some(1)` and not `Some(2)`.
    gal.learn_pages(&paths[0]);
    assert_eq!(PageCtx::flat(&gal, at(1, 0)), Some(1));

    // A position that does not exist is `None`, and is not a panic.
    assert_eq!(PageCtx::flat(&gal, at(4, 0)), None);
}

/// The body of `fn <sig>` in a source file, found by **brace matching**.
///
/// Extracted so that a second scan of the same function is one assertion rather than
/// a second copy of this, which is the whole reason
/// [`invalidate_path_drops_the_cached_dimensions`] is not duplicated below.
///
/// **By brace matching, not by "up to the next `fn`".** The first version split
/// on `"\n    fn "`, which is wrong in the direction that matters: `split` always
/// yields at least one item, so when the delimiter did not match — `invalidate_path`
/// last in the impl, or re-indented — the "body" became the whole rest of the
/// file, the later call stood in for this one, and the scan passed
/// **vacuously**. It could not fail; that is the opposite of what a guard is for.
///
/// The depth starts at **0**, not 1, and that is load-bearing rather than
/// incidental. It is 0 because the scan starts *before* the opening brace — the
/// signature's `&mut self, p: &Path, ctx: &Context` has none — so the first `{`
/// is the function's and the matching `}` is the one that ends it. Starting at 1
/// (the shape `keybindings.rs::dispatch_region` uses, correctly, because it
/// begins *after* `if !typing {` and so has already consumed a brace) misses
/// that closing brace and runs on until the file's own braces happen to balance,
/// 60 KB later. **That was the second version of that test, and it passed with the
/// call deleted.** Verified by mutation, not reasoned about.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("{sig} is gone; update this test to match"))
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
    let end = end.expect("could not find the end of the function: its braces do not \
                         balance, or the signature changed shape. Update this test.");
    &src[start..end]
}

/// A re-exported file must be re-judged, or its old page's shape is frozen for the
/// session and **every spread after it stays mis-paired**.
///
/// What is pinned here is the **call**, and nothing else. The behaviour it calls is
/// asserted once, by [`forget_shapes_makes_the_dimension_change_again`], and repeating
/// it here would only be a second copy to keep in step — so this test is *only* the
/// source scan, and it says why.
///
/// The reason it needs one: the thing that decides a file changed is
/// `MyApp::invalidate_path`, and that lives in `src/main.rs` — a binary no
/// integration test can reach. Adding a `#[cfg(test)]` module to it for this would
/// break a documented property of the project (`AGENTS.md`, "Tests"), so the call is
/// asserted against the source as text, for the same reason `tests/renderer.rs` and
/// `tests/keybindings.rs` do.
///
/// A source scan is not behaviour, and it is weak in a specific way: it cannot tell
/// whether `invalidate_path` *reaches* `forget_shapes` on this path or on some
/// other. What it does catch is the regression that actually happens — the call being
/// deleted, or a later edit adding a cache here and forgetting this one — and neither
/// is visible at runtime. Deleting the line fails this test; that has been run.
///
/// Scoped to `invalidate_path`'s own body so a `forget_shapes` anywhere else in
/// the file cannot stand in for this one — and `remove_current_from_gallery`
/// calls it too, so that is not hypothetical.
#[test]
fn invalidate_path_drops_the_cached_dimensions() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
    let body = fn_body(&src, "fn invalidate_path(");

    assert!(
        body.contains("forget_shapes("),
        "invalidate_path no longer forgets the cached dimensions, so a re-exported\n\
         file keeps its old page's shape and every spread after it stays mis-paired\n\
         for the session, with nothing on screen to say so.\nBody was:\n{body}"
    );
}

/// ...and the same function is the only thing that can drop a file's *overrides*, so
/// this is where the wiring for those lives too.
///
/// Two calls, and they are one feature: dropping the entry in memory without saving
/// would make "a re-export clears the override" true for one session and false
/// across a restart, because the next load would read the stale entry straight back
/// out of the file. So the `save_spreads` assertion is not a separate concern from
/// the `forget_file` one — it is the half that makes it durable.
///
/// The source scan is weak in the way [`invalidate_path_drops_the_cached_dimensions`]
/// says, and for the same reason it exists at all. It has one advantage over that
/// test, though: it reuses the brace matching already written there (see
/// [`fn_body`]) rather than adding a second copy of it.
#[test]
fn invalidate_path_drops_the_overrides_and_saves_them() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
    let body = fn_body(&src, "fn invalidate_path(");

    assert!(
        body.contains("spreads.forget_file("),
        "invalidate_path no longer forgets the user's overrides, so a re-exported\n\
         file keeps page 3 a spread for the session whatever it now contains - and\n\
         because an override beats the detector, nothing on screen says why.\n\
         Body was:\n{body}"
    );
    assert!(
        body.contains("save_spreads("),
        "invalidate_path drops the override but does not save, so the stale entry is\n\
         still in spreads.json and comes back on the next launch.\nBody was:\n{body}"
    );
}

/// ...and the same reasoning pins the other end of the turn: a **partner's** preview
/// has to re-plan, or a spread that paired on an unmeasured page and then turned out
/// to be a wide page stands wrong with nothing on screen to say why.
///
/// The rule is asserted by
/// [`a_partners_arrival_re_plans_because_it_can_change_the_turn`], which is
/// `image_viewer::arrival_reschedules` called directly. **What is pinned here is the
/// call site**, because `MyApp::pump` is a binary and a call deleted from one is
/// invisible to every behavioural test in the tree - `resched` would simply stop being
/// set, and the app would go on looking correct.
///
/// A source scan is not behaviour, and it is weak in a specific way: **it cannot tell
/// whether `pump` ever *reaches* this line** - only that the line is in the arm. A
/// guard could fire the branch but never call this, or the call could sit behind a
/// condition nobody exercises, and both pass here. What it does catch is the
/// regression that actually happens, which is the deletion or the move out of the arm,
/// and neither is visible at runtime. Deleting the call fails this test; that has been
/// run.
///
/// Scoped to the **`Ok` half of the `Msg::Preview` arm**, not to `pump` as a whole,
/// and narrower than that sounds. Brace matching starts after the signature, whose
/// trailing `{` is the `match result`'s own, so the next brace it meets is the one
/// that opens `Ok(p) => {` — and the brace it stops on is that arm's. The span is
/// therefore the `Ok` arm's body and nothing more: the `Err` arm, the `Msg::Full`
/// and `Msg::Exif` arms, and the `turn_partner` capture above the message loop are
/// all outside it and cannot stand in for the call. (The existing `fn_body` doc
/// records that a naive "up to the next `fn`" split once made a scan of this shape
/// pass **vacuously**; the brace match is what makes this one falsifiable, and the
/// guard is the proof.)
#[test]
fn arriving_previews_re_plan_through_the_partner() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
    let arm = fn_body(&src, "Msg::Preview { key, result } => match result {");

    assert!(
        arm.contains("image_viewer::arrival_reschedules("),
        "the Msg::Preview arm no longer asks arrival_reschedules whether to re-plan,\n\
         so a partner's decode no longer re-plans. That is the half that heals a turn:\n\
         pairable treats unknown dimensions optimistically, so a partner planned at the\n\
         current priority can turn out to be a wide page, note_shapes learns it, and the\n\
         turn collapses to one page - with no re-plan the two-page spread stays painted\n\
         until the reader moves, and nothing says why.\nBody was:\n{arm}"
    );
}

// ---------------------------------------------------------------------------
// The overrides: what the user said, and where it is written
// ---------------------------------------------------------------------------
//
// Everything above answers "what does the detector say?". This section answers
// "what did the user say, and does it survive a restart?".
//
// Two things make it its own section rather than three more cases in the ones
// above. First, an override is only worth anything if it *beats* the detector, so
// `an_override_beats_the_detector` drives a real `Gallery` through `PageCtx` with
// a store attached and asserts the tiling moved — asserting on the store's own
// bookkeeping would pass against a store nothing ever reads, which was checked by
// making `override_shape` return `None`. Second, this is a file on disk, so it
// inherits the thumbnail cache's rules: `%LOCALAPPDATA%` and not `%APPDATA%`, write
// to a temporary name and rename, and **every failure is an empty override set** —
// nothing a user or an editor does to this file may be able to stop the app
// starting.

/// A `Gallery` plus an override store: what the pairing actually reads.
///
/// The same two fields the binary's `AppPageCtx` will hold, written out here for
/// the same reason that one has to be: [`PageCtx`] is the only way to ask a
/// pairing question, so an override never routed through one has not been shown
/// to change anything a reader could see. Every method delegates; only
/// `override_shape` consults the store.
struct WithOverrides<'a> {
    gallery: &'a Gallery,
    spreads: &'a GallerySpreads,
}

impl PageCtx for WithOverrides<'_> {
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
        PageCtx::step(self.gallery, from, by)
    }

    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
        PageCtx::step_no_wrap(self.gallery, from, by)
    }

    fn key(&self, pos: PagePos) -> Option<ImgKey> {
        PageCtx::key(self.gallery, pos)
    }

    fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
        PageCtx::dims(self.gallery, pos)
    }

    /// The one method that reads the store, and the whole point of the type: a
    /// page's override is asked for *instead of* its dimensions, so the answer
    /// wins over whatever the detector would have said about those dimensions.
    fn override_shape(&self, pos: PagePos) -> Option<Shape> {
        let key = PageCtx::key(self.gallery, pos)?;
        self.spreads.shape_for(&key.path, key.page)
    }

    fn flat(&self, pos: PagePos) -> Option<usize> {
        PageCtx::flat(self.gallery, pos)
    }
}

/// The turn tiling as flat ordinals, walked forwards with [`next_anchor`].
///
/// **Bounded, and stopped by what it has already collected.** A walk that stopped
/// making progress would otherwise hang the suite rather than fail it, and a test
/// that can only end by hanging reports nothing. With `loop_back` off the walk
/// ends at the last turn, so neither guard is reached by a correct implementation
/// — they are here so a wrong one fails.
fn tiling(ctx: &dyn PageCtx, lead_lone: u32) -> Vec<Vec<usize>> {
    let flat = |p: PagePos| {
        PageCtx::flat(ctx, p).expect("every page count in these fixtures is set")
    };
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur = PagePos { index: 0, page: 0 };
    for _ in 0..256 {
        let mut turn = vec![flat(cur)];
        if turn_len(cur, lead_lone, ctx) == 2 {
            if let Some(partner) = ctx.step_no_wrap(cur, 1) {
                turn.push(flat(partner));
            }
        }
        out.push(turn);
        let Some(next) = next_anchor(cur, lead_lone, ctx) else {
            break;
        };
        if out.iter().any(|t| t.contains(&flat(next))) {
            break;
        }
        cur = next;
    }
    out
}

/// The override wins over the aspect ratio, which is the entire feature.
///
/// Four portrait pages of one document, and all four **measured** at 800x1200 — so
/// the detector has a positive answer to disagree with rather than falling through
/// its optimistic default for an unmeasured page. Both readings are `Single`, so the
/// measurement is not what makes the tilings below move; it is what makes "beats"
/// mean *overrides a measurement*.
///
/// The baseline tiling is asserted first, because without it "the tiling changed"
/// is a claim about two tilings without saying what either was.
///
/// Page 2 is the page whose *partner* changes: it was the second half of `{1, 2}`,
/// and a spread takes a whole turn to itself, so the override splits that turn and
/// pushes everything after it along.
#[test]
fn an_override_beats_the_detector() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("page.png");
    write_png(&page, 800, 1200);
    let mut gal = gallery_of(&[page.clone()], &[4]);
    gal.set_loop_back(false);
    for p in 0..4u32 {
        gal.note_shapes(
            ImgKey {
                path: page.clone(),
                page: p,
            },
            (800, 1200),
        );
    }

    let plain = GallerySpreads::default();
    assert_eq!(
        tiling(&WithOverrides { gallery: &gal, spreads: &plain }, LEAD_LONE_DEFAULT),
        vec![vec![0], vec![1, 2], vec![3]],
        "four portrait pages: the cover alone, one pair, then the odd one out"
    );

    let mut spreads = GallerySpreads::default();
    spreads.set_shape(&page, 2, Double);
    assert_eq!(
        tiling(&WithOverrides { gallery: &gal, spreads: &spreads }, LEAD_LONE_DEFAULT),
        vec![vec![0], vec![1], vec![2], vec![3]],
        "page 2 is a spread now, so it takes a whole turn and the pair splits"
    );

    // The other end of "per page, not per file": an override naming the last page
    // changes nothing, because page 3 was already alone. The assertion above is the
    // one that catches a store keyed by name alone; this states the property from
    // the page a store keyed that way would wrongly drag along.
    let mut elsewhere = GallerySpreads::default();
    elsewhere.set_shape(&page, 3, Double);
    assert_eq!(
        tiling(&WithOverrides { gallery: &gal, spreads: &elsewhere }, LEAD_LONE_DEFAULT),
        vec![vec![0], vec![1, 2], vec![3]],
        "an override on page 3 does not reach page 2"
    );
}

/// A set survives a restart, which is the whole reason it is written down.
///
/// **Both** fields are non-default, because a round trip that only carries the
/// pages would pass against a `lead_lone` that was never serialised. The store is
/// written to a directory that does not exist yet, since that is the first
/// gallery a user opens.
///
/// The last assertion is that a **successful** save leaves the store and nothing
/// else in the directory, which catches a write that reached the temporary name
/// and stopped there. It is deliberately not presented as a test of the rename's
/// atomicity: a mutation that writes the store directly and skips the temporary
/// altogether passes every test in this file, and that was run rather than
/// assumed. Crash-atomicity is not observable without fault injection, so it rests
/// on `fs::rename` replacing on Windows — the same thing `write_cached_thumbnail`
/// rests on, and likewise untested there.
#[test]
fn overrides_survive_a_reload_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("nested").join("spreads.json");
    let root = dir.path().join("books");
    let page = root.join("003.png");

    let mut g = GallerySpreads::default();
    g.nudge_lead_lone(1);
    g.set_shape(&page, 0, Double);
    g.set_shape(&page, 4, Single);
    save_spread_overrides(&store, &root, &g).expect("a fresh directory is created");

    let loaded = load_spread_overrides(&store, &root);
    assert_eq!(loaded, g, "everything written came back");
    assert_eq!(loaded.lead_lone, LEAD_LONE_DEFAULT + 1, "the offset is stored, not reset");
    assert_eq!(loaded.shape_for(&page, 0), Some(Double));
    assert_eq!(loaded.shape_for(&page, 4), Some(Single));
    assert_eq!(loaded.shape_for(&page, 1), None, "a page nobody overrode is still un-overridden");

    let mut left: Vec<String> = std::fs::read_dir(store.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec!["spreads.json".to_string()],
        "the temporary is renamed into place, not left beside it"
    );
}

/// "Empty" means what it says, and the second half is the one that matters.
///
/// A store that fails to read quietly producing `lead_lone = 0` would pair the
/// cover with page 2 in **every gallery opened after it** — the pairing changes,
/// nothing on screen says why, and the cause is a file the user never knew
/// existed. So the default offset is asserted by value, not by `== Default`.
fn assert_empty(g: &GallerySpreads) {
    assert!(
        g.pages.is_empty(),
        "a store that cannot be read must not name any page, got {:?}",
        g.pages
    );
    assert_eq!(
        g.lead_lone, LEAD_LONE_DEFAULT,
        "an empty override set still stands the cover alone; 0 would silently re-pair every gallery"
    );
}

/// A file this app writes must never be able to stop it starting.
///
/// Three ways of being unusable, because they fail differently and only the first
/// is obvious: not JSON at all, JSON of the wrong *shape*, and a path that cannot
/// be read at all (a directory where the file should be — the read error that is
/// not "not found"). All three are the same empty override set. There is no repair
/// and no warning anywhere, deliberately: the alternative is refusing to open a
/// folder over a cache file, which is a far worse outcome than losing an override
/// the user can re-press.
#[test]
fn a_malformed_overrides_file_is_an_empty_override_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = dir.path().join("books");

    std::fs::write(&store, "{ not json").unwrap();
    assert_empty(&load_spread_overrides(&store, &root));

    std::fs::write(&store, "[1, 2, 3]").unwrap();
    assert_empty(&load_spread_overrides(&store, &root));

    std::fs::remove_file(&store).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    assert_empty(&load_spread_overrides(&store, &root));
}

/// No file yet is the state every gallery is in until the user overrides
/// something, so it is a first-class case rather than an accident of the malformed
/// one above.
#[test]
fn a_missing_overrides_file_is_an_empty_override_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("not-created-yet").join("spreads.json");
    assert_empty(&load_spread_overrides(&store, &dir.path().join("books")));
}

/// The offset is nudged with a key, and a key repeats. It cannot leave the range
/// [`LEAD_LONE_MAX`] documents: past it, "is this page front matter?" has no
/// answer, and every leading page would be shown alone.
///
/// Both extremes are given as `i32::MIN`/`MAX` rather than a large literal,
/// because the interesting version of this is arithmetic that **wraps** rather than
/// arithmetic that is merely big: `0u32` minus `1` is four billion, which is in
/// range, and the result would be a gallery of nothing but single pages.
#[test]
fn nudging_lead_lone_is_clamped_to_the_documented_range() {
    let mut g = GallerySpreads::default();
    assert_eq!(g.lead_lone, LEAD_LONE_DEFAULT, "a fresh value stands the cover alone");

    g.nudge_lead_lone(0);
    assert_eq!(g.lead_lone, LEAD_LONE_DEFAULT, "a nudge of nothing changes nothing");

    for _ in 0..LEAD_LONE_DEFAULT {
        g.nudge_lead_lone(-1);
    }
    assert_eq!(g.lead_lone, 0);
    g.nudge_lead_lone(-1);
    assert_eq!(g.lead_lone, 0, "one below the floor is still the floor");
    g.nudge_lead_lone(i32::MIN);
    assert_eq!(g.lead_lone, 0, "a huge negative nudge clamps; it must not wrap");

    for _ in 0..LEAD_LONE_MAX {
        g.nudge_lead_lone(1);
    }
    assert_eq!(g.lead_lone, LEAD_LONE_MAX);
    g.nudge_lead_lone(1);
    assert_eq!(g.lead_lone, LEAD_LONE_MAX, "LEAD_LONE_MAX is the ceiling");
    g.nudge_lead_lone(i32::MAX);
    assert_eq!(g.lead_lone, LEAD_LONE_MAX, "a huge positive nudge clamps; it must not wrap");
}

/// `toggle` answers with the shape the page landed on **and stores it**, because
/// the caller needs the answer to re-plan against — the tiling it is asking about
/// has already changed by the time this returns.
///
/// The stored value is deliberately *not* the input: the same `detected` answers
/// the same way twice. That is the contract, not an accident of the order of two
/// statements — `toggle` does not read its own last answer back, so the flip is
/// always away from what the *caller* says the page is now rather than away from
/// what this store last wrote. (For the caller that passes the effective shape the
/// two agree every time; where they part company is an override that has outlived
/// the file it was set against.)
#[test]
fn toggling_a_page_reports_the_shape_it_landed_on() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("001.png");
    let mut g = GallerySpreads::default();

    assert_eq!(g.toggle(&page, 0, Double), Single, "a spread becomes one page");
    assert_eq!(g.shape_for(&page, 0), Some(Single), "and it is stored, not just returned");
    assert_eq!(g.toggle(&page, 0, Single), Double, "and the other way");
    assert_eq!(g.shape_for(&page, 0), Some(Double));

    assert_eq!(g.toggle(&page, 0, Single), Double, "the same input answers the same way");
    assert_eq!(g.shape_for(&page, 0), Some(Double), "so a second press re-asserts it");

    // Pages are separate keys: flipping page 0 must not disturb page 1.
    g.set_shape(&page, 1, Single);
    g.toggle(&page, 0, Double);
    assert_eq!(g.shape_for(&page, 1), Some(Single), "page 1 was not touched");
    assert_eq!(g.shape_for(&page, 0), Some(Single));
}

/// Two instances of the app must not write the same temporary.
///
/// **The reader guarantee holds either way** — `fs::rename` replaces on Windows, so
/// a reader sees either the old store or the new one, never a half-written file.
/// What a *fixed* temporary name breaks is what gets renamed: `fs::write` opens with
/// `Create`/`TRUNCATE`, so two savers milliseconds apart both hold a handle to the
/// same file, both truncate it, and their writes interleave. The first to finish
/// renames a mixture of the two over `spreads.json` — and because an unparseable
/// store is treated as empty and replaced, the next override press then rewrites the
/// file with one gallery's entry and every other gallery's decisions are gone. There
/// is no second copy anywhere, so that is not recoverable.
///
/// **Two halves, because the property has two halves.** Naming is a pure function of
/// the store path and an id, so "two ids, two names" is checkable — but that alone
/// does not pin the id the app *passes*, and a call site passing a constant would
/// sail past it. So the second half observes the real thing: a save whose rename
/// cannot succeed leaves its temporary behind, and the name it leaves is
/// `spreads.json.tmp.<this process's id>`.
///
/// The rename fails when the store path is a **directory**, which is also the only
/// way to see a temporary at all — a successful save renames it away. Nothing here
/// can see a *torn* write, and that is the part that stays untested: crash-atomicity
/// is not observable without fault injection, and the thumbnail cache has no test for
/// it either. Its fixed `.tmp` is not the precedent to copy, though — that is safe
/// only because two writers of one entry write the same bytes, and this store is a
/// read-modify-write, so they do not.
#[test]
fn two_instances_of_the_app_cannot_write_the_same_temporary() {
    let store = std::path::Path::new(r"C:\Users\me\AppData\Local\image-viewer\spreads.json");

    let one = spreads_tmp(store, 1000);
    let two = spreads_tmp(store, 2000);

    assert_ne!(one, two, "two processes must not share a temporary name");
    assert_eq!(
        one.parent(),
        store.parent(),
        "beside the store, so the rename is on one filesystem and therefore atomic"
    );
    assert_eq!(
        one,
        store.with_file_name("spreads.json.tmp.1000"),
        "the store's own name with .tmp.<pid> appended - not with_extension, which \
         would rewrite the .json and read as a different file"
    );
    assert!(two.to_string_lossy().ends_with(".tmp.2000"));

    // ...and the id the app actually passes. The store path is a directory here, so
    // the write succeeds and the rename cannot: `MoveFileEx` will not replace a
    // directory with a file.
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    std::fs::create_dir(&store).unwrap();
    let g = GallerySpreads::default();
    save_spread_overrides(&store, dir.path(), &g)
        .expect_err("renaming a file over a directory must fail, or this test proves nothing");
    // The `ErrorKind` is not asserted: Windows reports `ERROR_ACCESS_DENIED`, so it
    // is `PermissionDenied` here, and pinning it would be pinning an OS's choice
    // rather than this function's behaviour. That the save *reported* failure and
    // still left the temporary behind is the whole of what is being checked.

    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    // The store path itself is in there too — it is the directory this test put
    // there — so it is excluded rather than the assertion being weakened to
    // "contains".
    left.retain(|n| n != "spreads.json");
    left.sort();
    assert_eq!(
        left,
        vec![format!("spreads.json.tmp.{}", std::process::id())],
        "the temporary this process left behind carries its own id, so a second \
         instance of the app writes beside it rather than into it"
    );
}

/// An over-cap store is **left alone by a save**, and honoured as empty by a load.
///
/// **The two halves are separate assertions because one cannot witness the other.**
/// Both a load and a correctly-refusing save leave an over-cap store *looking* the
/// same to the next load — empty either way — so an assertion on the load alone
/// cannot tell "left alone" from "rewritten as empty", which is the failure that
/// matters: the store is a read-modify-write, so a save that could not read it would
/// replace that gallery's real entry, and every other gallery's with it, and then
/// there is no second copy anywhere. (This is the same trap as the two caps in
/// `the_two_caps_degrade_to_nothing_rather_than_to_an_error`, where one cap could
/// satisfy the other's assertion; the remedy is the same — state what each half
/// proves, and check the premises.)
///
/// The fixture is **hand-built**, because a correct save refuses to write a store
/// over the cap — asking `save_spread_overrides` to produce one would be asking for
/// the bug this test exists to catch. It is checked three ways before it is used:
/// over the cap, parseable, and still holding the entry that a clobbering save would
/// destroy.
#[test]
fn an_over_cap_store_is_left_alone_by_a_save_and_ignored_by_a_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = std::path::PathBuf::from(r"C:\books\vol1.cbz");

    // Wide names rather than many of them, so the *page count* stays under
    // `SPREADS_MAX_PAGES` and the byte cap is unambiguously the one in play.
    let wide: String = "p".repeat(200);
    // The root goes into a JSON string, so its backslashes are escaped. That is not
    // a detail: the first version of this fixture wrote the raw path, and the
    // "must be a valid store" premise check below caught it on the first run.
    let key = root.to_string_lossy().replace('\\', "\\\\");
    let mut json = format!(
        "{{ \"{key}\": {{ \"lead_lone\": 2, \"pages\": {{ \"001.png\": {{ \"0\": \"double\" }}"
    );
    for i in 0..5_000usize {
        json.push_str(&format!(", \"{wide}{i}.png\": {{ \"0\": \"double\" }}"));
    }
    json.push_str(" } } }");
    std::fs::write(&store, &json).unwrap();

    let len = std::fs::metadata(&store).unwrap().len();
    assert!(
        len > SPREADS_MAX_BYTES,
        "this fixture must be over the byte cap ({len} bytes) or nothing is being tested"
    );
    let before = std::fs::read_to_string(&store).unwrap();
    serde_json::from_str::<serde_json::Value>(&before).expect("the fixture must be a valid store");
    let pages_in_fixture = before.matches("\": { \"0\": \"double\" }").count();
    assert!(
        pages_in_fixture <= SPREADS_MAX_PAGES,
        "the fixture has {pages_in_fixture} page entries, over SPREADS_MAX_PAGES, so \
         the page cap could be what is being tested instead of the byte cap"
    );

    // Half one: the load honours nothing, which is safe because a load only reads.
    assert_empty(&load_spread_overrides(&store, &root));

    // Half two: the save refuses, so the bytes are untouched. An `Err` *and* no
    // write — both are asserted, because a save that reported success over the cap
    // and one that reported failure after writing it are different bugs with the
    // same consequence.
    let mut small = GallerySpreads::default();
    small.set_shape(&root.join("001.png"), 0, Shape::Single);
    assert!(
        save_spread_overrides(&store, &root, &small).is_err(),
        "a save that cannot read the store must report that it did not persist, not \
         return Ok and leave the caller thinking the decision was saved"
    );
    let after = std::fs::read_to_string(&store).unwrap();
    assert_eq!(
        after, before,
        "an over-cap store must be left byte-for-byte intact: a save that read it as \
         empty would have replaced this gallery's real entry with the value it just \
         handed in, and every other gallery's with nothing"
    );

    // ...and "intact" is checked against the *current* file's content, not only
    // against the bytes captured before: the entry that would have been destroyed is
    // still in it, parsed independently of this library's own reader.
    let parsed: serde_json::Value = serde_json::from_str(&after).expect("the file still parses");
    assert_eq!(
        parsed[&*root.to_string_lossy()]["pages"]["001.png"]["0"],
        "double",
        "the override a clobbering save would have destroyed is still in the file"
    );

    // The control, so "nothing was written" cannot be confused with "nothing could
    // be written": a store inside the cap does take the new value.
    let small_store = dir.path().join("small.json");
    save_spread_overrides(&small_store, &root, &small).unwrap();
    assert_eq!(
        load_spread_overrides(&small_store, &root).shape_for(&root.join("001.png"), 0),
        Some(Single),
        "a store inside the cap is read-modify-written as usual"
    );
}

/// The written spelling of a shape is pinned, because the file is meant to be edited
/// by hand.
///
/// `an_older_or_hand_edited_store_still_loads` proves that `"double"` *loads*, and a
/// round trip stays green under any self-consistent serde setting — including one
/// that writes `"Double"` and reads `"Double"`, which would leave a hand-edited file
/// loading as nothing at all. So this asserts the bytes, not the round trip.
#[test]
fn the_written_shape_is_the_spelling_the_file_documents() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = dir.path().join("books");

    let mut g = GallerySpreads::default();
    g.set_shape(&root.join("001.png"), 0, Double);
    g.set_shape(&root.join("002.png"), 0, Single);
    save_spread_overrides(&store, &root, &g).unwrap();

    let text = std::fs::read_to_string(&store).unwrap();
    assert!(
        text.contains("\"double\""),
        "the documented spelling must be what is written, not just what loads:\n{text}"
    );
    assert!(
        !text.contains("\"Double\""),
        "serde's own variant spelling leaked into the file:\n{text}"
    );
}

/// A page key that is not a number costs **that page**, and nothing else.
///
/// This is the seam between two documented rules that pull in opposite directions,
/// so it is worth naming rather than leaving to be discovered:
///
/// * A store that cannot be **read at all** — missing, malformed, the wrong shape —
///   is an empty override set. One unusable byte costs the user every override.
///
/// * A store that reads, with one page key that is not a number, keeps every other
///   override. The alternative is failing the parse over a key nothing can address
///   anyway, which turns one typo into the whole-empty case above.
///
/// The second is the *tolerant* reading and it is the one implemented, because the
/// cost asymmetry is not symmetric: dropping one unreachable key loses nothing, and
/// failing the parse loses everything. (The thumbnail cache's "every failure is a
/// miss" is a *per-entry* rule and its entries are separate files; here one file
/// holds every gallery, so the same reasoning has to be applied at the key rather
/// than at the file.)
#[test]
fn a_page_key_that_is_not_a_number_costs_that_page_and_no_other() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = std::path::PathBuf::from(r"C:\books\vol1.cbz");
    std::fs::write(
        &store,
        concat!(
            "{\n",
            "  \"C:\\\\books\\\\vol1.cbz\": { \"lead_lone\": 3, \"pages\": {\n",
            "    \"001.png\": { \"0\": \"double\", \"x\": \"single\", \"\": \"double\" },\n",
            "    \"002.png\": { \"0\": \"single\" }\n  } }\n",
            "}",
        ),
    )
    .unwrap();

    let g = load_spread_overrides(&store, &root);
    assert_eq!(g.lead_lone, 3, "the rest of the entry is untouched");
    assert_eq!(g.shape_for(&root.join("001.png"), 0), Some(Double));
    assert_eq!(g.shape_for(&root.join("002.png"), 0), Some(Single));
    assert_eq!(g.shape_for(&root.join("001.png"), 1), None, "no page 1 was ever named");
    assert_eq!(
        g.pages.len(),
        2,
        "the two keys that are not numbers are dropped, and nothing else is: {:?}",
        g.pages
    );
}

/// A store bigger than [`SPREADS_MAX_BYTES`] is not read, and an entry with more
/// pages than [`SPREADS_MAX_PAGES`] is dropped whole.
///
/// **Premises are checked, not assumed.** Both fixtures are written *through the
/// library*, so a typo in a hand-built JSON string could not be mistaken for a cap
/// biting; and each half asserts the *other* cap is not the one firing, so "empty
/// because it was capped" cannot pass for "empty because it did not parse".
///
/// Whole-entry-or-nothing for the entry cap is deliberate and load-bearing: a
/// `HashMap`'s iteration order is randomised per process, so keeping "the first
/// `SPREADS_MAX_PAGES` of them" would hand back a different set of overrides on
/// every launch, and a user could never reproduce what they saw.
#[test]
fn the_two_caps_degrade_to_nothing_rather_than_to_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::path::PathBuf::from(r"C:\books\vol1.cbz");

    // Entry cap: valid, and *under* the byte cap, so only the entry cap can fire.
    let mut many = GallerySpreads::default();
    let pages = SPREADS_MAX_PAGES + 1;
    for page in 0..pages as u32 {
        many.set_shape(&root.join("001.png"), page, Shape::Double);
    }
    let store = dir.path().join("entries.json");
    save_spread_overrides(&store, &root, &many).unwrap();
    let len = std::fs::metadata(&store).unwrap().len();
    assert!(
        len <= SPREADS_MAX_BYTES,
        "this fixture must stay under the byte cap ({len} bytes) or the other cap \
         would be what is being tested"
    );
    let g = load_spread_overrides(&store, &root);
    assert!(
        g.pages.is_empty(),
        "every page override is dropped over the page cap, not a subset of them: a \
         subset would be a different subset on every launch"
    );
    assert_eq!(
        g.lead_lone, LEAD_LONE_DEFAULT,
        "the offset is not what the cap is about, so it survives the emptied page map"
    );

    // Byte cap: over it, so the read is skipped whatever the contents are — and
    // **under the page cap, so the other cap cannot be what empties it.** That
    // attribution is the whole difficulty: the first version of this fixture was
    // 40 000 short-named entries, which is over both caps, and it passed with the
    // byte cap removed because the page cap caught it instead. A mutation-verified
    // guard that another guard can satisfy is not one.
    let mut huge = GallerySpreads::default();
    let wide: String = "p".repeat(200);
    let entries = 5_000usize;
    for i in 0..entries {
        huge.set_shape(&root.join(format!("{wide}{i}.png")), 0, Shape::Double);
    }
    let store = dir.path().join("huge.json");
    save_spread_overrides(&store, &root, &huge).unwrap();
    let len = std::fs::metadata(&store).unwrap().len();
    assert!(
        len > SPREADS_MAX_BYTES,
        "this fixture must be over the byte cap ({len} bytes) or nothing is being tested"
    );
    assert!(
        entries <= SPREADS_MAX_PAGES,
        "this fixture must be under the page cap ({entries} entries) or the page cap \
         would be what is being tested"
    );
    assert_empty(&load_spread_overrides(&store, &root));

    // The control: the same shape of store, comfortably inside both caps, loads.
    let mut few = GallerySpreads::default();
    few.set_shape(&root.join("001.png"), 0, Shape::Double);
    let store = dir.path().join("few.json");
    save_spread_overrides(&store, &root, &few).unwrap();
    assert_eq!(
        load_spread_overrides(&store, &root).shape_for(&root.join("001.png"), 0),
        Some(Double),
        "a store inside both caps is not affected by either"
    );
}

/// A path with no file name stores nothing, rather than colliding on the empty one.
///
/// `file_name_of` answers `""` for one, and the store is keyed by that name, so
/// every such path would share the entry `("", page)` and the last write would
/// silently replace the previous one's decision. `spread_root_key` refuses the same
/// empty string for a *root*; this closes the same asymmetry one level down.
///
/// Nothing reachable today produces such a path — a folder scan and `extract_zip`
/// both name every file they yield — which is why the guard is a refusal rather
/// than an error: the entry could never have been addressed again anyway.
#[test]
fn a_path_with_no_file_name_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = GallerySpreads::default();

    // Two different nameless paths: with the empty key they would share one entry.
    g.set_shape(std::path::Path::new(r"C:\"), 0, Double);
    g.set_shape(std::path::Path::new(".."), 0, Single);
    assert!(
        g.pages.is_empty(),
        "nothing can address an entry keyed by an empty name, so none is written: {:?}",
        g.pages
    );
    assert_eq!(g.shape_for(std::path::Path::new(".."), 0), None);

    // The control: a name that merely *contains* a separator is fine, which is the
    // whole point of keying on the file name rather than the path.
    g.set_shape(&dir.path().join("sub").join("001.png"), 3, Double);
    assert_eq!(g.shape_for(&dir.path().join("sub").join("001.png"), 3), Some(Double));
    assert_eq!(g.pages.len(), 1);
}

/// An archive's overrides are keyed by the **archive**, and never by the `%TEMP%`
/// directory its members were extracted into.
///
/// The trap this exists for: `MyApp::folder` holds the extraction directory while
/// an archive is open, and that directory's name is a hash of the archive's path,
/// mtime and length — so it is stable only until the archive is edited. Keying
/// overrides by it would look right for as long as nobody touched the `.cbz`, then
/// silently lose every override the first time they did.
///
/// So this asserts three separate things: the override is *found* after the
/// extraction directory changes, the store contains *no* spelling of that
/// directory, and asking for the extraction directory as the root finds nothing.
#[test]
fn an_archive_is_keyed_by_its_original_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let archive = std::path::PathBuf::from(r"C:\books\vol3.cbz");
    // Two extractions of one archive. Neither exists on disk: what is being pinned
    // is which key is written and read, so the names are all that matter.
    let first = std::path::PathBuf::from(r"C:\Temp\image-viewer\000000000000a01");
    let second = std::path::PathBuf::from(r"C:\Temp\image-viewer\000000000000b02");

    let mut g = GallerySpreads::default();
    g.set_shape(&first.join("012.png"), 0, Double);
    save_spread_overrides(&store, &archive, &g).unwrap();

    assert_eq!(
        load_spread_overrides(&store, &archive).shape_for(&second.join("012.png"), 0),
        Some(Double),
        "the override has to survive a re-extract, which renames the extraction directory"
    );

    let text = std::fs::read_to_string(&store).unwrap();
    assert!(text.contains("vol3.cbz"), "the archive's own path is the key:\n{text}");
    for leak in ["Temp", "image-viewer", "000000000000a01"] {
        assert!(
            !text.contains(leak),
            "an extraction path reached the store as {leak:?}, so the overrides\n\
             die with the temp directory:\n{text}"
        );
    }

    assert!(
        load_spread_overrides(&store, &first).pages.is_empty(),
        "asking for the extraction directory as the root finds nothing - which is \
         exactly what keying by `folder` would have done"
    );
}

/// One file, many galleries, and each gallery sees only its own decisions.
///
/// **The same file name in both roots**, which is what makes the scoping
/// load-bearing rather than incidental: two folders of scans each containing
/// `001.png` must not answer for one another, and the page key deliberately holds
/// a bare name (a member of an archive has no meaningful directory to record).
#[test]
fn overrides_are_scoped_to_one_gallery() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let archive = std::path::PathBuf::from(r"C:\books\vol1.cbz");
    let folder = dir.path().join("scans");
    let member = "001.png";

    let mut a = GallerySpreads::default();
    a.set_shape(&archive.join("pages").join(member), 0, Double);
    save_spread_overrides(&store, &archive, &a).unwrap();
    let mut b = GallerySpreads::default();
    b.set_shape(&folder.join(member), 0, Single);
    save_spread_overrides(&store, &folder, &b).unwrap();

    assert_eq!(
        load_spread_overrides(&store, &archive).shape_for(&archive.join("pages").join(member), 0),
        Some(Double)
    );
    assert_eq!(
        load_spread_overrides(&store, &folder).shape_for(&folder.join(member), 0),
        Some(Single),
        "the same file name in another gallery is a different page"
    );
    assert!(
        load_spread_overrides(&store, &dir.path().join("elsewhere")).pages.is_empty(),
        "a root nobody saved anything for has no overrides"
    );
}

/// One store holds every gallery, so a save is a **read-modify-write**, never a
/// whole-file overwrite.
///
/// This is the failure the scoping test above cannot see: both saves succeed, both
/// loads return the right answer, and the first gallery's overrides are gone
/// anyway because the second save replaced the file. `spreads.json` is the only
/// place the app keeps these, so that is the user losing every override in every
/// other folder — with no error and nothing on screen to say so.
#[test]
fn saving_one_gallery_keeps_the_others_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let one = dir.path().join("one");
    let two = dir.path().join("two");

    let mut a = GallerySpreads::default();
    a.nudge_lead_lone(1);
    a.set_shape(&one.join("001.png"), 0, Double);
    save_spread_overrides(&store, &one, &a).unwrap();

    let mut b = GallerySpreads::default();
    b.set_shape(&two.join("001.png"), 0, Single);
    save_spread_overrides(&store, &two, &b).unwrap();

    assert_eq!(load_spread_overrides(&store, &one), a, "gallery one survived");
    assert_eq!(load_spread_overrides(&store, &two), b);

    // ...and re-saving one gallery replaces **its own** entry rather than merging
    // with itself: a page the user has since un-overridden has to actually go.
    let mut a2 = GallerySpreads::default();
    a2.set_shape(&one.join("002.png"), 0, Double);
    save_spread_overrides(&store, &one, &a2).unwrap();
    let reloaded = load_spread_overrides(&store, &one);
    assert_eq!(reloaded.pages.len(), 1, "the entry was replaced, not added to");
    assert_eq!(reloaded.shape_for(&one.join("002.png"), 0), Some(Double));
}

/// A re-exported file's overrides are dropped, so the next read judges the pages
/// again.
///
/// **Every page, not one**, and for a sharper reason than the dimension cache's:
/// an override naming page 3 of a document that is now one page long is not a
/// stale *measurement*, it is a decision about pixels that no longer exist — and
/// because an override beats the detector, keeping it holds page 3 a spread for
/// the session no matter what the file now contains. `invalidate_path` is what
/// calls this, and it is the only thing that knows the file's contents changed.
#[test]
fn forget_file_drops_every_page_of_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");

    let mut g = GallerySpreads::default();
    g.set_shape(&a, 0, Double);
    g.set_shape(&a, 7, Double);
    g.set_shape(&b, 0, Single);

    g.forget_file(&a);
    assert_eq!(g.shape_for(&a, 0), None, "forgotten means absent, not stale");
    assert_eq!(g.shape_for(&a, 7), None, "every page of the file, not just the first");
    assert_eq!(g.shape_for(&b, 0), Some(Single), "another file's decision is not collateral");
}

/// Which gallery an open archive is keyed by, as a pure function.
///
/// The rule is here rather than in `MyApp::open_archive` because the thing it
/// exists to prevent is *silent*: with no archive open, `folder` really is the
/// gallery's root, and with one open `folder` is the extraction directory. A
/// single expression that prefers the archive path is the only version of this
/// that can be checked, and the empty-folder case is why it is `Option` at all —
/// two galleries with no resolvable parent would otherwise share one entry, and
/// each one's overrides would answer for the other.
#[test]
fn an_open_archive_is_keyed_by_its_own_path_not_its_extraction_directory() {
    let archive = std::path::PathBuf::from(r"C:\books\vol3.cbz");
    let extracted = std::path::PathBuf::from(r"C:\Temp\image-viewer\000000000000a01");
    let folder = std::path::PathBuf::from(r"C:\books\vol4");

    assert_eq!(
        spread_root_key(Some(&archive), &extracted),
        Some(r"C:\books\vol3.cbz".to_string()),
        "with an archive open, `folder` is the extraction directory and must be ignored"
    );
    assert_eq!(
        spread_root_key(None, &extracted),
        Some(extracted.to_string_lossy().into_owned()),
        "with no archive open, `folder` is the root and is used"
    );
    assert_eq!(spread_root_key(None, &folder), Some(folder.to_string_lossy().into_owned()));
    assert_eq!(
        spread_root_key(Some(&archive), std::path::Path::new("")),
        Some(r"C:\books\vol3.cbz".to_string()),
        "an empty folder must not matter while an archive is open"
    );
    assert_eq!(
        spread_root_key(None, std::path::Path::new("")),
        None,
        "the empty path is the one value that names no gallery, so it keys nothing \
         rather than one shared empty entry"
    );
    // **No parent is not the test.** `.` has no parent and is not empty, so it is a
    // name like any other - and the doc comment says so, because "a root with no
    // name" reads like it covers this case and it does not.
    assert_eq!(
        spread_root_key(None, std::path::Path::new(".")),
        Some(".".to_string()),
        "a relative root is a name like any other"
    );
}

/// The overrides live beside the thumbnail cache, **not** beside the config.
///
/// `AGENTS.md` ("A cache of any sort is `%LOCALAPPDATA%`, not `%APPDATA%`") and the
/// thumbnail cache's own doc comment: these are *this machine's* reading of files
/// that may be on a network share, they are worthless on another machine, and a
/// roaming profile is the wrong place for them. `config.json` is the opposite
/// case and it does live in `%APPDATA%`, which is why the two being together
/// would be a mistake rather than a tidiness question.
///
/// The file *name* is asserted because it is a published name, not an internal one:
/// the store is meant to be found, opened, edited and deleted by whoever is reading
/// it, and a path described three different ways in three places is a path nobody
/// can follow.
#[test]
fn the_overrides_live_beside_the_thumbnail_cache_not_beside_the_config() {
    let store = spreads_root().expect("LOCALAPPDATA is set on Windows");
    assert_eq!(
        store.file_name().and_then(|n| n.to_str()),
        Some("spreads.json"),
        "the documented file name: {store:?}"
    );
    assert_eq!(
        store.parent(),
        thumb_cache_root().parent(),
        "the same image-viewer folder as the thumbnail cache: local, not roaming"
    );
    let roaming = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .expect("APPDATA is set on Windows")
        .join("image-viewer");
    assert_ne!(
        store.parent(),
        Some(roaming.as_path()),
        "a roaming profile is the wrong home for this"
    );
}

/// A store written by an earlier build, or edited by hand, still loads.
///
/// Written as **text** rather than through `save_spread_overrides`, because every
/// one of these is something a hand or an older build does and the round trip
/// cannot produce any of them:
///
/// * `"double"` — the spelling `#[serde(rename_all = "lowercase")]` exists to
///   produce. Without it serde writes `"Double"` and refuses to read `"double"`,
///   so a hand-edited file using the documented spelling loads as *nothing*, and
///   `AGENTS.md` records that exact trap for `WgpuBackend`.
/// * a root with no `lead_lone` — an older build, or a hand that added one page.
///   It gets [`LEAD_LONE_DEFAULT`], not `0`: see `assert_empty`.
/// * a root with no `pages` — likewise, and it gets none.
/// * `"lead_lone": 99` — past the documented maximum. Clamped on load, or the
///   first 99 pages of the gallery stand alone, which is a pairing no reader
///   asked for.
/// * `"note"` — an unknown key. Ignored, never rejected: that tolerance is the
///   only reason a file from an older build still loads at all.
#[test]
fn an_older_or_hand_edited_store_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    std::fs::write(
        &store,
        concat!(
            "{\n",
            "  \"C:\\\\books\\\\vol1.cbz\": { \"pages\": { \"001.png\": { \"0\": \"double\" } } },\n",
            "  \"C:\\\\books\\\\vol2.cbz\": { \"lead_lone\": 99, \"note\": \"hand-written\" }\n",
            "}",
        ),
    )
    .unwrap();

    let one = load_spread_overrides(&store, std::path::Path::new(r"C:\books\vol1.cbz"));
    assert_eq!(
        one.shape_for(std::path::Path::new(r"C:\books\vol1\001.png"), 0),
        Some(Double),
        "\"double\" is the spelling this file documents, so it must load"
    );
    assert_eq!(one.lead_lone, LEAD_LONE_DEFAULT, "a missing key is the default, not zero");

    let two = load_spread_overrides(&store, std::path::Path::new(r"C:\books\vol2.cbz"));
    assert!(two.pages.is_empty(), "a missing `pages` key is no pages");
    assert_eq!(two.lead_lone, LEAD_LONE_MAX, "a hand-edited 99 is clamped, not obeyed");
}

// ---------------------------------------------------------------------------
// Where a turn's pages go
// ---------------------------------------------------------------------------
//
// Two pure functions and no egui: the panel is `[x, y, width, height]` in plain
// floats and the rects come back the same way, so a whole turn's layout is checked
// without a window. What is pinned here is the three decisions the pairing walk
// above cannot see:
//
// * **one shared scale** — two halves cannot be drawn at two different zoom
//   levels, so the constrained page decides and the other gets a margin under it;
// * **input order out** — `rtl` decides which *side* a page lands on, so a
//   function returning screen order swaps which page is which in exactly one
//   reading direction, and nowhere else;
// * **the seam at the centre, always** — zero gutter, and no special case for a
//   lone page, which is the case that reads as a bug.

/// The panel every layout test below uses. Its seam is `x + width / 2` = 400.
const PANEL: Rect4 = [0.0, 0.0, 800.0, 600.0];

/// A rect's right-hand edge: every assertion below is about which side of the seam
/// something landed on, so this names the line instead of spelling `x + width` out
/// at each one.
fn right(r: Rect4) -> f32 {
    r[0] + r[2]
}

/// How much two rects overlap horizontally, or 0.0 for rects that merely touch.
///
/// Used instead of comparing edges so "neither overlaps the other" is a claim
/// about the two rects together — two pages drawn at the same `x` overlap no matter
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
/// assertions are that it hangs off the seam and stops there — and the control
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
/// halves at two scales is not a layout — it is one zoom level for the reader.
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

/// An **unflagged** turn — a lone portrait page, which is what a lone page in practice
/// is now — is fitted into its half and laid out there.
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
/// turn" — which would give a cover the whole window and lose the seam it shares with
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
        assert!(
            r[2] <= HALF,
            "rtl={rtl}: and it still keeps to that half: {} wide",
            r[2]
        );
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
/// 296x178 in the panel — so a fix that only centred the rect would leave the page at
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
        let half_rect = spread_rects(PANEL, &pages, spread_fit(panel, &pages, false), rtl, false)[0];
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
/// that is too wide — which would hand a real spread two half-width slots.
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
    // `pairable` is what keeps a `Double` out of a pair — not the layout's flag. Without
    // this, "a two-page turn is unaffected" is satisfiable by a `spread_rects` that lays
    // every turn out as a lone spread whenever the flag happens to be true, on a page
    // count nobody checked.
    let wide_pair = spread_rects(PANEL, &[WIDE_SCAN, WIDE_SCAN], scale, true, true);
    assert_eq!(
        wide_pair[0][0], SEAM,
        "two wide pages still meet at the seam: a flag true on a turn of two is not a \
         request to fill the panel twice over"
    );
    assert!(
        overlap(wide_pair[0], wide_pair[1]) == 0.0,
        "and do not overlap while doing it"
    );
}

/// The discriminator is the **shape**, and these are the two cases where a geometric
/// `w > h` would answer differently.
///
/// A lone near-square page is 1.05 — wider than tall, so a geometry test calls it a
/// spread — and it is a `Single` (`SPREAD_ASPECT` is 1.2), so it keeps its half like a
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
/// The finiteness half is the property the layout depends on — a `NaN` scale reaches
/// every rect and leaves an unpaintable page with no error to point at — and the two
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

// ---------------------------------------------------------------------------
// Where the zoom anchors
// ---------------------------------------------------------------------------
//
// The zoom keeps the point under the cursor fixed, which needs one number: where
// the content's centre is. `MyApp::central` used `anchor - panel.center()`, and
// that is right **only when the content is centred on the panel's centre**. In a
// spread it is not, on either axis:
//
// * **Horizontally**, `spread_rects` puts the *seam* on the panel's centre and
//   extends each page outward, so the turn's centre is the seam plus half the
//   difference of the two widths. Zero for two equal pages, which is why the bug
//   is invisible on a clean pair and appears the moment pages differ.
// * **Vertically**, the turn is top-aligned, so its centre is the panel's top plus
//   half the *tallest* page - never the panel's vertical centre.
// * **For a lone page** the content sits entirely on one side of the seam, so it
//   is displaced by up to a quarter of the panel.
//
// So the anchor has to be the content's centre, and it has to be **derived from
// `spread_rects` itself** - a second statement of the seam rule is a second rule,
// and the two drifting apart shows as a zoom that creeps towards the cursor.

/// A two-page turn's anchor is displaced from the panel centre by half the width
/// difference - and the number is pinned, because "not the panel centre" is
/// satisfied by any wrong displacement.
///
/// The pages are 400 and 700 wide at 0.5, so 200 and 350 on screen: the seam is at
/// `PANEL[0] + PANEL[2]/2` = 400 either way, and the content spans
/// `[400-350, 400+200]` under `rtl` and `[400-200, 400+350]` under `ltr`. Both are
/// 700 wide - the two halves together are always as wide as the seam plus both
/// pages - so the centre is the midpoint of that span and the displacement is
/// ±(350-200)/2 = ±75.
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
/// centre is the seam ± 100 horizontally and 200 vertically.
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
        assert_eq!(spread_centre(PANEL, &pages, scale, rtl, false).0, expected_x, "rtl={rtl}: pinned");
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

/// The zoom block asks for the content's centre — **twice, at two scales** — and
/// never for the panel's.
///
/// **A source scan, and the four tests above are not it.** They pin
/// [`spread_centre`]; nothing pins the call, because `MyApp::central` is in a binary
/// no integration test can reach (`AGENTS.md`, "Tests"), and re-introducing
/// `anchor - panel.center()` there leaves **all 84 tests in this file green** — that
/// was run, and it is why this exists.
///
/// It is weak in the way [`invalidate_path_drops_the_cached_dimensions`] is weak in:
/// it cannot tell whether the anchor maths *works*, only that the centre asked for
/// is the one the library computes. What it catches is the regression that actually
/// happens — the call being reverted, or a later edit reusing one centre where the
/// two scales differ. The behaviour was verified separately, by instrumenting the
/// running app and checking its own per-frame offsets against the formula (see the
/// task report); that evidence is not a test and does not survive here.
#[test]
fn the_zoom_measures_from_the_content_centre_at_both_scales() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
    let body = fn_body(&src, "fn central(");
    // **Comments are stripped first**, and that is not tidiness: the block quotes the
    // expression it replaced ("`anchor - panel.center()` is right only when..."), so
    // scanning the raw text would pass on the comment alone and mean nothing.
    let code = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        code.contains("centre_at(scale)") && code.contains("centre_at(target_scale)"),
        "the zoom must bracket the content point between the centre at the current\n\
         scale and the centre at the target one. Reusing one of them is the old line\n\
         again with a different centre, and it is exact only when the content's centre\n\
         does not move with the zoom - true for one image, false for every turn.\n\
         Body was:\n{body}"
    );
    assert!(
        !code.contains("anchor - panel.center()"),
        "the zoom is anchoring on the panel's centre again, which is displaced from the\n\
         content's on BOTH axes in a spread: the seam is on the panel centre, so a\n\
         turn of unequal pages is off by half the width difference, and being top\n\
         aligned it is off vertically by half the tallest page.\nBody was:\n{body}"
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
///   session rather than for one decode — and because the plan asks again every
///   schedule, it was a re-decode of a 30 MB page on every navigation.
///
/// Scoped to their own functions so a `pv.full` or a `Kind::Full` elsewhere in the
/// file cannot stand in for either. The tile loop is inside `central`, so the scan
/// cannot tell which page it was reading — that was verified by instrumentation
/// instead, and the numbers are in the task report.
#[test]
fn both_halves_of_a_turn_are_cut_from_their_own_full_resolution_buffers() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");

    let central = fn_body(&src, "fn central(");
    assert!(
        central.contains("tile_pages") && central.contains("page_rects[i]") && central.contains("rects[i]"),
        "the tile loop is not ranging over the pages of the turn any more. It has to cut\n\
         each page from that page's own buffer at that page's own rect, or a zoomed\n\
         spread is sharp on one half and soft on the other - with the expensive decode\n\
         already paid for and sitting in memory.\nBody was:\n{central}"
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

// ---------------------------------------------------------------------------
// Where a turn may be panned
// ---------------------------------------------------------------------------
//
// The zoom keeps the point under the cursor where it is (above), and then the offset
// is clamped so the picture cannot be lost off-screen. **The clamp was the same bug one
// line further down**: `central` clamped to `±(content·scale − panel.size()) / 2`, and
// that is the range that holds a content *centred on the panel's centre* and nothing
// else. A turn's content is displaced from the panel's centre by exactly what
// `spread_centre` reports - half the width difference horizontally, half the tallest
// page vertically because the turn is top-aligned - so the bound is the symmetric one
// **translated by minus that displacement**, and on a clean pair's horizontal axis it
// is the old bound exactly.
//
// Measured on the running app, on a two-page spread of a 2000-wide and a 2800-wide
// page (so the widths differ and the pair holds) over 40 zoom steps about a point on
// the partner's half: the symmetric bound truncated **36 of the 41 steps that still had
// room to pan**, by up to 67 points - 360 source pixels of the picture dragged
// somewhere the cursor is not. The composition test at the bottom of this section is
// that step in arithmetic, and it is the one that matters. The 5 truncations both
// builds share are at scales where the content still fits, where a pan would open a
// gap and refusing it is the point.

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
///   bound is `±(foot·scale − panel.size()) / 2` **bit for bit**. That is the case the
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
/// offsets are large. The bounds at 1.0 are `x (0, 300)` and `y (−600, 0)` against the
/// symmetric `±150` and `±300`.
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
/// `c_to = anchor − C(target)` and `c_from = anchor − C(current)` bracket the same
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
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
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
         section fixes: it is `±(content - panel) / 2` about the PANEL's centre, so in a spread\n\
         it truncates the zoom's anchor as soon as the zoom wants a pan the content's own\n\
         edges do not allow - 36 of 41 zoom steps on the running app, by up to 67 points.\n\
         Body was:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// The two settings
// ---------------------------------------------------------------------------

/// A config written before either manga key existed still loads, and gets the
/// values a reader of a manga library wants rather than the ones an absent field
/// would default to.
///
/// **The `#[serde(default)]` attributes are what this pins**, and both halves are
/// pinned because they are opposites: `manga_mode` must be **off**, because
/// single-image browsing is the app's original behaviour and every plan it makes
/// is pinned by `tests/flip_through.rs` — a config that turned a spread on by
/// omission would change that suite's meaning without changing a line of it. And
/// `manga_rtl` must be **on**, because a left-to-right reader who turns manga
/// mode on has asked for a manga reader and gets one; the order pages meet in is
/// the single most load-bearing thing about the feature and it is not a thing to
/// make someone find in the settings panel.
#[test]
fn a_config_written_before_manga_mode_still_loads() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.manga_mode, "manga mode is opt-in");
    assert!(cfg.manga_rtl, "right-to-left is the default for a manga library");
}

/// A leading **BOM** must not read as an empty file.
///
/// **The failure this pins is "every setting silently reverted".** `Set-Content
/// -Encoding utf8` in Windows PowerShell - and several editors' "save as UTF-8 with
/// signature" - writes `EF BB BF` before the first byte of JSON. `serde_json` does
/// not accept it: the parse fails, `Config::load` takes its error branch and returns
/// the **defaults**, and the app starts looking as though it had never been
/// configured. Nothing is printed that a user would read, the folder is forgotten,
/// every toggle is back where it was, and the file on disk is perfectly valid JSON -
/// so the obvious thing to try does not help.
///
/// Every field asserted is a bool, deliberately: `false` where the default is `true`
/// and `true` where the default is `false`, so a reader who deletes the whole assert
/// block still cannot mistake this for a defaults-are-correct test. Both manga keys
/// are in it, because those are the settings whose loss is hardest to explain.
///
/// The **premise is asserted too**, against `serde_json` directly. Without it this
/// test would pass on any future serde that learned to skip a BOM, and the strip in
/// `Config::load` would be reported as dead code by a test that never checked
/// whether anything needed stripping.
#[test]
fn a_bom_prefixed_config_still_loads_its_settings() {
    let body = format!("\u{feff}{}", CONFIG_WITH_EVERY_FLAG_FLIPPED);
    serde_json::from_str::<image_viewer::Config>(&body).expect_err(
        "serde_json now accepts a leading BOM. The strip in Config::load is still \
         correct and harmless, but this test no longer tests anything - say so rather \
         than leaving it green.",
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, &body).unwrap();

    let cfg = image_viewer::Config::load(&path);
    assert!(
        !cfg.load_all_on_startup,
        "load_all_on_startup: the BOM ate the file"
    );
    assert!(!cfg.linear_resample, "linear_resample");
    assert!(!cfg.loop_back, "loop_back");
    assert!(!cfg.thumbnail_cache, "thumbnail_cache");
    // These two default to *off*, so the fixture has them on: the assertion is
    // flipped from the ones above, which is the point - a defaults-are-correct
    // reader cannot skim past all nine.
    assert!(cfg.wheel_navigation, "wheel_navigation");
    assert!(cfg.convert_wide_gamut, "convert_wide_gamut");
    assert!(
        cfg.manga_mode,
        "manga_mode: the whole feature, back to its default"
    );
    assert!(
        !cfg.manga_rtl,
        "manga_rtl: and the reading direction with it"
    );

    // The same file without the BOM parses to the same thing, so the strip is the
    // only difference between them. This is what makes the assertions above about
    // the BOM rather than about the JSON.
    let plain = dir.path().join("plain.json");
    std::fs::write(&plain, CONFIG_WITH_EVERY_FLAG_FLIPPED).unwrap();
    let without = image_viewer::Config::load(&plain);
    assert_eq!(
        format!("{cfg:?}"),
        format!("{without:?}"),
        "a BOM-prefixed config and the same bytes without one must load identically"
    );
}

/// Every boolean field set to the *opposite* of its default.
///
/// `Config::default()` is spelled here rather than taken from the library, because
/// the point of this constant is that a reader can check it against `Config::load`'s
/// default arm with their eyes: `load_all_on_startup`, `linear_resample`,
/// `loop_back` and `thumbnail_cache` default to on and are off here;
/// `block_while_loading`, `wheel_navigation`, `convert_wide_gamut` and `manga_mode`
/// default to off and are on here; `manga_rtl` defaults to on and is off.
const CONFIG_WITH_EVERY_FLAG_FLIPPED: &str = r#"{
  "load_all_on_startup": false,
  "linear_resample": false,
  "block_while_loading": true,
  "wheel_navigation": true,
  "loop_back": false,
  "convert_wide_gamut": true,
  "thumbnail_cache": false,
  "manga_mode": true,
  "manga_rtl": false
}"#;

/// `spreads.json` needs the same strip as `config.json`, and it is **more**
/// dangerous with one.
///
/// Two reasons, and the second is why this is a separate test rather than a note on
/// the config one:
///
/// * A load that fails is treated as empty, so the reader loses their pairing
///   decisions for that gallery - visible, and fixable by deleting the file.
/// * **A save that fails is a read-modify-write against the file it could not read.**
///   `save_spread_overrides` parses the whole store and writes one gallery's entry
///   back into it; a parse failure means "empty", so the write replaces every other
///   gallery's entry with nothing. The decisions are not reverted for one session -
///   they are **deleted, permanently, with no error anywhere**.
///
/// That second one is why the store is *written by the library first* and then
/// prefixed with a BOM here: a fixture hand-written as `"\u{feff}{...}"` would pass
/// against a save that ignored its input entirely, because there would be nothing in
/// the file to preserve.
#[test]
fn a_bom_prefixed_spreads_store_does_not_cost_anybody_their_choices() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = dir.path().join("books");
    let page = root.join("003.png");

    let mut g = GallerySpreads::default();
    g.nudge_lead_lone(2);
    g.set_shape(&page, 1, Double);
    g.set_shape(&page, 5, Single);
    save_spread_overrides(&store, &root, &g).expect("a fresh directory is created");

    // A second gallery, so the save below has something to destroy.
    let other = dir.path().join("other-book");
    let mut h = GallerySpreads::default();
    h.set_shape(&other.join("cover.png"), 0, Double);
    h.set_shape(&other.join("cover.png"), 3, Single);
    save_spread_overrides(&store, &other, &h).expect("a second entry");

    // Now make the store BOM-prefixed the way a shell round trip would.
    let plain = std::fs::read_to_string(&store).unwrap();
    std::fs::write(&store, format!("\u{feff}{plain}")).unwrap();

    // Half one: the load honours it.
    assert_eq!(
        load_spread_overrides(&store, &root),
        g,
        "a BOM-prefixed store is not being read, so this gallery's pairing decisions are \
         gone for the session"
    );
    assert_eq!(
        load_spread_overrides(&store, &other).pages,
        h.pages,
        "the second gallery's too"
    );

    // Half two: a save against it keeps every other entry.
    let mut g2 = g.clone();
    g2.nudge_lead_lone(3);
    save_spread_overrides(&store, &root, &g2).expect("a save over a BOM-prefixed store");
    assert_eq!(
        load_spread_overrides(&store, &other),
        h,
        "the save read the store as empty and replaced it with one gallery's entry. Every \
         other gallery's decisions are deleted from disk, permanently and silently."
    );
    assert_eq!(
        load_spread_overrides(&store, &root).lead_lone,
        LEAD_LONE_DEFAULT + 3,
        "and the gallery that was saved kept its new value"
    );
}

// ---------------------------------------------------------------------------
// Task 7: navigating by turn
// ---------------------------------------------------------------------------
//
// Everything above answers "what does a turn look like?". This section answers
// "where does one press put the cursor" - which is the whole of manga mode's
// navigation, and the reason a turn needs no list of its own.
//
// **These run against a real `Gallery`**, not `TestGallery`, because the thing
// under test is the *cursor*: which `(file, page)` the walk leaves it on, and
// whether that is a page of the file it names. So every position is asserted as a
// `PagePos` and never as a flat ordinal.
//
// No filesystem is involved. Page counts come from `Gallery::set_page_counts` and
// shapes from `Gallery::note_shapes`, and the files are only names - which is also
// what makes every page here an unmeasured `Shape::Single`, the optimistic default
// the whole pairing is built to survive. So the turns are decided by the
// leading-lone offset, the ends of the gallery, and whichever page a test marks
// wide, and nothing else.
//
// The pieces of navigation that are *not* here are the ones in the binary: whether a
// step walks a turn or a page (`MyApp::step`), which anchor each direction asks for
// (`turn_forward`/`turn_backward`), and that the anchor is handed to
// `Gallery::goto_page` rather than to the file index alone (`goto_turn`). All four
// are pinned by source scans at the end of this section, for the reason
// `invalidate_path_drops_the_cached_dimensions` says.

/// A gallery of `counts.len()` files named in order, with those page counts and
/// nothing measured.
///
/// `gallery_of` does the three things needed and no more: install the list, set the
/// counts, put the cursor on the first page. Nothing here reads a file, so the
/// names need not exist - which is why a test about navigation cannot also fail
/// because a fixture was not written.
fn turns_gallery(counts: &[u32]) -> Gallery {
    let names: Vec<std::path::PathBuf> = (0..counts.len())
        .map(|i| std::path::PathBuf::from(format!("/vol/page{i:02}.jpg")))
        .collect();
    gallery_of(&names, counts)
}

/// Record that one page *displays* at a spread's proportions.
///
/// Through [`Gallery::note_shapes`] rather than by writing a file: that is the route
/// a real decode's dimensions take, and it is the only one that needs no bytes on
/// disk. It is fill-only, so it cannot overwrite a measurement - harmless here,
/// because these galleries have none.
///
/// 2000x1400 is 1.43, over [`SPREAD_ASPECT`](image_viewer::SPREAD_ASPECT) and a
/// long way from a real page's 0.71, so "wide" and "portrait" are not in the same
/// class by a hair.
fn mark_wide(gal: &mut Gallery, index: usize, page: u32) {
    gal.note_shapes(
        ImgKey {
            path: gal.files[index].clone(),
            page,
        },
        (2000, 1400),
    );
}

/// One step forwards, as `MyApp::turn_forward` performs it.
///
/// Two lines and no rule: which anchor the next turn opens on is [`next_turn`], and
/// the move is `Gallery::goto_page`. The app's copies of both are in the binary, and
/// the mode gate that chooses between this and `Gallery::next` is the source scan at
/// the end of this section.
fn step_forward(gal: &mut Gallery, lead_lone: u32) -> bool {
    let Some(to) = next_turn(gal.page_pos(), lead_lone, &*gal) else {
        return false;
    };
    gal.goto_page(to);
    true
}

/// One step backwards: [`prev_turn`] rather than [`prev_anchor`], for the same
/// reason `step_forward` uses [`next_anchor`] rather than `Gallery::next` - and
/// with a difference that matters, because `prev_anchor` stops at the first turn
/// whatever `loop_back` says while a backward *step* on a looping gallery wraps.
fn step_backward(gal: &mut Gallery, lead_lone: u32) -> bool {
    let Some(to) = prev_turn(gal.page_pos(), lead_lone, &*gal) else {
        return false;
    };
    gal.goto_page(to);
    true
}

/// Every position a turn-step lands on is a page that exists, in both directions.
///
/// The premise for the two claims made about this walk elsewhere: that a turn-step
/// cannot put the cursor on a page its file does not have, and that
/// [`Gallery::goto_page`] does not have to defend against one - it refuses a file
/// index past the end but trusts the page, so the walk's correctness *is* the page's
/// safety.
///
/// `tests/multiframe.rs` asserts the same property of `next`/`prev`; this is the
/// turn walk's version of it, over galleries that actually hold documents.
///
/// `forward` picks the direction, and the walk ends when a step returns `false` or
/// when a position repeats: with `loop_back` on a full round trip returns to the
/// anchor it started from, which is the end of the walk and not a failure. A walk
/// that stopped only on `false` would spin forever on a looping gallery, and a test
/// that hangs is worse than one that fails.
fn assert_turn_step_landings_exist(gal: &mut Gallery, lead_lone: u32, forward: bool) {
    for from in walk_start_positions(gal) {
        let mut gal = gal.clone();
        gal.goto_page(from);
        let mut seen = vec![gal.page_pos()];
        while if forward {
            step_forward(&mut gal, lead_lone)
        } else {
            step_backward(&mut gal, lead_lone)
        } {
            let at = gal.page_pos();
            if seen.contains(&at) {
                break;
            }
            assert!(
                at.index < gal.len(),
                "a step landed on file {} of a gallery of {} (from {from:?})",
                at.index,
                gal.len()
            );
            assert!(
                at.page < gal.pages_in_current(),
                "a step landed on page {} of a {}-page file: {at:?} (from {from:?})",
                at.page,
                gal.pages_in_current()
            );
            seen.push(at);
        }
    }
}

/// The positions a walk is started from: the first page, and **one second half from
/// the middle**.
///
/// The second half is the whole point, and it is here rather than in each test because
/// a cursor off an anchor is what three ways into the app produce - a grid pick leaves
/// `page` alone, a start-up or resume lands on page 0, and a `Del` keeps the freed
/// index. Paging forward to reach one would stop at the first anchor, so it is named as
/// a position instead: the third page of a gallery of at least three pages is a second
/// half whenever `lead_lone` is 0 (turns `{0,1} {2,3}...`) and the second page is one
/// whenever `lead_lone` is 1 (`{0} {1,2}...`), so both are covered by the two starts
/// below across the leads this section walks.
fn walk_start_positions(gal: &Gallery) -> Vec<PagePos> {
    let mut out = vec![PagePos { index: 0, page: 0 }];
    for n in [1usize, 2] {
        let mut seen = 0usize;
        for (index, _) in gal.files.iter().enumerate() {
            let pages = gal.pages_in(index);
            if n < seen + pages as usize {
                out.push(PagePos {
                    index,
                    page: (n - seen) as u32,
                });
                break;
            }
            seen += pages as usize;
        }
    }
    out
}

/// One press moves a whole turn: two pages, not one.
///
/// The whole difference between the two navigations, on the smallest fixture that
/// shows it. Eight single-page files with `lead_lone` 0 tile `{0,1} {2,3} {4,5}
/// {6,7}`, so the second anchor is page **2** - a file boundary, not page 1.
///
/// Page 1 is the interesting one to *not* land on: it is this turn's second half,
/// and every rule in the pairing is written on the promise that the cursor is never
/// there (`lone` answers "is this page alone?" wrongly at a non-anchor, which is
/// what the section header of `src/lib.rs` is about).
///
/// **Both `lead_lone` values, and the second half of this test is the app's default.**
/// At `lead_lone` 1 the tiling is `{0} {1,2} {3,4} {5,6} {7}` - the cover alone, which
/// is the only case in the whole section where the *first* turn is not a pair - so the
/// second anchor is page 1 rather than page 2 and the step still moves a whole turn.
#[test]
fn a_step_forward_advances_a_whole_turn() {
    let mut gal = turns_gallery(&[1; 8]);
    gal.set_loop_back(false);
    let lead = 0;

    // The premise, so a failure below cannot be a turn that was a pair of one for
    // some other reason.
    assert_eq!(turn_len(gal.page_pos(), lead, &gal), 2, "the first turn is a pair");

    assert!(step_forward(&mut gal, lead), "there is a second turn");
    assert_eq!(
        gal.page_pos(),
        PagePos { index: 2, page: 0 },
        "the second anchor - not page 1, which is this turn's own second half"
    );

    // The same gallery at the app's default lead_lone, where the cover is a turn of
    // one: the second anchor is page 1, and the step still moves a whole turn rather
    // than a page.
    let mut cover = turns_gallery(&[1; 8]);
    cover.set_loop_back(false);
    assert_eq!(
        turn_len(cover.page_pos(), LEAD_LONE_DEFAULT, &cover),
        1,
        "the premise: the cover goes alone at lead_lone 1"
    );
    assert!(step_forward(&mut cover, LEAD_LONE_DEFAULT), "there is a second turn");
    assert_eq!(
        cover.page_pos(),
        PagePos { index: 1, page: 0 },
        "which opens {{1,2}}: two pages, and page 2 is the turn's second half"
    );
    assert_eq!(
        turn_partner(cover.page_pos(), LEAD_LONE_DEFAULT, &cover),
        Some(ImgKey {
            path: cover.files[2].clone(),
            page: 0
        }),
        "and its other half is the third file: every file here is one page, so the \
         turn's second page is a page number of 0 in a different file"
    );
}

/// Back one turn, not back one page.
///
/// The assertion [`prev_anchor`] exists for. From the second anchor, the previous
/// page is page 1 - the second half of the turn *before* this one, which is the
/// position every rule in the pairing is written on the cursor never occupying.
/// Backwards there is no local test for that (see `prev_anchor`'s doc comment for
/// the three formulations that were tried and are all wrong), so the anchor walk is
/// the only way to answer it.
///
/// The default `lead_lone` case is here for the same reason it is in the forward test:
/// at 1 the anchors are `0,1,3,5,7`, so back from the second anchor is the lone cover
/// rather than a pair's first page.
#[test]
fn a_step_backward_returns_to_the_previous_turn_and_not_the_previous_page() {
    let mut gal = turns_gallery(&[1; 8]);
    gal.set_loop_back(false);
    let lead = 0;
    step_forward(&mut gal, lead);
    assert_eq!(gal.page_pos(), PagePos { index: 2, page: 0 }, "on the second anchor");

    assert!(step_backward(&mut gal, lead), "there is a turn before this one");
    assert_eq!(
        gal.page_pos(),
        PagePos { index: 0, page: 0 },
        "back to the anchor of the previous turn"
    );
    assert!(
        !step_backward(&mut gal, lead),
        "and no turn before the first one, with looping off"
    );
    assert_eq!(gal.page_pos(), PagePos { index: 0, page: 0 }, "which did not move the cursor");

    let mut cover = turns_gallery(&[1; 8]);
    cover.set_loop_back(false);
    step_forward(&mut cover, LEAD_LONE_DEFAULT);
    assert_eq!(cover.page_pos(), PagePos { index: 1, page: 0 }, "on {{1,2}}'s anchor");
    assert!(step_backward(&mut cover, LEAD_LONE_DEFAULT), "there is the cover before it");
    assert_eq!(
        cover.page_pos(),
        PagePos { index: 0, page: 0 },
        "which is a turn of one, so back one turn is back to page 0"
    );
}

/// A page that is itself a scan of two printed pages takes a whole turn, and
/// everything behind it re-pairs around it.
///
/// Flat page 4 is marked wide, so the tiling is `{0,1} {2,3} {4} {5,6} {7}`: page 3
/// pairs with page 2 rather than with the wide page - it cannot pair with a spread
/// at all - so that turn ends at page 3 and the wide page opens the one after it.
/// The two consequences are asserted separately because they are separate rules: the
/// wide page taking a turn of its own, and the page behind it no longer reaching
/// forward.
#[test]
fn a_wide_page_is_stepped_past_as_one_turn() {
    let mut gal = turns_gallery(&[1; 8]);
    gal.set_loop_back(false);
    let lead = 0;
    mark_wide(&mut gal, 4, 0);

    // The premise: the fixture really is a wide page, judged by the detector.
    // There is no override in this gallery - `mark_wide` writes the shape cache,
    // and `shape_of` reads `override_shape` first, so an override would be a
    // different route to the same verdict.
    let wide = PagePos { index: 4, page: 0 };
    assert_eq!(shape_of(wide, &gal), Double, "page 4 is a scan of two printed pages");

    let mut anchors = vec![gal.page_pos()];
    while step_forward(&mut gal, lead) {
        anchors.push(gal.page_pos());
    }
    assert_eq!(
        anchors,
        vec![
            PagePos { index: 0, page: 0 },
            PagePos { index: 2, page: 0 },
            PagePos { index: 4, page: 0 },
            PagePos { index: 5, page: 0 },
            PagePos { index: 7, page: 0 },
        ],
        "the whole tiling: {{0,1}} {{2,3}} {{4}} {{5,6}} {{7}}"
    );
    assert_eq!(
        anchors.iter().filter(|p| **p == wide).count(),
        1,
        "the wide page is visited once, as a turn of its own"
    );
    assert!(
        !anchors.contains(&PagePos { index: 3, page: 0 }),
        "and the cursor never rests on page 3, which is {{2,3}}'s second half"
    );
    assert_eq!(turn_len(wide, lead, &gal), 1, "so the wide page's turn holds one page");
    assert_eq!(turn_partner(wide, lead, &gal), None, "and it has no other half");

    // The same two facts as a step, because a tiling that is right and a cursor
    // that does not follow it are separate failures: from the turn before it, one
    // step lands on the wide page rather than on its own second half; from the wide
    // page, one step lands on 5 rather than staying or going back to 3.
    gal.goto_index(2);
    assert!(step_forward(&mut gal, lead));
    assert_eq!(gal.page_pos(), wide, "stepped over page 3 onto the wide page");
    assert!(step_forward(&mut gal, lead));
    assert_eq!(
        gal.page_pos(),
        PagePos { index: 5, page: 0 },
        "and on past it, having taken a whole turn"
    );
}

/// A full round of turns returns to where it started - forwards and back.
///
/// The wrap is the seam this design puts in one place: `lone` asks for a partner
/// with `step_no_wrap`, so a turn never straddles the seam and the last turn of a
/// looping gallery is the same turn the gallery would have with looping off. What
/// the round trip then needs is *both* directions to agree about the tiling, and
/// `prev_anchor` is a rebuild rather than a lookup - so if it disagreed with
/// `next_anchor` anywhere, this is where it shows.
///
/// Eight pages are four turns, and **the walks stop on the anchor they started
/// from rather than on a refusal** - a looping gallery never refuses, so a walk
/// that waited for `false` would go round for ever. Four forwards is a full round:
/// three steps reach the last turn and the fourth is the wrap.
///
/// **Both `lead_lone` values, with the whole anchor list spelled out for each.** At
/// the app's default of 1 the tiling is `{0} {1,2} {3,4} {5,6} {7}` - five turns, the
/// lone cover included - so the round is five steps rather than four, and the two
/// walks have to agree on *that* count as well as on the tiling. Pinning the count per
/// lead is the point: a walk that agreed with itself about the wrong number of turns
/// would still come back to the start.
#[test]
fn navigating_a_full_round_returns_to_the_start() {
    // `loop_back` on, which is what `Gallery::from_file` leaves: the setting under
    // test is that a step wraps, and the alternative would be
    // `nothing_moves_with_looping_off_at_the_end`'s case.
    for (lead, anchors, last_anchor_pos) in [
        (
            0u32,
            vec![PagePos { index: 0, page: 0 }, PagePos { index: 2, page: 0 },
                 PagePos { index: 4, page: 0 }, PagePos { index: 6, page: 0 }],
            PagePos { index: 6, page: 0 },
        ),
        (
            LEAD_LONE_DEFAULT,
            vec![PagePos { index: 0, page: 0 }, PagePos { index: 1, page: 0 },
                 PagePos { index: 3, page: 0 }, PagePos { index: 5, page: 0 },
                 PagePos { index: 7, page: 0 }],
            PagePos { index: 7, page: 0 },
        ),
    ] {
        let mut gal = turns_gallery(&[1; 8]);
        assert!(gal.loop_back, "the premise: looping is on");
        let start = gal.page_pos();
        let rounds = anchors.len();

        // The anchor list itself, reached by stepping - one fewer step than a round,
        // because the last step of the round *is* the wrap.
        let mut walked = vec![gal.page_pos()];
        for _ in 1..rounds {
            assert!(step_forward(&mut gal, lead), "lead {lead}: there is a turn after each anchor");
            walked.push(gal.page_pos());
        }
        assert_eq!(walked, anchors, "lead {lead}: the anchors in reading order");
        // Back to the front for the round trip: the walk above has left the cursor on
        // the last anchor, and the round is measured from the first.
        gal.goto_index(0);
        assert_eq!(gal.page_pos(), start, "lead {lead}: back at the first turn");

        // At least one step: the cursor starts *on* `start`, so the walk is a do-while
        // rather than a while.
        let mut forwards = 0usize;
        loop {
            assert!(step_forward(&mut gal, lead), "a looping gallery never runs out of turns");
            forwards += 1;
            assert!(forwards <= rounds, "lead {lead}: the forward walk does not come back round");
            if gal.page_pos() == start {
                break;
            }
        }
        assert_eq!(forwards, rounds, "lead {lead}: one step per turn, the last being the wrap");

        let mut backwards = 0usize;
        loop {
            assert!(step_backward(&mut gal, lead), "a looping gallery never runs out of turns");
            backwards += 1;
            assert!(backwards <= rounds, "lead {lead}: the backward walk does not come back round");
            if gal.page_pos() == start {
                break;
            }
        }
        assert_eq!(backwards, forwards, "lead {lead}: as many turns backwards as forwards");
        assert_eq!(
            gal.page_pos(),
            start,
            "lead {lead}: so a full round trip is a round trip, which is what makes looping usable"
        );

        // The turn both walks met at the far end is the same one either way round the
        // gallery, which is the property `last_anchor` exists to preserve: `lone` asks
        // for a partner with `step_no_wrap`, so a looping gallery tiles exactly as a
        // non-looping one does. A `last_anchor` that consulted the wrap would make the
        // last turn depend on `loop_back` and put the gallery's last page beside its
        // first.
        let mut unlooped = turns_gallery(&[1; 8]);
        unlooped.set_loop_back(false);
        assert_eq!(last_anchor(lead, &unlooped), Some(last_anchor_pos), "lead {lead}");
        assert_eq!(last_anchor(lead, &gal), Some(last_anchor_pos), "lead {lead}, looping");
    }
}

/// `last_anchor`'s other three fixtures, and the `None` its doc comment promises.
///
/// The eight-single-page gallery above is one case; this is an empty gallery, a
/// multi-page document, and a turn of two at the very end - the last three shapes
/// where the answer could be a page that is not on screen. **`prev_turn` calls this on
/// the keypress path**, on every backward step off the first turn of a looping gallery,
/// so "it answers for a gallery with nothing in it" is a claim about the hot path and
/// not about a corner nobody reaches.
///
/// The empty gallery is the one that matters most: its answer is `None` because the
/// walk asks `step_no_wrap` whether page 0 exists rather than starting there, and a
/// walk that started there would answer `Some((0, 0))` - a position that is not on
/// screen, for a gallery with nothing on it.
#[test]
fn the_last_turn_is_the_same_with_looping_on_and_off_and_an_empty_gallery_has_none() {
    // An empty gallery, from both ends of the API that reaches for it.
    let empty = Gallery::empty();
    assert!(empty.is_empty(), "the premise: nothing is in this gallery");
    assert_eq!(last_anchor(0, &empty), None, "no gallery, no last turn");
    assert_eq!(last_anchor(LEAD_LONE_DEFAULT, &empty), None, "whatever the lead_lone");
    assert_eq!(
        prev_turn(PagePos { index: 0, page: 0 }, 0, &empty),
        None,
        "and no backward step either, rather than a panic or a move"
    );

    for counts in [vec![3u32, 1], vec![1, 4], vec![2, 2], vec![1, 1, 1]] {
        let lead = 0;
        let mut unlooped = turns_gallery(&counts);
        unlooped.set_loop_back(false);
        // The expected answer, read off the flat sequence: the anchor of the last turn
        // is the last page, or the page before it when that pair holds.
        let flat: usize = counts.iter().map(|c| *c as usize).sum();
        let expected = if flat % 2 == 0 {
            flat_position(&unlooped, flat - 2)
        } else {
            flat_position(&unlooped, flat - 1)
        };
        assert_eq!(
            last_anchor(lead, &unlooped),
            Some(expected),
            "{counts:?}: the last turn opens on {expected:?}"
        );
        // The whole point of the `step_no_wrap` walk: the answer does not depend on
        // `loop_back`, or the last turn would straddle the seam.
        let mut looping = turns_gallery(&counts);
        looping.set_loop_back(true);
        assert_eq!(
            last_anchor(lead, &looping),
            Some(expected),
            "{counts:?}: a looping gallery's last turn is the same one"
        );
        // And the turn it names really does reach the gallery's last page, which is
        // what "last" has to mean for a wrap to land somewhere sensible: either it is
        // that page, or it is the one before it in a pair.
        let last = last_anchor(lead, &unlooped).unwrap();
        let final_page = flat_position(&unlooped, flat - 1);
        assert!(
            last == final_page || PageCtx::step_no_wrap(&unlooped, last, 1) == Some(final_page),
            "{counts:?}: the last turn {last:?} does not reach page {flat} ({final_page:?})"
        );
    }
}

/// A turn is not a file, so the walk crosses a document's page boundary the same
/// way `Gallery::next` does.
///
/// A three-page document followed by an ordinary file. The second turn is
/// `{page 2 of the document, page 0 of the next file}` - so one press lands on a
/// page inside the document and the turn beside it is a page of another file, and
/// the key the planner gets names that second file's **first** page rather than
/// `ImgKey::first` of the wrong file.
///
/// The contrast assertion is the point: [`PageCtx::step`] by one page does get to
/// page 0 of file 1, and that is exactly the page a per-page step would stop the
/// cursor on.
#[test]
fn a_multi_page_document_is_navigated_by_turn_too() {
    let mut gal = turns_gallery(&[3, 1]);
    gal.set_loop_back(false);
    let lead = 0;

    // The premise: three pages then one, which is four pages and so two turns.
    assert_eq!(gal.pages_in_current(), 3);
    assert_eq!(gal.len(), 2);

    assert!(step_forward(&mut gal, lead), "there is a second turn");
    assert_eq!(
        gal.page_pos(),
        PagePos { index: 0, page: 2 },
        "the second turn opens on the document's last page"
    );
    assert_eq!(
        turn_partner(gal.page_pos(), lead, &gal),
        Some(ImgKey {
            path: gal.files[1].clone(),
            page: 0
        }),
        "and its other half is the next file's first page"
    );
    assert_eq!(
        PageCtx::step(&gal, gal.page_pos(), 1),
        Some(PagePos { index: 1, page: 0 }),
        "which is also the page a per-page step would have stopped on - the difference"
    );

    assert!(!step_forward(&mut gal, lead), "the second turn is the last one");
    assert!(step_backward(&mut gal, lead), "and there is one turn before it");
    assert_eq!(gal.page_pos(), PagePos { index: 0, page: 0 });
}

/// Deleting the page a turn opens on leaves its partner, and the partner becomes a
/// turn of one rather than a stranded half.
///
/// The delete itself is in the binary - `MyApp::remove_current_from_gallery` - so
/// what is asserted here is the *pairing* consequence, which is the half with a
/// wrong answer that looks right. Dropping both pages of a pair also leaves a
/// perfectly valid gallery: one page short of the book, with nothing on screen
/// saying a page is missing. Dropping one leaves the reader with the page they
/// were turning to, alone, which is the answer a book gives you.
///
/// So the fixture is two single-page files - the smallest gallery where "the
/// partner is left alone on screen" and "there is nothing left at all" are different
/// answers - and the test asserts the file list as well as the turn length, because
/// `turn_len == 1` is also what a gallery emptied of both pages would report.
///
/// **What this models is a `Del` on a whole file, and only because every page in the
/// fixture is page 0.** `MyApp::remove_current_from_gallery` keeps `gallery.page`, so a
/// delete *inside* a document leaves a page number the file that slides into the freed
/// index may not have - which is `Gallery::learn_pages`' clamp in `goto` to repair, and
/// `MyApp::snap_to_turn_anchor`'s snap on top of that. Neither is in the model here, so
/// this test says nothing about a `Del` on page 3 of a four-page document.
#[test]
fn removing_the_cursor_page_leaves_its_partner_a_turn_of_one() {
    let mut gal = turns_gallery(&[1, 1]);
    gal.set_loop_back(false);
    let lead = 0;
    let partner = gal.files[1].clone();

    // The premise in both halves: the cursor is on an anchor, and its turn is a
    // pair whose other half is the second file.
    assert_eq!(turn_len(gal.page_pos(), lead, &gal), 2, "the cursor's turn has two pages");
    assert_eq!(
        turn_partner(gal.page_pos(), lead, &gal),
        Some(ImgKey {
            path: partner.clone(),
            page: 0
        }),
        "and the partner is the other file's first page"
    );

    // What the app does to the gallery, in the same order: forget the departing
    // file's counts and dimensions (keyed on paths, so they have to go with it),
    // drop it from the list, and leave the cursor on the index it was on - which is
    // now the partner.
    let victim = gal.files[0].clone();
    gal.forget_page_count(&victim);
    gal.forget_shapes(&victim);
    gal.files.remove(0);
    gal.index = 0;

    assert_eq!(gal.len(), 1, "one file was deleted, not the whole turn");
    assert_eq!(gal.files[0], partner, "and the one left is the partner");
    assert_eq!(
        turn_len(gal.page_pos(), lead, &gal),
        1,
        "so it takes a turn of its own: alone on screen, not stranded"
    );
    assert_eq!(
        turn_partner(gal.page_pos(), lead, &gal),
        None,
        "with nothing left to pair with"
    );
    assert!(
        !step_forward(&mut gal, lead),
        "and one page left is one turn, so with looping off there is no next turn either"
    );
}

/// Review Focus 5 at the navigation layer: with looping off, a step at either end
/// goes nowhere, and does not move the cursor.
///
/// `Gallery::next`/`prev` have answered `false` at the ends since pages existed, and
/// the turn walk could easily have lost it: `prev_anchor` answers `None` at the
/// first turn **whatever `loop_back` says**, so the wrap has to be a second,
/// separate decision - and getting that decision the wrong way round shows up as
/// pressing left on the cover and landing somewhere in the middle of the book.
///
/// The refused step is checked to leave the cursor where it was, which is the other
/// half: `MyApp::step` skips `goto` on a refusal, so that the full-res dwell timer
/// and the queue plan are not reset for an image that did not change.
///
/// **Both `lead_lone` values**, because the last turn is a different turn under each:
/// at 1 the final page is alone (`{0} {1,2} {3,4} {5,6} {7}`) and at 0 the last two
/// pages are a pair, so "the end" is two different positions and a refusal has to hold
/// at both.
#[test]
fn nothing_moves_with_looping_off_at_the_end() {
    for (lead, steps_to_the_end, last) in [
        (0u32, 3usize, PagePos { index: 6, page: 0 }),
        (LEAD_LONE_DEFAULT, 4usize, PagePos { index: 7, page: 0 }),
    ] {
        let mut gal = turns_gallery(&[1; 8]);
        gal.set_loop_back(false);

        let mut forwards = 0usize;
        while step_forward(&mut gal, lead) {
            forwards += 1;
            assert!(forwards <= 8, "lead {lead}: the forward walk does not terminate");
        }
        assert_eq!(
            forwards, steps_to_the_end,
            "lead {lead}: one step fewer than the turn count, since the first turn is where it starts"
        );
        assert_eq!(gal.page_pos(), last, "lead {lead}: the last turn opens here");
        assert!(
            !step_forward(&mut gal, lead),
            "lead {lead}: and there is nothing after it"
        );
        assert_eq!(gal.page_pos(), last, "lead {lead}: a refused step does not move the cursor");

        let mut backwards = 0usize;
        while step_backward(&mut gal, lead) {
            backwards += 1;
            assert!(backwards <= 8, "lead {lead}: the backward walk does not terminate");
        }
        assert_eq!(backwards, steps_to_the_end, "lead {lead}: back to the first turn");
        assert_eq!(gal.page_pos(), PagePos { index: 0, page: 0 }, "lead {lead}");
        assert!(!step_backward(&mut gal, lead), "lead {lead}: and nothing before it");
        assert_eq!(
            gal.page_pos(),
            PagePos { index: 0, page: 0 },
            "lead {lead}: which did not move it either"
        );
    }
}

/// The bug this section's second round of tests exists for: a backward step from a
/// **second half** landed on the last spread of the volume.
///
/// `prev_anchor` answers `None` for two different reasons - the first turn, and a
/// position that is not an anchor - and reading it as "at the front" sent the cursor
/// to `last_anchor` instead. At the app's default `lead_lone` of 1 an ordinary folder
/// of eight single pages tiles `{0} {1,2} {3,4} {5,6} {7}`, so **the third file is
/// page 2, the second half of `{1,2}`**, and three ways into the app put the cursor
/// there without asking: a grid pick leaves `page` alone, a start-up or resume lands on
/// page 0 of the file it resumed, and a `Del` keeps the freed index. Left from there
/// jumped to page 7 with `loop_back` on **and** off.
///
/// The premise is asserted rather than assumed - `prev_anchor` really does answer
/// `None` for this position, and `anchor_of` really does say it is not an anchor -
/// because a test that only checked where the cursor went would pass against a
/// `prev_anchor` that answered `Some` and simply gave a different wrong answer.
#[test]
fn a_backward_step_from_a_second_half_does_not_jump_to_the_last_spread() {
    let lead = LEAD_LONE_DEFAULT;
    for looping in [true, false] {
        let mut gal = turns_gallery(&[1; 8]);
        gal.set_loop_back(looping);
        gal.goto_index(2);

        // The premise: page 2 is a second half at the default lead_lone, and
        // `prev_anchor` cannot tell that from "at the front".
        let second_half = PagePos { index: 2, page: 0 };
        assert_eq!(
            anchor_of(second_half, lead, &gal),
            PagePos { index: 1, page: 0 },
            "the turn page 2 is on opens on page 1"
        );
        assert_ne!(anchor_of(second_half, lead, &gal), second_half, "so page 2 is not an anchor");
        assert!(
            prev_anchor(second_half, lead, &gal).is_none(),
            "and prev_anchor answers None for it - the same answer it gives at the front"
        );
        assert_eq!(turn_len(anchor_of(second_half, lead, &gal), lead, &gal), 2, "which is a pair");

        assert!(step_backward(&mut gal, lead), "a backward step from a second half moves");
        assert_ne!(
            gal.page_pos(),
            PagePos { index: 7, page: 0 },
            "looping {looping}: Left from the second half must not land on the last spread"
        );
        assert_eq!(
            gal.page_pos(),
            PagePos { index: 0, page: 0 },
            "looping {looping}: it lands on the turn before {{1,2}}, which is {{0}} alone"
        );
    }
}

/// A backward step from the **second half of the first turn** still reaches the wrap.
///
/// The one case `prev_anchor` cannot answer and neither can `anchor_of` alone: the
/// cursor is on page 1 of a two-page first turn, so there is no turn before it *and*
/// the position is not an anchor. Resolving the turn with `anchor_of` gets as far as
/// the first anchor, whose predecessor is also nothing - so the wrap question still has
/// to be asked, and the answer has to be the gallery's last turn rather than "nowhere".
///
/// On this fixture the last turn *is* the first, so the walk comes back to where it
/// started: which is exactly what `Gallery::prev` does from the same position (it goes
/// back one page, inside the turn), and what a reader pressing Left twice in a row
/// expects. With looping off there is nothing to wrap to and nothing moves.
///
/// **Written after a mutation came back green.** `prev_turn`'s second branch was
/// mutated to `return None` - refusing rather than falling through to the wrap - and
/// every test in the file passed, because the proptest below skips positions that do
/// not move and the two walk tests all start at an anchor. A guard that cannot see a
/// step *not happening* is not a guard.
#[test]
fn a_backward_step_from_the_first_turns_second_half_still_wraps() {
    let lead = 0;

    let mut looping = turns_gallery(&[1, 1]);
    looping.set_loop_back(true);
    looping.goto_index(1);
    // The premise: page 1 is the first turn's second half, so neither of the two
    // `None`-disambiguating answers comes back on its own.
    let second_half = PagePos { index: 1, page: 0 };
    assert_ne!(anchor_of(second_half, lead, &looping), second_half, "it is a second half");
    assert_eq!(anchor_of(second_half, lead, &looping), PagePos { index: 0, page: 0 });
    assert!(prev_anchor(second_half, lead, &looping).is_none(), "and there is no anchor before it");
    assert!(prev_anchor(PagePos { index: 0, page: 0 }, lead, &looping).is_none(), "nor before the first turn");

    assert!(step_backward(&mut looping, lead), "looping on: a backward step still moves");
    assert_eq!(
        looping.page_pos(),
        PagePos { index: 0, page: 0 },
        "looping on: to the first page of the only turn, as Gallery::prev would"
    );

    // And with looping off the same position refuses, which is the other half of the
    // wrap decision: `loop_back` is the only thing that separates these two answers.
    let mut stopped = turns_gallery(&[1, 1]);
    stopped.set_loop_back(false);
    stopped.goto_index(1);
    assert!(!step_backward(&mut stopped, lead), "looping off: there is nowhere to go");
    assert_eq!(stopped.page_pos(), second_half, "looping off: and the cursor does not move");
}

/// The same trap in the forward direction: a step from a second half lands where the
/// same step from that turn's anchor lands.
///
/// Quieter than the backward case, which is why it needs its own test. `turn_len` at a
/// second half is *that page's* answer - it looks for a partner after itself and finds
/// one - so `next_anchor` accepts it and steps by two, which from `{0,1}`'s page 1
/// lands on page 3 where the turn on screen is `{0,1}` and the next turn opens on page
/// 2. Nothing crashes, every position exists, and the reader skips a spread per press.
///
/// Stated as agreement with the step from the anchor rather than as a spelled-out
/// position, so the same fixture works at either `lead_lone`.
#[test]
fn a_forward_step_from_a_second_half_lands_where_its_anchor_would() {
    // (lead_lone, the second half to start from, the turn after it opens on page)
    for (lead, from, to) in [(0u32, 1usize, 2usize), (LEAD_LONE_DEFAULT, 2usize, 3usize)] {
        let mut gal = turns_gallery(&[1; 8]);
        gal.set_loop_back(false);
        gal.goto_index(from);
        assert_ne!(
            anchor_of(gal.page_pos(), lead, &gal),
            gal.page_pos(),
            "lead {lead}: page {from} is a second half"
        );

        assert!(step_forward(&mut gal, lead), "lead {lead}: there is a next turn");
        assert_eq!(
            gal.page_pos(),
            PagePos { index: to, page: 0 },
            "lead {lead}: a step from a second half lands on the turn after its own, not a page further"
        );
        assert_eq!(
            anchor_of(gal.page_pos(), lead, &gal),
            gal.page_pos(),
            "lead {lead}: and it lands on an anchor, which is what the cursor needs"
        );
    }
}

/// `anchor_of` is total and idempotent: it answers an anchor, for every position in
/// the gallery, and that anchor is the one whose turn holds the position.
///
/// **Both halves, because either alone is half a claim.** A function that returned
/// *some* anchor would satisfy "always an anchor" while pointing at a turn the reader
/// is not on - which is the failure `prev_anchor`'s `None` caused, one function over.
/// And idempotence is what makes it safe to call defensively: `anchor_of(anchor_of(x))
/// == anchor_of(x)` says a second snap cannot move the cursor again, so a caller that
/// snaps twice is not a caller with a new bug.
///
/// Checked over three gallery shapes and both `lead_lone` values, because the lone
/// front page at `lead_lone` 1 is the only case where the *first* anchor is not the
/// first pair, and a document in the middle is the only case where the turn holding a
/// position is in another file.
#[test]
fn every_position_is_on_the_turn_its_anchor_opens() {
    for counts in [vec![1u32; 8], vec![3, 1, 1], vec![1, 4]] {
        for lead in [0, LEAD_LONE_DEFAULT] {
            let gal = turns_gallery(&counts);
            let flat: usize = counts.iter().map(|c| *c as usize).sum();
            for n in 0..flat {
                let pos = flat_position(&gal, n);
                let at = anchor_of(pos, lead, &gal);

                assert_eq!(
                    anchor_of(at, lead, &gal),
                    at,
                    "not idempotent at {pos:?} (lead {lead}, {counts:?}): {at:?}"
                );
                // `pos` is in `at`'s turn: either it is the anchor, or it is the
                // second half of a turn of two. Stated through `turn_partner` and the
                // successor rather than through `turn_len` alone, because
                // `turn_len(anchor) == 2` alone would also be satisfied by the
                // neighbouring page, which is what makes a wrong anchor invisible.
                let holds = pos == at
                    || (turn_len(at, lead, &gal) == 2 && PageCtx::step_no_wrap(&gal, at, 1) == Some(pos));
                assert!(
                    holds,
                    "{pos:?} (lead {lead}, {counts:?}) is not in the turn anchored at {at:?}"
                );
            }
        }
    }
}

/// Front matter is front matter even when the file in front of it has an unlearned
/// page count.
///
/// **The case the old spelling of this rule got wrong, and it is a pairing, not a
/// detail.** `lone`'s first arm used to be `ctx.flat(anchor).is_some_and(|f| f <
/// lead_lone)`, and `Gallery::flat` answers `None` whenever a *preceding* file's page
/// count has not been learned - so the `is_some_and` guard turned "I cannot tell you
/// this page's ordinal" into "this page is not front matter". Three loose images with no
/// counts learned and `lead_lone` 2: page 2 of the flat sequence is the second page, so
/// it is front matter and opens alone, and the old rule paired it with page 3 instead.
///
/// The tiling is asserted rather than the predicate, because the predicate is one arm
/// of three and the pairing is what a reader sees: `{0} {1} {2,3} {4,5} {6}` rather than
/// `{0} {1,2} {3,4} {5,6}`.
///
/// The state is transient in the app - `goto` learns the page count of the file it
/// arrives at, so by the time the cursor is on file 2 file 1 has been through it - which
/// is exactly why a rule that degrades quietly in this state is worth pinning. What the
/// step-based test asks is a question the page arithmetic can always answer, so the
/// unknown count no longer reaches the answer at all.
#[test]
fn a_front_page_behind_an_unlearned_page_count_is_still_front_matter() {
    // No `set_page_counts`: every page count is unknown, which is the state a gallery
    // is in before anything has been opened.
    let mut gal = Gallery::from_file(std::path::PathBuf::from("/vol/000.jpg"));
    gal.files = (0..7).map(|i| std::path::PathBuf::from(format!("/vol/{i:03}.jpg"))).collect();
    assert_eq!(gal.known_page_counts(), 0, "the premise: nothing is known about any file");
    let lead = 2;

    // The premise, stated as the failure it prevents: `flat` really does refuse.
    assert_eq!(PageCtx::flat(&gal, PagePos { index: 1, page: 0 }), None);
    // ...and the page is the second of the flat sequence under the counts as they stand.
    assert_eq!(PageCtx::step_no_wrap(&gal, PagePos { index: 1, page: 0 }, -2), None);

    assert!(lone(PagePos { index: 1, page: 0 }, lead, &gal), "the second page is front matter");
    assert!(lone(PagePos { index: 0, page: 0 }, lead, &gal), "and so is the first");

    // The tiling, which is the user-visible half. Seven loose images at lead_lone 2.
    let mut anchors = vec![PagePos { index: 0, page: 0 }];
    let mut cur = anchors[0];
    while let Some(next) = image_viewer::next_anchor(cur, lead, &gal) {
        if anchors.contains(&next) {
            break;
        }
        anchors.push(next);
        cur = next;
    }
    assert_eq!(
        anchors,
        vec![
            PagePos { index: 0, page: 0 },
            PagePos { index: 1, page: 0 },
            PagePos { index: 2, page: 0 },
            PagePos { index: 4, page: 0 },
            PagePos { index: 6, page: 0 },
        ],
        "two pages open alone and the rest pair: {{0}} {{1}} {{2,3}} {{4,5}} {{6}}"
    );
    assert_eq!(
        turn_partner(PagePos { index: 1, page: 0 }, lead, &gal),
        None,
        "so the second page has no partner, which is what 'front matter' means here"
    );
}
fn flat_position(gal: &Gallery, n: usize) -> PagePos {
    let mut seen = 0usize;
    for (index, _) in gal.files.iter().enumerate() {
        let pages = gal.pages_in(index) as usize;
        if n < seen + pages {
            return PagePos {
                index,
                page: (n - seen) as u32,
            };
        }
        seen += pages;
    }
    panic!("page {n} does not exist in a gallery of {seen}");
}

/// Every landing of a turn-step is a position that exists: a file in the list and a
/// page of it.
///
/// **Written after the `Del`-on-a-second-half bug, so the walk is driven from
/// positions that are not anchors on purpose.** The walk a cursor takes in manga mode
/// starts at an anchor, so a test that only ever starts there would never have caught
/// the bug this file now pins in
/// [`a_backward_step_from_a_second_half_does_not_jump_to_the_last_spread`] - and the
/// cursor in the app is not always on one, for three reasons [`MyApp::goto`] now
/// handles. Every shape below is walked from a mid-gallery second half as well as
/// from the front.
///
/// Four gallery shapes - all single pages, a document at the front, a document at the
/// back, and two documents of different lengths - in both directions, with
/// looping on and off. The shapes are chosen so a walk crosses a file boundary, a
/// page boundary and the end of the gallery, which is where a page number past the
/// end would come from.
///
/// **This is the property `goto_page` deliberately does not enforce for itself**,
/// which is the reason it is asserted rather than assumed - see
/// [`goto_page_refuses_a_file_that_is_not_there_and_trusts_the_page`].
///
/// Four gallery shapes - all single pages, a document at the front, a document at
/// the back, and two documents of different lengths - in both directions, with
/// looping on and off. The shapes are chosen so a walk crosses a file boundary, a
/// page boundary and the end of the gallery, which is where a page number past the
/// end would come from.
///
/// **This is the property `goto_page` deliberately does not enforce for itself**,
/// which is the reason it is asserted rather than assumed - see
/// [`goto_page_refuses_a_file_that_is_not_there_and_trusts_the_page`].
#[test]
fn every_turn_step_lands_on_a_position_that_exists() {
    for counts in [
        vec![1u32, 1, 1, 1, 1, 1, 1, 1],
        vec![3, 1, 1],
        vec![1, 4],
        vec![2, 3, 1],
    ] {
        for looping in [true, false] {
            let mut gal = turns_gallery(&counts);
            gal.set_loop_back(looping);
            for forward in [true, false] {
                assert_turn_step_landings_exist(&mut gal, 0, forward);
            }
        }
    }
}

/// `goto_page` is the low-level setter: it refuses a file that is not in the list,
/// and takes the page number on trust.
///
/// **Both halves are deliberate and the second one is the surprising one.** The
/// bounds check on the index is the one guard here, and it is for a caller holding
/// a stale position - nothing in the turn walk can produce one, which
/// [`every_turn_step_lands_on_a_position_that_exists`] checks rather than claims.
/// The page is **not** clamped to `pages_in_current`, and the assertion below puts
/// the cursor on page 4 of a one-page file to say so.
///
/// **Why that is right, and why it needs a test.** Every position the walk produces
/// came out of a `PageCtx` step, which already respects the page count, so a clamp
/// here would be a second rule that silently disagrees with the walk's - and the
/// disagreement would not be visible in a photo folder, where every file is one
/// page and the clamp never fires. Pinning the absence of it is the only way to stop
/// a well-meaning `min` from going in: with a clamp, every other test in this file
/// still passes (checked by mutation), because no walk ever asks for a page past the
/// end.
#[test]
fn goto_page_refuses_a_file_that_is_not_there_and_trusts_the_page() {
    let mut gal = turns_gallery(&[1, 1]);
    gal.goto_index(1);
    assert_eq!(gal.page_pos(), PagePos { index: 1, page: 0 });

    // A file index past the end of the list: refused, and the cursor stays put.
    gal.goto_page(PagePos { index: 9, page: 0 });
    assert_eq!(gal.page_pos(), PagePos { index: 1, page: 0 });

    // The same call with a file that does exist is taken, so the guard is refusing
    // the *file* rather than ignoring the call.
    gal.goto_page(PagePos { index: 0, page: 0 });
    assert_eq!(gal.page_pos(), PagePos { index: 0, page: 0 });

    // A page past the end of the file is **taken**, not clamped: the walk already
    // respected the count, and a clamp here would be a second rule.
    gal.goto_page(PagePos { index: 1, page: 4 });
    assert_eq!(
        gal.page_pos(),
        PagePos { index: 1, page: 4 },
        "goto_page does not clamp the page - only the file index"
    );
    // Which is a position the walk would never hand it: the page count of file 1 is
    // one. Checked here so the assertion above cannot pass on a fixture whose count
    // happened to be four.
    assert_eq!(gal.pages_in_current(), 1, "and page 4 of it does not exist");
}

/// ...and the three pieces of wiring that make a step walk a turn live in the
/// binary, so these are source scans for the reason
/// [`invalidate_path_drops_the_cached_dimensions`] is: nothing else in the tree can
/// see that `step` consults `manga_mode` at all, or that the two walks hand the
/// anchor to `Gallery::goto_page` rather than to the file index alone. All three
/// would leave every behavioural test in this file green - they are the same code
/// the library tests above re-implement in two lines.
///
/// They are weak in the way that scan is weak - they cannot tell whether `step` is
/// ever called, nor what `goto_turn` does with a refusal - and they catch the
/// regressions that actually happen, which are the mode check or one of the calls
/// being edited out of a function that still compiles and still browses.
///
/// Scoped by [`fn_body`] to each function's own braces, and every string listed is
/// load-bearing:
///
/// * `manga_mode` and both `turn_*()` calls in `step`, because a half-wired `step` -
///   forward through turns, backward through pages - is exactly the kind of thing
///   that reads correctly in a photo folder, where every page is a turn of one.
/// * `Gallery::next`/`prev` in `step`, for the other half of the same question: with
///   manga mode off, navigation must still be the per-page one `tests/flip_through.rs`
///   pins.
/// * `next_turn`/`prev_turn` and `goto_page` in the two walks, because a walk that
///   computed the right anchor and then assigned `gallery.index` instead of moving the
///   cursor would drop the page of a multi-page document's turn while looking
///   correct everywhere else, and because `next_anchor` in `turn_forward` would be a
///   walk from a position that may be a second half - which is the bug
///   `a_backward_step_from_a_second_half_does_not_jump_to_the_last_spread` traces.
/// * `snap_to_turn_anchor` in `goto` and `anchor_of` in the snap, because the
///   three ways into the app that assign the cursor by file index are only corrected
///   by that one call, and a `goto` that stopped calling it would leave every one of
///   them off an anchor with nothing in the tree to notice.
#[test]
fn manga_mode_makes_a_step_walk_a_turn() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");

    for (sig, needed) in [
        (
            "fn step(&mut self, ctx: &Context, forward: bool)",
            &[
                "self.config.manga_mode",
                "self.turn_forward()",
                "self.turn_backward()",
                "self.gallery.next()",
                "self.gallery.prev()",
            ][..],
        ),
        (
            "fn turn_forward(&mut self)",
            &["next_turn(", "goto_turn("][..],
        ),
        (
            "fn turn_backward(&mut self)",
            &["prev_turn(", "goto_turn("][..],
        ),
        (
            "fn goto_turn(&mut self, to: Option<PagePos>)",
            &["self.gallery.goto_page(pos)"][..],
        ),
        ("fn goto(&mut self, ctx: &Context)", &["self.snap_to_turn_anchor();"][..]),
        (
            "fn snap_to_turn_anchor(&mut self)",
            &["self.config.manga_mode", "image_viewer::anchor_of(", "self.gallery.goto_page(at)"][..],
        ),
    ] {
        let body = fn_body(&src, sig);
        for want in needed {
            assert!(
                body.contains(want),
                "`{sig}` no longer holds `{want}`, so a step in manga mode walks a page\n\
                 instead of a turn, or walks a turn and leaves the cursor on the wrong\n\
                 page of it (or, in `step`'s Gallery pair, a step with manga mode off\n\
                 stops being the per-page navigation every photo folder depends on).\n\
                 Body was:\n{body}"
            );
        }
    }
}

/// An **anchor** stays an anchor as the pairing learns about its neighbourhood.
///
/// This is the invariant that lets `turn_partner_at` skip `anchor_of` — an
/// O(distance from the front) walk — on the per-frame path that derives the turn's
/// second half from where `shown` is. Without it, the derivation would have to snap,
/// and snapping every frame is the cost the deferred anchor-list fix exists to remove.
/// So it is asserted rather than left in `main.rs`'s doc comment, where a claim about
/// `Shape` resolutions cannot be checked.
///
/// The argument, stated as the property: `lone(anchor)`'s three arms are `is_lead`,
/// `shape_of(anchor) == Double` and `!pairable(anchor, next)`. Learning resolves a
/// dimension from unknown — which `shape_of` reads as [`Shape::Single`] — to `Single`
/// or to `Double`, so `shape_of` can only go `Single -> Double` and `pairable` can only
/// go true -> false. **Both make `lone` more true, never less**, so a turn of two can
/// only shrink to a turn of one, never grow, and a position that was an anchor can
/// neither stop being one (its own turn shrinking still leaves it anchoring) nor become
/// the second half of a turn *behind* it (that would need an earlier anchor to stop
/// being lone).
///
/// The test is two galleries over the same shape pattern and both halves asserted,
/// because the interesting direction is the one where **nothing changes**: a pattern
/// of `Single`s must leave every position an anchor under every offset, or the
/// derivation would be answering about a second half and painting the turn beside it.
///
/// Drawn from the `turns_gallery` fixture and seeded with `mark_wide`, so a wide page
/// really is wide in the shape cache rather than named in a comment — `mark_wide`'s
/// doc explains why it goes through `note_shapes`.
#[test]
fn an_anchor_stays_an_anchor_as_the_pairing_learns() {
    // Four files, and file 2 is a two-page scan whose *second* page is wide, so the
    // learning that collapses a turn lands on a page that is not one of the anchors.
    let mut gal = turns_gallery(&[1, 1, 2, 1]);
    gal.set_loop_back(false);
    mark_wide(&mut gal, 2, 1);

    for lead in 0..=2u32 {
        // The anchor list **by walking** — `anchors_of` is `next_anchor` repeatedly,
        // and `anchor_of` is a separate forward search for "which turn is this page
        // on". They are two implementations of the same idea and they are asked to
        // agree: every position the walk calls an anchor must be one `anchor_of`
        // agrees is. Without the walk this would be `anchor_of(p) == p` on every
        // position, which cannot fail.
        let anchors = anchors_of(&gal, lead);
        assert!(
            !anchors.is_empty(),
            "lead {lead}: the walk from (0,0) produced no anchors, which cannot be true"
        );
        for a in &anchors {
            assert_eq!(
                anchor_of(*a, lead, &gal),
                *a,
                "lead {lead}: the walk calls {a:?} an anchor and anchor_of disagrees"
            );
        }
    }

    // **And the collapse itself**, asserted as a whole tiling rather than as one
    // position, because "cannot grow" is a claim about a partition and not about a
    // single page.
    //
    // Two galleries over the same files — four files, the third with two pages — one
    // with page `(2,1)` measured wide and one with nothing measured at all. The second
    // is the *premise* and it matters: an unmeasured page reads as [`Shape::Single`],
    // so the untouched gallery is the optimistic one, and every pair in it is a pair
    // only until something is learned.
    //
    // Asserted in the direction that is easy to get wrong. **Learning shrinks, and a
    // shrink only frees a second half**, so:
    //
    // * every anchor before is an anchor after (this is the invariant
    //   `turn_partner_at` skips `anchor_of` over), and
    // * no two turns became one (a merge would mean a page joined a turn that did not
    //   hold it, which is what growing looks like).
    let lead = LEAD_LONE_DEFAULT;
    let mut learned = turns_gallery(&[1, 1, 2, 1]);
    learned.set_loop_back(false);
    let before = anchors_of(&learned, lead);
    assert!(
        before.contains(&PagePos { index: 1, page: 0 })
            && before.contains(&PagePos { index: 2, page: 1 }),
        "the premise: with nothing measured the tiling is {{0}} {{1,2}} ... and (2,1) \
         opens a turn, so marking it wide is a real shrink rather than a no-op.\n\
         Anchors were: {before:?}"
    );

    mark_wide(&mut learned, 2, 1);
    let after = anchors_of(&learned, lead);

    assert!(
        before.iter().all(|a| after.contains(a)),
        "learning turned an anchor into a non-anchor: {before:?} -> {after:?}. `lone` can \
         only go true as a dimension resolves from unknown (read as Single) to a \
         measurement, so a turn can only shrink - and a shrink never un-anchors its own \
         first page."
    );
    assert!(
        after.len() >= before.len(),
        "learning merged turns ({before:?} -> {after:?}). Shrinking frees a second half; \
         it does not join two pages to a turn that did not hold them."
    );
    // The freed page is now the anchor of its own turn, which is the *consequence* the
    // shrinking arm has to produce and the reason the invariant above is stated over
    // anchors rather than over positions.
    assert!(
        after.contains(&PagePos { index: 2, page: 1 }),
        "the page the wide page displaced did not become an anchor ({before:?} -> \
         {after:?})"
    );
}

/// Every anchor of `gal`'s tiling at `lead_lone`, by repeated `next_anchor`.
///
/// Stops when the walk stops moving. With `loop_back` off that is `None` at the end
/// of the gallery; the visited check as well, because a tiling that wrapped would
/// otherwise run until `PREV_SCAN_LIMIT` and the test would hang rather than fail.
fn anchors_of(gal: &Gallery, lead_lone: u32) -> Vec<PagePos> {
    let mut out = vec![PagePos { index: 0, page: 0 }];
    let mut cur = out[0];
    while let Some(next) = next_anchor(cur, lead_lone, gal) {
        if out.contains(&next) {
            break;
        }
        out.push(next);
        cur = next;
    }
    out
}

// ---------------------------------------------------------------------------
// Fix wave: the second half is derived, `self.full` is keyed by page, and a
// document's page count is read before the step that could step over it
// ---------------------------------------------------------------------------
//
// Three defects, one shape. Each was a value **latched at one event and read at
// another**: `shown_partner` assigned when a preview was adopted and read every
// frame; `self.full`'s identity held to be "a path" and read as "an image"; a
// document's page count learned on arrival and relied on by the step that had
// already happened. **The review question that finds them is "what is the lifetime
// of this value and who re-derives it"**, not "is this right?" - each had a passing
// test or a passing source scan asserting the call existed.
//
// What is asserted here, and what is not:
//
// * The pairing rules and the geometry are **behavioural**, against the real
//   `Gallery` - see `a_document_behind_an_unknown_page_count_is_stepped_over`.
// * The *wiring* is a **source scan**, because `main.rs` is a binary. The scans
//   below are scoped by [`fn_body`] and by [`block_after`], and the one that
//   matters most (`the_turns_second_half_is_derived_not_latched`) checks the
//   **position** of a call relative to a brace, not merely its presence, because
//   presence is exactly what was true of the bug: the assignment was in the file,
//   it was in the right function, and it was in the wrong *block*.

/// The braces-delimited block that starts at `marker`, found by brace matching.
///
/// Exists because [`fn_body`] answers "where does this function end" and this
/// answers "where does this block end" - which is the question the derived-partner
/// scan needs. **Brace matching, and for the reason `fn_body` documents**: splitting
/// a body on text is how a scan passes vacuously.
///
/// Panics rather than returning `""` when the marker is gone: a scan that quietly
/// sees nothing is a scan that cannot fail, which is the opposite of a guard.
fn block_after<'a>(src: &'a str, marker: &str) -> &'a str {
    let at = src
        .find(marker)
        .unwrap_or_else(|| panic!("{marker} is gone; update this test to match"))
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

/// A key goes back to the position it came from, and a key from nowhere is `None`.
///
/// The inverse of `PageCtx::key`, and it exists because **the pairing is asked in
/// positions and "what is on screen" is a key**: deriving a turn's second half for
/// the picture the app is painting means turning that key back into a position every
/// frame. Without this function the only honest alternative is a second stored
/// position that could disagree with the key it belongs to - which is the defect
/// this whole wave is about.
///
/// Three cases, and each is a way the obvious implementation is wrong:
///
/// * **A page past the first round-trips.** `ImgKey { page: 0 }` and
///   `ImgKey { page: 1 }` of one document are different pictures, so a scan that
///   resolved the file and threw the page away would answer page 0 for page 1.
/// * **The page is trusted, not range-checked** - [`Gallery::goto_page`]'s rule.
///   `pages_in` answers 1 for a file whose count has not been learned, so a range
///   check here would make the answer depend on *knowledge* rather than on the key,
///   and `None` is a legitimate state the derivation has to survive as "no turn".
/// * **A path that is not in the gallery is `None`, not a guess.** A file removed
///   from under the picture on screen names a position no turn can contain.
#[test]
fn a_key_goes_back_to_the_position_it_came_from() {
    let gal = turns_gallery(&[3, 1]);
    let key = |i: usize, p: u32| ImgKey {
        path: gal.files[i].clone(),
        page: p,
    };

    // The round trip, over both files and every page of the document.
    for (i, p) in [(0usize, 0u32), (0, 1), (0, 2), (1, 0)] {
        assert_eq!(
            gal.pos_of(&key(i, p)),
            Some(PagePos { index: i, page: p }),
            "pos_of is not the inverse of key at ({i}, {p})"
        );
        assert_eq!(
            PageCtx::key(&gal, PagePos { index: i, page: p }),
            Some(key(i, p)),
            "the premise: key built this key"
        );
    }

    // A page the file does not have: answered, not refused.
    assert_eq!(
        gal.pos_of(&key(1, 9)),
        Some(PagePos { index: 1, page: 9 }),
        "the page is trusted, for goto_page's reason: pages_in answers 1 for an \
         unlearned file, so a range check would make the answer depend on what is \
         known rather than on the key"
    );

    // A path that is not in the gallery.
    assert_eq!(gal.pos_of(&ImgKey::first(std::path::Path::new("/elsewhere.jpg"))), None);
    // And an empty gallery answers nothing rather than panicking.
    let empty = Gallery::from_file(std::path::PathBuf::from("/vol/page00.jpg"));
    let mut empty = empty;
    empty.files.clear();
    assert_eq!(empty.pos_of(&ImgKey::first(std::path::Path::new("/vol/page00.jpg"))), None);
}

/// A multi-page document is only unreachable by `→` while its page count is
/// unknown, and learning it is what makes it reachable.
///
/// **This is the rule; the call that supplies the count is asserted by source scan
/// below** (`goto` learns the turn's other file). Split that way on purpose: the
/// walk's answer to the same press genuinely does depend on a fact about the
/// filesystem, so a test that drove the whole app would be testing I/O, while a
/// test that only scanned the source would not know whether the scan mattered.
///
/// The fixture is the review's `[a, b, vol.tiff(200pp), c]`, all portrait, so
/// every page pairs optimistically, and the offset is [`LEAD_LONE_DEFAULT`]
/// because `b` is only an **anchor** there: at 0 the first turn is `{a, b}` and a
/// cursor arriving on `b` is on its second half, which is a different question
/// (and `anchor_of` snaps it, correctly). From `b`:
///
/// * **Unknown count** - `pages_in(vol) == 1`, `step` applies `turn_len` two pages
///   at a time one page at a time, and the second of those pages steps *past* the
///   document. The turn claims the document's first page as its own second half and
///   the cursor lands on `c`.
/// * **Known count** - the same press lands on `(vol, 1)`, page 1 of 200.
///
/// So the cursor spends the session on `{a} {b,vol.p1} {c}` with 199 pages
/// invisible, and the only way in is backwards, `Home`/`End`, or the grid. The
/// non-manga path is immune, which is why it went unnoticed: `Gallery::next` steps
/// one page and lands on it.
///
/// `forget_page_count` rather than a second `Gallery` so the *same object* answers
/// both halves - a test that built two galleries could be passing because the two
/// were not comparable.
#[test]
fn a_document_behind_an_unknown_page_count_is_stepped_over() {
    let mut gal = turns_gallery(&[1, 1, 200, 1]);
    gal.set_loop_back(false);
    let lead = LEAD_LONE_DEFAULT;
    let b = PagePos {
        index: 1,
        page: 0,
    };

    // The premise: `b` is an anchor at this offset, so the step off it is a step off
    // a turn rather than off the second half of one.
    gal.goto_page(b);
    assert_eq!(anchor_of(gal.page_pos(), lead, &gal), b, "b is an anchor here");

    // ...and the document is the third file and has not been asked about, so
    // `pages_in` answers 1 - which is the whole of the defect.
    let vol = gal.files[2].clone();
    gal.forget_page_count(&vol);
    assert_eq!(gal.pages_in(2), 1, "an unlearned file answers one page");
    assert_eq!(gal.files.len(), 4);

    assert_eq!(
        next_turn(gal.page_pos(), lead, &gal),
        Some(PagePos {
            index: 3,
            page: 0
        }),
        "with the count unknown the turn claims the document's only (believed) page \
         and the step lands past it"
    );

    // The same gallery, the same press, once the count is known - which is what
    // `Gallery::learn_pages` supplies from a four-byte read.
    gal.set_page_counts([1, 1, 200, 1]);
    gal.goto_page(b);
    assert_eq!(gal.pages_in(2), 200);
    assert_eq!(
        next_turn(gal.page_pos(), lead, &gal),
        Some(PagePos {
            index: 2,
            page: 1
        }),
        "with the count known the step lands *inside* the document, which is the \
         difference this fix makes"
    );

    // And its pages are then walkable a turn at a time, so the reader reaches the end
    // of the volume by pressing right rather than by jumping. The cursor is moved to
    // the turn the assertion above only *asked* about - `next_turn` is a query, so the
    // two assertions above leave the cursor on `b`.
    gal.goto_page(PagePos {
        index: 2,
        page: 1,
    });
    // Two pages per turn, because every page of this document is portrait and
    // unmeasured and `pairable` pairs optimistically: 1 -> 3 -> 5.
    for page in [3u32, 5] {
        assert!(
            step_forward(&mut gal, lead),
            "the document is walkable: page {page} is reachable by pressing right"
        );
        assert_eq!(gal.page_pos(), PagePos { index: 2, page });
    }
    // **The whole document, walked.** This is the claim the defect was about - 199
    // pages unreachable by pressing right - so it is asserted by walking rather than
    // by landing on the last page: `learn_shapes` and `learn_pages` are per *page*,
//    and a count that is known for the document while its shapes are known for none
    // of its pages is not the state a reader is ever in. ~98 presses, each an
    // `anchor_of` walk over ~100 turns of cached arithmetic, which is the cost the
    // app pays for a real volume anyway.
    let mut page = 1u32;
    while page < 199 {
        let before = gal.page_pos();
        assert!(
            step_forward(&mut gal, lead),
            "the walk into the volume stopped at page {page} of 200"
        );
        let after = gal.page_pos();
        assert_eq!(
            after.index, 2,
            "the cursor left the document at page {page}, so the step is over it again"
        );
        assert!(
            after.page > before.page,
            "page {page} -> {after:?} did not advance inside the document"
        );
        page = after.page;
    }
    assert_eq!(gal.page_pos(), PagePos { index: 2, page: 199 });

    // The gallery's last turn is `{vol.p199, c}` - the document's last page pairs
    // with the next file's only page - so there is nothing after it and a forward step
    // from its anchor is a refusal. Which is `Gallery::loop_back`'s business, not this
    // test's; what is asserted here is only that the refusal is *at the end* rather
    // than in the middle of the volume, which is what the bug produced.
    assert!(
        !step_forward(&mut gal, lead),
        "there is a turn after the gallery's last one"
    );
}

/// `goto` learns the page count of the file the turn's **other half** names, and
/// `shown_partner` is re-derived every frame rather than latched at the adopt.
///
/// Two source scans in one test because they are one fix: the partner's file is the
/// reason a turn's length is not knowable on arrival, and the partner itself is the
/// thing that was frozen. Both live in `src/main.rs`, a binary no integration test
/// can reach, for the reason `AGENTS.md` records.
///
/// ## The derived-partner scan, and why it checks *position*
///
/// `shown_partner` used to be assigned inside
/// `if previews.contains_key(&cur) && shown != cur { ... }`. That gate is false on
/// every frame the cursor stands still - and `M`, `O` and `X` all re-`goto`
/// **without moving the cursor** - so all three changed what a page is while the
/// screen kept the old answer. `→ → → M` in a folder of single pages halves the panel
/// and lays out **one** page, so the reader gets a half-width picture beside an empty
/// half; the next `→` produces a correct spread, which is why it reads as "mostly
/// working".
///
/// **A `contains` scan would have passed on the bug.** The assignment was present, in
/// the right function, doing the right call. What was wrong was which block it was
/// in, so this checks three separate things:
///
/// 1. `sync_shown_partner()` is called from `sync_shown` at all.
/// 2. **It is outside the adopt gate** - found by brace matching on the gate's own
///    `if` line, so moving the call back inside fails this.
/// 3. `invalidate_path` does not assign `shown_partner` at all. It used to, as a
///    twenty-line documented workaround for exactly this latch (clear the partner
///    when the changed file *is* the partner, then re-ask it), and it had to: with
///    the assignment gated, `shown` surviving the invalidation still equalled `cur`,
///    so the gate was false from there to the end of the session and the spread stayed
///    collapsed for good. The workaround is deleted because there is no latch to work
///    around.
///
/// Point 3 doubles as the guard for I2, which is the same defect seen from the other
/// side: a partner whose decode reveals it is a two-page scan collapses the *plan*
/// (`arrival_reschedules` re-runs `schedule`), and with a latch nothing re-derived
/// the screen, so three printed pages stayed squashed into one viewport until the
/// reader navigated. Deriving every frame closes it as a class rather than at one
/// site - which is the claim, and this is what is being asserted about it.
///
/// `shown_pos` is scanned too, because it is where the derivation gets its position
/// and it has two halves that must both be there: the `cur` fast path (which is the
/// reason the per-frame cost is a pointer comparison rather than a scan of `files`)
/// and the `Gallery::pos_of` fallback for the frames where `shown` lags.
///
/// What none of this can tell you: whether a spread *looks* right. See `TODO.md`'s
/// "nobody has looked at a spread".
#[test]
fn the_turns_second_half_is_derived_not_latched() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");

    let sync = fn_body(&src, "fn sync_shown(&mut self, ctx: &Context)");
    assert!(
        sync.contains("self.sync_shown_partner();"),
        "`sync_shown` no longer re-derives the turn's second half at all, so the\n\
         pairing is whatever it was the last time a preview was adopted.\nBody was:\n{sync}"
    );

    // 2. **Outside the gate.** Scoped by brace matching on the gate's own `if` line,
    //    so this is a statement about the block the call is in and not about the file.
    let gate = block_after(sync, "if self.previews.contains_key(&cur)");
    assert!(
        !gate.contains("shown_partner"),
        "the second half is assigned inside the adopt gate again, which is the latch.\n\
         The gate is false on every frame the cursor stands still, and `M`, `O` and\n\
         `X` all re-goto without moving the cursor - so the reader gets a half-width\n\
         picture beside an empty half until they navigate.\nGate was:\n{gate}"
    );

    // 1. ...and it really is called after the gate rather than only from elsewhere.
    //    A byte offset is the honest way to say "textually after", and the gate is
    //    the last thing in `sync_shown` that ends in `;` before it.
    let gate_end = sync
        .find("if self.previews.contains_key(&cur)")
        .expect("the gate is still there")
        + gate.len();
    let call_at = sync
        .find("self.sync_shown_partner();")
        .expect("the call is in sync_shown");
    assert!(
        call_at >= gate_end,
        "`sync_shown_partner()` has moved *into* the adopt gate (byte {call_at} is \
         inside a block ending at {gate_end})."
    );

    let derive = fn_body(&src, "fn sync_shown_partner(&mut self)");
    assert!(
        derive.contains("self.shown_pos()") && derive.contains("turn_partner_at"),
        "the second half is no longer derived from where the picture on screen is, so\n\
         it is read from the cursor's turn and the two halves of the spread can be\n\
         two different turns for the length of a decode.\nBody was:\n{derive}"
    );

    let pos = fn_body(&src, "fn shown_pos(&self)");
    for want in ["self.gallery.pos_of(shown)", "self.gallery.page_pos()"] {
        assert!(
            pos.contains(want),
            "`shown_pos` no longer holds `{want}`. Without the `page_pos` fast path this\n\
             is a scan of every file in the gallery on every frame; without `pos_of` it\n\
             cannot answer at all during the lag between a navigation and its decode.\n\
             Body was:\n{pos}"
        );
    }

    // 3. The workaround is gone, and its absence is load-bearing. The scan is for
    //    `self.shown_partner`, not the bare name: `fn_body` includes the function's own
    //    `//` comments, and the comment there explains *why* it was removed and must be
    //    allowed to say so.
    let inv = fn_body(&src, "fn invalidate_path(");
    assert!(
        !inv.contains("self.shown_partner"),
        "`invalidate_path` assigns the turn's second half again. It used to, as a\n\
         documented workaround for the latch - clear the partner when the changed file\n\
         *is* the partner, then re-ask it - and the workaround is what made the spread\n\
         reappear after a re-export. It is only removable because the answer is now\n\
         derived every frame; putting it back means two places that can disagree.\n\
         Body was:\n{inv}"
    );
}

/// The turn's other file's page count is learned on arrival, so a turn's step cannot
/// walk over a multi-page document.
///
/// The scan is scoped and every string in it is load-bearing:
///
/// * the call in `goto`, or nothing learns the count on arrival and
///   `a_document_behind_an_unknown_page_count_is_stepped_over` describes the app;
/// * `learn_partner_pages` asking `turn_partner` rather than "the next page", or a
///   lone turn reads a count nothing is asking about;
/// * the same-file bail, or a turn inside a document re-learns the cursor's own file
///   on every navigation - harmless, but it hides which of the two cases the call is
///   for;
/// * `learn_pages`, which is the cache-first entry point: `tiff_page_count` on its own
///   would put a `File::open` on the critical path of every keypress.
#[test]
fn goto_learns_the_page_count_of_the_turns_other_file() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");

    let goto = fn_body(&src, "fn goto(&mut self, ctx: &Context)");
    assert!(
        goto.contains("self.learn_partner_pages();"),
        "`goto` no longer learns the page count of the file the turn's second half\n\
         names. A turn anchored just before a 200-page document then claims its\n\
         first page as its own second half, the step lands on the file *after* it, and\n\
         pressing `->` never reaches pages 2..200 for the rest of the session - which\n\
         is the content this feature exists for.\nBody was:\n{goto}"
    );

    let learn = fn_body(&src, "fn learn_partner_pages(&mut self)");
    for want in [
        "self.turn_partner()",
        "self.gallery.learn_pages(&partner.path)",
        "partner.path == here",
    ] {
        assert!(
            learn.contains(want),
            "`learn_partner_pages` no longer holds `{want}`. The cost of this call is\n\
             meant to be one cache-first four-byte read per *file*, taken only when the\n\
             turn's other half is a different file.\nBody was:\n{learn}"
        );
    }
}

/// `self.full` is keyed by `(file, page)` everywhere it is asked a page question.
///
/// Three sites, and each was a silent wrong picture rather than an error:
///
/// * **`needs_full`** answered "already done" from a path comparison, so on a turn
///   inside a document - every turn in a manga volume at the default `lead_lone`,
///   where the partner is `(index, page + 1)` of the *same file* - `plan_jobs` was
///   handed `partner_needs_full == false`, no `Kind::Full` job was planned, and the
///   second half of a zoomed spread stayed at preview resolution for the session with
///   the expensive decode sitting in the queue unasked-for. Its own doc states the
///   invariant its first line violated.
/// * **`Ctrl+C`** filtered the buffer on the path, so copying page 2 of a document
///   pasted page 1's pixels. No error, no counter.
/// * **the tile loop** cut page 0's pixels with page N's `uv` - page 0 drawn over
///   page N - which is reachable in one keystroke from the paging path manga mode is
///   built on, and invisible to `tests/probe_chords.ps1`, which uses a real `.jpg`.
///
/// The negative assertions matter as much as the positive ones: `f.path == shown.path`
/// still *compiles* and still returns the right answer for every single-page image,
/// which is every photo folder and every fixture in the suite. A scan that only
/// looked for the new spelling would not notice it being replaced.
///
/// This is a scan and not behaviour because `self.full` is a field of `MyApp`. What
/// closes the gap for real is the third item on `TODO.md`'s eyeball list: open a
/// multi-page document, page forward, zoom, and confirm the tiles are that page's.
#[test]
fn the_full_resolution_buffer_is_keyed_by_page_not_by_path() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");

    let full = fn_body(&src, "struct FullImage {");
    assert!(
        full.contains("page: u32"),
        "`FullImage` has no page field, so every \"is this buffer the one for this\n\
         image?\" check below is a question about a file answered about a page. The\n\
         struct is:\n{full}"
    );

    for (sig, want, what) in [
        (
            "fn needs_full(&self, key: &ImgKey) -> bool",
            "f.is(key)",
            "the partner of a turn inside a document is never planned a full decode, so \
             the second half of a zoomed spread is permanently soft",
        ),
        (
            "fn copy_image_to_clipboard(&mut self, unedited: bool)",
            "f.is(&cur)",
            "Ctrl+C on page 2 of a document pastes page 1's pixels",
        ),
    ] {
        let body = fn_body(&src, sig);
        assert!(
            body.contains(want),
            "`{sig}` no longer holds `{want}`, so {what}.\nBody was:\n{body}"
        );
    }

    let central = fn_body(&src, "fn central(");
    assert!(
        central.contains("f.is(&shown)"),
        "`central` no longer holds `f.is(&shown)`, so the tile loop cuts one page's \
         pixels with another's `uv`.\nBody was:\n{central}"
    );
    assert!(
        !central.contains("f.path == shown.path"),
        "`central` is comparing the full buffer by path again, so a document past page 1 \
         draws page 0 over the page on screen.\nBody was:\n{central}"
    );

    let sync = fn_body(&src, "fn sync_shown(&mut self, ctx: &Context)");
    for want in ["!f.is(&cur)", "!f.is(s)"] {
        assert!(
            sync.contains(want),
            "`sync_shown` no longer holds `{want}`. `decode_page` returns `full: None` for \
             every page past the first of a document, so a path comparison freezes the \
             document's buffer at whichever page happened to be current.\nBody was:\n{sync}"
        );
    }
}

/// `Ctrl+C` refuses a page of a multi-page document, as `Ctrl+S` does.
///
/// A source scan because the refusal is in the binary. **The spelling matters and is
/// therefore pinned**: `if cur.page > 0` plus an `error`, not a `notice` - a notice is
/// what the app says when it did the thing, and this is the app declining to.
///
/// The reason is *not* only the one `Ctrl+S` has. Adding `page` to `FullImage` closed
/// the wrong-pixels hole in the fast path; what is left is that the crop and the turn
/// live in `ImageEdits`, which is keyed by path because they are an edit to the
/// document - so a frame drawn on page 1 would be applied to page 3's picture, and the
/// result is a valid picture of the wrong part of the scan. That is the outcome with no
/// error anywhere, which is the one this project keeps paying for.
///
/// `tests/probe_chords.ps1` is the script that would check this on the real app, and
/// it cannot: it uses a real `.jpg`, which has exactly one page.
#[test]
fn copy_refuses_a_page_of_a_multi_page_document() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
    let body = fn_body(&src, "fn copy_image_to_clipboard(&mut self, unedited: bool)");

    let at = body
        .find("if cur.page > 0")
        .unwrap_or_else(|| panic!("the page refusal is gone from `copy_image_to_clipboard`:\n{body}"));
    let window = &body[at..(at + 400).min(body.len())];
    assert!(
        window.contains("self.error = Some(") && window.contains("multi-page document"),
        "`Ctrl+C`'s page refusal no longer reports an error naming the reason. A silent\n\
         refusal, or one that reads as success, is worse than copying the wrong page.\n\
         Window was:\n{window}"
    );
    // It has to come **before** the fast path, or the pixels leave the app first.
    let fast = body
        .find("if let Some(f) = self.full.as_ref()")
        .expect("the fast path is still there");
    assert!(
        at < fast,
        "the page refusal has moved below the fast path, so the clipboard is already \
         filled by the time it is checked."
    );

    // The save's own refusal, for the comparison: same shape, same wording family.
    let save = fn_body(&src, "fn save_transformed_copy(&mut self)");
    assert!(
        save.contains("if cur.page > 0"),
        "`Ctrl+S` no longer refuses a page, and the refusal is the feature there (the \
         lossless path rewrites the whole file). Its removal is not this test's business \
         but the divergence is worth a look.\nBody was:\n{save}"
    );
}

// ---------------------------------------------------------------------------
// What a turn is *decoded* for
// ---------------------------------------------------------------------------

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
/// of it — and the picture was soft in proportion to how much of the panel it used.
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

    // The height was never the axis in question — the seam runs vertically, so one
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
/// The test derives the drawn rectangle from [`spread_fit`] — the function the app
/// hands the same flag to — and requires the decode's long side to cover it, at both
/// pixel ratios. Two weaker assertions were available and both are worthless: "the
/// decode is at least half the panel" is satisfied by the bug, and "the decode does not
/// exceed the panel" is satisfied by a texture ten times too large.
///
/// **And the numbers that fail.** For the lone spread the draw is 1518.75 x 1080
/// points (the height binds: `min(1920/2700, 1080/1920) = 0.5625`), so 1920 of 1920 is
/// **1.26** of it and the half-panel answer's 1080 is **0.71**. At `ppp` 2 that is
/// 3840 of 3037.5 against 2160 of 3037.5. The portrait page and the pair both land
/// exactly on 1080, because one page fitted into a half panel is bound by the panel's
/// height — which is why the half-panel rule is not merely conservative on those two
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
    let drawn = (scan.0 * scale).max(scan.1 * scale);
    assert_eq!(drawn, 1518.75, "1518.75 x 1080, the number AGENTS.md records");
    assert_eq!(coverage(1920, drawn), 1.264, "ppp 1: 1920 of 1518.75");
    assert_eq!(
        coverage(1080, drawn),
        0.711,
        "ppp 1, before the fix: 1080 of 1518.75 - close enough to hide, which is why this \
         shipped"
    );
    assert_eq!(coverage(3840, drawn * 2.0), 1.264, "ppp 2: 3840 of 3037.5");
    assert_eq!(
        coverage(2160, drawn * 2.0),
        0.711,
        "ppp 2, before the fix: 2160 of 3037.5 - visibly soft"
    );
}

/// How much of a drawn picture a decode target covers, to three decimals.
///
/// Below 1.0 is the defect in one number: a request smaller than the rectangle the
/// picture is drawn into.
fn coverage(target: u32, drawn: f32) -> f64 {
    ((target as f64 / drawn as f64) * 1000.0).round() / 1000.0
}

/// End to end: the **texture** a lone spread comes back at, before and after — not
/// the request, which is what the tests above pin.
///
/// They are not the same number, and only one of them is a picture.
/// `decode_jpeg_scaled` picks the smallest of 1/8, 1/4, 1/2 and 1 whose long side
/// still covers the request, so a request below the drawn size can land on a *lower
/// rung* and return a texture narrower than the rectangle it is drawn into. The
/// fixture is 2700x1920 — the double-page scan in every geometry test above, and a
/// size whose rungs straddle the two requests: half of it is 1350, which covers the
/// old 1080 and not the new 1920. So the old answer is a **1350-wide texture painted
/// over 1518.75 pixels** and the new one is 2700 wide for the same 1518.75.
///
/// **What this test does *not* claim, and the reason the fixture's size is load-bearing
/// here.** At `ppp` 2 both requests are past this source's long side, so both land on
/// 1/1 and the texture is the same 2700 either way: for *this* page the defect is
/// obvious at `ppp` 1 and invisible at `ppp` 2, while the *request* was wrong at both.
/// Which rung a source lands on is a property of that source's dimensions — a 1600-wide
/// page never leaves 1/2 and a 6000-wide one leaves it either way — so the rule the fix
/// restores is about the request (never smaller than the draw), and pinning a rung here
/// would pin the fixture rather than the rule.
///
/// Decoding for real is what makes this worth a 5 MP fixture: the pure tests above would
/// all stay green if `decode_jpeg_scaled` chose the *largest* reduction that covers the
/// request, which is a plausible edit and the opposite of right.
#[test]
fn a_lone_spread_is_decoded_at_the_size_it_is_drawn_at() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("spread.jpg");
    write_jpeg(&path, WIDE_SCAN.0 as u32, WIDE_SCAN.1 as u32);

    // The drawn rect, from the layout that is handed the same flag.
    let scale = spread_fit(DECODE_PANEL, &[WIDE_SCAN], true);
    let drawn = WIDE_SCAN.0 * scale;
    assert_eq!(drawn, 1518.75, "the same 1518.75 the layout tests pin");

    for (ppp, half, whole) in [
        (1u32, (960u32, 1080u32), decode_panel_px(panel_px_at(1), true, true)),
        (2, (1920, 2160), decode_panel_px(panel_px_at(2), true, true)),
    ] {
        let want = drawn * ppp as f32;
        let old = load_image(&path, preview_target(half)).expect("the old target decodes");
        let new = load_image(&path, preview_target(whole)).expect("the new target decodes");
        assert!(
            (old.width as f32) < want,
            "ppp {ppp}: the half-panel request decoded {}x{} and the page is drawn {want} \
             pixels wide, so the texture is upscaled to paint it",
            old.width,
            old.height
        );
        assert!(
            (new.width as f32) >= want || new.width == WIDE_SCAN.0 as u32,
            "ppp {ppp}: the whole-panel request decoded {}x{}, still short of the {want} pixels \
             the page is drawn across",
            new.width,
            new.height
        );
        if ppp == 1 {
            // The rungs, pinned: this is the mechanism, in two pairs of numbers.
            assert_eq!(
                (old.width, old.height),
                (1350, 960),
                "1080 is asked for, half of 2700 is 1350 which covers it, a quarter does not, so \
                 1/2 is the rung"
            );
            assert_eq!(
                (new.width, new.height),
                (2700, 1920),
                "1920 is asked for, 1/2 does not cover it, so 1/1 does - and 1/1 is all the \
                 source has"
            );
            assert_eq!(
                coverage(old.width, want),
                0.889,
                "and the old texture is 0.889 of what is painted across it: soft, at one pixel \
                 ratio"
            );
            assert_eq!(
                coverage(new.width, want),
                1.778,
                "and the new one is comfortably past it, which is what 'the source has no more \
                 to give' looks like"
            );
        } else {
            assert_eq!(
                (old.width, old.height),
                (2700, 1920),
                "ppp 2: both requests are past this source's long side, so both land on 1/1 and \
                 the texture is identical. The request was still wrong, and a page twice this \
                 size would show it."
            );
            assert_eq!((new.width, new.height), (2700, 1920));
            assert_eq!(
                coverage(old.width, want),
                0.889,
                "which is why this page was not soft at ppp 2 - and the reason the report was \
                 written about HiDPI is the *request*, which is 0.711 of the draw at both ratios"
            );
        }
    }
}

/// A plain baseline JPEG of `w` x `h`, for the one test here that decodes rather than
/// arithmetic.
fn write_jpeg(path: &std::path::Path, w: u32, h: u32) {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(w, h, image::Luma([128])))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The resize machine sees a lone spread's target change **once**, refine **once**, and
/// then nothing at all — including when the target does not change, which is the case
/// the mode toggle now is.
///
/// Both halves are the real risk. The old half-panel answer made `M` on a lone spread
/// change the target in both directions (1920 to 1080 and back) and charge a refine
/// each way for a picture that did not change; the new answer makes the toggle
/// **inert**, because on and off decode the same panel and lay out the same
/// single-image rectangle. Neither is a storm — the tracker is sticky — but "no storm"
/// was never the question. The question is whether a genuine change costs exactly one
/// decode, and a flicker of the flag (a shape measured a frame later, a partner that
/// lands) would either alternate the target and never settle, or settle twice.
///
/// The counts are over a **run of frames**, not one call, because that is the only
/// shape in which "one" means anything: a per-call assertion cannot tell a settled
/// refine from a storm of them.
#[test]
fn a_lone_spread_costs_one_refine_when_its_target_really_changes() {
    let source_long = WIDE_SCAN.0 as u32;
    let ppp = 2u32;
    let target_of = |lone_spread: bool| {
        preview_target(decode_panel_px(panel_px_at(ppp), true, lone_spread))
    };

    let half = target_of(false);
    let whole = target_of(true);
    assert_eq!((half, whole), (2160, 3840), "ppp 2 on a 1920x1080 panel");

    // A turn whose shape has just been measured: the preview on screen was decoded for
    // the half-panel answer, and the flag is now true.
    let mut tracker = ResizeTracker::new();
    let mut actions = Vec::new();
    for _ in 0..12 {
        let target = whole;
        actions.push(tracker.frame(
            target,
            needs_refine(half, source_long, half, target, RESIZE_PX_FLOOR),
        ));
    }
    assert_eq!(
        actions.iter().filter(|a| **a == ResizeAction::Refine).count(),
        1,
        "twelve frames of one real change are one refine, not one per frame and not a \
         storm of them: {actions:?}"
    );
    assert_eq!(
        actions.first(),
        Some(&ResizeAction::Wait),
        "and the first frame only waits: the machine settles on the frame after a size \
         changes, which is the rule that makes a window drag cost one decode"
    );
    // The decode lands, and a settled target costs nothing afterwards.
    tracker.landed();
    assert_eq!(
        tracker.frame(whole, needs_refine(whole, source_long, whole, whole, RESIZE_PX_FLOOR)),
        ResizeAction::Idle,
        "once the sharper texture is in place the target is met exactly, so there is \
         nothing left to ask for"
    );

    // **The toggle itself, with the fix in place.** Both modes decode the whole panel
    // for a lone spread, so the target does not move and there is no refine to charge.
    assert_eq!(
        preview_target(decode_panel_px(panel_px_at(ppp), false, true)),
        whole,
        "mode off on a lone spread is the same panel as mode on - it is drawn as a single \
         image either way"
    );
    let mut tracker = ResizeTracker::new();
    let mut actions = Vec::new();
    for target in [whole, whole] {
        actions.push(tracker.frame(target, needs_refine(whole, source_long, whole, target, RESIZE_PX_FLOOR)));
    }
    assert_eq!(
        actions,
        vec![ResizeAction::Idle, ResizeAction::Idle],
        "toggling `M` on a lone spread changes the decode target not at all, so it must not \
         cost a decode for a picture that did not change"
    );

    // The control: on a page that is *not* a lone spread the mode does change the
    // target, so the "no refine" above is not the tracker being deaf. Off to on is a
    // shrink, which `needs_refine` refuses; on to off is a growth, which it accepts
    // once.
    let mut tracker = ResizeTracker::new();
    let mut actions = Vec::new();
    for target in [half, half, whole, whole, whole] {
        actions.push(tracker.frame(
            target,
            needs_refine(half, source_long, half, target, RESIZE_PX_FLOOR),
        ));
    }
    tracker.landed();
    assert_eq!(
        actions.iter().filter(|a| **a == ResizeAction::Refine).count(),
        1,
        "a portrait page or a pair does pay one refine when the mode goes back off, so the \
         lone-spread case above is a property of the flag and not of the machine: {actions:?}"
    );

    // And a flicker would be visible as a storm of waits with no refine at all, which
    // is what makes "twelve frames of one real change" the assertion worth having.
    let mut tracker = ResizeTracker::new();
    let flicker: Vec<ResizeAction> = (0..12)
        .map(|i| {
            let target = if i % 2 == 0 { half } else { whole };
            tracker.frame(
                target,
                needs_refine(half, source_long, half, target, RESIZE_PX_FLOOR),
            )
        })
        .collect();
    assert_eq!(
        flicker.iter().filter(|a| **a == ResizeAction::Refine).count(),
        0,
        "a flag that changed every frame would leave the machine waiting forever and never \
         decoding at all - which is how a flickering flag presents, not as a storm: {flicker:?}"
    );
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
/// — once for the decode and once for the fit — would satisfy every positive assertion
/// in this test and could disagree with itself on the frame a shape is learned.
#[test]
fn central_decides_and_threads_the_lone_spread_flag() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
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
    // the layout calls would be checking nothing — `central` fits after the picture is
    // known, so an ask placed anywhere above `spread_fit` would satisfy it.
    let preview_target_at = central
        .find("preview_target(")
        .expect("`central` no longer asks for the preview target");
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
/// because it is the third scan in this suite of the same defect class** — the
/// block, not the call, being the property — and because it wants the
/// brace-matching [`fn_body`] and [`block_after`] this file already owns.
/// `central` and `MyApp::schedule` are in the binary, which no integration test
/// can reach; `flip_through.rs`'s
/// `there_is_no_decode_target_before_the_display_area_is_measured` covers the rule
/// the library applies and cannot see either of these two call sites.
///
/// **The load-bearing assertion is the negative one, and it is about position.**
/// `display_dim` is what `schedule` reads, so a `self.schedule()` one line *above*
/// the assignment inside `central` would be a correct-looking call in the correct
/// function re-planning against the same empty `display_dim` it is trying to fill —
/// the exact defect shape `the_turns_second_half_is_derived_not_latched` records,
/// where the assignment was present, in the right function, in the wrong block.
/// So this brace-matches the `if` that assigns `display_dim` and requires the call
/// to be inside it, which a `contains` on the whole body could not tell.
#[test]
fn the_plan_is_empty_until_the_display_area_is_measured() {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
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

/// The text between a call's `(` and its matching `)`, starting from the call's own name.
///
/// **Paren-matched rather than a line or a fixed window**, because every one of the four
/// calls this is used on is written across several lines - two of them end with a `)` on a
/// line of its own, which is not `);` - so both a `find(");")` and a 400-byte window are
/// guesses about formatting rather than about the call. This is the same brace-matching
/// [`fn_body`] does, for parentheses.
///
/// Weak in the way every source scan here is: it reads the text, so it cannot tell
/// whether the value passed is the one the reader expects, only that *an* argument by
/// that name is there. What it does catch is the regression that actually happens, which
/// is a call site dropping a parameter that a later edit added.
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

/// A navigation walk costs one `turn_len` per turn *behind* the cursor, in both
/// directions.
///
/// The measurement is a **count of `dims` reads through a counting `PageCtx`**, not a
/// time. That is deliberate and it is this suite's standing rule (see
/// `thumb_cache.rs`): a timing assertion here would be dominated by the profile, the
/// machine and how many dimensions happen to be cached, and would say nothing about the
/// claim. What is being asserted is the *shape* of the cost — linear in the turns
/// behind the cursor, and the same shape in both directions — and a count is exactly
/// what answers that. Timing belongs in `bench_browse`.
///
/// **The number being corrected.** `anchor_of` walks forward from `(0, 0)`, so it is
/// O(distance from the **front**), not from `pos`. `MyApp::turn_forward` calls
/// `next_turn`, not `next_anchor`, and `next_turn` snaps first — so a *forward* press in
/// manga mode is that whole walk plus one addition. Three places said otherwise
/// (`anchor_of`'s doc, the design spec at "Why this needs no page map", and RULING 4),
/// and the spec's claim is load-bearing: it is what says no pairing table is needed.
///
/// **And the direction is the wrong way round in RULING 4**, which said "forward
/// navigation - the hot path for hold-to-browse - stays O(1)". Both directions are the
/// same walk: `prev_anchor` also walks forward from `(0, 0)`. Forward only adds
/// `next_anchor`'s single addition on top. So the two are the same order of magnitude
/// and the honest statement is "**either direction costs a walk from the front**",
/// with forward marginally worse.
///
/// What this does **not** claim: that it is slow. Every iteration is four lookups in
/// caches that are already warm, against a preview decode measured in hundreds of
/// milliseconds, and this suite has already spent 117-385 ms on an *actually* quadratic
/// rule (`flat`, since replaced by `is_lead`) to make it 10x cheaper than that. The
/// cost is not a hang and not a frame-time risk. It is a cost that is **linear in
/// gallery size**, which is what a reader with a 2000-file volume pays on every press
/// at the back of it, and the deferred fix — a per-gallery anchor list — is recorded in
/// `TODO.md`.
///
/// The comparison the counter makes exact: the same `Gallery` pattern at 4 files and at
/// 200, asked about the **last** turn in both. The reads must be proportional to the
/// turn count (2 turns vs 100), and the per-turn figure must be identical, so a walk
/// that early-exited would fail on the total and a walk that grew faster than linearly
/// would fail on nothing measurable here — which is stated rather than papered over.
#[test]
fn a_navigation_walk_costs_one_turn_len_per_turn_behind_the_cursor() {
    /// Reads through `dims`, which is what `turn_len` reads, wrapped so the same
    /// `Gallery` answers. `Cell` rather than `AtomicUsize`: single-threaded, and it
    /// keeps `PageCtx`'s methods `&self`.
    struct Counting<'a> {
        gal: &'a Gallery,
        reads: std::cell::Cell<usize>,
    }
    impl PageCtx for Counting<'_> {
        fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
            self.gal.step(from, by)
        }
        fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
            self.gal.step_no_wrap(from, by)
        }
        fn key(&self, pos: PagePos) -> Option<ImgKey> {
            self.gal.key(pos)
        }
        fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
            self.reads.set(self.reads.get() + 1);
            self.gal.dims(pos)
        }
        fn override_shape(&self, pos: PagePos) -> Option<Shape> {
            self.gal.override_shape(pos)
        }
        fn flat(&self, pos: PagePos) -> Option<usize> {
            self.gal.flat(pos)
        }
    }
    /// `dims` reads the walk to `anchor_of(pos)` makes, and the position it landed on.
    fn walk(gal: &Gallery, pos: PagePos, lead: u32) -> (usize, PagePos) {
        let c = Counting {
            gal,
            reads: std::cell::Cell::new(0),
        };
        let at = anchor_of(pos, lead, &c);
        (c.reads.get(), at)
    }

    // All portrait, `loop_back` off, so every turn is a pair of adjacent files and the
    // anchors sit at even indices — which is what makes "the k-th anchor" a position
    // this test can write down rather than one it has to rediscover. 400 files, 200
    // turns: big enough that the constant at the end of the walk is not the number
    // being compared, small enough that the walk is instant.
    let gal = {
        let mut g = turns_gallery(&vec![1u32; 400]);
        g.set_loop_back(false);
        g
    };
    let anchors = anchors_of(&gal, 0);
    assert_eq!(anchors.len(), 200, "400 portrait files make 200 turns");
    // The walk and the anchor list agree, which is the premise for reading anything off
    // the list: `anchor_of` is a separate search from `next_anchor`'s walk.
    for (k, a) in anchors.iter().enumerate() {
        assert_eq!(*a, PagePos { index: 2 * k, page: 0 });
        assert_eq!(walk(&gal, *a, 0).1, *a, "anchor_of disagrees at turn {k}");
    }

    // **Linear in the turns behind the cursor.** The marginal cost per anchor is measured
    // between two *adjacent* anchors — where the constant cost of starting the walk is
    // not in the difference — and then checked against the total to the far end. Exact
    // equality is the claim, because `anchor_of`'s loop body is a fixed number of
    // `turn_len` calls and nothing about a later turn costs more than an earlier one.
    let first = walk(&gal, anchors[0], 0).0;
    let second = walk(&gal, anchors[1], 0).0;
    let per_turn = second - first;
    assert!(per_turn > 0, "one turn behind cost {per_turn} reads, which is not a cost");
    let last = walk(&gal, anchors[199], 0).0;
    assert_eq!(
        last - first,
        per_turn * 199,
        "the walk is not linear in the turns behind the cursor: {first} reads to the \
         first turn, {second} to the second ({per_turn} per turn), {last} to the 200th \
         - which is not 199 turns' worth."
    );

    // **And it is not O(1)**, which is what three places claimed. 199 times the
    // per-turn cost is the number that claim denies; asserting it makes the correction
    // fail if someone puts the O(1) prose back without the code.
    assert_eq!(
        last - first,
        per_turn * 199,
        "the walk reads {last} dims to find the last anchor of a 400-file gallery, which \
         is not the bounded number of local reads the doc comment used to claim"
    );

    // **And both directions walk.** `prev_anchor` also starts at `(0, 0)` and compares
    // successors, so it walks from the front to find the turn *before* the cursor's —
    // the same cost, and RULING 4's "forward navigation — the hot path for hold-to-
    // browse — stays O(1)" had the direction backwards. Only `next_anchor`'s single
    // addition is forward-only, so a forward press is that walk plus one addition.
    //
    // Asked from the last of 200 turns, which is the maximum for both. `prev_anchor`
    // asks one `turn_len` per iteration where `anchor_of` asks two, so it lands at
    // about half (995 against 1990 here); **that ratio is the point**, not a detail:
    // the two directions are within a constant of each other and neither is free.
    // Asserted as a factor-of-four band rather than an equality because the exact
    // constants belong to `anchor_of`'s loop body and are not what this test is about.
    // What it is about is that both are hundreds of reads at the back of a 400-file
    // gallery, and a reader who believes forward is O(1) will size the fix wrongly.
    let prev_reads = {
        let c = Counting {
            gal: &gal,
            reads: std::cell::Cell::new(0),
        };
        let got = prev_anchor(anchors[199], 0, &c);
        assert_eq!(
            got,
            Some(anchors[198]),
            "the premise: one turn back from the last of 200 is the 199th"
        );
        c.reads.get()
    };
    assert!(
        last / 4 <= prev_reads && prev_reads <= last * 4,
        "a backward step from the last of 200 turns read {prev_reads} dims where \
         `anchor_of` at the same position read {last}. Expected the same order of \
         magnitude: they are the same walk from the same place."
    );
    // **The number that would make it O(1), stated as a number.** `prev_anchor` walked from
    // `(0,0)` to the 199th anchor, so it passed 199 turns; at the `per_turn` figure
    // measured above for `anchor_of`'s two reads per iteration and one read per
    // iteration here, that is hundreds of reads rather than a constant. The bound is
    // deliberately loose — the claim being contradicted is "a bounded number of local
    // reads", so anything that scales with the gallery refutes it and anything within a
    // factor of a constant confirms the counter is measuring the walk.
    assert!(
        prev_reads > per_turn * 50,
        "a backward step from the back of a 400-file gallery read {prev_reads} dims, \
         which is not the bounded number of local reads that would make it O(1)"
    );
}
