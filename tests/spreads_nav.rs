//! Navigating by turn: one press, one turn.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! Everything in the other targets answers "what does a turn look like?". This
//! one answers "where does one press put the cursor" - which is the whole of manga
//! mode's navigation, and the reason a turn needs no list of its own.
//!
//! **These run against a real `Gallery`**, not `TestGallery`, because the thing
//! under test is the *cursor*: which `(file, page)` the walk leaves it on, and
//! whether that is a page of the file it names. Page counts come from
//! `Gallery::set_page_counts` and shapes from `Gallery::note_shapes`, and the files
//! are only names.

use image_viewer::{
    Gallery, ImgKey, LEAD_LONE_DEFAULT, PageCtx, PagePos, Shape, last_anchor, prev_turn, shape_of,
    turn_len, turn_partner,
};

use Shape::Double;

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

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
        Some(ImgKey { path: cover.files[2].clone(), page: 0 }),
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
    assert!(!step_backward(&mut gal, lead), "and no turn before the first one, with looping off");
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
            vec![
                PagePos { index: 0, page: 0 },
                PagePos { index: 2, page: 0 },
                PagePos { index: 4, page: 0 },
                PagePos { index: 6, page: 0 },
            ],
            PagePos { index: 6, page: 0 },
        ),
        (
            LEAD_LONE_DEFAULT,
            vec![
                PagePos { index: 0, page: 0 },
                PagePos { index: 1, page: 0 },
                PagePos { index: 3, page: 0 },
                PagePos { index: 5, page: 0 },
                PagePos { index: 7, page: 0 },
            ],
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
        let expected = if flat.is_multiple_of(2) {
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
        Some(ImgKey { path: gal.files[1].clone(), page: 0 }),
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
        Some(ImgKey { path: partner.clone(), page: 0 }),
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
    assert_eq!(turn_partner(gal.page_pos(), lead, &gal), None, "with nothing left to pair with");
    assert!(
        !step_forward(&mut gal, lead),
        "and one page left is one turn, so with looping off there is no next turn either"
    );
}
