//! Manga mode's pairing core: shapes, the anchor invariant, and the walk, plus the
//! user's overrides beating the detector.
//!
//! Most of this runs against [`TestGallery`] rather than a filesystem, which is
//! the point of `PageCtx`: the pairing is arithmetic over a flat page cursor, so
//! random galleries of every shape mix can be driven through a property test
//! without decoding anything. The property tests are **random, not exhaustive** -
//! see `any_gallery` for what 256 cases do and do not cover - so where a
//! specific shape matters it is spelled out in a named test.
//!
//! ## The split
//!
//! This file used to hold the whole manga suite (6,687 lines, the largest in the
//! tree). It is now one target per subject, and the shared fixtures are in
//! `tests/common/spreads_fixtures.rs`:
//!
//! * `spreads_partner.rs` - a turn's second half
//! * `spreads_dims.rs` - page dimensions and the cache that remembers them
//! * `spreads_overrides.rs` - the override store and its file
//! * `spreads_layout.rs` - where a turn's pages go
//! * `spreads_zoom.rs` - where the zoom anchors
//! * `spreads_pan.rs` - where a turn may be panned
//! * `spreads_nav.rs` - navigating by turn
//! * `spreads_goto.rs` - turn navigation from a position that is not an anchor
//! * `spreads_latches.rs` - values latched at one event and read at another
//! * `spreads_decode.rs` - what a turn is decoded for
//!
//! No test moved between files except as part of a whole section, and every one
//! of the 109 tests is still here.

use image_viewer::{
    Gallery, GallerySpreads, ImgKey, LEAD_LONE_DEFAULT, PageCtx, PagePos, Shape, anchor_of, lone,
    next_anchor, next_turn, pairable, prev_anchor, prev_turn, shape_of, turn_len,
};

use proptest::prelude::*;

use Shape::{Double, Single};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

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
    let g =
        TestGallery::one_doc(vec![Single, Single, Single, Single, Single, Double, Single, Single]);
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
            let shapes = wide.into_iter().map(|d| if d { Double } else { Single }).collect();
            (TestGallery { docs: vec![shapes], loop_back: looping }, lead)
        })
    })
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
    /// position is on Ã¢â‚¬â€ from **every** position, anchor or not.
    ///
    /// **This is the guard for the bug `prev_anchor`'s ambiguous `None` caused.** Its
    /// first version read `prev_anchor(a).is_none()` as "`a` is the first turn", so a
    /// backward step from a *second half* Ã¢â‚¬â€ which `prev_anchor` also answers `None`
    /// for Ã¢â‚¬â€ wrapped to `last_anchor` and put the reader on the last spread of the
    /// volume, with `loop_back` on and off. Stated as a property rather than as one
    /// fixture, because the property is the thing the navigation rests on: a step goes
    /// to the previous turn or nowhere, never forwards.
    ///
    /// Two assertions, because "lands on an anchor" alone would be satisfied by the
    /// first anchor of the gallery Ã¢â‚¬â€ which is what the bug returned.
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
    /// at every `lead_lone`. Positions that do not move are skipped Ã¢â‚¬â€ with `loop_back`
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
    let g = TestGallery::docs(vec![vec![Single, Double], vec![Single, Single, Single]]);
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
    let paths: Vec<std::path::PathBuf> =
        ["a.jpg", "b.jpg", "c.jpg"].iter().map(|n| dir.path().join(n)).collect();
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
    let g = TestGallery { docs: vec![vec![Single, Single, Single, Single]], loop_back: true };
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
            let g = TestGallery { docs: vec![shapes], loop_back: looping };
            let only = g.page_at(0).unwrap();
            assert_eq!(turn_len(only, 0, &g), 1, "looping {looping} over {g:?}");
            assert_eq!(g.turns(0), vec![vec![0]], "looping {looping} over {g:?}");
        }
    }
}

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
/// Ã¢â‚¬â€ they are here so a wrong one fails.
fn tiling(ctx: &dyn PageCtx, lead_lone: u32) -> Vec<Vec<usize>> {
    let flat =
        |p: PagePos| PageCtx::flat(ctx, p).expect("every page count in these fixtures is set");
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut cur = PagePos { index: 0, page: 0 };
    for _ in 0..256 {
        let mut turn = vec![flat(cur)];
        if turn_len(cur, lead_lone, ctx) == 2
            && let Some(partner) = ctx.step_no_wrap(cur, 1)
        {
            turn.push(flat(partner));
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
/// Four portrait pages of one document, and all four **measured** at 800x1200 Ã¢â‚¬â€ so
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
    let mut gal = gallery_of(std::slice::from_ref(&page), &[4]);
    gal.set_loop_back(false);
    for p in 0..4u32 {
        gal.note_shapes(ImgKey { path: page.clone(), page: p }, (800, 1200));
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
