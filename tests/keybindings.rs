//! Which keys do what.
//!
//! These are assertions about `src/main.rs` as **text**, not about a running app.
//! That is normally a weak thing to test â€” the source is not the behaviour â€” and it
//! is here anyway because of what it protects.
//!
//! The rule is that a bare letter may only do something that cannot lose data.
//! `M` (move) and `C` (copy) used to be bound to actions that write to the
//! filesystem, on a keystroke with nothing between the key and the file; `C` is now
//! Crop, which touches nothing until `Ctrl+S`, and `M` is manga mode. Nothing about
//! that is visible at runtime â€” there is no way to observe "a shortcut does not
//! exist" except by pressing it â€” and the cost of the regression is somebody's
//! photo moved by a stray keypress. A source scan is the only place it can be
//! caught.
//!
//! The second half of the file is about *what a key means* rather than which keys
//! exist: there is one direction rule, every navigation input is handed to it, and
//! the wheel is the one input it deliberately ignores. The reason that needs pinning
//! is OpenComic, which inverts the navigation keys for right-to-left manga and omits
//! `Space` from its set â€” so Space silently becomes "previous page" there.
//!
//! What it deliberately does *not* do is check that every bound key *works*. That
//! needs a running app and a synthetic keystroke, and synthetic input injection
//! silently does nothing over RDP, so such a test would be green forever without
//! ever having pressed anything. See `AGENTS.md`.

use image_viewer::{KEY_ACTIONS, default_binding};

/// The region that reads keys and dispatches on them: the body of the
/// `if !typing && self.capturing.is_none() {` block, found by matching braces.
///
/// Scoped rather than the whole file so "does a bare key reach a file-writing
/// action" is a question about the *dispatch*, not about the Move/Copy buttons,
/// which call the same actions on a click. Brace matching rather than "up to the
/// next `fn`", because that overshot into the panel code and made this test fail on
/// the very buttons it is supposed to be ignoring.
fn dispatch_region() -> String {
    dispatch_region_in(&source())
}

/// [`dispatch_region`] against a caller-supplied source.
///
/// Separate so a test that scans a *comment-stripped* copy can get the same block
/// back: slicing the raw file with a block found in the stripped one finds nothing,
/// and `split` then returns the whole file - which is how
/// `only_delete_reaches_the_recycle_bin` spent a while asserting against a region
/// that was not there.
fn dispatch_region_in(src: &str) -> String {
    let anchor = "if !typing && self.capturing.is_none() {";
    let start =
        src.find(anchor).expect("the key-dispatch block is gone; update this test to match")
            + anchor.len();
    let mut depth = 1i32;
    let mut end = src.len();
    for (i, b) in src.as_bytes()[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    assert!(depth == 0, "unbalanced braces in the key-dispatch block");
    src[start..end].to_owned()
}

fn source() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut texts: Vec<String> = Vec::new();
    collect_src(&root, &mut texts);
    texts.sort();
    texts.join("\n")
}

fn collect_src(dir: &std::path::Path, texts: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_src(&path, texts);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            texts.push(std::fs::read_to_string(&path).expect("read"));
        }
    }
}

/// The body of `fn <sig>` in `src/main.rs`, found by **brace matching**.
///
/// Same helper as `tests/common/spreads_fixtures.rs::fn_body` and the same reason it starts its
/// depth at 0: the scan begins *before* the function's `{` (the signature has
/// none), so the first brace it meets is the function's. Starting at 1 misses the
/// closing brace and runs on until the file's own braces happen to balance - and
/// a scan like that passes with the thing it is looking for deleted.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src.find(sig).unwrap_or_else(|| panic!("{sig} is gone; update this test to match"))
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
    let end = end.expect(
        "could not find the end of the function: its braces do not \
                         balance, or the signature changed shape. Update this test.",
    );
    &src[start..end]
}

