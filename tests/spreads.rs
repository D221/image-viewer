//! Manga mode's pairing core: shapes, the anchor invariant, and the walk.
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
    exif_orientation, lone, next_anchor, page_dims, pairable, prev_anchor, shape_of, turn_len, Gallery,
    ImgKey, PageCtx, PagePos, Shape, PROBE_PREFIX_BYTES,
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

    fn key(&self, _pos: PagePos) -> Option<image_viewer::ImgKey> {
        None
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
#[test]
fn invalidate_path_drops_the_cached_dimensions() {
    // Scoped to `invalidate_path`'s own body so a `forget_shapes` anywhere else in
    // the file cannot stand in for this one — and `remove_current_from_gallery`
    // calls it too, so that is not hypothetical.
    //
    // **By brace matching, not by "up to the next `fn`".** The first version split
    // on `"\n    fn "`, which is wrong in the direction that matters: `split` always
    // yields at least one item, so when the delimiter did not match — `invalidate_path`
    // last in the impl, or re-indented — the "body" became the whole rest of the
    // file, the later `forget_shapes` stood in for this one, and the scan passed
    // **vacuously**. It could not fail; that is the opposite of what a guard is for.
    //
    // The depth starts at **0**, not 1, and that is load-bearing rather than
    // incidental. It is 0 because the scan starts *before* the opening brace — the
    // signature's `&mut self, p: &Path, ctx: &Context` has none — so the first `{`
    // is the function's and the matching `}` is the one that ends it. Starting at 1
    // (the shape `keybindings.rs::dispatch_region` uses, correctly, because it
    // begins *after* `if !typing {` and so has already consumed a brace) misses
    // that closing brace and runs on until the file's own braces happen to balance,
    // 60 KB later. **That was the second version of this test, and it passed with the
    // call deleted.** Verified by mutation, not reasoned about.
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
    let start = src
        .find("fn invalidate_path(")
        .expect("invalidate_path is gone; update this test to match")
        + "fn invalidate_path(".len();
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
    let end = end.expect("could not find the end of invalidate_path: its braces do not \
                         balance, or the signature changed shape. Update this test.");
    let body = &src[start..end];

    assert!(
        body.contains("forget_shapes("),
        "invalidate_path no longer forgets the cached dimensions, so a re-exported\n\
         file keeps its old page's shape and every spread after it stays mis-paired\n\
         for the session, with nothing on screen to say so.\nBody was:\n{body}"
    );
}
