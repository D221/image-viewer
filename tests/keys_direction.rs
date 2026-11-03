//! The direction rule: every navigation input asks [`nav_is_forward`], and the
//! wheel is the one input it deliberately ignores.
//!
//! The reason this needs pinning is OpenComic, which inverts the navigation
//! keys for right-to-left manga and omits `Space` from its set â€” so Space
//! silently becomes "previous page" there. Nothing errors, the arrows are
//! right, and the space bar quietly goes the wrong way. Five inputs, one enum,
//! one function â€” a sixth input is a sixth row that has to say what it means,
//! and it cannot be forgotten in a way nothing notices.
//!
//! The helpers below are twins of `tests/keybindings.rs`'s: one test target
//! cannot import from another, so `source`, `collect_src`, `fn_body`,
//! `collapse`, `code_only` and `dispatch_region_in` exist once per file.
//! The trap note moves with them â€” `code_only` strips `//` comments because
//! a scan of the whole file reads the explanations as well as the code, and
//! this file's subject matter is code that must not appear.
use image_viewer::{NavInput, default_binding, nav_is_forward};
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

/// `src` with all runs of whitespace collapsed to single spaces.
///
/// **For comparing against a statement written out in the test's own formatting.**
/// Rustfmt's line breaking is not a claim about behaviour, and a scan that asserted
/// on the formatted shape would fail on a reflow rather than on a defect. Collapsing
/// also removes the trap of scanning "as far as the first `)`", which is how the
/// `last_anchor` guard passed against a `.filter()` sitting past the end of its
/// window.
fn collapse(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut space = false;
    for c in src.chars() {
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
    }
    out
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
// ---------------------------------------------------------------------------
// One direction rule
// ---------------------------------------------------------------------------
//
// The rest of this file is about *which* keys exist. This section is about what a
// key means, and it is here because of a specific bug in a specific other reader:
// OpenComic inverts the navigation keys for right-to-left manga, and its
// `mangaInvert` set omits `Space` - so in manga mode **Space silently becomes
// "previous page"**. Nothing errors. The app looks correct, the arrows work, and
// the space bar quietly goes the wrong way.
//
// So the rule is that there is exactly one function which knows which way forwards
// is, every navigation input names itself as a [`NavInput`] and is handed to it,
// and nothing anywhere else in the tree is allowed to have an opinion.
//
// The last part is the hard part, and it is not testable from here: `main.rs` is a
// binary and the bindings live in it. What is checkable is the *rule*, below, plus
// the source scans after it - which are weak in the way every scan in this project
// is weak, and are labelled as such.

/// Every navigation key agrees with every other about which way is forwards.
///
/// **This is the test the rule exists for.** `Space`, `PageDown` and the mouse's
/// forward button are all "next" in a left-to-right reader and all "backwards" in a
/// right-to-left one; the only way that stops being true is for one of them to be
/// spelled out somewhere instead of named, which is the OpenComic bug exactly.
///
/// The shape is *agreement between the variants* rather than a table of expected
/// answers, so adding a sixth navigation input cannot be satisfied by editing the
/// expectations: it has to land in one of the two arms. The two expectations below
/// it are what stop the whole function being "agreeing on `false`".
#[test]
fn every_navigation_key_agrees_about_which_way_is_forwards() {
    for rtl in [false, true] {
        let right = nav_is_forward(NavInput::Right, rtl);
        let next = nav_is_forward(NavInput::Next, rtl);
        let left = nav_is_forward(NavInput::Left, rtl);
        let previous = nav_is_forward(NavInput::Previous, rtl);

        assert_eq!(
            right, next,
            "rtl {rtl}: Space and PageDown (NavInput::Next) disagree with ArrowRight"
        );
        assert_eq!(
            left, previous,
            "rtl {rtl}: PageUp (NavInput::Previous) disagrees with ArrowLeft"
        );
        assert_ne!(
            right, left,
            "rtl {rtl}: the two directions are the same, so the setting does nothing"
        );
        // And the two settings are opposites of each other, which is the whole
        // point of `manga_rtl`.
        assert_ne!(
            right,
            nav_is_forward(NavInput::Right, !rtl),
            "rtl {rtl}: flipping the setting did not flip what ArrowRight means"
        );
    }
    // Written out, so the function cannot be satisfied by making "forwards" mean
    // nothing: with no manga direction in play, a rightward press is forwards.
    assert!(nav_is_forward(NavInput::Right, false));
    assert!(nav_is_forward(NavInput::Next, false));
    assert!(!nav_is_forward(NavInput::Left, false));
    assert!(!nav_is_forward(NavInput::Previous, false));
    // And under a right-to-left book it is exactly reversed - which is the
    // deliberate trade, and the reason `manga_rtl` exists at all.
    assert!(!nav_is_forward(NavInput::Right, true));
    assert!(!nav_is_forward(NavInput::Next, true));
    assert!(nav_is_forward(NavInput::Left, true));
    assert!(nav_is_forward(NavInput::Previous, true));
}

/// The wheel does not follow the arrow direction.
///
/// **It is the one input with no spatial meaning to preserve.** Arrows have a
/// *side* on screen, and a book that opens to the left has to move the hand with
/// it; a wheel delta has only a sign, and that sign already means "onward" in both
/// directions. Mihon excludes it deliberately, and so does this.
///
/// Stated as "unchanged by the setting" and as "different from the arrow", because
/// the two are different claims: a function that mapped the wheel *with* the arrows
/// would satisfy the first in one direction only.
#[test]
fn the_wheel_does_not_follow_the_arrow_direction() {
    for rtl in [false, true] {
        assert!(
            nav_is_forward(NavInput::WheelDown, rtl),
            "rtl {rtl}: wheel-down stopped meaning 'onward'. It is the one input with no \
             side to preserve, so it is the same in both directions."
        );
    }
    // The second half of the claim, and the reason this is not just "wheel is
    // true": **the arrow does change**, so the two agreeing in one direction is the
    // wheel having ignored the setting rather than the setting having no effect. A
    // function that mapped the wheel with the arrows would agree here and disagree
    // under `rtl`, which is what the loop above already rules out.
    assert!(
        !nav_is_forward(NavInput::Right, true),
        "the premise: with right-to-left on, a rightward press is backwards"
    );
    assert_ne!(
        nav_is_forward(NavInput::WheelDown, true),
        nav_is_forward(NavInput::Right, true),
        "under a right-to-left book the wheel is following the arrow direction"
    );
}

/// Every navigation input in the binary names itself and asks the rule.
///
/// **A source scan, and it is the only thing that can see this** - `main.rs` is
/// unreachable from any test, so the five bindings and the hold-to-browse path are
/// exactly the code nothing else in the tree can look at. It is weak in the way
/// every scan in this project is weak: it cannot tell whether the reads happen
/// while the window has focus. What it does catch is the regression that actually
/// happens, which is a sixth input spelled out as a bare `true`.
///
/// Three assertions, each a different way of failing:
///
/// * **No literal direction ever reaches `step`.** `MyApp::step` takes a `bool`, so
///   `self.step(ctx, true)` compiles and reads correctly and is the whole defect.
///   Asserting its absence is stronger than asserting that the right
///   `nav_is_forward` calls are *present*, which a second hand-written direction
///   would satisfy.
/// * **All five variants are named.** A new input added without one shows up here.
/// * **No navigation key is read as `key_down`.** `key_down` is the *held* read, and
///   the only two places it appears are the two halves of hold-to-browse; see the
///   next test.
#[test]
fn every_navigation_input_in_the_binary_asks_the_direction_rule() {
    // Comments stripped: the comment above the navigation read spells out the
    // literal this test forbids, because naming the defect is the only way a reader
    // knows what the assertion is for.
    let src = code_only(&source());

    for literal in ["step(ctx, false)", "step(&ctx, true)", "step(&ctx, false)"] {
        assert!(
            !src.contains(literal),
            "`{literal}` decides a direction on its own. Every navigation input has to \
             name itself as a NavInput and be handed to `nav_is_forward`, so that adding \
             a key cannot produce a second opinion about which way forwards is - which is \
             the defect OpenComic ships, where Space is missing from its mangaInvert set \
             and silently goes the wrong way."
        );
    }

    // The one exception is the slideshow tick: it is not a navigation input
    // at all â€” no key, no side to preserve â€” so forwards is the only thing
    // it can mean, the same argument the wheel comment below makes. It must
    // be the *only* literal-direction call in the binary, and it must live
    // in the slideshow block rather than near any input. (`if self.slideshow`
    // alone is not a unique anchor â€” the status line and the settings button
    // read the same flag â€” so this anchors on the pacing call.)
    let tick_at =
        src.find("slideshow_due(").expect("the slideshow tick is gone; update this test to match");
    let tick = &src[tick_at..(tick_at + 500).min(src.len())];
    assert!(
        tick.contains("self.step(ctx, true)"),
        "the literal-direction step is no longer in the slideshow tick"
    );
    let total = src.matches("step(ctx, true)").count();
    assert_eq!(
        total,
        1,
        "expected exactly one literal-direction step (the slideshow tick):\n{}",
        src.lines().filter(|l| l.contains("step(ctx, true)")).collect::<Vec<_>>().join("\n")
    );

    // `ui` must read the direction rather than decide it.
    let ui = fn_body(&src, "fn ui(&mut self, ui: &mut egui::Ui");
    assert!(
        ui.contains("nav_direction("),
        "`ui` does not call nav_direction, so nothing in the dispatch reads a \
         navigation key."
    );
    assert!(
        ui.contains("nav_is_forward"),
        "`ui` calls nav_is_forward itself, which means it is resolving a direction \
         somewhere outside the table. `nav_direction` is the only place that should \
         turn an input into a `bool`."
    );

    // The wheel is the fifth input, and it is resolved in `central` rather than
    // through the table, because it is not a key press. Spelling it as a literal
    // would be correct today and is exactly the branch that could drift away from
    // the setting without looking wrong.
    assert!(
        fn_body(&src, "fn central(").contains("nav_is_forward(NavInput::WheelDown"),
        "the wheel is not being asked about. It has no side to preserve, so it is the one \
         input the rule answers the same way in both directions - spelling it as a literal \
         would be correct today and is exactly the branch that could drift away from the \
         setting without looking wrong."
    );
}

/// `Home` and `End` reach the *ends* in manga mode, not the first and last **file**.
///
/// `goto_edge` is the whole of that and it lives in the binary, so this is a source
/// scan. It is here because the obvious wrong version â€” leaving `Home`/`End` setting
/// `gallery.index` â€” is nearly invisible: in a folder of single-page images, file 0
/// and page 0 are the same turn and the last file usually *is* the last turn, so the
/// bug reads correctly everywhere except a multi-page document at one end. A test
/// that could only see the folder case would pass against it.
///
/// Four strings, each load-bearing:
///
/// * `goto_edge` in the dispatch, so the keys are not still assigning an index;
/// * `last_anchor` in its body, because the last turn is a walk and **not** a
///   subtraction from the end â€” `lone` says "alone" at the last page whether it opens
///   the final turn or closes it, so nothing asked *at* the end can answer this;
/// * `PagePos { index: 0, page: 0 }` for the first turn, which is the other half of
///   the asymmetry: the walk starts there, so it is its own anchor by construction;
/// * the `manga_mode` branch, since both modes must not do the same thing.
#[test]
fn home_and_end_reach_a_turn_rather_than_a_file() {
    let src = code_only(&source());

    let region = dispatch_region_in(&src);
    assert!(
        region.contains("goto_edge(false, ctx)") && region.contains("goto_edge(true, ctx)"),
        "Home/End no longer both go through `goto_edge`, so one of them is back to \
         assigning a file index. In a folder of single-page images that looks correct; \
         at a multi-page document it lands on the wrong turn."
    );
    assert!(
        !region.contains("self.gallery.index = 0")
            && !region.contains("self.gallery.index = self.gallery.len()"),
        "Home/End are still assigning a file index, which in manga mode is a page of \
         the turn rather than its anchor."
    );

    let body = fn_body(&src, "fn goto_edge(&mut self, end: bool, ctx: &Context)");
    assert!(
        body.contains("last_anchor(self.spreads.lead_lone, &self.page_ctx())"),
        "`goto_edge` no longer asks the library for the last turn with the reader's own \
         offset and overrides, so End has to be computing it some other way. The \
         arguments matter too: a walk with somebody else's `lead_lone` answers about a \
         different tiling.\nBody was:\n{body}"
    );
    // ...and the answer is **used**, not decorated. A `.filter()` or `.and_then()`
    // after the call is a scan that still sees `last_anchor(` and a reader whose End
    // goes nowhere; verified by mutation, because "the call is present" was the
    // assertion before and it passed against exactly that.
    // ...and the answer is **used**, not decorated. A `.filter()` or `.and_then()`
    // after the call is a scan that still sees `last_anchor(` and a reader whose End
    // goes nowhere. **Verified by mutation, and the first version of this assertion
    // passed against exactly that**: it searched only as far as the first `)`, which
    // is the one closing `page_ctx()`, so anything appended after the call was out of
    // range. It is now read against whitespace-collapsed text with the *whole*
    // `let to = if end { ... } else { ... }` as the subject, which is the statement
    // whose shape is the claim.
    let flat = collapse(body);
    assert!(
        flat.contains("let to = if end { last_anchor(self.spreads.lead_lone, &self.page_ctx()) } else { Some(PagePos { index: 0, page: 0 }) }"),
        "the two ends are not the statement this test is pinning, so one of them is being \
         computed differently from what is written above - a `.filter()` on the last \
         turn is the version that matters, because `last_anchor` is still in the source \
         and End still goes nowhere:\n{flat}"
    );
    assert!(
        body.contains("PagePos { index: 0, page: 0 }"),
        "`goto_edge` no longer names the first turn's anchor as (0, 0).\nBody was:\n{body}"
    );
    assert!(
        body.contains("self.config.manga_mode"),
        "`goto_edge` no longer branches on the mode, so both modes do the same thing \
         and one of them is wrong.\nBody was:\n{body}"
    );
}

/// The settings panel offers both double-page settings, and `M`/`D` are named on them.
///
/// The keyboard is how a reader finds a feature nobody documents, and a setting that
/// exists only as a key is a setting a user who has lost the key cannot get back.
/// The label is the user-facing half of the same promise `BARE_LETTERS` pins, and
/// **advertising a key that is not bound is how a stale binding survives a change to
/// the handler** â€” the same trap `the_buttons_do_not_advertise_keys_that_are_gone`
/// exists for.
///
/// The disclosure under them is the other half. `AGENTS.md` requires it beside any
/// cache, and this is not a cache but it is worse in one way: a file that silently
/// re-pairs a whole library must not be enabled quietly. The line has to name the
/// path, because "deleting it clears everything" is not actionable without it.
#[test]
fn the_settings_panel_offers_both_manga_settings_and_says_where_overrides_live() {
    let src = code_only(&source());

    for (base, action, field) in
        [("Double page mode", "manga", "manga_mode"), ("Right-to-left pages", "rtl", "manga_rtl")]
    {
        assert!(
            src.contains(&format!("action_button_text(&self.config, \"{base}\", \"{action}\")")),
            "the settings panel no longer offers `{base}` with its live chord. The keys are how a reader \
             finds this feature, so a checkbox that lost its label is a setting that only \
             exists as a keystroke."
        );
        assert!(
            src.contains(&format!("&mut self.config.{field}")),
            "the `{base}` checkbox is not bound to `config.{field}`."
        );
    }

    // Both must save, or the panel is a lie about persistence: a reader who sets a
    // preference, restarts, and finds it gone has been told the wrong thing. Read out
    // of the panel's own text rather than counted across the file, because a
    // count-of-two would be satisfied by two saves in the renderer row.
    let panel = src.split("Panel::right(\"settings\")").nth(1).expect("the settings panel is gone");
    assert_eq!(
        panel.matches("self.config.save(&self.config_path)").count() >= 2,
        true,
        "the manga checkboxes do not each save the config, so a preference set in the \
         panel is gone at exit."
    );

    // The disclosure: the path, and that deleting it clears the overrides.
    assert!(
        src.contains("Pairing overrides live in") && src.contains("Delete that file to clear"),
        "the manga settings no longer say where `spreads.json` is or that deleting it \
         clears every override. A file that silently re-pairs a library must not be \
         enabled quietly - that is the same disclosure `AGENTS.md` requires beside the \
         thumbnail cache, and it is not actionable without the path."
    );
    assert!(
        src.contains("spreads_root()"),
        "the disclosure does not get the path from `spreads_root()`, so it cannot be showing \
         the real one - and `%LOCALAPPDATA%` is not `%APPDATA%`, so a hardcoded path would \
         be wrong."
    );

    // ASCII only, because a missing glyph is a blank box rather than an error and
    // `tests/unit.rs::no_ui_text_uses_a_glyph_the_font_lacks` covers the files rather
    // than a list. Spelled out here so the check is next to the labels it is about.
    for line in src.lines().filter(|l| l.contains("Manga mode") || l.contains("Right-to-left")) {
        assert!(
            line.is_ascii(),
            "the manga settings labels must be ASCII: `line` has a non-ASCII character, and \
             egui's default font turns that into a blank box rather than an error."
        );
    }
}

/// Every navigation key in the tables is paired with the input it is **supposed** to
/// mean â€” and this is the test that would have caught the OpenComic bug on its own.
///
/// `every_navigation_key_agrees_about_which_way_is_forwards` checks that the rule is
/// self-consistent, and `every_navigation_input_in_the_binary_asks_the_direction_rule`
/// checks that the binary asks it. **Neither can see *which* key means *which*
/// input**, because that mapping is a table in a binary: a version where `Space` were
/// bound to `NavInput::Previous` satisfies both of them perfectly and sends the space
/// bar the wrong way in manga mode. That is not hypothetical â€” it is the exact shape
/// of the defect OpenComic ships, and an earlier draft of the two tests above was
/// verified green against it.
///
/// So the table is read as data. Each `(key, input)` pair is extracted from its line
/// and checked against the pairing the design fixes:
///
/// | key | input | why |
/// |---|---|---|
/// | `â†’` | `Right` | rightward on screen, the one input with a side |
/// | `â†` | `Left` | leftward on screen |
/// | `PageDown` | `Next` | onward, no side |
/// | `PageUp` | `Previous` | back, no side |
/// | Extra2 | `Next` | the browser's forward button |
/// | Extra1 | `Previous` | the browser's back button |
///
/// `Space` was in this table and is out on purpose: it is the zoom toggle's
/// second chord (`fit_alt`, JPEGView's own Space binding), not navigation â€”
/// pinned by `space_is_the_fit_toggle_and_not_a_navigation_key` below.
///
/// The grid's own reads are excluded, because the grid is a different question â€” a
/// highlight moving across a grid has no reading order â€” and are checked to exist
/// separately below, so that excluding them here is not a way to lose them.
#[test]
fn every_navigation_key_is_paired_with_the_input_it_is_supposed_to_mean() {
    let src = code_only(&source());

    // (table name, the pairs that must be in it)
    for (table, expected) in [
        (
            "NAV_KEYS",
            &[
                ("ArrowRight", "Right"),
                ("ArrowLeft", "Left"),
                ("PageDown", "Next"),
                ("PageUp", "Previous"),
            ][..],
        ),
        ("NAV_BUTTONS", &[("Extra2", "Next"), ("Extra1", "Previous")][..]),
    ] {
        // Bounded by the closing `];` rather than by braces, because a `const` has
        // no braces of its own â€” `fn_body` would run to the end of the file and read
        // the *other* table's pairs in as well, which is what it did the first time.
        let at = src
            .find(&format!("const {table}:"))
            .unwrap_or_else(|| panic!("{table} is gone; update this test to match"));
        let after = &src[at..];
        let end =
            after.find("];").unwrap_or_else(|| panic!("{table} is no longer a bracketed list"));
        let body = &after[..end];

        // Only the entry lines. The declaration line itself also contains a comma
        // and two `::`, and taking it produced a phantom pair â€” which the count
        // assertion below caught, and which is why it is here rather than a
        // `filter(...).count()` nobody would question.
        let found: Vec<(&str, &str)> = body
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("(egui::"))
            .filter_map(|l| {
                // Both halves are read off the *comma-separated* fields, so
                // `egui::Key::Space` yields `Space` rather than the last `::`
                // segment of the whole line, which would be the `NavInput` side.
                let mut fields = l.split(',').map(str::trim);
                let key = fields.next()?.rsplit("::").next()?;
                let input = fields.next()?.rsplit("::").next()?;
                Some((key.trim(), input.trim_end_matches(')')))
            })
            .collect();

        assert_eq!(
            found.len(),
            expected.len(),
            "{table} should hold exactly {} pairs on one line each, and {found:?} is what \
             was read out of it. A pair broken across two lines is the likely cause.",
            expected.len()
        );
        for (key, input) in expected {
            let (found_key, found_input) =
                found.iter().find(|(k, _)| k == key).copied().unwrap_or_else(|| {
                    panic!(
                        "{table} has no entry for {key}. The table is the whole of which \
                         key means what, so a missing pair is a key that navigates to \
                         nothing rather than a test that cannot see it."
                    )
                });
            assert_eq!(found_key, *key, "{table}: found the wrong key where {key} was expected");
            assert_eq!(
                found_input, *input,
                "{table} binds {key} to the wrong meaning. This is the whole class of bug \
                 this file's direction section exists for: OpenComic omits Space from its \
                 `mangaInvert` set, so Space silently becomes 'previous page' in manga \
                 mode, and the arrows keep working so nothing looks wrong."
            );
        }
    }

    // The grid reads arrows separately and on purpose. Their *use* is checked in
    // `the_grid_keeps_its_own_arrows_and_does_not_follow_the_reading_direction`;
    // only their existence belongs here, so that the exclusion above cannot quietly
    // become a loss rather than a decision.
    let ui = fn_body(&src, "fn ui(&mut self, ui: &mut egui::Ui");
    assert!(
        ui.contains("grid_right") && ui.contains("grid_left"),
        "the grid's own arrow reads are gone. They are excluded from the table above \
         because a grid highlight has no reading order, not because they stopped \
         mattering."
    );
}