/// `src/main.rs` with every `//` line comment removed.
///
/// **Needed because a scan of the whole file reads the explanations as well as the
/// code**, and this file's own subject matter is code that must not appear: the
/// comment above the navigation read spells out `step(ctx, true)` as the thing that
/// must never exist, so a scan that counted it would fail on the description of the
/// defect. Same approach as `tests/unit.rs::strip_line_comment`, including the string
/// handling, because a `//` inside a string (a path, a URL) is not a comment.
///
/// Not applied to the scans that *want* the text: `bound_bare` reads whole
/// expressions and `dispatch_region` finds a block by its literal opening, and
/// stripping would only introduce a way for the two to disagree about what the file
/// says.
fn code_only(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        let b = line.as_bytes();
        let (mut in_str, mut in_chr, mut esc) = (false, false, false);
        let mut cut = line.len();
        for (i, &c) in b.iter().enumerate() {
            if esc {
                esc = false;
                continue;
            }
            match c {
                b'\\' if in_str || in_chr => esc = true,
                b'"' if !in_chr => in_str = !in_str,
                b'\'' if !in_str => in_chr = !in_chr,
                b'/' if !in_str && !in_chr && i + 1 < b.len() && b[i + 1] == b'/' => {
                    cut = i;
                    break;
                }
                _ => {}
            }
        }
        out.push_str(&line[..cut]);
        out.push('\n');
    }
    out
}

/// Is `letter` bound to a **bare** press â€” one that fires with no modifier held?
///
/// Answered from the shipped defaults table (`KEY_ACTIONS`), not from reads:
/// every binding in the binary goes through the resolver, so the defaults
/// table is where bare chords live. A single uppercase letter with no `+`.
/// User overrides can move any of these (that is the feature), but the
/// shipped default a fresh install gets is what this pins.
fn bound_bare(letter: char) -> bool {
    KEY_ACTIONS.iter().any(|(_, chord, _)| chord.len() == 1 && chord.chars().next() == Some(letter))
}

/// The bare letters this app allows, and what each one is allowed to do.
///
/// **Adding a line here is a claim that the action cannot lose data.** That is the
/// point: the next person to bind a key to something writes their own name next to
/// it and has to think about it, rather than adding a `key_pressed` and never
/// thinking about it at all. `no_unexpected_bare_letter_shortcuts` fails until they
/// do.
const BARE_LETTERS: &[(&str, &str)] = &[
    (
        "B",
        "flag/unflag the current image - this WRITES a .xmp sidecar beside the \
         file, which is an extension of the bare-letter rule; it is accepted \
         because a flag is a value with no wrong answer, it never touches the \
         image itself, and the write is atomic. See the spec for why undo \
         deliberately does not cover it.",
    ),
    ("C", "crop mode - a view transform, nothing written until Ctrl+S"),
    ("D", "right-to-left page order - view only, a bool in config.json"),
    ("E", "reveal the containing folder in Explorer - read-only"),
    ("F", "toggle fit-view mode - view only"),
    ("G", "toggle the gallery grid - view only"),
    ("I", "toggle the side and bottom panels - view only"),
    ("K", "pause/resume the animation - view only"),
    ("L", "toggle the left (EXIF) pane - view only"),
    ("M", "manga mode - view only, a bool in config.json"),
    (
        "O",
        "lead-lone offset - writes only to spreads.json under %LOCALAPPDATA%, a file \
         this app owns, so it cannot touch a user's images",
    ),
    ("P", "toggle the right (settings) pane - view only"),
    ("R", "rescan the folder - read-only"),
    ("T", "fit the window to the picture - view only"),
    ("W", "set as desktop wallpaper - writes, but to the OS, never to your photos"),
    (
        "X",
        "toggle one page between single and double - writes only to spreads.json \
         under %LOCALAPPDATA%, the same file O writes",
    ),
];

