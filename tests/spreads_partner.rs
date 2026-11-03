//! The turn's second half.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! `turn_partner` is the one function here that hands back a *key* rather than a
//! position, because a key is what the loader and every pixel cache are keyed on.
//! That makes it the function the rest of the pipeline can get wrong silently: a
//! key built as `ImgKey::first` of the right path is the wrong picture of a
//! multi-page document, and `Job::path()` - which drops the page - cannot see the
//! difference. So the key is asserted whole, and never through `path()`.

use image_viewer::{
    next_anchor, pairable, prev_anchor, shape_of, turn_len, turn_partner, ImgKey, PageCtx,
    PagePos, Shape,
};

use Shape::{Double, Single};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

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
    // `pairable(0, 0)` would compare the page with itself and say yes Ã¢â‚¬â€ the
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
/// `assert_ne` is the point Ã¢â‚¬â€ it says the fixture cannot pass by accident, so a
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
/// `next_anchor(anchor)` is `step(anchor, turn_len(anchor))` Ã¢â‚¬â€ on any turn of two
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

/// A decode is a measurement, and a measurement can change the pairing Ã¢â‚¬â€ so
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
/// is never stranded waiting for a dimension read. The spec's Ã‚Â§5 rule 3: refusing
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
