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
    exif_orientation, load_spread_overrides, lone, next_anchor, page_dims, pairable, prev_anchor,
    save_spread_overrides, shape_of, spread_fit, spread_rects, spread_root_key, spreads_root,
    spreads_tmp, thumb_cache_root, turn_len, turn_partner, Gallery, GallerySpreads, ImgKey, PageCtx,
    PagePos, Rect4, Shape, LEAD_LONE_DEFAULT, LEAD_LONE_MAX, PROBE_PREFIX_BYTES, SPREADS_MAX_BYTES,
    SPREADS_MAX_PAGES,
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
    let rects = spread_rects(PANEL, &pages, 0.5, false);

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
    let rects = spread_rects(PANEL, &pages, 1.0, true);

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
    let scale = spread_fit((PANEL[2], PANEL[3]), &pages);
    let rects = spread_rects(PANEL, &pages, scale, true);

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
    let scale = spread_fit((PANEL[2], PANEL[3]), &pages);
    let rects = spread_rects(PANEL, &pages, scale, false);

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
    let rects = spread_rects(panel, &pages, 0.5, false);

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
        let rects = spread_rects(PANEL, &pages, 1.0, rtl);
        assert_eq!(rects.len(), 2);
        assert_eq!(rects[0][2], 400.0, "rtl={rtl}: rect 0 is the 400-wide page");
        assert_eq!(rects[1][2], 700.0, "rtl={rtl}: rect 1 is the 700-wide page");
    }

    // ...and the two orders really are opposite under `rtl`, which is what makes the
    // assertions above mean something. Screen order fails them there.
    let ltr = spread_rects(PANEL, &pages, 1.0, false);
    let rtl = spread_rects(PANEL, &pages, 1.0, true);
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
        spread_fit(panel, &pages),
        0.375,
        "600.0 / 1600.0: the tall page's own fit decides the turn"
    );
    assert_eq!(
        spread_fit(panel, &pages[..1]),
        0.75,
        "the short page alone would be drawn larger, which is exactly the per-page fit this is not"
    );
    assert!(
        spread_fit(panel, &pages) < spread_fit(panel, &pages[..1]),
        "so the turn's scale is the constrained page's and the short page keeps a margin"
    );
}

/// A page wider than its half — a fold-out insert, a landscape layout page — is
/// fitted into its half and laid out there.
///
/// The case a naive fit gets wrong: fitting it into the *whole* panel gives
/// `800 / 1200 = 0.667` and hands it the entire screen, and the page then reads as a
/// two-page scan drawn across both halves.
#[test]
fn a_wide_insert_page_lays_out_at_the_half_the_seam_is_on() {
    let panel: (f32, f32) = (800.0, 600.0);
    let pages = [(1200.0, 800.0)];
    let fit = spread_fit(panel, &pages);
    let naive = (panel.0 / 1200.0).min(panel.1 / 800.0);
    assert!(
        (fit - naive / 2.0).abs() < 1e-6,
        "fitted into its half, not into the panel: {fit} against {naive} for the whole panel"
    );

    let rtl = spread_rects(PANEL, &pages, fit, true);
    assert_eq!(rtl.len(), 1, "a page comes back, not an empty vector");
    assert_eq!(rtl[0][0], 400.0, "its inner edge is the seam");
    assert!((rtl[0][2] - 400.0).abs() < 1e-3, "as wide as the half it is on");
    assert!(rtl[0][3] <= PANEL[3], "and no taller than the panel");

    let ltr = spread_rects(PANEL, &pages, fit, false);
    assert_eq!(right(ltr[0]), 400.0, "the seam side under ltr is the left half");
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
        let scale = spread_fit(panel, &pages);
        assert!(
            scale.is_finite() && scale > 0.0,
            "panel {panel:?} pages {pages:?} gave the scale {scale}"
        );
        let rects = spread_rects([0.0, 0.0, panel.0, panel.1], &pages, scale, false);
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
    assert_eq!(spread_fit((0.0, 0.0), &[(400.0, 800.0)]), 1.0, "no panel, no fit");
    assert_eq!(spread_fit((800.0, 600.0), &[]), 1.0, "no pages, no fit");
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
