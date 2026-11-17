//! Window chrome, animation keys, slideshow, Esc, printing and touch dates.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// Fit-window follow-through: pinned panel widths in the mode (auto-sizing
/// panels move the measured chrome after the fact, which is how the window
/// landed bigger than the content), two-phase centering (position after the
/// resize settles, with a monitor fallback), and a button that shows the
/// mode like Crop does.
#[test]
fn fit_window_pins_chrome_places_late_and_shows_state() {
    let src = main_src();
    assert!(
        src.contains("exact_size("),
        "side panes must pin their width in the mode, or chrome drifts post-measure"
    );
    assert!(
        src.contains("pending_pos"),
        "centering must wait for the resize to settle, not race it"
    );
    let at = src.find("self.config.fit_window_mode,").expect("the mode toggle must exist");
    let from = at.saturating_sub(200);
    let arm = &src[from..(at + 400).min(src.len())];
    assert!(
        arm.contains("selectable_label(")
            && arm.contains("action_button_text(&self.config, \"Fit window\", \"fit_window\")"),
        "the mode needs a selectable button showing the live chord, like Crop"
    );
    assert!(
        src.contains("action_button_text(&self.config, \"Fit window\", \"fit_window\")"),
        "the mode button must show the live chord, not a hardcoded one"
    );
}

/// The settings pane is pinned to a fixed width in fit-window mode, so no row
/// in it may be a plain `ui.horizontal`: a row wider than the pin grows the
/// panel's content rect past it, egui clamps the size but re-anchors the rect
/// at the *grown* right edge, and the pane is then drawn shifted off the
/// window with the central panel covering its left end. That was the
/// fit-window "only the right side of Settings is drawn" bug. Wrapped rows stop
/// at the pane edge at any width, and the command row sizes its text edit so
/// the trailing remove button still fits.
#[test]
fn settings_rows_wrap_inside_the_pinned_pane() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/ui");
    let code = |name: &str| {
        let text = std::fs::read_to_string(dir.join(name)).expect("settings pane source");
        // Comments quote the very call being ruled out.
        text.lines().map(strip_line_comment).collect::<Vec<_>>().join("\n")
    };
    for name in ["settings.rs", "settings_keys.rs"] {
        assert!(
            !code(name).contains("ui.horizontal("),
            "{name}: a row in the pinned settings pane is a plain `horizontal`; a row wider than \
             the pin shifts the whole pane off the window - use `horizontal_wrapped`"
        );
    }
    assert!(
        code("settings.rs").contains("edit_width(ui)"),
        "the command name row must size its text edit, or the trailing remove button is pushed \
         past the pinned pane's edge"
    );
}

/// Animation stepping and window chrome must be **wired**: `,`/`.`/`K` reach
/// the frame methods, `1`–`4` reach the presets, and both window flags reach
/// the viewport builder. `main.rs` is unreachable from every test, so these
/// are source scans - the established pattern here.
#[test]
fn the_animation_keys_presets_and_chrome_are_wired() {
    let src = main_src();
    assert!(
        src.contains("self.step_animation(false)")
            && src.contains("self.step_animation(true)")
            && src.contains("self.toggle_animation_play()"),
        "`,`/`.`/`K` must reach the animation frame methods"
    );
    assert!(
        src.contains("egui::Key::Comma") && src.contains("egui::Key::Period"),
        "stepping must be bound to comma and period"
    );
    assert!(
        src.contains("zoom_preset(&self.config.zoom_presets"),
        "`1`–`4` must read the configured presets"
    );
    assert!(
        src.contains("with_always_on_top()") && src.contains("with_decorations(false)"),
        "both window flags must reach the viewport builder"
    );
    assert!(
        src.contains("self.config.always_on_top") && src.contains("self.config.hide_title_bar"),
        "both window flags must be offered in Settings"
    );
}

/// The alt-next key resumes a paused animation instead of stepping: the
/// step branch stands down for it, and it only reroutes the paused case —
/// playing animations and stills step on as before. Shipped **unbound** now
/// (Space left navigation to become the fit/100% toggle — JPEGView parity);
/// the hook stays rebindable in Settings.
#[test]
fn the_alt_next_key_resumes_a_paused_animation() {
    let src = main_src();
    assert!(
        src.contains("step_dir.filter(|_| !unpause)"),
        "the step branch must stand down when the key means resume"
    );
    let at = src.find("let unpause =").expect("the unpause flag must exist");
    let arm = &src[at..(at + 700).min(src.len())];
    assert!(
        arm.contains("\"next_alt\"") && arm.contains("paused.is_some()"),
        "only the alt-next chord on a paused animation reroutes"
    );
}