/// Every bare letter in the app is one somebody signed for.
#[test]
fn no_unexpected_bare_letter_shortcuts() {
    // Sanity: the block really is the one under test. Without this, a refactor that
    // emptied the dispatch would make every assertion below pass for the wrong
    // reason.
    let region = dispatch_region();
    assert!(
        region.contains("crop_key") || region.contains("step("),
        "the key-dispatch block no longer looks like the key-dispatch block:\n{region}"
    );
    assert!(source().contains("Key::Delete"), "expected Delete to still be bound");
    // The manga controls have to be *dispatched* inside this block, which is what
    // makes them stand down while the filter box has focus. Their handlers are named
    // rather than their keys, because the keys are read in the `ctx.input` tuple above
    // the block - as every other key here is - and a key read up there would still be
    // dispatched from inside it. A handler that moved above the block stops being
    // gated by `!typing`, and this is the only assertion that notices.
    for (handler, what) in [
        ("toggle_manga_mode", "M (manga mode)"),
        ("toggle_manga_rtl", "D (right-to-left order)"),
        ("nudge_lead_lone", "O / Shift+O (lead-lone offset)"),
        ("toggle_cursor_shape", "X (single <-> double for this page)"),
    ] {
        assert!(
            region.contains(handler),
            "`{what}` is dispatched outside the `!typing` block, so its handler runs \
             while the filter box has the keyboard. Handlers found in the block:\n{region}"
        );
    }
    // The four letters are read *above* the block, in the `ctx.input` tuple with the
    // other single keys, and that is deliberate â€” `Esc` has to be read before anything
    // acts on it and the panels are drawn last. So "read above the block" is correct and
    // only the dispatch has to be inside it, which is what is asserted above. Checked
    // here as well because a key with **no** read at all would sail through: the
    // handler is in the block, the key is not bound to anything, and the feature is
    // unreachable with every test green.
    for letter in ['M', 'D', 'O', 'X'] {
        assert!(
            bound_bare(letter),
            "`{letter}` is documented in BARE_LETTERS and dispatched inside the block, \
             but it is not read anywhere - so the binding does not exist and the handler \
             is dead code."
        );
    }

    // And each handler *does* something, which `bound_bare` cannot see: a binding that
    // reads the key and then does nothing is a key that appears to work. The four
    // requirements are different, so each is named.
    //
    //  * `M` re-`goto`s, because turning the mode on with the cursor on the second
    //    half of what is now a pair leaves it mid-turn, which is the state every
    //    pairing rule is written not to be asked from.
    //
    //    **What the re-`goto` is for has changed, and the assertion below has to stop
    //    claiming otherwise.** It used to say "without the goto the turn it changed is
    //    not recomputed and the reader sees nothing happen", and that was the load-bearing
    //    claim for the whole fix wave this test sits next to â€” it named the re-`goto` as
    //    the thing that refreshes the pairing. It is not. `shown_partner` is now derived
    //    every frame from `shown`'s own position (`sync_shown_partner` in `src/main.rs`),
    //    so the painting updates without a `goto` at all; the `goto` is still required
    //    for three other reasons and is still load-bearing for each:
    //
    //      * **the cursor snap** (`snap_to_turn_anchor`), which is the only thing that
    //        puts a cursor the app assigned by *file index* back on an anchor;
    //      * **the shape and page re-learn**, so the new pairing is measured rather
    //        than planned against what was known before; and
    //      * **the loader re-plan** (`schedule`), which is what asks for the partner's
    //        decode â€” the picture being laid out is not the picture being fetched, and
    //        without a `schedule` a newly-chosen partner has no job and stays blank.
    //
    //    The old wording survived a fix it had become false about, which is the failure
    //    mode this suite is built to catch everywhere else; `tests/spreads.rs`'s
    //    `the_turns_second_half_is_derived_not_latched` is the scan that now owns the
    //    derivation, and it fails when the call is moved back inside the adopt gate.
    //  * `D` must **not** re-`goto`: the turn's membership has not changed, and a
    //    `goto` would reset the full-res dwell timer and re-plan the loader queue for
    //    a picture that did not change. Asserted as an absence, which is the harder
    //    direction and the one a reader is most likely to "fix" by adding a `goto`.
    //  * `O` and `X` both write `spreads.json`, which is what makes a bare letter
    //    safe for them, so a dropped save is the regression.
    let whole = code_only(&source());
    let (m, d) = (
        fn_body(&whole, "fn toggle_manga_mode(&mut self, ctx: &Context)"),
        fn_body(&whole, "fn toggle_manga_rtl(&mut self, ctx: &Context)"),
    );
    assert!(
        m.contains("self.goto(ctx)") && m.contains("self.config.save(&self.config_path)"),
        "`M` does not re-goto and save, so the mode flips and is not persisted and the \
         cursor is left mid-turn.\nBody was:\n{m}"
    );
    assert!(
        !d.contains("self.goto(ctx)"),
        "`D` re-gotos. The turn's membership does not change when the reading direction \
         does - the same pages in the same pairs, laid out the other way round - so this \
         resets the full-res dwell timer and re-plans the loader queue for a picture that \
         did not change. `toggle_manga_rtl`'s doc comment says so; this is what stops a \
         reader 'fixing' it.\nBody was:\n{d}"
    );
    for (sig, what) in [
        ("fn nudge_lead_lone(&mut self, by: i32, ctx: &Context)", "O / Shift+O"),
        ("fn toggle_cursor_shape(&mut self, ctx: &Context)", "X"),
    ] {
        let body = fn_body(&whole, sig);
        assert!(
            body.contains("self.save_spreads()") && body.contains("self.goto(ctx)"),
            "`{what}` ({sig}) does not both save the overrides and re-goto. Without the \
             save the decision is forgotten at exit; without the goto the cursor is not \
             re-snapped onto a turn anchor, the new pairing is not re-measured, and the \
             loader is not re-planned - so a newly-chosen partner has no job and its half \
             of the turn stays blank. Note this is NOT what repaints the spread: the \
             second half is derived every frame from where `shown` is \
             (`sync_shown_partner`), which `tests/spreads.rs` pins by position.\n\
             Body was:\n{body}"
        );
    }

    let allowed: Vec<char> = BARE_LETTERS.iter().map(|(k, _)| k.chars().next().unwrap()).collect();
    for letter in 'A'..='Z' {
        if !bound_bare(letter) {
            continue;
        }
        assert!(
            allowed.contains(&letter),
            "`{letter}` is bound to a bare keystroke but is not in BARE_LETTERS in \
             tests/keybindings.rs.\nAdd it there with a note saying why the action \
             cannot lose data, or give the key a modifier."
        );
    }
    // And nothing in the list has quietly been unbound, which would leave a lie in
    // the comment.
    for (key, why) in BARE_LETTERS {
        assert!(
            bound_bare(key.chars().next().unwrap()),
            "`{key}` is documented ({why}) but is no longer bound"
        );
    }
}

