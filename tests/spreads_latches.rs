//! Values latched at one event and read at another.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! Three defects, one shape: `shown_partner` assigned when a preview was adopted
//! and read every frame; `self.full`'s identity held to be "a path" and read as
//! "an image"; a document's page count learned on arrival and relied on by the step
//! that had already happened. **The review question that finds them is "what is
//! the lifetime of this value and who re-derives it"**, not "is this right?" - each
//! had a passing test or a passing source scan asserting the call existed.
//!
//! The pairing rule is behavioural, against the real `Gallery`. The *wiring* is a
//! source scan, because `main.rs` is a binary; the scan that matters most checks the
//! **position** of a call relative to a brace, not merely its presence, because
//! presence is exactly what was true of the bug.

use image_viewer::{Gallery, ImgKey, LEAD_LONE_DEFAULT, PageCtx, PagePos, anchor_of, next_turn};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

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
    let at =
        src.find(marker).unwrap_or_else(|| panic!("{marker} is gone; update this test to match"))
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
    let key = |i: usize, p: u32| ImgKey { path: gal.files[i].clone(), page: p };

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

/// A multi-page document is only unreachable by `Ã¢â€ â€™` while its page count is
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
    let b = PagePos { index: 1, page: 0 };

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
        Some(PagePos { index: 3, page: 0 }),
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
        Some(PagePos { index: 2, page: 1 }),
        "with the count known the step lands *inside* the document, which is the \
         difference this fix makes"
    );

    // And its pages are then walkable a turn at a time, so the reader reaches the end
    // of the volume by pressing right rather than by jumping. The cursor is moved to
    // the turn the assertion above only *asked* about - `next_turn` is a query, so the
    // two assertions above leave the cursor on `b`.
    gal.goto_page(PagePos { index: 2, page: 1 });
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
    assert!(!step_forward(&mut gal, lead), "there is a turn after the gallery's last one");
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
/// screen kept the old answer. `Ã¢â€ â€™ Ã¢â€ â€™ Ã¢â€ â€™ M` in a folder of single pages halves the panel
/// and lays out **one** page, so the reader gets a half-width picture beside an empty
/// half; the next `Ã¢â€ â€™` produces a correct spread, which is why it reads as "mostly
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
    let src = main_src();

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
    let gate_end =
        sync.find("if self.previews.contains_key(&cur)").expect("the gate is still there")
            + gate.len();
    let call_at = sync.find("self.sync_shown_partner();").expect("the call is in sync_shown");
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
    let src = main_src();

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
    for want in
        ["self.turn_partner()", "self.gallery.learn_pages(&partner.path)", "partner.path == here"]
    {
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
    let src = main_src();

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
        !central.contains("f.path == shown.path"),
        "`central` is comparing the full buffer by path again, so a document past page 1 \
         draws page 0 over the page on screen.\nBody was:\n{central}"
    );

    // The `f.is(&shown)` keying moved into `central_tiles` (its own module: central.rs
    // passed the 1000-line target). The tile loop must still key each page's buffer by
    // that page's own key, not by path.
    let tiles = fn_body(&src, "fn central_tiles(");
    assert!(
        tiles.contains("f.is(shown)"),
        "`central_tiles` no longer holds `f.is(shown)`, so the tile loop cuts one page's \
         pixels with another's `uv`.\nBody was:\n{tiles}"
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
    let src = main_src();
    let body = fn_body(&src, "fn copy_image_to_clipboard(&mut self, unedited: bool)");

    let at = body.find("if cur.page > 0").unwrap_or_else(|| {
        panic!("the page refusal is gone from `copy_image_to_clipboard`:\n{body}")
    });
    let window = &body[at..(at + 400).min(body.len())];
    assert!(
        window.contains("self.error = Some(") && window.contains("multi-page document"),
        "`Ctrl+C`'s page refusal no longer reports an error naming the reason. A silent\n\
         refusal, or one that reads as success, is worse than copying the wrong page.\n\
         Window was:\n{window}"
    );
    // It has to come **before** the fast path, or the pixels leave the app first.
    let fast =
        body.find("if let Some(f) = self.full.as_ref()").expect("the fast path is still there");
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
