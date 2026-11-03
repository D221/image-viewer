//! Turn navigation from a position that is not an anchor.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! The cursor is not always on an anchor: a grid pick leaves the page alone, a
//! start-up or resume lands on page 0 of the file it resumed, and a `Del` keeps the
//! freed index. `prev_anchor` answers `None` both for "the first turn" and for "not
//! an anchor", and reading the two as one answer is the bug this file exists for:
//! Left from a second half jumped to the last spread of the volume.

use image_viewer::{
    anchor_of, lone, prev_anchor, turn_len, turn_partner, Gallery, ImgKey, PageCtx, PagePos, Shape,
    LEAD_LONE_DEFAULT,
};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

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
    let src = main_src();

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
/// This is the invariant that lets `turn_partner_at` skip `anchor_of` Ã¢â‚¬â€ an
/// O(distance from the front) walk Ã¢â‚¬â€ on the per-frame path that derives the turn's
/// second half from where `shown` is. Without it, the derivation would have to snap,
/// and snapping every frame is the cost the deferred anchor-list fix exists to remove.
/// So it is asserted rather than left in `main.rs`'s doc comment, where a claim about
/// `Shape` resolutions cannot be checked.
///
/// The argument, stated as the property: `lone(anchor)`'s three arms are `is_lead`,
/// `shape_of(anchor) == Double` and `!pairable(anchor, next)`. Learning resolves a
/// dimension from unknown Ã¢â‚¬â€ which `shape_of` reads as [`Shape::Single`] Ã¢â‚¬â€ to `Single`
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
/// really is wide in the shape cache rather than named in a comment Ã¢â‚¬â€ `mark_wide`'s
/// doc explains why it goes through `note_shapes`.
#[test]
fn an_anchor_stays_an_anchor_as_the_pairing_learns() {
    // Four files, and file 2 is a two-page scan whose *second* page is wide, so the
    // learning that collapses a turn lands on a page that is not one of the anchors.
    let mut gal = turns_gallery(&[1, 1, 2, 1]);
    gal.set_loop_back(false);
    mark_wide(&mut gal, 2, 1);

    for lead in 0..=2u32 {
        // The anchor list **by walking** Ã¢â‚¬â€ `anchors_of` is `next_anchor` repeatedly,
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
    // Two galleries over the same files Ã¢â‚¬â€ four files, the third with two pages Ã¢â‚¬â€ one
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
    // anchors sit at even indices Ã¢â‚¬â€ which is what makes "the k-th anchor" a position
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
    // between two *adjacent* anchors Ã¢â‚¬â€ where the constant cost of starting the walk is
    // not in the difference Ã¢â‚¬â€ and then checked against the total to the far end. Exact
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
    // successors, so it walks from the front to find the turn *before* the cursor's Ã¢â‚¬â€
    // the same cost, and RULING 4's "forward navigation Ã¢â‚¬â€ the hot path for hold-to-
    // browse Ã¢â‚¬â€ stays O(1)" had the direction backwards. Only `next_anchor`'s single
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
    // deliberately loose Ã¢â‚¬â€ the claim being contradicted is "a bounded number of local
    // reads", so anything that scales with the gallery refutes it and anything within a
    // factor of a constant confirms the counter is measuring the walk.
    assert!(
        prev_reads > per_turn * 50,
        "a backward step from the back of a 400-file gallery read {prev_reads} dims, \
         which is not the bounded number of local reads that would make it O(1)"
    );
}
