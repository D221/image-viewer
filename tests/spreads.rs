//! Manga mode's pairing core: shapes, the anchor invariant, and the walk.
//!
//! Everything here runs against [`TestGallery`] rather than a filesystem, which
//! is the point of `PageCtx`: the pairing is arithmetic over a flat page cursor,
//! so it can be driven through every shape pattern in a property test without
//! decoding anything.

use image_viewer::{
    lone, next_anchor, pairable, prev_anchor, shape_of, turn_len, PageCtx, PagePos, Shape,
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
    /// The stop condition is "a position we have already seen" rather than "the
    /// position we started at", because those differ once looping is on: a
    /// three-page gallery loops `0, 2, 1, 0` and only returns to the start after
    /// passing two other positions. A helper that assumed otherwise would hang
    /// rather than fail, which is the worst way for a test to end.
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
    fn turn_at(&self, a: PagePos, lead_lone: u32) -> Vec<PagePos> {
        let mut turn = vec![a];
        if turn_len(a, lead_lone, self) == 2 {
            if let Some(partner) = self.step(a, 1) {
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

/// Every shape pattern from length 1 to 12, at each leading-lone offset a user
/// can actually reach.
///
/// Exhaustive over the *shapes* rather than over the page counts a reader
/// happens to have, because the interesting inputs are the mixtures: a run of
/// pairs, a wide page mid-run, a wide page at either end, and the offset moving
/// where the odd page out lands.
///
/// **Looping is off here, deliberately.** "Every page is in exactly one turn" is
/// a claim about a *linear* tiling. A looping gallery's tiling is a cycle: a
/// four-page all-`Single` gallery tiles `{0,1}{2,3}` and then wraps onto page 0
/// again, and an odd-length one has a turn that straddles the wrap and so names
/// the same page twice. That is correct behaviour, not a defect, and asserting a
/// partition over it would be asserting something the design does not promise.
/// The looping cases have their own tests -
/// `a_looping_gallery_wraps_at_both_ends` and
/// `the_only_page_of_a_looping_gallery_is_its_own_predecessor` - because they are
/// about the two ways `prev_anchor`'s walk can end rather than about the tiling.
fn any_gallery() -> impl Strategy<Value = (TestGallery, u32)> {
    (1usize..=12, 0u32..=2).prop_flat_map(|(len, lead)| {
        proptest::collection::vec(any::<bool>(), len).prop_map(move |wide| {
            let shapes = wide
                .into_iter()
                .map(|d| if d { Double } else { Single })
                .collect();
            (
                TestGallery {
                    docs: vec![shapes],
                    loop_back: false,
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
    /// Asserted as a sorted multiset rather than a sequence, so it pins the
    /// property that matters and not the order the walk happened to use: a turn
    /// that claimed a page another turn already had would show up as a duplicate
    /// and displace some other page, so equality with `0..n` catches overlap and
    /// stranding in the same comparison.
    ///
    /// This is the check that catches an off-by-one in `turn_len` - a walk that
    /// stepped by 2 unconditionally would strand the last page of an odd-length
    /// gallery, and one that stepped by 1 would pair nothing at all.
    #[test]
    fn the_turns_partition_the_gallery((g, lead) in any_gallery()) {
        let anchors = g.anchors(lead);
        prop_assert!(!anchors.is_empty(), "a non-empty gallery has at least one anchor");

        let mut covered: Vec<usize> = Vec::new();
        let mut previous: Option<usize> = None;
        for &anchor in &anchors {
            let flat = g.flat(anchor).unwrap();
            if let Some(prev) = previous {
                prop_assert!(prev < flat, "anchors must strictly increase: {} then {}", prev, flat);
            }
            previous = Some(flat);
            covered.extend(g.flat_of(&g.turn_at(anchor, lead)));
        }

        let mut sorted = covered.clone();
        sorted.sort_unstable();
        prop_assert_eq!(sorted, (0..g.total_pages()).collect::<Vec<_>>(),
            "every page is in exactly one turn: no overlap, none missing");
        prop_assert!(covered.iter().all(|&f| f < g.total_pages()),
            "no turn names a page outside the gallery");
    }

    /// Forward to the far end and then back the same number of times returns to
    /// the start - the round trip, over every shape pattern.
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

/// The two walks are two implementations of one rule, so they must produce the
/// same anchor list. Checked over the same space as the property tests, because
/// a disagreement only needs one awkward shape to show up and a hand-picked
/// fixture will not find it.
#[test]
fn backwards_agrees_with_forwards() {
    for lead in 0..=2u32 {
        for g in [singles(1), singles(2), singles(7), TestGallery::one_doc(vec![Double, Single])] {
            let anchors = g.anchors(lead);
            let forwards = g.flat_of(&anchors);
            // Seeded with the last anchor: the backward walk *starts* there, and
            // a gallery of one turn has no predecessors to collect.
            let mut backwards = vec![g.flat(*anchors.last().unwrap()).unwrap()];
            let mut cur = *anchors.last().unwrap();
            while let Some(prev) = prev_anchor(cur, lead, &g) {
                backwards.push(g.flat(prev).unwrap());
                cur = prev;
            }
            backwards.reverse();
            assert_eq!(backwards, forwards, "lead_lone {lead} over {g:?}");
        }
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

/// A gallery with looping on wraps instead, and this is the behaviour a task
/// that adds real navigation will lean on. Kept here because the wrap is a
/// property of `step`, and a test that only ever runs with looping off would not
/// notice `step` ignoring the setting at all.
#[test]
fn a_looping_gallery_wraps_at_both_ends() {
    let g = TestGallery {
        docs: vec![vec![Single, Single, Single, Single]],
        loop_back: true,
    };
    assert_eq!(g.flat_of(&g.anchors(0)), vec![0, 2]);
    assert_eq!(next_anchor(g.page_at(2).unwrap(), 0, &g), g.page_at(0));
    assert_eq!(prev_anchor(g.page_at(0).unwrap(), 0, &g), g.page_at(2));
}

/// A one-page gallery with looping on is its own predecessor, in both directions.
///
/// This is the case that decides how `prev_anchor` is written. The only turn is
/// `{0}`, so walking back from page 0 has to land on page 0 - returning `None`
/// would mean the cursor could never step back, and worse, could never step
/// forward either, because `turn_len` would be asked about a position the walk
/// had already rejected.
///
/// It is the one input where **comparing against the successor instead of the
/// anchor is the difference between working and not**. Testing `anchor == a` at
/// the top of the loop returns `None` here, because the loop starts on page 0
/// and page 0 is the position being asked about; the answer only appears one
/// `step` later, as the successor of a real anchor.
///
/// Pinned separately because nothing else covers it: every other gallery in this
/// file has a front page that is genuinely in front of something.
#[test]
fn the_only_page_of_a_looping_gallery_is_its_own_predecessor() {
    for shapes in [vec![Single], vec![Double]] {
        let g = TestGallery {
            docs: vec![shapes],
            loop_back: true,
        };
        let only = g.page_at(0).unwrap();
        assert_eq!(prev_anchor(only, 0, &g), Some(only), "{g:?}");
        // Forward wraps onto itself for the same reason. `turn_len` here is 2,
        // because with looping on `step(0, 1)` is page 0 and `pairable(0, 0)`
        // compares the page with itself - an artefact of wrapping rather than
        // anything a real one-image gallery does, so the length is not asserted.
        assert_eq!(next_anchor(only, 0, &g), Some(only), "{g:?}");
    }

    // With looping off the same gallery has nothing before it, which is the
    // other half of the distinction: `Some(only)` above is a fact about
    // looping, not about one-page galleries.
    let open = TestGallery::one_doc(vec![Single]);
    assert_eq!(prev_anchor(open.page_at(0).unwrap(), 0, &open), None);
}

/// An unmeasured page is judged `Single` and **pairs** optimistically, so a turn
/// is never stranded waiting for a dimension read. The spec's §5 rule 3: refusing
/// to pair on missing data would strand a turn, and a stranded turn is worse than
/// a pair that turns out to be wrong.
///
/// The turn is of *two*, not one - which is the whole point, and the reason the
/// page is placed second. At flat 0 a `lead_lone` of 1 makes the page front
/// matter and it takes its own turn whatever its dimensions say, which would make
/// this test pass for the wrong reason.
#[test]
fn an_unknown_page_pairs_and_is_a_turn_of_two_until_it_is_measured() {
    struct Unknown;
    impl PageCtx for Unknown {
        fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
            let to = from.page as isize + by;
            (0..2).contains(&to).then_some(PagePos { index: 0, page: to as u32 })
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

    // And a zero height is a page rather than a division by zero.
    assert_eq!(shape_of(page, &Dims(2000, 0)), Single);
}