/// Picking a grid cell lands on the turn **anchored at that file's first page**.
///
/// `pick_grid_selection` is in the binary, so this is a source scan, and the claim is
/// narrow on purpose: `goto_index` rather than an assignment to `gallery.index`.
///
/// **The bug it exists for is `gallery.index = â€¦`, which looks right.** A grid cell
/// names a *file*, so picking one has to mean that file's **first** page, and
/// `goto_index` is what says so â€” it resets `page` as well as `index`. Assigning
/// `index` leaves the page wherever the cursor was, so picking file 3 while sitting
/// on page 2 of file 7 arrived on page 2 of file 3. That is a page of the picked
/// file, which is why nothing looked wrong; in manga mode it is also a *second half*,
/// so `goto`'s `snap_to_turn_anchor` walks the cursor somewhere the reader did not
/// pick.
///
/// `goto_index` also refuses an index past the end of the list, which the
/// `min(len - 1)` in the same expression already guarantees â€” so this is about which
/// setter is used, not about the bounds.
#[test]
fn picking_a_grid_cell_lands_on_the_turn_at_that_files_first_page() {
    let src = code_only(&source());
    let body = fn_body(&src, "fn pick_grid_selection(&mut self, ctx: &Context)");

    assert!(
        body.contains("self.gallery.goto_index("),
        "`pick_grid_selection` no longer goes through `goto_index`, so a pick can land on \
         a page other than the picked file's first - which in manga mode is a second half, \
         and `snap_to_turn_anchor` then moves the cursor somewhere else entirely.\nBody \
         was:\n{body}"
    );
    assert!(
        !body.contains("self.gallery.index ="),
        "`pick_grid_selection` assigns `gallery.index` directly, which leaves the page \
         alone. Assigning the index was the bug: the cell names a file, and the reader \
         expects its first page.\nBody was:\n{body}"
    );
}