/// Every action id in the defaults table is actually read somewhere in the
/// binary: a table row nobody dispatches is a binding that does nothing,
/// which reads exactly like a working key that silently stopped. (This is
/// the test that would have caught `fit_win` vs `fit_window`.)
#[test]
fn every_listed_action_is_read_in_the_binary() {
    let src = source();
    for (id, _, _) in KEY_ACTIONS {
        assert!(
            src.contains(&format!("\"{id}\"")),
            "action `{id}` is listed in Settings but never read â€” rebindable on screen, dead in practice"
        );
    }
}

/// `C` is Crop, and Crop touches nothing on disk. The pairing matters: this is the
/// letter that used to copy a file.
#[test]
fn c_is_crop_and_does_not_write_to_disk() {
    assert!(bound_bare('C'), "C should still be bound, to crop mode");
    assert_eq!(
        default_binding("crop"),
        "C",
        "crop must default to a bare C, matched exactly so Ctrl+C stays 'copy image'"
    );
    let src = source();
    assert!(src.contains("crop_key"), "C should feed the crop toggle");
}

/// Move and Copy are buttons now. Their handlers must still exist, or removing the
/// keys would have quietly removed the features.
///
/// **`M` is bound again, to manga mode** - the letter is worth having for a mode that
/// gates a whole second half of the viewer, and this test is where that decision
/// belongs. What it must *not* become is the old binding, so the claim this pins has
/// changed from "no bare key reaches `M`" to "the bare key that reaches `M` does not
/// move a file": the assertion is now that `M`'s handler is `toggle_manga_mode`, and
/// the ones below still say that nothing reachable from a bare key writes to somebody's
/// pictures.
#[test]
fn move_and_copy_are_still_reachable_just_not_by_key() {
    let src = source();
    // `M` is bare-bound, to a view-only toggle. Its four siblings are checked in
    // `no_unexpected_bare_letter_shortcuts`, which is where the `!typing` gate and the
    // "the handler actually does something" assertions live.
    assert!(bound_bare('M'), "M should be bound, to manga mode");
    let m_body = fn_body(&src, "fn toggle_manga_mode(&mut self");
    assert!(
        m_body.contains("self.config.manga_mode = !self.config.manga_mode"),
        "`M` now does something other than toggling manga mode. It used to move a file \
         on disk from one keystroke, which is why it was unbound; give the new action \
         its own key rather than this one.\nBody was:\n{m_body}"
    );
    assert!(
        !m_body.contains("transfer_current") && !m_body.contains("delete_current"),
        "`M` reaches a file-writing action."
    );
    for action in ["transfer_current(ctx, false)", "transfer_current(ctx, true)"] {
        assert!(src.contains(action), "{action} is gone - the button should still work");
    }
    // And no key may reach a file-writing action through the dispatch at all.
    let region = dispatch_region();
    assert!(
        !region.contains("transfer_current"),
        "a bare key is calling transfer_current, which moves or copies files"
    );
    // `delete_current` is deliberately *not* asserted absent: `Del` dispatches to
    // it, which is the one accepted exception. It cannot be reached by a bare
    // letter, because `no_unexpected_bare_letter_shortcuts` allows no letter at all
    // that is not in BARE_LETTERS. **That used to rest on an argument about `D`**
    // - "`D` is not among them" - and `D` is now bound, to the reading direction,
    // so the argument is gone and the property has to be asserted directly. See
    // `only_delete_reaches_the_recycle_bin`, which walks the whole chain.
}