/// Window chrome applies live: decorations go through the viewport command
/// and always-on-top through Win32 (`set_topmost`), not through a restart.
#[test]
fn window_chrome_applies_live() {
    let src = main_src();
    assert!(
        src.contains("ViewportCommand::Decorations("),
        "hiding the title bar must send a live viewport command"
    );
    assert!(
        src.contains("set_topmost(self.config.always_on_top)"),
        "always-on-top must apply straight away, not on restart"
    );
    let lib = lib_src();
    assert!(
        lib.contains("fn set_topmost(") && lib.contains("SetWindowPos"),
        "always-on-top must reach Win32 directly"
    );
}

/// Slideshow must be **wired**: a toggle plus pace in Settings, a tick that
/// wakes the frame loop and steps, and an end that stops rather than wraps
/// silently when looping is off. `main.rs` is unreachable from every test,
/// so this is a source scan - the established pattern here.
#[test]
fn the_slideshow_is_wired_to_a_tick_and_a_stop() {
    let src = main_src();
    assert!(
        src.contains("Start slideshow") && src.contains("slideshow_secs"),
        "Settings must offer the toggle and the pace"
    );
    assert!(
        src.contains("slideshow_due(") && src.contains("request_repaint_after("),
        "the tick must check the clock and keep waking the loop"
    );
    assert!(
        src.contains("fn step(&mut self, ctx: &Context, forward: bool) -> bool"),
        "step must report whether it moved, so the end can stop the show"
    );
}

/// A fresh config closes on Esc (the shipped behaviour), and the Esc chain
/// orders the arms correctly: save-as panel → typing → grid → crop → archive →
/// fullscreen → quit.
///
/// Two arms changed. The **archive** arm comes before fullscreen and quit, so Esc
/// inside an archive goes back instead of closing the app. The **save-as panel**
/// arm comes before everything *including* `typing`, because the panel's path box
/// holds focus the moment it opens — so `typing` is true while it is up, and Esc
/// landed in the search box's arm, clearing the *filter* and leaving the panel
/// standing. The quit branch still exists and still consults the toggle, but only
/// when there is nothing to close.
///
/// The window below was 3200 bytes and is now 8000, and that change is part of the
/// fix's story: the doc comment above pushed the fullscreen arm to byte +3242, so
/// the test failed with "fullscreen branch" missing while the Esc chain was
/// perfectly correct. A byte window a comment can move is a test that measures
/// the comment.
#[test]
fn esc_defaults_to_closing_and_crop_comes_first() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-config.json");
    assert!(
        image_viewer::Config::load(&missing).esc_closes_app,
        "a fresh config must keep the shipped Esc-closes behaviour"
    );
    let src = main_src();
    let at = src.find("if esc {").expect("the Esc chain must exist");
    let arm = &src[at..(at + 8000).min(src.len())];

    // Order: save-as panel → typing → crop → archive → fullscreen → quit, each
    // inside the previous `else`. **Position, not mere presence**: an arm that is
    // earlier in the text but not nested in the previous else stops nothing, and
    // that is the whole content of the save-as fix.
    let panel = arm.find("if self.save_as.open {").expect("save-as panel arm");
    let typing = arm.find("} else if typing {").expect("typing arm");
    let crop = arm.find("self.crop_mode").expect("crop branch");
    let archive = arm.find("zip_archive.is_some()").expect("archive branch");
    let fullscreen = arm.find("in_fullscreen").expect("fullscreen branch");
    let close = arm.find("ViewportCommand::Close").expect("quit branch");
    assert!(
        panel < typing
            && typing < crop
            && crop < archive
            && archive < fullscreen
            && fullscreen < close,
        "Esc chain must be: save-as panel → typing → crop → archive → fullscreen \
         → quit, got order: panel={} typing={} crop={} archive={} fullscreen={} \
         close={}",
        panel,
        typing,
        crop,
        archive,
        fullscreen,
        close
    );

    // The quit branch must still consult the toggle (it does, but it's now
    // nested inside the else of the archive check)
    assert!(
        arm.contains("self.config.esc_closes_app"),
        "quitting on Esc must still consult the toggle"
    );
}

/// Printing goes through the OS `print` verb on the file on screen: a
/// physical Ctrl+P only (repeat-filtered like every spawn chord), through
/// the spawn guard, and the handler gets the file — crops and turns are not
/// applied, because a path carries no pixels.
#[test]
fn printing_is_wired_to_ctrl_p() {
    let lib = lib_src();
    assert!(
        lib.contains("fn print_file(") && lib.contains("ShellExecuteW"),
        "printing must go through the native print verb"
    );
    let src = main_src();
    assert!(src.contains("fn print_current(&mut self"), "there must be a print action");
    assert!(
        src.contains("action_pressed_fresh(ctx, &kb, \"print\")"),
        "print must go through the edge-triggered matcher, not a bespoke chord"
    );
    let body_at = src.find("fn print_current(&mut self)").expect("the print action must exist");
    let body = &src[body_at..(body_at + 1500).min(src.len())];
    assert!(
        body.contains("spawn_guard("),
        "printing must go through the spawn guard like every external action"
    );
}