/// The grid's arrows are **not** the navigation rule's arrows, and that is a
/// decision rather than an oversight.
///
/// A highlight moving across a grid drawn left to right has a side, and the grid has
/// one cell per file with no reading order at all â€” so under `manga_rtl` a rightward
/// press must still move the highlight right, or the selection walks backwards while
/// the *image* it opens went forwards.
///
/// So the grid branch reads its own keys and must keep doing so. A version that routed
/// it through `nav_direction` would move the highlight the wrong way in every
/// right-to-left library with nothing on screen to say why, and no other test in the
/// tree would notice: `tests/spreads.rs` asks what a turn contains, not which way a
/// grid selection walks.
///
/// **Space is included in the grid's read**, because it always was. A key that stops
/// working because a setting is on is worse than one that never did, and the grid's
/// reads are excluded from the direction table precisely so that stays true.
#[test]
fn the_grid_keeps_its_own_arrows_and_does_not_follow_the_reading_direction() {
    let src = code_only(&source());
    let region = dispatch_region_in(&src);

    // The grid's branch, taken as text: from `if self.gallery_view {` to the
    // `else` that ends it. **Cut at the navigation branch rather than at the first
    // `} else if`**, because the grid branch is a chain of its own and the first
    // `else if` is one of its arms - which made the slice empty.
    let grid_branch = region
        .split("if self.gallery_view {")
        .nth(1)
        .and_then(|after| after.split("} else if let Some(dir) = step_dir").next())
        .unwrap_or_else(|| {
            panic!(
                "the grid branch of the dispatch is gone, or no longer ends where it \
                     did:\n{region}"
            )
        });
    assert!(
        grid_branch.contains("grid_right") && grid_branch.contains("grid_left"),
        "the grid branch no longer uses its own arrow reads:\n{grid_branch}"
    );
    assert!(
        !grid_branch.contains("step_dir"),
        "the grid branch is now reading the navigation direction, so under `manga_rtl` a \
         rightward press moves the highlight left. A grid has no reading order - one cell \
         per file, drawn left to right - so its arrows mean screen directions.\n{grid_branch}"
    );

    // And the grid's keys are still the arrows plus Space: the defaults
    // table is where that lives now that reads go through the resolver.
    // `Space` has always moved the grid highlight, and dropping it is the
    // kind of regression that reads as 'the key is flaky'.
    for (action, key) in [
        ("grid_up", "ArrowUp"),
        ("grid_down", "ArrowDown"),
        ("grid_left", "ArrowLeft"),
        ("grid_right", "ArrowRight"),
        ("grid_right_alt", "Space"),
        ("grid_page_up", "PageUp"),
        ("grid_page_down", "PageDown"),
        ("grid_open", "Enter"),
    ] {
        assert_eq!(
            default_binding(action),
            key,
            "grid action `{action}` no longer defaults to {key}"
        );
    }
}