/// `Del` is the only thing in the app that reaches the Recycle Bin, and it goes
/// there rather than deleting.
///
/// Asserted as a **chain** rather than as "no bare letter is near
/// `delete_current`", because the keys are read in one `ctx.input` tuple and
/// dispatched in another, so no single line holds both a letter and the call and a
/// line-local scan would pass whatever the code did:
/// 1. `del` is read from `Key::Delete` and nothing else;
/// 2. inside the dispatch, `if del` is followed *immediately* by `delete_current`;
/// 3. every other call is behind a button click;
/// 4. no `BARE_LETTERS` letter is read on a line that names `delete_current`.
///
/// **Step 1 is the one that moved.** The old text argued the property from `D`
/// being absent from the list; `D` is now bound, to `manga_rtl`, and a reader one
/// stray keypress from the Recycle Bin is the exact regression this file exists to
/// catch.
#[test]
fn only_delete_reaches_the_recycle_bin() {
    let code = code_only(&source());

    // 1. `del` comes from Delete alone: the shipped default is the bare key,
    //    and matching is exact everywhere, so no modifier combo reaches it.
    //    (The old text counted `key_pressed(Key::Delete)` lines; reads now go
    //    through the resolver, so the default table is where this lives.)
    assert_eq!(default_binding("del"), "Delete", "del must default to a bare Delete");

    // 2. In the dispatch, `Del` reaches `delete_current` and nothing between the
    //    two is another condition. Read as an exact shape rather than by looking
    //    back for a brace: the nearest brace before the call is the *closure's*, not
    //    the guard's, so a brace scan passed a bare-letter guard by finding the wrong
    //    one.
    let region = dispatch_region_in(&code);
    let del_at = region
        .find("if del")
        .unwrap_or_else(|| panic!("the `Del` branch is gone from the dispatch:\n{region}"));
    let after = region[del_at + "if del".len()..].trim_start();
    assert!(
        after.starts_with('{'),
        "something is tested between `if del` and `delete_current`, so the Del branch no \
         longer reaches the Recycle Bin directly:\n{after}"
    );
    assert!(
        after.trim_start_matches('{').trim_start().starts_with("self.delete_current(ctx);"),
        "`if del` does not call `delete_current` next, so whatever it does call is the \
         thing one stray keypress reaches:\n{after}"
    );

    // 3. And the calls outside the dispatch are button clicks - the bottom panel has
    //    always had a Delete button, and a click is not a keystroke.
    let outside: String = code.split(region.as_str()).collect::<Vec<_>>().join("\n");
    let mut from = 0usize;
    let mut seen = 0usize;
    while let Some(rel) = outside[from..].find("self.delete_current(") {
        seen += 1;
        let at = from + rel;
        let head = &outside[..at];
        let start = head.len().saturating_sub(300);
        assert!(
            head[start..].contains(".clicked()"),
            "`delete_current` is called outside the key dispatch and is not behind a button \
             click, so something that is not a click got a hand on the Recycle Bin:\n{}",
            &head[start..]
        );
        from = at + 1;
    }
    assert!(seen >= 1, "the Delete button is gone - it should still work");

    // 4. And no bare letter names the call on the line that reads it.
    for (letter, why) in BARE_LETTERS {
        let needle = format!("Key::{letter})");
        for line in code.lines().filter(|l| l.contains(&needle)) {
            assert!(
                !line.contains("delete_current"),
                "`{letter}` ({why}) is read on a line that reaches `delete_current`:\n{line}"
            );
        }
    }
}

