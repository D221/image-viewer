//! Module `app_ui`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::app::gallery_ops::UndoDeleteResult;
use crate::*;

impl eframe::App for MyApp {
    /// Persist the last-viewed image per folder on app close, then clean up
    /// any extracted archive temp dir.
    fn on_exit(&mut self, _gl: Option<&glow::Context>) {
        // Key by the archive path when one is open (the temp folder name is
        // meaningless to the user).
        let key: String = match &self.zip_archive {
            Some(a) => a.to_string_lossy().into_owned(),
            None => self.folder.to_string_lossy().into_owned(),
        };
        if let Some(cur) = self.gallery.current() {
            // Store just the file name so the same key works for archives
            // (member name) and folders (file name) alike.
            let value = cur
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cur.to_string_lossy().into_owned());
            self.config.last_by_folder.insert(key, value);
        }
        self.config.save(&self.config_path);
        self.close_transients();
        if image_viewer::perf::enabled() {
            let report = image_viewer::perf::report();
            eprint!("{report}");
            // A release build has no console, so also leave the report where it
            // can be found: %TEMP%\image-viewer-perf.txt.
            let path = std::env::temp_dir().join("image-viewer-perf.txt");
            image_viewer::perf::write_report(&path);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // egui 0.31+ replaced `App::update(&Context)` with `App::ui(&mut Ui)`.
        // The panels below are now shown on this `ui` rather than on a
        // `Context`, which is the whole point of the change: Ui-first means
        // nested containers, not a flat sequence of Context-rooted panels.
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.frame_prelude(ctx);

        // Read every key up front, before anything acts on them: Esc needs to
        // know whether the search box has focus, and the panels are drawn last.
        //
        // `typing` is true when a text field owns the keyboard. All the global
        // single-key handling below has to stand down then, or typing "g" into
        // the filter would toggle the grid and "r" would reload the folder.
        let typing = ctx.egui_wants_keyboard_input();
        // Every binding resolved once per frame from the keybindings map
        // (user overrides over shipped defaults). Each read below asks this
        // table by action name instead of naming a key, so there is exactly
        // one place where a physical key becomes a meaning.
        let kb = build_bindings(&self.config);
        let f11 = action_pressed(ctx, &kb, "fullscreen");
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));

        // ---- one direction rule, read once --------------------------------
        //
        // **Every input that can move the view is named in [`NAV_KEYS`] /
        // [`NAV_BUTTONS`] and nowhere else, and every direction is asked of
        // `nav_direction`, which asks `nav_is_forward`.** Nothing below may decide
        // a direction on its own, and this shape is what makes that enforceable
        // rather than a convention: `step` takes a `bool`, so `self.step(ctx, true)`
        // would compile, read correctly and be the whole defect.
        //
        // The reason it has to be *one* table is a specific bug in a specific other
        // reader. OpenComic inverts the navigation keys for right-to-left manga and
        // its `mangaInvert` set omits `Space`, so **Space silently becomes
        // "previous page"** there: nothing errors, the arrows are right, and the
        // space bar quietly goes the wrong way. Five inputs, one enum, one
        // function - a sixth input is a sixth row that has to say what it means,
        // and it cannot be forgotten in a way nothing notices.
        let rtl = self.config.manga_rtl;
        let nav = ctx.input(|i| {
            nav_direction(
                |key, c, s, a| {
                    i.key_pressed(key)
                        && i.modifiers.ctrl == c
                        && i.modifiers.shift == s
                        && i.modifiers.alt == a
                },
                |button| i.pointer.button_clicked(button),
                rtl,
            )
        });
        // Read once, and the *only* navigation read in the frame that is a press
        // rather than a hold.
        //
        // `Option<bool>` because `MyApp::step` already takes a `bool` for its
        // direction: forwards is `true`. So `nav` is passed straight through and
        // there is no arithmetic on it anywhere in the dispatch — a
        // `let (fwd, back) = nav` with `if fwd && !back` beside it would be a
        // second place where a direction is decided.
        let step_dir = nav;

        // The **grid's** arrows, which are a different question and deliberately
        // do not go through the rule: a highlight moving across a grid drawn left
        // to right has a side, and the grid has one cell per file with no reading
        // order at all. `Space` is in here because it has always moved the
        // highlight, and a key that stops working because a manga setting is on
        // would be the kind of regression nothing in the tree can see.
        let (grid_right, grid_left) = ctx.input(|i| {
            (
                binding_down(i, &kb, "grid_right") || binding_down(i, &kb, "grid_right_alt"),
                binding_down(i, &kb, "grid_left"),
            )
        });

        // Keyboard navigation + file triage
        let fs_default = image_viewer::resolve_binding(&self.config.keybindings, "focus_search")
            == image_viewer::default_binding("focus_search");
        let (first, last, toggle, grid, page_up, page_down, focus_search) = ctx.input(|i| {
            (
                binding_down(i, &kb, "first"),
                binding_down(i, &kb, "last"),
                binding_down(i, &kb, "panels"),
                binding_down(i, &kb, "grid"),
                binding_down(i, &kb, "grid_page_up"),
                binding_down(i, &kb, "grid_page_down"),
                // `/` is the usual "search" chord and is not otherwise used.
                // Shift stays optional (so `?` works too), but only while the
                // shipped binding holds — a rebound search gets exact matching
                // like everything else.
                binding_down(i, &kb, "focus_search")
                    || (fs_default
                        && i.key_pressed(egui::Key::Slash)
                        && i.modifiers.shift
                        && !i.modifiers.ctrl
                        && !i.modifiers.alt),
            )
        });
        // `Alt+Left` / `Alt+Right`: back/forward through navigation history.
        // Read separately from `prev`/`next` so the shipped arrow keys keep
        // their step behaviour and `Alt+arrows` own history.
        let (history_back, history_forward) = ctx.input(|i| {
            (binding_down(i, &kb, "history_back"), binding_down(i, &kb, "history_forward"))
        });

        if f11 {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
            self.suppress_resize_redecode = true;
            // **Entered fullscreen hides the panels; leaving restores what the
            // user had — not hard `true`.** The master is a persisted
            // preference (`Config::show_panels`), and the fullscreen dance is
            // a transient overlay: entering stashes the pre-fullscreen value,
            // leaving hands it back, so minimal-state users keep their
            // minimal state across a fullscreen detour instead of reacquiring
            // chrome they switched off. The stash also protects `on_exit`'s
            // whole-config save: with a hardcoded restore, one fullscreen
            // detour would rewrite the saved "off" at close.
            if fs {
                self.show_panels = self.panels_before_fullscreen.take().unwrap_or(true);
            } else {
                self.panels_before_fullscreen = Some(self.show_panels);
                self.show_panels = false;
            }
        }
        if esc {
            // **The save-as panel is above everything, `typing` included.**
            //
            // Its path box holds focus the moment it opens, so `typing` is true
            // while the user is naming a file — and Esc landed in the search
            // box's arm, which cleared the *filter* and left the panel standing.
            // One Esc later it fell through to the quit branch and closed the
            // app with the panel still up.
            //
            // So the panel is checked before `typing`, and the rule it follows
            // is the same one the archive arm states below: Esc is "go back one
            // level", and a modal panel is a level above the viewer. It is also
            // the only modal here that *writes* — Esc on a panel holding a
            // half-typed destination must not be a quit, for the same reason Esc
            // in crop mode drops the frame instead.
            if self.save_as.open {
                self.save_as.open = false;
            } else if typing {
                // Esc belongs to the search box while it has focus: clear the
                // filter, or if there is nothing to clear just defocus. It must
                // not fall through to "close the grid" or "quit the app" —
                // clicking into the search box and pressing Esc should never
                // exit the viewer.
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.on_filter_changed(ctx);
                } else {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::NULL));
                }
            } else if self.gallery_view {
                self.gallery_view = false; // close the grid first
            } else if self.crop_mode {
                // In crop mode Esc drops the frame instead of doing anything
                // behind it: losing a framed crop to a quit is the data-loss
                // shape bare-letter discipline exists to prevent, and leaving
                // fullscreen under a crop would strand the frame on a new size.
                self.toggle_crop_mode(ctx);
            } else if self.zip_archive.is_some() {
                // **Above fullscreen and above the quit**, and that ordering is the
                // point. Esc is "go back one level", and inside an archive there
                // is a level to go back to: without this arm Esc fell straight
                // through to the quit branch, so a reader three pages into a
                // volume who pressed Esc to get out closed the app instead.
                //
                // A behaviour change to `esc_closes_app`, and the right one: the
                // setting decides whether Esc quits *the viewer*, and inside an
                // archive Esc is answering "leave the archive" — the same
                // relationship crop mode and the grid already have above it.
                // One level down and Esc quits exactly as before.
                if !self.leave_archive(ctx, false) {
                    self.notice = Some("No folder to go back to (archive opened directly)".into());
                }
            } else {
                let in_fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
                if in_fullscreen {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    // What the master stood at *before this fullscreen*, not
                    // hard `true` — same reason the `F11` exit restores the
                    // stash: the value is the user's persisted choice, and a
                    // hardcoded restore rewrites it (and then `on_exit`
                    // saves the rewrite).
                    self.show_panels = self.panels_before_fullscreen.take().unwrap_or(true);
                    self.suppress_resize_redecode = true;
                } else if self.config.esc_closes_app {
                    self.close_transients();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                } else {
                    self.notice = Some("Esc close is off (Settings)".into());
                }
            }
        }

        // In grid mode the arrows drive the grid's own selection instead of
        // changing the shown image, and Enter opens the highlighted cell.
        let (up, down, enter) = ctx.input(|i| {
            (
                binding_down(i, &kb, "grid_up"),
                binding_down(i, &kb, "grid_down"),
                binding_down(i, &kb, "grid_open"),
            )
        });
        // `M` (move) and `C` (copy) used to be bare single-key shortcuts. They are
        // not any more, and the reason is worth keeping: both **write to the
        // filesystem** on a keystroke with nothing standing between the key and
        // the file. `C` in particular is the wrong letter to have bound to a
        // mutation, because it is also the first thing anyone reaches for when
        // they think they are in a text box somewhere else. `C` is now Crop,
        // which touches nothing on disk until `Ctrl+S`.
        //
        // So the single-key budget goes to actions that cannot lose data. Triage
        // is still one keystroke for Delete, which is the one people use while
        // culling; Move and Copy are buttons. If they turn out to be wanted back
        // on the keyboard, a modifier is the shape to add rather than a bare
        // letter.
        // `Ctrl+C` is **not** a key event, and reading it as one is why it did
        // nothing. `egui-winit` intercepts the clipboard chords and converts
        // them, dropping the key:
        //
        //     } else if is_copy_command(self.modifiers, active_key) {
        //         self.egui_input.events.push(egui::Event::Copy);
        //         return;                      // <- no `Key { key: C, .. }` ever
        //     }
        //
        // and `is_copy_command` is `modifiers.command && key == C`, which on
        // Windows is `ctrl` — the same thing. So `key_pressed(Key::C) &&
        // modifiers.ctrl` is *unreachable*: the log of that frame shows
        // `Copy` followed by `Key { key: C, pressed: false }`, and nothing
        // between them. `Ctrl+S` survives because it is not on the list.
        //
        // So the chord is read from the converted event, which is also the
        // honest way round: this is what the platform means by copy, so it
        // now also answers to the Edit menu's Copy, `Ctrl+Insert`, and the
        // dedicated Copy key, none of which would ever match a `C` test.
        //
        // It sits inside the `!typing` block below, so `Ctrl+C` in the filter
        // box still copies the selected text rather than the picture — the
        // text field handles the same event first, and copying a whole
        // full-res image because you were renaming a filter is not a thing
        // anyone asked for.
        let (clip_img, clip_shift) = ctx.input(|i| {
            (i.events.iter().any(|e| matches!(e, egui::Event::Copy)), i.modifiers.shift)
        });
        let (
            del,
            rename_key,
            explore,
            reload,
            wall,
            wall_shown,
            save_rot,
            save_screen,
            save_as,
            undo_key,
            redo_key,
            crop_key,
            rating_up,
            rating_down,
            flag_key,
            restore_delete,
            pane_left,
            pane_right,
            color_pick,
        ) = ctx.input(|i| {
            (
                binding_down(i, &kb, "del"),
                binding_down(i, &kb, "rename"),
                binding_down(i, &kb, "explore"),
                binding_down(i, &kb, "reload"),
                binding_down(i, &kb, "wall"),
                binding_down(i, &kb, "wall_shown"),
                binding_down(i, &kb, "save"),
                binding_down(i, &kb, "save_screen"),
                binding_down(i, &kb, "save_as"),
                binding_down(i, &kb, "undo"),
                binding_down(i, &kb, "redo"),
                // `Ctrl+C` never arrives as a key (toolkit interception, see
                // the Copy comment above), so this bare press coexists with
                // the chord below rather than competing with it.
                binding_down(i, &kb, "crop"),
                binding_down(i, &kb, "rating_up"),
                binding_down(i, &kb, "rating_down"),
                binding_down(i, &kb, "flag"),
                binding_down(i, &kb, "restore_delete"),
                binding_down(i, &kb, "pane_left"),
                binding_down(i, &kb, "pane_right"),
                binding_down(i, &kb, "color_pick"),
            )
        });
        // Mouse side buttons: back = previous image, forward = next image.
        //
        // **Read up here, mapped in the `nav` tuple above** - which is why there
        // is no separate `mouse_back` / `mouse_fwd` pair any more. Extra1/Extra2
        // are two of the five inputs in that tuple precisely because they are
        // `Previous`/`Next` rather than something with a direction of their own.
        // The block that used to be here is gone, not moved: a second read of the
        // same button is how a binding ends up with two opinions about what it
        // means.
        //
        // The manga controls. All view-only or app-owned files, and
        // `tests/keybindings.rs` holds each of them to that in `BARE_LETTERS`
        // (as shipped defaults — rebound chords keep the same guarantee by
        // construction, since no action in the table can write to disk).
        let (manga_key, rtl_key, lead_down, lead_up, shape_key) = ctx.input(|i| {
            (
                binding_down(i, &kb, "manga"),
                binding_down(i, &kb, "rtl"),
                binding_down(i, &kb, "lead_down"),
                binding_down(i, &kb, "lead_up"),
                binding_down(i, &kb, "shape"),
            )
        });
        // Animation frame stepping (`,` back, `.` forward, YouTube-style) and
        // play/pause (`K`). View-only — stepping pauses onto a frame rather
        // than writing anything.
        let (anim_back, anim_fwd, anim_toggle) = ctx.input(|i| {
            (
                binding_down(i, &kb, "anim_back"),
                binding_down(i, &kb, "anim_fwd"),
                binding_down(i, &kb, "anim_toggle"),
            )
        });
        // Fit window to image (`T` shipped): shrink the window to hug the
        // picture as drawn. View-only like the rest.
        let fit_win = action_pressed(ctx, &kb, "fit_window");
        // User commands: the nth entry in `Config::user_commands`. Read as
        // one `Option<usize>` so the dispatch below has a single branch
        // rather than nine. Edge-triggered like print: spawning must never
        // repeat with a held chord. Names spelled out (not `format!`) so the
        // action-coverage test can see every one.
        const USER_ACTIONS: [&str; 9] = [
            "user_1", "user_2", "user_3", "user_4", "user_5", "user_6", "user_7", "user_8",
            "user_9",
        ];
        let user_cmd = USER_ACTIONS.iter().position(|a| action_pressed_fresh(ctx, &kb, a));
        // Print: the OS `print` verb on the file on screen. Edge-triggered
        // for the same reason. Bare `P` is untouched (right pane), so this
        // chord and that key coexist.
        let print_key = action_pressed_fresh(ctx, &kb, "print");
        // Clipboard alternatives to the toolkit chords below: `Ctrl+C` never
        // arrives as a key (interception), so these only add options — the
        // event path stays exactly as it was.
        let copy_alt = action_pressed_fresh(ctx, &kb, "copy");
        let copy_path_alt = action_pressed_fresh(ctx, &kb, "copy_path");
        // `typing` is true when the search box owns the keyboard, and
        // `capturing` when a rebind button is waiting for keys: both stand
        // the whole dispatch down, or typing "g" toggles the grid and a
        // capture press of Delete recycles a photo.
        if !typing && self.capturing.is_none() {
            // The alt-next key resumes a paused animation instead of stepping:
            // `,`/`.` pause onto a frame, and this is the handiest way back
            // to motion (YouTube parity with the stepping keys). Only the
            // paused case reroutes — playing animations and stills step on.
            // Shipped **unbound** since Space became the fit/100% toggle —
            // the hook survives for anyone who rebinds it in Settings.
            // Computed up front so the step branch below can stand down for it.
            let unpause = !self.gallery_view
                && action_pressed(ctx, &kb, "next_alt")
                && self.anim.as_ref().is_some_and(|a| {
                    Some(&a.path) == self.shown.as_ref().map(|k| &k.path) && a.paused.is_some()
                });
            if self.gallery_view {
                // Grid: arrows move the highlight, PageUp/PageDown move a page,
                // Home/End jump to the ends, Enter opens it. Clamped, so walking
                // off an edge stops instead of jumping to the far end.
                let cols = self.grid_cols.max(1);
                let page = self.grid_page_rows.max(1) * cols;
                let extra = if self.zip_archive.is_some() { 1 } else { 0 };
                let last_idx = self.gallery.len().saturating_sub(1) + extra;
                let sel = &mut self.grid_sel;
                if page_down {
                    *sel = (*sel + page).min(last_idx);
                } else if page_up {
                    *sel = sel.saturating_sub(page);
                } else if grid_right || down {
                    *sel = (*sel + if down { cols } else { 1 }).min(last_idx);
                } else if grid_left || up {
                    *sel = sel.saturating_sub(if up { cols } else { 1 });
                } else if first {
                    *sel = 0;
                } else if last {
                    *sel = last_idx;
                } else if enter {
                    self.pick_grid_selection(ctx);
                }
            } else if let Some(dir) = step_dir.filter(|_| !unpause) {
                self.step(ctx, dir);
            } else if first {
                self.goto_edge(false, ctx);
            } else if last {
                self.goto_edge(true, ctx);
            } else if history_back {
                self.go_back(ctx);
            } else if history_forward {
                self.go_forward(ctx);
            }
            // The alt-next key resumes a paused animation instead of stepping:
            // `,`/`.` pause onto a frame, and this is the handiest way back
            // to motion (YouTube parity with the stepping keys). Only the
            // paused case reroutes — playing animations and stills step on.
            // Shipped **unbound** since Space became the fit/100% toggle —
            // the hook survives for anyone who rebinds it in Settings.
            // Placed after the step branch so it never runs together with it
            // (the branch above already stood down for it).
            if unpause {
                self.toggle_animation_play();
            }
            if toggle {
                self.toggle_panels_master();
            }
            // Individual panes. `suppress_resize_redecode` like the master:
            // removing a side pane grows the central panel, which must not
            // read as image growth and re-decode. Each press names what it
            // did: with the master off a toggle changes nothing on screen,
            // and silence then reads as a dead key.
            if pane_left {
                self.suppress_resize_redecode = true;
                self.show_left = !self.show_left;
                self.notice = Some(
                    if self.show_left { "Left pane shown" } else { "Left pane hidden" }.into(),
                );
            }
            if pane_right {
                self.suppress_resize_redecode = true;
                self.show_right = !self.show_right;
                self.notice = Some(
                    if self.show_right { "Right pane shown" } else { "Right pane hidden" }.into(),
                );
            }
            if grid {
                self.toggle_gallery();
            }
            // The manga controls, dispatched here so they stand down while the
            // filter box has the keyboard. `M` and `D` first because they are the
            // two a reader reaches for without thinking, and `X`/`O` last because
            // they are the two that write a file.
            if manga_key {
                self.toggle_manga_mode(ctx);
            }
            if rtl_key {
                self.toggle_manga_rtl(ctx);
            }
            if shape_key {
                self.toggle_cursor_shape(ctx);
            }
            // Animation stepping pauses onto the stepped frame; `K` resumes.
            // No-ops without an animation on screen, grid included.
            if anim_back {
                self.step_animation(false);
            } else if anim_fwd {
                self.step_animation(true);
            } else if anim_toggle {
                self.toggle_animation_play();
            }
            if fit_win {
                self.fit_window_to_image();
            }
            if lead_down {
                self.nudge_lead_lone(-1, ctx);
            } else if lead_up {
                self.nudge_lead_lone(1, ctx);
            }
            if rename_key {
                self.rename_current(ctx);
            }
            if restore_delete {
                self.undo_delete(ctx);
            }
            if color_pick {
                self.toggle_color_picker();
            }
            if del {
                self.delete_current(ctx);
            } else if explore {
                self.explore_current();
            } else if reload {
                self.rescan(ctx, true);
            } else if clip_img || copy_alt || copy_path_alt {
                // Toolkit chord and rebound chord both land here; path wins
                // ties, matching the shift-disambiguation above.
                if copy_path_alt || (clip_img && clip_shift) {
                    self.copy_path_to_clipboard(ctx);
                } else {
                    self.copy_image_to_clipboard(false);
                }
            } else if save_rot {
                self.save_transformed_copy();
            } else if save_screen {
                self.save_scaled_copy();
            } else if save_as {
                self.open_save_as();
            } else if undo_key {
                self.undo(ctx);
            } else if redo_key {
                self.redo(ctx);
            } else if crop_key {
                self.toggle_crop_mode(ctx);
            } else if rating_up {
                self.set_stars(ctx, 1);
            } else if rating_down {
                self.set_stars(ctx, -1);
            } else if flag_key {
                self.toggle_flag(ctx);
            } else if wall {
                self.set_wallpaper();
            } else if wall_shown {
                self.set_wallpaper_shown();
            } else if print_key {
                self.print_current();
            }
            if let Some(n) = user_cmd {
                self.run_user_command(n);
            }
            // Slideshow: advance on the interval, stop at the end when looping
            // is off. The repaint keeps the clock running: a static image
            // paints nothing on its own, and without a wake-up the due check
            // below would never run until something else moved.
            if self.slideshow {
                ctx.request_repaint_after(Duration::from_millis(250));
                // Paused: keep the loop ticking so the UI can repaint, but do
                // not advance. Resuming restarts the interval, so a pause never
                // steals time from the next slide.
                if !self.slideshow_paused
                    && image_viewer::slideshow_due(
                        self.slideshow_last,
                        Instant::now(),
                        self.config.slideshow_secs,
                    )
                {
                    self.slideshow_last = Instant::now();
                    if !self.step(ctx, true) {
                        self.stop_slideshow();
                        self.notice = Some("Slideshow finished".into());
                    }
                }
            }
        }

        // Drag-and-drop: files or folders
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.first().map(|f| f.path().to_path_buf()) {
            self.load_dropped(ctx, path);
        }

        // Rename dialog: prefilled with the current name, confirmed with Enter
        // or Confirm, cancelled with Esc or Cancel.
        let mut confirm = false;
        let mut cancel = false;
        {
            if let Some(name) = self.rename_input.as_mut() {
                egui::Window::new("Rename")
                    .collapsible(false)
                    .resizable(false)
                    .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                    .show(ctx, |ui| {
                        ui.set_min_width(260.0);
                        ui.label("New name:");
                        let resp = ui.add(egui::TextEdit::singleline(name).desired_width(240.0));
                        let enter =
                            resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        confirm = enter || ui.button("Confirm").clicked();
                        cancel = ui.button("Cancel").clicked()
                            || ui.input(|i| i.key_pressed(egui::Key::Escape));
                    });
            }
        }
        // The `name` borrow ended with the block; now it is safe to touch self again.
        if confirm {
            if let Some(new_name) = self.rename_input.take() {
                if new_name.is_empty() {
                    self.rename_input = Some(new_name);
                } else if self.rename_commit(ctx, &new_name).is_none() {
                    // Failed: put the name back so the dialog stays open.
                    self.rename_input = Some(new_name);
                }
            }
        }
        if cancel {
            self.rename_input = None;
        }

        // Undo-delete result from the background thread. Applied here so the
        // gallery re-scan and navigation happen on the UI thread.
        if let Some(rx) = &self.rx_undo_delete {
            match rx.try_recv() {
                Ok(UndoDeleteResult::Restored(original)) => {
                    let files = self.scan_current();
                    self.set_files(files, ctx);
                    if let Some(pos) = self.gallery.files.iter().position(|p| p == &original) {
                        self.gallery.index = pos;
                        self.goto(ctx);
                    }
                    self.notice = Some(format!(
                        "Restored {}",
                        original.file_name().and_then(|n| n.to_str()).unwrap_or("?")
                    ));
                    self.rx_undo_delete = None;
                }
                Ok(UndoDeleteResult::NotFound { name, original }) => {
                    self.deleted_stack.push(original);
                    self.error = Some(format!("{name} is not in the Recycle Bin"));
                    self.rx_undo_delete = None;
                }
                Ok(UndoDeleteResult::Failed { error, original }) => {
                    self.deleted_stack.push(original);
                    self.error = Some(error);
                    self.rx_undo_delete = None;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still running: wait for the next frame.
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.rx_undo_delete = None;
                }
            }
        }

        // Remember the window geometry: size, position and maximized state.
        // Written at most once a second so a live resize does not hammer the
        // disk; the last frame before close wins because the loop keeps
        // painting until the window is gone.
        let (new_size, new_pos, new_maximized) = ctx.input(|i| {
            let vp = &i.viewport();
            let size = vp.inner_rect.map(|r| (r.size().x as u32, r.size().y as u32));
            let pos = vp.outer_rect.map(|r| (r.min.x as i32, r.min.y as i32));
            (size, pos, vp.maximized.unwrap_or(false))
        });
        let changed = self.config.window_size != new_size
            || self.config.window_pos != new_pos
            || self.config.window_maximized != new_maximized;
        if changed && self.last_geometry_save.elapsed() > Duration::from_secs(1) {
            self.config.window_size = new_size;
            self.config.window_pos = new_pos;
            self.config.window_maximized = new_maximized;
            self.config.save(&self.config_path);
            self.last_geometry_save = Instant::now();
        }

        if self.show_panels {
            egui::Panel::top("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(self.status_text(ui.ctx().pixels_per_point()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Clear button first so it sits at the right edge.
                        if !self.filter.is_empty() && ui.small_button("x").clicked() {
                            self.filter.clear();
                            self.on_filter_changed(ctx);
                        }
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.filter)
                                .hint_text("filter (/)")
                                .desired_width(160.0),
                        );
                        if focus_search {
                            resp.request_focus();
                        }
                        if resp.changed() {
                            self.on_filter_changed(ctx);
                        }
                    });
                });
                if !self.filter.is_empty() {
                    ui.label(format!("{} of {} shown", self.gallery.len(), self.all_files.len()));
                }
            });
        }

        if self.show_panels {
            egui::Panel::bottom("menu").show(ui, |ui| {
                // Wrapped: even with the actions in menus, a very narrow
                // window can still overflow one row, and without wrapping
                // the tail gets clipped.
                //
                // **The shape**: the picture the user looks at *next* is one
                // click (browsing, stepping, the five mode toggles); the
                // rest is four menus, whose items keep the exact handlers,
                // enable/disable rules, hover texts and live-chord suffixes
                // the old row of buttons had. A menu item calls `ui.close()`
                // after acting, so the popup does not linger over the
                // picture.
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Browse Folder…").clicked() {
                        self.pick_folder(ctx);
                    }
                    if ui.button("Browse File…").clicked() {
                        self.pick_file(ctx);
                    }
                    ui.separator();
                    // "Left"/"Right" rather than the arrow glyphs: the default
                    // egui font has no U+2190/U+2192, so the arrows rendered as
                    // blank boxes, and the name of the key is clearer anyway.
                    //
                    // **Labelled by side, and routed through the rule, rather than
                    // "Prev"/"Next" and a hardcoded direction.** These two are the
                    // mouse's version of the arrow keys, so they have to move the
                    // way the keys move - and under `manga_rtl` "Prev" next to a
                    // button that goes the other way is a label that lies about
                    // what the button does. "Left" is true in both modes.
                    let rtl = self.config.manga_rtl;
                    if ui.button("Left").clicked() {
                        self.step(ctx, nav_is_forward(NavInput::Left, rtl));
                    }
                    if ui.button("Right").clicked() {
                        self.step(ctx, nav_is_forward(NavInput::Right, rtl));
                    }
                    if ui
                        .selectable_label(self.config.fit_mode, action_button_text(&self.config, "Fit view", "fit"))
                        .on_hover_text("Fit every image to the window")
                        .clicked()
                    {
                        self.toggle_fit_mode();
                    }
                    if ui
                        .selectable_label(
                            self.config.fit_width_mode,
                            action_button_text(&self.config, "Fit width", "fit_width"),
                        )
                        .on_hover_text("Scale every image to window width; pan vertically for the rest")
                        .clicked()
                    {
                        self.toggle_fit_width_mode();
                    }
                    if ui
                        .selectable_label(
                            self.config.fit_window_mode,
                            action_button_text(&self.config, "Fit window", "fit_window"),
                        )
                        .on_hover_text("Shrink the window to the picture; follows images and zooms while on")
                        .clicked()
                    {
                        self.fit_window_to_image();
                    }
                    if ui.button(action_button_text(&self.config, "Gallery", "grid")).clicked() {
                        self.toggle_gallery();
                    }
                    // Crop is a mode, not an action: the frame stays after the drag
                    // so it can be adjusted, and the mode says so. The hover text
                    // spells out the one real cost, which is that panning is
                    // unreachable while it is on.
                    if ui
                        .selectable_label(self.crop_mode, action_button_text(&self.config, "Crop", "crop"))
                        .on_hover_text("Drag on the image to frame. Hold Shift for a square.")
                        .clicked()
                    {
                        self.toggle_crop_mode(ctx);
                    }
                    if ui.button(action_button_text(&self.config, "Panels", "panels")).clicked() {
                        self.toggle_panels_master();
                    }
                    // Individual panes, beside the master, as glowing toggles like
                    // Crop — they are modes of the layout, not actions. The
                    // filled state is the pane's own visibility.
                    if ui
                        .selectable_label(
                            self.show_left,
                            action_button_text(&self.config, "Left pane", "pane_left"),
                        )
                        .clicked()
                    {
                        self.suppress_resize_redecode = true;
                        self.show_left = !self.show_left;
                    }
                    if ui
                        .selectable_label(
                            self.show_right,
                            action_button_text(&self.config, "Right pane", "pane_right"),
                        )
                        .clicked()
                    {
                        self.suppress_resize_redecode = true;
                        self.show_right = !self.show_right;
                    }
                    ui.separator();
                    // Everything below is touched too rarely to spend row width on:
                    // four menus with the same handlers the buttons had. The
                    // "⏷" says this opens a menu rather than acting on click —
                    // pending the font check: a missing glyph is a blank box,
                    // and `no_ui_text_uses_a_glyph_the_font_lacks` is what
                    // stops that shipping.
                    ui.menu_button("Transform ⏷", |ui| {
                        if ui.button("Rotate ⟲").clicked() {
                            ui.close();
                            self.rotate(-1, ctx);
                        }
                        if ui.button("Rotate ⟳").clicked() {
                            ui.close();
                            self.rotate(1, ctx);
                        }
                        // Mirrors are here for the same reason the rotate buttons are:
                        // a photo shot with a wrong orientation tag, or a scan that
                        // came out the wrong way round, is a one-press fix and there is
                        // no other way to get it. They compose with the turns rather
                        // than replacing them, so turn-then-mirror is a diagonal
                        // mirror and mirror-twice is back to where it started.
                        //
                        // "H" and "V" rather than ⇋ and ⇅: the default egui font has
                        // neither, and a missing glyph is a blank box, not a subtle
                        // omission. `no_ui_text_uses_a_glyph_the_font_lacks` is what
                        // stops that coming back.
                        let xform = self.current_xform();
                        let any = xform != Xform::Identity;
                        if ui
                            .add_enabled(
                                self.cur.is_some(),
                                egui::Button::new("Mirror H").shortcut_text(""),
                            )
                            .on_hover_text("Mirror left-to-right")
                            .clicked()
                        {
                            ui.close();
                            self.mirror(true, ctx);
                        }
                        if ui
                            .add_enabled(
                                self.cur.is_some(),
                                egui::Button::new("Mirror V").shortcut_text(""),
                            )
                            .on_hover_text("Mirror top-to-bottom")
                            .clicked()
                        {
                            ui.close();
                            self.mirror(false, ctx);
                        }
                        if ui
                            .add_enabled(
                                any,
                                egui::Button::new("Reset turn").shortcut_text(""),
                            )
                            .on_hover_text("Back to the file's own orientation, and drop any crop")
                            .clicked()
                        {
                            ui.close();
                            self.reset_transform(ctx);
                        }
                        ui.separator();
                        if ui
                            .add_enabled(
                                self.can_undo_current(),
                                egui::Button::new("Undo").shortcut_text(""),
                            )
                            .on_hover_text("Undo the last turn, mirror or crop on this image")
                            .clicked()
                        {
                            ui.close();
                            self.undo(ctx);
                        }
                        if ui
                            .add_enabled(
                                self.can_redo_current(),
                                egui::Button::new("Redo").shortcut_text(""),
                            )
                            .on_hover_text("Redo the last undone transform")
                            .clicked()
                        {
                            ui.close();
                            self.redo(ctx);
                        }
                        ui.separator();
                        let has_crop = self.cur.as_ref().is_some_and(|k| self.edits.crop(&k.path).is_some());
                        if ui
                            .add_enabled(
                                has_crop,
                                egui::Button::new("Clear crop").shortcut_text(""),
                            )
                            .on_hover_text("Remove the crop, keeping any rotation or mirroring")
                            .clicked()
                        {
                            ui.close();
                            if let Some(p) = self.cur.clone() {
                                self.history.record(&p.path, self.edits.get(&p.path));
                                self.edits.clear_crop(&p.path);
                            }
                            ctx.request_repaint();
                        }
                    });
                    // One writing action per group, headed by the only one that
                    // app-side is a plain save; without this the app's one writing
                    // action was reachable only by remembering a chord. Save-as sits
                    // beside it because it is the same action with a destination
                    // attached, and a save you cannot choose the name of is not a
                    // save-as.
                    ui.menu_button("Save ⏷", |ui| {
                        if ui
                            .add_enabled(
                                self.cur.is_some(),
                                egui::Button::new(action_button_text(&self.config, "Save transformed…", "save")),
                            )
                            .on_hover_text("New file beside this one, original untouched")
                            .clicked()
                        {
                            ui.close();
                            self.save_transformed_copy();
                        }
                        if ui
                            .add_enabled(
                                self.cur.is_some(),
                                egui::Button::new(action_button_text(&self.config, "Save as…", "save_as")),
                            )
                            .on_hover_text("Choose file, format, quality and size")
                            .clicked()
                        {
                            ui.close();
                            self.open_save_as();
                        }
                        ui.separator();
                        if ui.button(action_button_text(&self.config, "Copy IMG", "copy")).clicked() {
                            ui.close();
                            self.copy_image_to_clipboard(false);
                        }
                        if ui.button("Copy original").clicked() {
                            ui.close();
                            self.copy_image_to_clipboard(true);
                        }
                    });
                    ui.menu_button("File ⏷", |ui| {
                        if ui.button(action_button_text(&self.config, "Explore", "explore")).clicked() {
                            ui.close();
                            self.explore_current();
                        }
                        if ui.button(action_button_text(&self.config, "Reload", "reload")).clicked() {
                            ui.close();
                            self.rescan(ctx, true);
                        }
                        ui.separator();
                        // No key advertised, because there is none: Move and Copy write
                        // to the filesystem and are not on a bare keystroke. See the
                        // note where the bindings are read.
                        if ui.button("Move…").clicked() {
                            ui.close();
                            self.transfer_current(ctx, false);
                        }
                        if ui.button("Copy…").clicked() {
                            ui.close();
                            self.transfer_current(ctx, true);
                        }
                        if ui.button(action_button_text(&self.config, "Delete", "del")).clicked() {
                            ui.close();
                            self.delete_current(ctx);
                        }
                        if ui
                            .button(action_button_text(&self.config, "Restore from Trash", "restore_delete"))
                            .on_hover_text("Restore the most recently deleted file")
                            .clicked()
                        {
                            ui.close();
                            self.undo_delete(ctx);
                        }
                        ui.separator();
                        if ui.button(action_button_text(&self.config, "Print…", "print")).clicked() {
                            ui.close();
                            self.print_current();
                        }
                    });
                    ui.menu_button("Tools ⏷", |ui| {
                        if ui.button(action_button_text(&self.config, "Wallpaper", "wall")).clicked() {
                            ui.close();
                            self.set_wallpaper();
                        }
                        if ui
                            .button(action_button_text(&self.config, "Wallpaper shown", "wall_shown"))
                            .clicked()
                        {
                            ui.close();
                            self.set_wallpaper_shown();
                        }
                        ui.separator();
                        if ui.button("Touch now").clicked() {
                            ui.close();
                            self.touch_current(false);
                        }
                        if ui.button("Touch EXIF date").clicked() {
                            ui.close();
                            self.touch_current(true);
                        }
                        if ui.button("Touch folder EXIF dates").clicked() {
                            ui.close();
                            self.touch_folder_to_exif_dates();
                        }
                        ui.separator();
                        if ui
                            .selectable_label(self.color_picker_active, "Colour picker")
                            .on_hover_text("Ctrl+K — click the picture to sample the pixel under the cursor, copied to the clipboard")
                            .clicked()
                        {
                            ui.close();
                            self.toggle_color_picker();
                        }
                    });
                    // The hints moved off the row: a hover tooltip costs no width,
                    // and the row they used to sit in would grow by a line.
                    ui.label("keys").on_hover_text(
                        "wheel: zoom · drag: pan · 1/F/Space: 100%/fit · double-click: fit",
                    );
                });
            });
        }

        if self.show_panels && self.show_left {
            // Pinned while fit-window mode is on: the window math measures
            // chrome as inner-minus-central, and auto-sizing panels move that
            // number after the measure (stale EXIF text, scrollbar flips), so
            // the window lands bigger than the content with bars to prove it.
            let mut pane = egui::Panel::left("exif");
            if self.config.fit_window_mode {
                pane = pane.exact_size(240.0);
            }
            pane.show(ui, |ui| {
                ui.heading("Rating");
                // The rating is a 300-byte sidecar, not a whole-file read, so it
                // is deliberately NOT part of the `wants_exif` gate below:
                // joining them would hide the rating whenever EXIF is not
                // wanted. Stars are ASCII `*` buttons — the default egui font
                // lacks a star glyph and renders it as a blank box.
                if let Some(cur) = self.cur.as_ref().map(|k| k.path().to_path_buf()) {
                    if is_archive_path(&cur) {
                        ui.label("Ratings: not available inside an archive");
                    } else if self.rating_for.as_deref() != Some(cur.as_path()) {
                        ui.label("Loading rating…");
                    } else {
                        let stars = self.rating.rating.stars;
                        ui.horizontal(|ui| {
                            for i in 1..=5u8 {
                                if ui.selectable_label(stars >= i, "*").clicked() {
                                    self.set_stars(
                                        ui.ctx(),
                                        image_viewer::ratings::star_click_delta(stars, i),
                                    );
                                }
                            }
                            let mut flag = self.rating.rating.flag;
                            if ui.checkbox(&mut flag, "Flag").changed() {
                                self.toggle_flag(ui.ctx());
                            }
                        });
                        ui.separator();
                        ui.label("Caption:");
                        let mut caption = self.rating.caption.clone();
                        if ui.text_edit_singleline(&mut caption).changed() {
                            self.set_caption(ctx, caption);
                        }
                        ui.label("Artist:");
                        let mut artist = self.rating.artist.clone();
                        if ui.text_edit_singleline(&mut artist).changed() {
                            self.set_artist(ctx, artist);
                        }
                    }
                } else {
                    ui.label("No image.");
                }
                ui.heading("EXIF");
                // The colour line comes with the preview, not with the metadata
                // read: it is classified on the worker from the bytes the decode
                // already had in hand — the read this replaces was `color_info`'s
                // whole-file `fs::read`, 8–12 ms on a real photograph, paid once
                // per navigation at a priority ahead of every prefetch. It is
                // cached on the preview like `exif_tag`, so a re-shown image
                // answers with no read of any kind, and it can appear while the
                // EXIF block below is still loading.
                if let Some(preview) = self.shown.as_ref().and_then(|k| self.previews.get(k)) {
                    if let Some(line) = preview.colour.panel_line() {
                        ui.label(line);
                    }
                }
                // The panel is the only thing that shows this, so it is also the
                // only thing that should ask for it. The gate is the *effective*
                // visibility — master and left flag — so hiding the pane stops
                // the read the same way `I` does. Asking here rather than
                // trusting the plan is what makes leaving fullscreen work: the
                // frame the panel comes back on is the frame the read starts, so
                // there is no "Loading EXIF…" with nothing behind it. `push`
                // dedups against queued and running work, and it is not in the
                // queue's `wanted` set — which is fine, because a rebuild that
                // drops it is followed by this same line on the next frame.
                if image_viewer::wants_exif(self.show_panels && self.show_left, self.exif_loading) {
                    // The panel pushes this rather than waiting for the plan, so
                    // it is the one metadata path `plan_jobs`' archive guard does
                    // not cover. An archive path names a container, not a
                    // picture, and has no EXIF of its own to read — the members
                    // show their own through this same block.
                    if let Some(path) = self
                        .cur
                        .as_ref()
                        .map(|k| k.path().to_path_buf())
                        .filter(|p| !is_archive_path(p))
                    {
                        self.loader.push(Job {
                            // EXIF describes the file, so it is read once and not
                            // per page — the same key `plan_jobs` uses.
                            key: ImgKey::first(&path),
                            kind: Kind::Exif,
                            prio: PRIO_EXIF,
                            target_dim: 0,
                        });
                    }
                }
                if self.exif_loading {
                    ui.label("Loading EXIF…");
                } else if let Some(info) = &self.exif_info {
                    for line in info.lines() {
                        if let Some(url) = line.strip_prefix("Maps: ") {
                            // GPS link: clickable, opens the system browser.
                            if ui.link("Open in Google Maps").clicked() {
                                if let Err(e) = open::that_detached(url) {
                                    self.error = Some(format!("Open link failed: {e}"));
                                }
                            }
                        } else {
                            ui.label(line);
                        }
                    }
                } else {
                    ui.label("No EXIF available.");
                }
                if let Some(p) = self.cur.as_ref() {
                    if let Ok(md) = std::fs::metadata(&p.path) {
                        ui.separator();
                        ui.label(format!("Size: {:.1} MB", md.len() as f64 / (1024.0 * 1024.0)));
                    }
                }
                // Exposure histogram: levels counted on the worker from the
                // preview-size buffer and carried with the preview, so this
                // draws numbers rather than pixels. Below the file's own
                // metadata because it describes the pixels, not the camera —
                // and what the file contains is the thing the EXIF lines
                // cannot say. Of the shown image (first frame for an
                // animation), at preview resolution: plenty for exposure,
                // and a full-res walk would buy nothing but time.
                if let Some(hist) =
                    self.shown.as_ref().and_then(|k| self.previews.get(k)).map(|pv| &pv.hist)
                {
                    ui.separator();
                    ui.heading("Histogram");
                    let total = hist.counted();
                    if total > 0 {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::Vec2::new(ui.available_width(), 64.0),
                            egui::Sense::hover(),
                        );
                        let painter = ui.painter().with_clip_rect(rect);
                        let max =
                            hist.bins.iter().flatten().copied().max().unwrap_or(1).max(1) as f32;
                        for (ch, color) in [
                            (0, egui::Color32::RED),
                            (1, egui::Color32::GREEN),
                            (2, egui::Color32::BLUE),
                        ] {
                            let pts: Vec<egui::Pos2> = (0..256)
                                .map(|i| {
                                    egui::Pos2::new(
                                        rect.min.x + i as f32 / 255.0 * rect.width(),
                                        rect.max.y - hist.bins[ch][i] as f32 / max * rect.height(),
                                    )
                                })
                                .collect();
                            painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0, color)));
                        }
                        let pct = |n: u32| 100.0 * n as f64 / total as f64;
                        ui.label(format!(
                            "Black clipped: {} ({:.1}%)",
                            hist.black,
                            pct(hist.black)
                        ));
                        ui.label(format!(
                            "White clipped: {} ({:.1}%)",
                            hist.white,
                            pct(hist.white)
                        ));
                    }
                }
            });
        }

        let mut resort = false;
        let mut refilter = false;
        if self.show_panels && self.show_right {
            let mut pane = egui::Panel::right("settings");
            if self.config.fit_window_mode {
                pane = pane.exact_size(300.0);
            }
            pane.show(ui, |ui| {
                ui.heading("Settings");
                // Scrollable so expanded sections don't run off the bottom of
                // short windows; no frame so it doesn't look like a "window
                // inside window" — each section is its own collapsible group.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::CollapsingHeader::new("Graphics & window")
                    .default_open(true)
                    .show(ui, |ui| {
                    // ---- renderer ------------------------------------------------
                    // Deliberately the first group: it is the only setting that can
                    // make the app fail to start at all, so it should not be
                    // something you have to go looking for.
                    ui.horizontal(|ui| {
                        ui.label("Graphics:")
                            .on_hover_text(
                                "Automatic probes each backend in its own process and keeps the \
                                 first that starts, because a driver that fails does so by \
                                 crashing rather than by returning an error.",
                            );
                        let mut choice = self.config.renderer.choice;
                        let mut changed = false;
                        egui::ComboBox::from_id_salt("renderer")
                            .selected_text(choice.label())
                            .show_ui(ui, |ui| {
                                for b in [
                                    image_viewer::WgpuBackend::Auto,
                                    image_viewer::WgpuBackend::Dx12,
                                    image_viewer::WgpuBackend::Gl,
                                    image_viewer::WgpuBackend::Vulkan,
                                ] {
                                    changed |= ui
                                        .selectable_value(&mut choice, b, b.label())
                                        .clicked();
                                }
                            });
                        if changed {
                            self.request_renderer_restart(ui.ctx(), choice);
                        }
                    });
                    ui.small(format!("In use: {}", self.renderer.label()));
                    if ui.button("Probe backends now").clicked() {
                        self.probe_renderer_now(ui.ctx());
                    }
                    if ui
                        .checkbox(&mut self.config.esc_closes_app, "Esc closes the app")
                        .on_hover_text("Off: Esc still leaves crop mode, the grid and fullscreen, but never quits.")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    ui.horizontal(|ui| {
                        ui.label("Worker threads (0 = auto):")
                            .on_hover_text("0 picks 2-4 from your CPU. Applies on restart.");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.workers)
                                    .range(0..=8),
                            )
                            .changed()
                        {
                            // The pool is built once at startup, so this takes
                            // effect on restart rather than rebuilding it live.
                            self.config.save(&self.config_path);
                        }
                    });
                    if ui
                        .checkbox(&mut self.config.single_instance, "Allow only one window")
                        .changed()
                    {
                        // Takes effect on next launch: the running process already
                        // holds the mutex, so flipping this cannot open or close one.
                        self.config.save(&self.config_path);
                        self.notice = Some(if self.config.single_instance {
                            "Single-instance on — takes effect on restart"
                        } else {
                            "Single-instance off — multiple windows allowed"
                        }.into());
                    }
                    // Window chrome, applied live: decorations have a viewport
                    // command, and always-on-top goes straight to Win32
                    // (`set_topmost`) because egui 0.36 only offers a builder for
                    // it. The launch loop reads the same flags, so a restart
                    // keeps whatever is set here.
                    if ui
                        .checkbox(&mut self.config.always_on_top, "Always on top")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        if !image_viewer::set_topmost(self.config.always_on_top) {
                            self.error = Some("Could not change window order".into());
                        }
                    }
                    if ui
                        .checkbox(&mut self.config.hide_title_bar, "Hide title bar")
                        .on_hover_text("Without a title bar there is no drag handle — Win+arrows still move the window.")
                        .changed()
                    {
                        // Left alone, winit keeps a decorations change's outer
                        // rect and lets the client area absorb (hide) or give
                        // up (show) the title strip: the picture jumps and
                        // re-fits, and in fit-window mode the window's hug
                        // breaks — "the option doesn't reset the window."
                        // Keep the client instead: it is re-asserted at its
                        // pre-toggle size, and the outer frame is put where
                        // [`image_viewer::outer_top_for_decoration_toggle`]
                        // computes, so only the chrome arrives or leaves.
                        let hiding = self.config.hide_title_bar;
                        let vp = ui.input(|i| {
                            let v = i.viewport();
                            v.inner_rect.zip(v.outer_rect).map(|(inner, outer)| {
                                (
                                    inner,
                                    outer,
                                    v.maximized.unwrap_or(false),
                                    v.fullscreen.unwrap_or(false),
                                )
                            })
                        });
                        match vp {
                            Some((inner, outer, maximized, fullscreen)) => {
                                // The strip is measurable only while the
                                // window is decorated, so a hide is the one
                                // moment to remember it; the reverse toggle
                                // reads it back. Measured even maximized — the
                                // clamped work area shifts `outer.top` by
                                // little more than the strip either way.
                                let strip = (inner.top() - outer.top()).max(0.0);
                                if hiding {
                                    self.config.title_strip = strip;
                                }
                                self.config.save(&self.config_path);
                                // Maximized and fullscreen windows are the
                                // monitor's work area before, during and
                                // after: WM_NCCALCSIZE clamps them, so moving
                                // or re-sizing would only put a maximized
                                // window in half measures. Leave those alone.
                                if maximized || fullscreen {
                                    ctx.send_viewport_cmd(
                                        egui::ViewportCommand::Decorations(!hiding),
                                    );
                                } else {
                                    let strip_after =
                                        if hiding { 0.0 } else { self.config.title_strip };
                                    let top = image_viewer::outer_top_for_decoration_toggle(
                                        inner.top(),
                                        strip_after,
                                    );
                                    // Enqueue order is settle order, and both
                                    // application orders converge: the size
                                    // re-assert anchors at the top-left it
                                    // sees, so whichever arrives first — the
                                    // frame already moved there, or the frame
                                    // moved after — ends with the frame at
                                    // `top` and the client's size and screen
                                    // position unchanged.
                                    ctx.send_viewport_cmd(
                                        egui::ViewportCommand::Decorations(!hiding),
                                    );
                                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                        egui::Pos2::new(inner.left(), top),
                                    ));
                                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                                        inner.size(),
                                    ));
                                }
                            }
                            // No geometry measured (the rects here are filled
                            // by eframe on Windows, but never say never): the
                            // toggle still applies, at yesterday's cost.
                            None => ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!hiding)),
                        }
                    }
                });

                egui::CollapsingHeader::new("Folders & files")
                    .default_open(true)
                    .show(ui, |ui| {
                    if ui
                        .checkbox(
                            &mut self.config.load_all_on_startup,
                            "Load all images in folder",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        if self.config.load_all_on_startup {
                            Self::load_folder_into(
                                &self.folder,
                                self.config.recursive_folders && self.zip_archive.is_none(),
                                &mut self.gallery,
                            );
                        }
                        self.goto(ctx);
                    }
                    if ui
                        .checkbox(
                            &mut self.config.recursive_folders,
                            "Include subfolders",
                        )
                        .on_hover_text("One gallery over the whole tree. The 2 s watcher walks it every poll, so this costs a walk per level.")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        // New files, possibly a new position: rescan rather than
                        // patching the list in place (`set_files` re-`goto`s when
                        // the cursor moved, so no explicit one here).
                        self.rescan(ctx, true);
                    }
                    if ui
                        .checkbox(&mut self.config.show_archives, "Show archives (zip, cbz, 7z, rar)")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        // The library reads this through `is_browsable`, so nothing is
                        // rescanned until it is told — and a listing rebuilt before
                        // the setter ran would keep the old answer.
                        image_viewer::set_show_archives(self.config.show_archives);
                        self.rescan(ctx, true);
                    }
                    if ui
                        .checkbox(
                            &mut self.config.thumbnail_cache,
                            "Cache grid thumbnails on disk",
                        )
                        .on_hover_text(format!(
                            "Up to {} of cells, so a folder of large scans opens once at full \
                             speed. Stored in {}.",
                            human_bytes(image_viewer::THUMB_CACHE_MAX_BYTES),
                            thumb_cache_root().display()
                        ))
                        .changed()
                    {
                        // Nothing to invalidate. The cells in `thumbs` are decoded
                        // pictures and stay correct whichever way this goes; only
                        // whether *new* ones are written changes. Dropping them here
                        // would cost a re-decode of every visible cell — up to 50 of
                        // them at ~700 ms each on a folder of large scans — to buy
                        // nothing at all.
                        set_thumbnail_cache(self.config.thumbnail_cache);
                        self.config.save(&self.config_path);
                    }
                    if ui
                        .checkbox(
                            &mut self.config.preview_store,
                            "Cache previews of large pictures on disk",
                        )
                        .on_hover_text(format!(
                            "Up to {} of previews, so showing a large scan again does not \
                             decode it again (690 ms to decode, 22-50 ms to read back, \
                             measured). Stored in {}.",
                            human_bytes(image_viewer::PREVIEW_STORE_MAX_BYTES),
                            image_viewer::preview_store_root().display()
                        ))
                        .changed()
                    {
                        // Nothing to invalidate, for the same reason the cell cache
                        // has nothing to invalidate: a stored preview is the
                        // decode's own pixels, so it stays correct whichever way
                        // this goes, and dropping them would cost a re-decode of
                        // every large picture for nothing.
                        image_viewer::set_preview_store(self.config.preview_store);
                        if self.config.preview_store {
                            // Turning it **on** has to start the thread that does
                            // the encoding: the store is written by a thread of
                            // its own so the encode never lands in front of the
                            // first pixels.
                            image_viewer::start_preview_store();
                        } else {
                            // Turning it **off** means off, disk included: a
                            // setting that stops the writing but leaves 512 MB
                            // behind is a promise kept in letter only.
                            image_viewer::clear_preview_store();
                        }
                        self.config.save(&self.config_path);
                    }
                    let mut new_sort = self.config.sort;
                    ui.horizontal(|ui| {
                        ui.label("Sort:");
                        egui::ComboBox::from_id_salt("sort_mode")
                            .selected_text(format!("{new_sort:?}"))
                            .show_ui(ui, |ui| {
                                for m in [
                                    SortMode::Name,
                                    SortMode::Date,
                                    SortMode::Size,
                                    SortMode::Taken,
                                    SortMode::Random,
                                ] {
                                    ui.selectable_value(&mut new_sort, m, format!("{m:?}"));
                                }
                            });
                    });
                    if new_sort != self.config.sort {
                        self.config.sort = new_sort;
                        self.config.save(&self.config_path);
                        resort = true;
                    }
                    if ui
                        .checkbox(&mut self.config.sort_descending, "Descending sort order")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        resort = true;
                    }
                    if ui
                        .checkbox(&mut self.config.loop_back, "Loop back at folder ends")
                        .changed()
                    {
                        // Apply straight away; the gallery owns the wrap behaviour,
                        // so it has to be told.
                        self.gallery.loop_back = self.config.loop_back;
                        self.config.save(&self.config_path);
                    }

                    // ---- back/forward history ----
                    ui.separator();
                    ui.label("Back/forward history");
                    ui.horizontal(|ui| {
                        let back_enabled = !self.history_back.is_empty();
                        let fwd_enabled = !self.history_forward.is_empty();
                        if ui
                            .add_enabled(
                                back_enabled,
                                egui::Button::new("← Back").shortcut_text("Alt+Left"),
                            )
                            .on_hover_text(if back_enabled {
                                "Return to the previously viewed image"
                            } else {
                                "Nothing to go back to"
                            })
                            .clicked()
                        {
                            self.go_back(ctx);
                        }
                        if ui
                            .add_enabled(
                                fwd_enabled,
                                egui::Button::new("Forward →").shortcut_text("Alt+Right"),
                            )
                            .on_hover_text(if fwd_enabled {
                                "Return to the image you went back from"
                            } else {
                                "Nothing to go forward to"
                            })
                            .clicked()
                        {
                            self.go_forward(ctx);
                        }
                        ui.label(format!(
                            "{} back / {} forward",
                            self.history_back.len(),
                            self.history_forward.len()
                        ));
                    });

                    // ---- favourite folders ----
                    ui.separator();
                    ui.label("Favourite folders");
                    for fav in self.config.favourite_folders.clone() {
                        let exists = fav.is_dir();
                        let label = format!(
                            "{} {}",
                            fav.display(),
                            if exists { "" } else { " (missing)" }
                        );
                        if ui
                            .add_enabled(
                                exists,
                                egui::Button::new(label).shortcut_text(""),
                            )
                            .on_hover_text("Open this folder")
                            .clicked()
                        {
                            self.close_transients();
                            self.open_folder(ctx, fav.clone());
                        }
                        if ui.button("✕").on_hover_text("Remove favourite").clicked() {
                            if let Some(pos) = self
                                .config
                                .favourite_folders
                                .iter()
                                .position(|p| p == &fav)
                            {
                                self.config.favourite_folders.remove(pos);
                                self.config.save(&self.config_path);
                            }
                        }
                    }
                    if ui.button("Add current folder").clicked() {
                        let already = self
                            .config
                            .favourite_folders
                            .iter()
                            .any(|p| p == &self.folder);
                        if !already {
                            self.config.favourite_folders.push(self.folder.clone());
                            self.config.save(&self.config_path);
                        }
                    }
                });

                egui::CollapsingHeader::new("View & zoom")
                    .default_open(true)
                    .show(ui, |ui| {
                    if ui
                        .checkbox(
                            &mut self.config.linear_resample,
                            "Linear-light downsampling",
                        )
                        .changed()
                    {
                        set_linear_resample(self.config.linear_resample);
                        self.config.save(&self.config_path);
                    }
                    // Off by default: this changes how photos look, so it is opt-in
                    // rather than a correction. Like the toggle above it applies at
                    // decode time and does not re-decode what is already cached — move
                    // to another image, or restart, to see it everywhere.
                    if ui
                        .checkbox(
                            &mut self.config.convert_wide_gamut,
                            "Convert Display P3 photos to sRGB",
                        )
                        .on_hover_text("Colours outside sRGB are clipped to fit, which is lossy.")
                        .changed()
                    {
                        image_viewer::set_wide_gamut_conversion(self.config.convert_wide_gamut);
                        self.config.save(&self.config_path);
                    }
                    // The same surface as the thumbnail cache — what a grid cell
                    // looks like. Applies live — the grid reads
                    // the value every frame, so no invalidation or re-decode.
                    ui.horizontal(|ui| {
                        ui.label("Grid cell size:");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.grid_cell)
                                    .range(64.0..=400.0)
                                    .speed(1.0),
                            )
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                        ui.label("pt");
                    });
                    let mut new_filter = self.config.downscale_filter;
                    ui.horizontal(|ui| {
                        ui.label("Filter:");
                        egui::ComboBox::from_id_salt("ds_filter")
                            .selected_text(format!("{:?}", self.config.downscale_filter))
                            .show_ui(ui, |ui| {
                                for f in [
                                    DownscaleFilter::Hermite,
                                    DownscaleFilter::Catrom,
                                    DownscaleFilter::Lanczos3,
                                ] {
                                    ui.selectable_value(&mut new_filter, f, format!("{f:?}"));
                                }
                            });
                    });
                    if new_filter != self.config.downscale_filter {
                        self.config.downscale_filter = new_filter;
                        set_downscale_filter(new_filter);
                        self.config.save(&self.config_path);
                        refilter = true;
                    }
                    // Sits with the shape dropdown rather than under it because it is
                    // the same decision — how a downscaled page is resampled — and
                    // the two axes have to be read together to make sense of either.
                    // The label says what it *does*, not that a `filter_scale` is
                    // multiplied: the user is choosing between a clean tone field
                    // and a crisp line, and neither word describes the mechanism.
                    //
                    // **Labelled for the effect, because the effect is what the user
                    // can see.** "Low-pass strength" is a true description and a
                    // useless one; nobody looking at a page with a lattice in it is
                    // going to find a checkbox that says "low-pass strength".
                    if ui
                        .checkbox(
                            &mut self.config.smooth_downscale,
                            "Smooth: soften the downscale to hide a dot grid",
                        )
                        .on_hover_text(
                            "Off by default. A downscaled page's fine detail can fold back \
                             on itself into a grid of dark and light. Smooth low-passes \
                             harder: it hides that grid on a page screened at a single \
                             pitch, and can bring one out on a page whose tones use more \
                             than one, and edges come out noticeably softer. Fast is \
                             crisper.",
                        )
                        .changed()
                    {
                        // Apply straight away. Nothing re-queues a decode on its own,
                        // so the alternative is "looks like it worked after a
                        // restart", which is the same failure as not applying it.
                        set_downscale_smoothing(self.config.smooth_downscale);
                        self.config.save(&self.config_path);
                        // **Invalidate, or this reads as a broken setting.** The
                        // previews already in `previews` were decoded by the *other*
                        // kernel, and nothing else would ever re-queue them: the
                        // filter dropdown has carried `refilter` for exactly this
                        // reason, and `linear_resample` a few rows up is the
                        // documented counter-example of what happens without it.
                        refilter = true;
                    }
                    // **Says the cost is real, and says it is not a clear win.** The
                    // label used to claim Smooth keeps a grid from forming, which is
                    // only half of what it does: on pages whose greys are screened at
                    // more than one pitch it can bring a grid *out* instead. It is
                    // off by default for exactly that reason, so the hover text is
                    // what a reader has to go on when they turn it on, and promising a
                    // one-sided win here would be the same defect as hiding the cost.
                    // The 1.71x figure is the measured step-edge softening and is
                    // still true; it is quoted so the trade's size is on the screen.

                    // Pixel art is a **sampling** choice, made when each content
                    // texture is uploaded: nearest on magnify, linear on minify.
                    // Nearest is for the *magnification* axis only, because
                    // minifying nearest is what drops pixel rows and reads as a
                    // broken picture — so the shipped linear minification stays
                    // for every fit and zoomed-out view, photo or sprite.
                    if ui
                        .checkbox(
                            &mut self.config.pixel_art,
                            "Pixel art: sharp pixels when zoomed in",
                        )
                        .on_hover_text(
                            "Off by default. For pixel drawings: the magnifying filter samples \
                             nearest, so a sprite zoomed past 100% stays squares of its own \
                             pixels instead of smearing; zoomed out on anything, the averaging \
                             filter still runs, so a downscaled picture does not lose rows. \
                             Applies to the picture, its zoom tiles and animation frames, not \
                             the grid. Flipping it re-requests the previews; the old picture \
                             stays until the replacement arrives.",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        // **Invalidate the same frame.** The sampling was baked
                        // into the textures already in `previews` and `full`'s
                        // tiles, and nothing else would re-ask: the box flips and
                        // the picture keeps the samplers it arrived with, which is
                        // exactly the broken-feature shape the refilter walk below
                        // (and `the_smooth_toggle_is_wired...` in tests/unit.rs)
                        // documents for a toggle whose work is not re-done. The
                        // replacements arrive into the same slot, so the old
                        // picture stays up until then — no blank flash.
                        refilter = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.config.block_while_loading,
                            "Wait for image when holding arrows",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    // Fixed zoom ratios on `1`–`4`, as source-pixel multiples
                    // (`1` is 100%). `zoom_preset` clamps on read, so anything
                    // typed here stays sane.
                    ui.horizontal(|ui| {
                        ui.label("Zoom keys 1-4:");
                        let mut changed = false;
                        for (n, z) in self.config.zoom_presets.iter_mut().enumerate() {
                            ui.label(format!("{}:", n + 1));
                            changed |= ui
                                .add(egui::DragValue::new(z).range(0.1..=32.0).speed(0.1))
                                .changed();
                        }
                        if changed {
                            self.config.save(&self.config_path);
                        }
                    });
                    if ui
                        .checkbox(
                            &mut self.config.wheel_navigation,
                            "Mouse wheel flips images (Ctrl+wheel zooms)",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    // Session view state — memory that exists nowhere but this
                    // run — so the flip needs no invalidation: it changes what a
                    // future arrival restores, not anything on screen or in a
                    // decode. The false arm is exactly the shipped behaviour,
                    // and both arms are `arriving_view`'s recorded answer.
                    if ui
                        .checkbox(
                            &mut self.config.remember_view,
                            "Remember zoom and pan per image",
                        )
                        .on_hover_text(
                            "An image you zoomed or panned comes back the way you left it, while \
                             you are browsing; others keep the zoom carried over. Per page of a \
                             document. Lost when the app closes.",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    // A property of the *gesture*, not of the app: it changes what
                    // a drag does, and nothing about it applies until somebody drags.
                    ui.horizontal(|ui| {
                        ui.label("Crop shape:")
                            .on_hover_text(
                                "Frames a new crop to this shape; Shift forces a square for \
                                 one drag. A frame already drawn keeps the shape it has.",
                            );
                        let mut choice = self.config.crop_aspect;
                        egui::ComboBox::from_id_salt("crop_aspect")
                            .selected_text(choice.label())
                            .show_ui(ui, |ui| {
                                for a in image_viewer::CropAspect::ALL {
                                    ui.selectable_value(&mut choice, a, a.label());
                                }
                            });
                        if choice != self.config.crop_aspect {
                            self.config.crop_aspect = choice;
                            self.config.save(&self.config_path);
                        }
                    });
                });

                ui.collapsing("Slideshow & animation", |ui| {
                    // Slideshow: one picture per interval until toggled off, or
                    // until the end with looping off. The pace persists; running
                    // does not survive a restart.
                    ui.horizontal(|ui| {
                        let label = if !self.slideshow {
                            "Start slideshow"
                        } else if self.slideshow_paused {
                            "Resume slideshow"
                        } else {
                            "Pause slideshow"
                        };
                        if ui.button(label).clicked() {
                            if !self.slideshow {
                                self.start_slideshow();
                            } else {
                                self.toggle_slideshow_pause();
                            }
                        }
                        ui.label("every");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.slideshow_secs)
                                    .range(1..=60),
                            )
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                        ui.label("s");
                        // Beside the pace because it is the same decision — how a
                        // show walks — and a shuffle buried in a submenu is a
                        // setting nobody finds. Session-only like the show itself:
                        // reshuffled every start, folder order restored on stop.
                        if ui
                            .checkbox(&mut self.config.slideshow_shuffle, "Shuffle")
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                    });
                    // Animation playback speed, next to the slideshow for the
                    // same reason it sits there: both are time-based playback,
                    // and both apply live with no re-decode.
                    ui.horizontal(|ui| {
                        ui.label("Animation speed (1 = file timing):");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.anim_speed)
                                    .range(0.1..=8.0)
                                    .speed(0.25),
                            )
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                    });
                });

                // ---- keys ---------------------------------------------------
                //
                // Every action rebindable: click a chord, press keys. Meanings
                // never change (navigation still asks `nav_is_forward`), only
                // the chords. While one waits, the dispatch stands down so the
                // capture press cannot act — deleting a photo by rebinding
                // Delete is exactly the accident this gate exists to prevent.
                // Capture: fresh (non-repeat) key presses become the chord.
                // Bare Escape cancels (so Escape is not bindable — it already
                // owns the typing/grid/fullscreen/quit chain, which is
                // structural rather than a binding).
                if let Some(action) = self.capturing.clone() {
                    // Fresh non-modifier presses only: the modifier keys
                    // themselves also arrive as Key events, and committing the
                    // first event would turn "hold Ctrl, press G" into
                    // "Ctrl+?" — the capture waits for the real key, whose
                    // event already carries the held modifiers.
                    let pressed: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
                        i.events
                            .iter()
                            .filter_map(|e| match e {
                                egui::Event::Key {
                                    key,
                                    pressed: true,
                                    repeat: false,
                                    modifiers,
                                    ..
                                } if !is_modifier_key(*key) => Some((*key, *modifiers)),
                                _ => None,
                            })
                            .collect()
                    });
                    if pressed.iter().any(|(k, m)| {
                        *k == egui::Key::Escape && !m.any()
                    }) {
                        self.capturing = None;
                    } else if let Some((key, mods)) = pressed.first() {
                        let mut raw = String::new();
                        if mods.ctrl {
                            raw.push_str("Ctrl+");
                        }
                        if mods.shift {
                            raw.push_str("Shift+");
                        }
                        if mods.alt {
                            raw.push_str("Alt+");
                        }
                        raw.push_str(key_name(*key));
                        match image_viewer::canonical_chord(&raw) {
                            Some(chord)
                                if image_viewer::split_chord(&chord)
                                    .is_some_and(|(_, _, _, name)| {
                                        parse_key_full(&name).is_some()
                                    }) =>
                            {
                                // Duplicates are allowed and shown, not
                                // refused: blocking second-guesses the reader
                                // (chords can be intentional — one key stepping
                                // two views is sometimes exactly the point),
                                // while a silent double-fire is the failure
                                // mode. So the commit always lands, the notice
                                // names the sharers, and every row below marks
                                // its own conflicts in red.
                                if chord == image_viewer::default_binding(&action) {
                                    self.config.keybindings.remove(&action);
                                } else {
                                    self.config.keybindings.insert(action.clone(), chord.clone());
                                }
                                set_nav_overrides(&self.config.keybindings);
                                self.config.save(&self.config_path);
                                let sharers: Vec<&str> = image_viewer::KEY_ACTIONS
                                    .iter()
                                    .filter(|(id, _, _)| {
                                        *id != action
                                            && image_viewer::resolve_binding(&self.config.keybindings, id)
                                                == chord
                                    })
                                    .map(|(_, _, label)| *label)
                                    .collect();
                                self.notice = Some(if sharers.is_empty() {
                                    format!("{label} is now {chord}", label = action_label(&action))
                                } else {
                                    format!(
                                        "{label} is now {chord} (also: {sharers})",
                                        label = action_label(&action),
                                        sharers = sharers.join(", ")
                                    )
                                });
                                self.capturing = None;
                            }
                            _ => {
                                self.notice = Some(format!(
                                    "{raw} is not a bindable key"
                                ));
                                self.capturing = None;
                            }
                        }
                    }
                }
                ui.collapsing("Keys", |ui| {
                    if ui.button("Reset keys").clicked() {
                        self.config.keybindings.clear();
                        set_nav_overrides(&self.config.keybindings);
                        self.config.save(&self.config_path);
                        self.notice = Some("Keys reset to defaults".into());
                    }
                    egui::ScrollArea::vertical()
                        .id_salt("key_list")
                        .max_height(300.0)
                        .show(ui, |ui| {
                            for (id, _, label) in image_viewer::KEY_ACTIONS {
                                ui.horizontal(|ui| {
                                    ui.label(*label);
                                    let current = image_viewer::resolve_binding(&self.config.keybindings, id);
                                    let text = if self.capturing.as_deref() == Some(*id) {
                                        "press keys… (Esc cancels)".to_owned()
                                    } else if current.is_empty() {
                                        // The one blank shipped row (`next_alt`):
                                        // deliberately unbound, so say so rather
                                        // than drawing a button with no name.
                                        "(unbound)".to_owned()
                                    } else {
                                        current.clone()
                                    };
                                    if ui.button(text).clicked() {
                                        self.capturing = Some((*id).to_owned());
                                    }
                                    // Conflict marker: every other action on this
                                    // exact chord, in red. Duplicates fire
                                    // together — this row is where that reads as
                                    // intentional rather than mysterious.
                                    let others: Vec<&str> = image_viewer::KEY_ACTIONS
                                        .iter()
                                        .filter(|(other, _, _)| {
                                            *other != *id
                                                && image_viewer::resolve_binding(
                                                    &self.config.keybindings,
                                                    other,
                                                ) == current
                                        })
                                        .map(|(_, _, other_label)| *other_label)
                                        .collect();
                                    if !others.is_empty() {
                                        ui.colored_label(
                                            ui.visuals().warn_fg_color,
                                            format!("also: {}", others.join(", ")),
                                        );
                                    }
                                });
                            }
                        });
                    ui.small("Esc quits the app chain and is not bindable; Ctrl+C keeps its platform meaning alongside any alternative you set.");
                });

                ui.collapsing("User commands", |ui| {
                    // ---- user commands ------------------------------------------
                    //
                    // External programs run against the current file (Tier 1 #2):
                    // Ctrl+1..9 in list order. `{file}`, `{dir}` and `{name}`
                    // expand in the program and in every arg, so
                    // `mspaint.exe "{file}"` opens the picture on screen in Paint.
                    // Spawned detached — the viewer never waits — so this is an
                    // "open with", not scripting with results.
                    ui.horizontal(|ui| {
                        ui.label("User commands (Ctrl+1..9):");
                        ui.colored_label(ui.visuals().warn_fg_color, "(beta)");
                    });
                    let mut cmds_changed = false;
                    let mut remove_at: Option<usize> = None;
                    for (n, cmd) in self.config.user_commands.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}:", n + 1));
                            if ui.text_edit_singleline(&mut cmd.name).changed() {
                                cmds_changed = true;
                            }
                            if ui.small_button("x").clicked() {
                                remove_at = Some(n);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("program");
                            if ui.text_edit_singleline(&mut cmd.program).changed() {
                                cmds_changed = true;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label("args");
                            let mut joined = cmd.args.join(" ");
                            if ui.text_edit_singleline(&mut joined).changed() {
                                // Space-separated; quote nothing. Args with real
                                // spaces in them need their program to take one
                                // `{file}` and split it itself — documented below.
                                cmd.args = joined
                                    .split_whitespace()
                                    .map(|s| s.to_owned())
                                    .collect();
                                cmds_changed = true;
                            }
                        });
                    }
                    if let Some(n) = remove_at {
                        self.config.user_commands.remove(n);
                        cmds_changed = true;
                    }
                    if ui.button("Add command").clicked() {
                        self.config.user_commands.push(image_viewer::UserCommand {
                            name: String::new(),
                            program: String::new(),
                            args: Vec::new(),
                        });
                        cmds_changed = true;
                    }
                    if cmds_changed {
                        // Saved on edit, not per frame: the flag above is only set
                        // by a field that reported `changed()`. Empty rows are
                        // kept, not pruned: pruning while typing deletes the row
                        // being typed in.
                        self.config.save(&self.config_path);
                    }
                    ui.small("One arg per space. {file} {dir} {name} expand. Empty args just passes {file}.");
                });

                ui.collapsing("Manga", |ui| {
                    // ---- manga mode -------------------------------------------
                    //
                    // `M` and `D` reach these same two fields, so this panel and the
                    // keyboard cannot drift: both flip the bool and save.
                    //
                    // **`Manga mode` re-`goto`s and `Right-to-left` does not**, the same
                    // asymmetry the key handlers have and for the same reason - see
                    // `toggle_manga_mode` and `toggle_manga_rtl`. With the mode off the
                    // second box is a setting for nothing, so it says so rather than
                    // sitting there looking live.
                    //
                    // Both beta features carry a red marker, the same one: this is
                    // the promise that the rough edges are known and written down
                    // (README's beta sections), not that the setting is dangerous.
                    let mut mode_changed = false;
                    let manga_label = action_button_text(&self.config, "Double page mode", "manga");
                    let rtl_label = action_button_text(&self.config, "Right-to-left pages", "rtl");
                    ui.horizontal(|ui| {
                        mode_changed = ui
                            .checkbox(&mut self.config.manga_mode, manga_label)
                            .changed();
                        ui.colored_label(ui.visuals().warn_fg_color, "(beta)");
                    });
                    if mode_changed {
                        self.config.save(&self.config_path);
                        self.goto(ctx);
                    }
                    if ui
                        .checkbox(&mut self.config.manga_rtl, rtl_label)
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        ctx.request_repaint();
                    }
                    if !self.config.manga_mode {
                        ui.small("Both are for double page mode, which is off.");
                    } else {
                        ui.small("A turn opens on the right, so Left reads forwards. \
                                  Right-to-left off makes it match Left-to-right.");
                        // The disclosure `AGENTS.md` requires beside any cache: a file
                        // that silently re-pairs a library must not be enabled quietly.
                        // Deleting it costs nothing but the decisions in it.
                        match spreads_root() {
                            Some(store) => {
                                ui.small(format!(
                                    "Pairing overrides live in {}. Delete that file to clear \
                                     every override and go back to the detected shapes.",
                                    store.display()
                                ));
                            }
                            None => {
                                ui.small(
                                    "No local app-data folder, so pairing overrides are kept in \
                                     memory only and are forgotten at exit.",
                                );
                            }
                        }
                    }
                });

                ui.collapsing("File associations", |ui| {
                    let registered = fileassoc::is_registered();
                    if registered {
                        ui.label(format!("Registered as '{}'", fileassoc::progid()));
                        // A missing icon is invisible here and only shows up as a
                        // generic application icon in a file list, so say so.
                        match fileassoc::default_icon() {
                            Some(icon) => {
                                ui.small(format!("Icon: {icon}"));
                            }
                            None => {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    "No icon registered — files will show a generic icon",
                                );
                            }
                        }
                        if ui.button("Unregister file types").clicked() {
                            match fileassoc::unregister() {
                                Ok(_) => self.notice = Some("File associations removed".into()),
                                Err(e) => self.error = Some(format!("Unregister failed: {e}")),
                            }
                        }
                    } else if ui.button("Register as image viewer…").clicked() {
                        match fileassoc::register() {
                            Ok(_) => self.notice = Some("Registered — set default via Explorer's 'Open with'".into()),
                            Err(e) => self.error = Some(format!("Register failed: {e}")),
                        }
                    }
                    ui.small("Registers in the user registry (no admin). To make it the default, right-click an image > Open with > Choose another app > Image Viewer > Always.");
                });
                ui.label(format!("Config: {}", self.config_path.display()));
                // Which checkout this binary came from, so a copy sitting in
                // Downloads can be told apart from the current build.
                ui.small(format!("Build {}", env!("IV_BUILD_ID")));
                });
            });
        }
        if resort {
            self.notice = Some(format!(
                "Sorted by {:?}{}",
                self.config.sort,
                if self.config.sort_descending { ", descending" } else { "" }
            ));
            self.rescan(ctx, true);
        }
        if refilter {
            // Realtime filter preview: drop every preview and re-queue at
            // the current target; old textures remain on screen until the
            // replacements arrive (no blank flash).
            self.previews.clear();
            self.full = None;
            self.anim = None;
            self.failed.clear();
            self.load_errors.clear();
            self.reset_display();
            self.goto(ctx);
        }

        egui::CentralPanel::default().frame(egui::Frame::new().fill(Color32::BLACK)).show(
            ui,
            |ui| {
                self.central(ui);
            },
        );

        // Who is asking for these repaints? egui tags every request with the
        // call site, so this names the culprit instead of guessing. No-op
        // unless sampling is on.
        for c in ctx.repaint_causes() {
            perf::count_repaint_cause(&c);
        }
    }
}