/// Space is the fit â†” 100% toggle, **not** navigation.
///
/// This is a decision, and it is JPEGView-shaped: `Space` is what JPEGView ships on
/// `IDM_TOGGLE_FIT_TO_SCREEN_100_PERCENTS` (its `KeyMap.txt.default`), the toggle
/// between fitted and actual size â€” which here is the `fit` action's second chord
/// (`F`'s own toggle: on fits every image, off drops to 100%, so space-space walks
/// fit â†’ 100% â†’ fit exactly like JPEGView). Navigation never ships it: an "onward"
/// Space and a zoom Space in one hand is two meanings for one key, and the
/// alt-navigation row (`next_alt`) ships **unbound** rather than on Space, still
/// rebindable by anyone who wants Space navigating again.
///
/// The grid keeps Space for moving the highlight (`grid_right_alt`) â€” a separate
/// read with a separate concern, checked for existence in the grid test above.
#[test]
fn space_is_the_fit_toggle_and_not_a_navigation_key() {
    assert_eq!(
        default_binding("fit_alt"),
        "Space",
        "Space must be the fit toggle's second chord (JPEGView parity), not navigation"
    );
    assert_eq!(
        default_binding("next_alt"),
        "",
        "no shipped navigation key may be Space; alt-navigation starts unbound so the \
         reader's hand cannot be split between a zoom key and a page key"
    );

    let src = source();
    // The dispatch reads both chords of the toggle.
    assert!(
        src.contains(r#"binding_down(i, &kb, "fit_alt")"#),
        "the fit dispatch does not read `fit_alt`, so Space would sit in the table \
         and do nothing (reads-as-working, works-as-nothing)"
    );
    // And the navigation table holds no Space pair â€” the fallback table is the
    // one place a copy of the old meaning could survive the rebind resolver.
    let at = src.find("const NAV_KEYS:").expect("NAV_KEYS is gone; update this test to match");
    let body = &src[at..src[at..].find("];").expect("NAV_KEYS is no longer a bracketed list") + at];
    assert!(!body.contains("Key::Space"), "NAV_KEYS still binds Space to navigation: `{body}`");
}

/// Hold-to-browse resolves to the same direction as the press that started it.
///
/// **This is the second half of the unit, and it is the half nobody writes.** The
/// "wait for image when holding arrows" setting reads the *held* keys when the
/// decode lands, decides whether the reader is still pushing forwards, and queues
/// the next step. Reading the raw keys there decides the direction a second time, in
/// a second place, with no rule in sight - and under `manga_rtl` the press would map
/// to one direction and the queued advance to the other, so holding an arrow in a
/// manga library would flip you back and forth.
///
/// Asserted as three properties rather than as a diff, because the two sites are in
/// different functions and the point is that *neither of them* has an opinion:
///
/// * every held navigation key is read on a line that **names** its [`NavInput`], so
///   there is no `(bool, bool)` left to interpret later;
/// * both halves of hold-to-browse â€” the one that queues the advance when a decode
///   lands, and the one that consumes it on the next frame â€” ask `nav_is_forward`;
///   and
/// * the value written into `pending_advance` comes from the rule, because a tuple
///   that *is* named can still be collapsed into two booleans and interpreted
///   underneath.
#[test]
fn hold_to_browse_asks_the_direction_rule_too() {
    let code = code_only(&source());

    // Both halves of hold-to-browse go through `nav_direction`, which is what asks
    // `nav_is_forward`. Naming the keys at the read is not enough on its own: a
    // tuple of held flags collapsed into two booleans has named four keys and still
    // decides the direction twice, in a `bool` per direction.
    for (sig, what) in [
        ("fn pump(&mut self, ctx: &Context)", "queuing the advance"),
        ("fn frame_prelude(&mut self, ctx: &Context)", "consuming it"),
    ] {
        let body = fn_body(&code, sig);
        assert!(
            body.contains("nav_direction("),
            "`{sig}` does not call `nav_direction` at all, so {what} decides the \
             direction by itself. `nav_is_forward` answers differently under \
             `manga_rtl`, so this is the setting being silently ignored rather than a \
             wrong picture.\nBody was:\n{body}"
        );
        // And it is the *same* function the one-shot press read uses, not a second
        // implementation of the table.
        assert_eq!(
            body.matches("nav_direction(").count(),
            1,
            "`{sig}` calls nav_direction more than once, so there are two readings of \
             the held keys and they can drift apart."
        );
    }

    // And the queued value is the tuple `nav_direction` returned rather than a
    // literal the site chose. This is the half that makes the previous two
    // assertions about this function rather than about the file: a site can call
    // `nav_direction` and then queue whatever it liked.
    //
    // Read from the *other* end, as "the pattern that sets it" rather than "the
    // literal that was set", because the value is a `bool` and `Some(true)` looks
    // identical to a direction the rule produced and to one that did not.
    let body = fn_body(&code, "fn pump(&mut self, ctx: &Context)");
    let write = body
        .find("self.pending_advance = Some(")
        .map(|at| {
            let tail = &body[at..];
            // Through the `;`, inclusive: the shape being matched includes it.
            let end = tail.find(';').map_or(tail.len(), |i| i + 1);
            &tail[..end]
        })
        .expect("nothing queues a follow-up advance any more");
    assert!(
        matches!(
            write,
            "self.pending_advance = Some(dir);" | "self.pending_advance = Some(forward);"
        ),
        "`pending_advance` is set to a direction that is not the value `nav_direction` \
         returned, so holding an arrow in manga mode queues the opposite step to the \
         press that started it. `Some(true)` written here would pass a scan that only \
         looked for a `bool` - the shape that made this assertion fiddly the first \
         time, and the reason the direction has a name at all.\n{write}"
    );

    for key in ["ArrowRight", "ArrowLeft", "PageUp", "PageDown", "Space"] {
        assert!(
            !code.contains(&format!("key_down(egui::Key::{key})")),
            "egui::Key::{key} is read as a held key outside the table. NAV_KEYS is the \
             one list of navigation keys; a key read beside it is a key the direction \
             rule cannot see, which is how hold-to-browse would disagree with the press."
        );
    }
}