/// Button and toggle labels must never hardcode a key: every label that
/// names a chord goes through `action_button_text`, which reads the live
/// binding â€” otherwise the first rebind turns every label into a lie, which
/// is exactly the stale-binding defect this test exists for.
///
/// Asserted as absence: any static `"...(X)"` label text left in a widget
/// call fails, so adding a hardcoded label is what breaks, not removing one.
#[test]
fn the_buttons_do_not_advertise_keys_that_are_gone() {
    let src = source();
    for label in ["Moveâ€¦ (M)", "Copyâ€¦ (C)"] {
        assert!(
            !src.contains(label),
            "the `{label}` button still advertises a shortcut that has been removed"
        );
    }
    for hint in [
        "(Ctrl+S)",
        "(Ctrl+C)",
        "(Ctrl+P)",
        "(Ctrl+Shift",
        "(Del)",
        "(Space)",
        "(Home)",
        "(End)",
        "(Enter)",
        "(F11)",
        "(Num",
        "(Comma)",
        "(Period)",
    ] {
        for (n, line) in src.lines().enumerate() {
            let line = line.trim();
            if (line.contains("ui.button(\"")
                || line.contains("selectable_label(")
                || line.contains(".checkbox("))
                && line.contains(hint)
            {
                panic!(
                    "line {} hardcodes a key hint `{hint}` in a widget label â€” route it through \
                     action_button_text so it stays true when rebound:\n{line}",
                    n + 1
                );
            }
        }
    }
    assert!(
        src.contains("action_button_text(&self.config, \"Crop\", \"crop\")"),
        "the crop toggle must show the live chord"
    );
}

/// `Ctrl+C` has to be read from `Event::Copy`, because no key event is emitted.
///
/// This is the one binding whose failure mode is a *silently dead* line of code.
/// The handler read `key_pressed(Key::C) && modifiers.ctrl`, which looks
/// correct, compiles, and can never be true: `egui-winit`'s `on_keyboard_input`
/// converts the clipboard chords and `return`s before pushing the `Key` event
/// it just matched on. So the app compiled, the shortcut was documented, the
/// button next to it worked, and the chord did nothing.
///
/// The premise is checked against real egui rather than asserted in a comment,
/// because the whole test is worthless if egui ever stops converting the chord
/// â€” that is what would make `Event::Copy` the wrong thing to read.
#[test]
fn ctrl_c_is_read_from_the_copy_event_and_not_from_a_key() {
    // Premise, against the real Context: an `Event::Copy` in the input is
    // visible as an event and invisible as a key press. This is the behaviour
    // the handler below is written around.
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(
        egui::RawInput { events: vec![egui::Event::Copy], ..Default::default() },
        |ctx| {
            let copy_seen = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
            let key_seen = ctx.input(|i| i.key_pressed(egui::Key::C) && i.modifiers.command);
            assert!(copy_seen, "egui no longer surfaces Event::Copy in InputState");
            assert!(
                !key_seen,
                "egui now DOES deliver a C key press for the copy chord, so the \
                 handler should go back to reading the key â€” this test's premise \
                 has changed and the fix needs revisiting"
            );
        },
    );
    // A headless pass emits the font atlas and nothing applies it, which epaint
    // debug-asserts on drop. See the `run_pass` helper in tests/unit.rs.
    out.textures_delta.clear();

    let src = source();
    assert!(
        !src.contains("i.key_pressed(egui::Key::C) && i.modifiers.ctrl"),
        "Ctrl+C must not be read as a key press: egui-winit converts the chord \
         to Event::Copy and drops the Key event, so this line can never fire."
    );
    assert!(
        src.contains("egui::Event::Copy"),
        "the copy chord should be read from `egui::Event::Copy`"
    );
    // The button label is live: it shows the current chord for the copy
    // action, which is Ctrl+C until rebound.
    assert!(
        src.contains("action_button_text(&self.config, \"Copy IMG\", \"copy\")"),
        "the Copy IMG button must show the live copy chord, not a hardcoded one"
    );
}
