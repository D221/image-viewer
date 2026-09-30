//! Which keys do what.
//!
//! These are assertions about `src/main.rs` as **text**, not about a running app.
//! That is normally a weak thing to test — the source is not the behaviour — and it
//! is here anyway because of what it protects.
//!
//! The rule is that a bare letter may only do something that cannot lose data.
//! `M` (move) and `C` (copy) used to be bound to actions that write to the
//! filesystem, on a keystroke with nothing between the key and the file; `C` is now
//! Crop, which touches nothing until `Ctrl+S`. Nothing about that is visible at
//! runtime — there is no way to observe "a shortcut does not exist" except by
//! pressing it — and the cost of the regression is somebody's photo moved by a
//! stray keypress. A source scan is the only place it can be caught.
//!
//! What it deliberately does *not* do is check that every bound key *works*. That
//! needs a running app and a synthetic keystroke, and synthetic input injection
//! silently does nothing over RDP, so such a test would be green forever without
//! ever having pressed anything. See `AGENTS.md`.

/// The region that reads keys and dispatches on them: the body of the
/// `if !typing {` block, found by matching braces.
///
/// Scoped rather than the whole file so "does a bare key reach a file-writing
/// action" is a question about the *dispatch*, not about the Move/Copy buttons,
/// which call the same actions on a click. Brace matching rather than "up to the
/// next `fn`", because that overshot into the panel code and made this test fail on
/// the very buttons it is supposed to be ignoring.
fn dispatch_region() -> String {
    let src = std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs");
    let start = src
        .find("if !typing {")
        .expect("the `if !typing` key-dispatch block is gone; update this test to match")
        + "if !typing {".len();
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
    std::fs::read_to_string("src/main.rs").expect("cannot read src/main.rs")
}

/// Is `letter` bound to a **bare** press — one that fires with no modifier held?
///
/// Scans the whole of `main.rs`, not just one `ui.input` block, because the
/// bindings live in two of them and a key bound in the other one is still a bare
/// key.
///
/// Two things to get right, and both were wrong in the first version:
///
///  * whole identifiers, so `D` does not match `Key::Delete`;
///  * the modifier guard that follows, so `Ctrl+S` is not counted as a bare `S`.
///    `&& !i.modifiers.any()` means "only with nothing held" — that *is* bare.
///    `&& i.modifiers.ctrl` means the opposite.
fn bound_bare(letter: char) -> bool {
    let src = source();
    let needle = format!("Key::{letter})");
    let mut from = 0;
    while let Some(at) = src[from..].find(&needle) {
        let after = from + at + needle.len();
        // Whatever follows the key must not continue the identifier.
        if src[after..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            from = after;
            continue;
        }
        // Read the rest of this expression, up to the comma that ends it.
        let tail_end = src[after..].find(',').map_or(src.len(), |i| after + i);
        let tail = &src[after..tail_end];
        let guarded = tail.contains("&& i.modifiers") && !tail.contains("&& !i.modifiers");
        if !guarded {
            return true;
        }
        from = after;
    }
    false
}

/// The bare letters this app allows, and what each one is allowed to do.
///
/// **Adding a line here is a claim that the action cannot lose data.** That is the
/// point: the next person to bind a key to something writes their own name next to
/// it and has to think about it, rather than adding a `key_pressed` and never
/// thinking about it at all. `no_unexpected_bare_letter_shortcuts` fails until they
/// do.
const BARE_LETTERS: &[(&str, &str)] = &[
    ("C", "crop mode - a view transform, nothing written until Ctrl+S"),
    ("E", "reveal the containing folder in Explorer - read-only"),
    ("F", "fit the image to the window - view only"),
    ("G", "toggle the gallery grid - view only"),
    ("I", "toggle the side and bottom panels - view only"),
    ("R", "rescan the folder - read-only"),
    ("W", "set as desktop wallpaper - writes, but to the OS, never to your photos"),
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
    assert!(
        source().contains("Key::Delete"),
        "expected Delete to still be bound"
    );

    let allowed: Vec<char> = BARE_LETTERS
        .iter()
        .map(|(k, _)| k.chars().next().unwrap())
        .collect();
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

/// `C` is Crop, and Crop touches nothing on disk. The pairing matters: this is the
/// letter that used to copy a file.
#[test]
fn c_is_crop_and_does_not_write_to_disk() {
    assert!(bound_bare('C'), "C should still be bound, to crop mode");
    let src = source();
    assert!(
        src.contains("i.key_pressed(egui::Key::C) && !i.modifiers.any()"),
        "bare C must not fire while a modifier is held, so Ctrl+C stays 'copy image'"
    );
    assert!(src.contains("crop_key"), "C should feed the crop toggle");
}

/// Move and Copy are buttons now. Their handlers must still exist, or removing the
/// keys would have quietly removed the features.
#[test]
fn move_and_copy_are_still_reachable_just_not_by_key() {
    assert!(
        !bound_bare('M'),
        "M should not be bound: it moves a file on disk"
    );
    let src = source();
    for action in ["transfer_current(ctx, false)", "transfer_current(ctx, true)"] {
        assert!(
            src.contains(action),
            "{action} is gone - the button should still work"
        );
    }
    // And no key may reach a file-writing action through the dispatch at all.
    let region = dispatch_region();
    assert!(
        !region.contains("transfer_current"),
        "a bare key is calling transfer_current, which moves or copies files"
    );
    // `delete_current` is deliberately *not* asserted absent: `Del` dispatches to
    // it, which is the one accepted exception. It cannot be reached by a bare
    // letter because `no_unexpected_bare_letter_shortcuts` allows no letter at all
    // that is not in BARE_LETTERS, and `D` is not among them. It also goes to the
    // Recycle Bin rather than deleting.;
}

/// The button labels must not advertise keys that no longer exist, which is how a
/// stale binding survives a change to the handler.
#[test]
fn the_buttons_do_not_advertise_keys_that_are_gone() {
    let src = source();
    for label in ["Move… (M)", "Copy… (C)"] {
        assert!(
            !src.contains(label),
            "the `{label}` button still advertises a shortcut that has been removed"
        );
    }
    assert!(
        src.contains("Crop (C)"),
        "the crop button should advertise C"
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
/// — that is what would make `Event::Copy` the wrong thing to read.
#[test]
fn ctrl_c_is_read_from_the_copy_event_and_not_from_a_key() {
    // Premise, against the real Context: an `Event::Copy` in the input is
    // visible as an event and invisible as a key press. This is the behaviour
    // the handler below is written around.
    let ctx = egui::Context::default();
    let mut out = ctx.run_ui(
        egui::RawInput {
            events: vec![egui::Event::Copy],
            ..Default::default()
        },
        |ctx| {
            let copy_seen =
                ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
            let key_seen = ctx.input(|i| i.key_pressed(egui::Key::C) && i.modifiers.command);
            assert!(copy_seen, "egui no longer surfaces Event::Copy in InputState");
            assert!(
                !key_seen,
                "egui now DOES deliver a C key press for the copy chord, so the \
                 handler should go back to reading the key — this test's premise \
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
    // The button label is the user-facing half of the same promise, and it is
    // still true — but only now.
    assert!(
        src.contains("Copy IMG (Ctrl+C)"),
        "the Copy IMG button should still advertise Ctrl+C"
    );
}