/// Touch dates must be **wired**: single file to now or its EXIF date,
/// whole visible folder to EXIF dates with the dateless counted, not failed.
#[test]
fn touch_dates_are_wired_to_menu_buttons() {
    let src = main_src();
    assert!(
        src.contains("fn touch_current(&mut self")
            && src.contains("fn touch_folder_to_exif_dates(&mut self"),
        "both touch actions must exist"
    );
    assert!(
        src.contains("touch_file_mtime(") && src.contains("parse_exif_datetime("),
        "touching must go through the tested helpers, not inline date math"
    );
}

/// The bottom bar's four menus carry the actions the old button row did.
///
/// `main.rs` is unreachable from the tests, so the wiring is a scan — and a
/// *positional* one, per the lesson recorded on `shown_partner`: presence of
/// a handler call proves nothing about which menu it lives in. Each block is
/// sliced from its `menu_button` label to the next one, so a stray handler
/// call anywhere else in the bottom panel fails.
#[test]
fn the_bottom_bar_menus_carry_the_buttons_actions() {
    let src = main_src();
    let at = src.find(r#"egui::Panel::bottom("menu")"#).expect("the bottom panel exists");
    // To the end of the panel: the bar is the last pane this section shows,
    // and the body after it starts a new pane, so slicing to the next
    // `egui::Panel::` keeps the scan inside the bar.
    let end = src[at + 1..]
        .find("egui::Panel::")
        .map(|i| at + 1 + i)
        .expect("a later pane closes the scan");
    let bar = &src[at..end];

    let menus = ["Transform", "Save", "File", "Tools"];
    for (i, name) in menus.iter().enumerate() {
        // Prefix match on purpose: the source label carries the "⏷" menu
        // marker, which is decoration and not part of the scan's contract,
        // and the needle stops before the label's closing quote.
        let start = bar.find(&format!("ui.menu_button(\"{name}")).unwrap_or_else(|| {
            panic!("the {name} menu exists [bar head: {:?}]", &bar[..bar.len().min(160)])
        });
        let block_end = menus[i + 1..]
            .iter()
            .find_map(|next| {
                bar[start + 1..].find(&format!("ui.menu_button(\"{next}")).map(|j| start + 1 + j)
            })
            .unwrap_or_else(|| bar.find(r#"ui.label("keys")"#).unwrap_or(bar.len()));
        let block = &bar[start..block_end];

        let want: &[&str] = match *name {
            "Transform" => &[
                "self.rotate(-1, ctx)",
                "self.rotate(1, ctx)",
                "self.mirror(true, ctx)",
                "self.mirror(false, ctx)",
                "self.reset_transform(ctx)",
                "self.undo(ctx)",
                "self.redo(ctx)",
                "edits.clear_crop(",
            ],
            "Save" => &[
                "self.save_transformed_copy()",
                "self.open_save_as()",
                "self.copy_image_to_clipboard(false)",
                "self.copy_image_to_clipboard(true)",
            ],
            "File" => &[
                "self.explore_current()",
                "self.rescan(ctx, true)",
                "self.transfer_current(ctx, false)",
                "self.transfer_current(ctx, true)",
                "self.delete_current(ctx)",
                "self.undo_delete(ctx)",
                "self.print_current()",
            ],
            _ => &[
                "self.set_wallpaper()",
                "self.set_wallpaper_shown()",
                "self.touch_current(false)",
                "self.touch_current(true)",
                "self.touch_folder_to_exif_dates()",
                "self.toggle_color_picker()",
            ],
        };
        for callee in want {
            assert!(block.contains(callee), "the {name} menu must own {callee}");
            // Close-before-act: the popup must not linger over the picture
            // the action leaves behind.
            let call_at = block.find(callee).unwrap();
            assert!(
                block[..call_at].contains("ui.close()"),
                "{callee} must close its menu before acting"
            );
        }
        // Every item closes the popup after acting, so the menu does not
        // linger over the picture — count them against the items we own.
        assert_eq!(
            block.matches("ui.close()").count(),
            want.len(),
            "every {name} item closes the menu"
        );
    }

    // The one-click top level: browsing, the side-step pair and the five mode
    // toggles are still on the row, in the bar (not in a menu after it).
    // Prefix again: the row ends where the first menu begins, and the
    // label now carries the "⏷" marker, so the find stops before the
    // label's closing quote.
    let row = &bar[..bar.find(r#"ui.menu_button("Transform"#).unwrap_or(bar.len())];
    for want in [
        "self.pick_folder(ctx)",
        "self.pick_file(ctx)",
        "nav_is_forward(NavInput::Left, rtl)",
        "nav_is_forward(NavInput::Right, rtl)",
        "self.toggle_fit_mode()",
        "self.fit_window_to_image()",
        "self.toggle_gallery()",
        "self.toggle_crop_mode(ctx)",
        "self.toggle_panels_master()",
        "show_left = !self.show_left",
        "show_right = !self.show_right",
    ] {
        assert!(row.contains(want), "the bottom row must keep {want}");
    }
}
