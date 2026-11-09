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
use image_viewer::FsMode;

mod exif;
mod menu;
mod settings;
mod status;

// Per-frame input reads, extracted from `ui()` so each dispatch site reads
// from a single resolved snapshot rather than re-querying egui.

/// Every input resolved once per frame, before any dispatch acts on it.
struct FrameInputs {
    typing: bool,
    kb: std::collections::HashMap<String, ResolvedBinding>,
    f11: bool,
    esc: bool,
}

impl MyApp {
    /// Read every input up front, before anything acts on it.
    ///
    /// `typing` gates all global single-key handling; `kb` maps action names
    /// to resolved chords; `f11`/`Esc` are the two global keys that override
    /// everything. Navigation (`nav_direction`) is deliberately *not* here:
    /// it must stay in `ui()` so the source scan can verify the one direction
    /// rule.
    fn read_inputs(&self, ctx: &Context) -> FrameInputs {
        let typing = ctx.egui_wants_keyboard_input();
        let kb = build_bindings(&self.config);
        let f11 = action_pressed(ctx, &kb, "fullscreen");
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        FrameInputs { typing, kb, f11, esc }
    }

    /// Whether the window is covering the whole screen: either a real fullscreen
    /// request (`ViewportCommand::Fullscreen`, which the viewport reports) or a
    /// cover experiment holding the monitor rect without one — the shell was
    /// never told, so only this app knows.
    pub(crate) fn is_fullscreen(&self, ctx: &egui::Context) -> bool {
        ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) || self.covered
    }

    /// The fullscreen-mode selector in Settings: `inset` (default, undecorated
    /// pulled in one pixel per edge), `real` (the shipped
    /// `ViewportCommand::Fullscreen`), `cover`, `resize`, or `safe`
    /// (decorated, maximized — nothing ever blanks). Drawn as its own labeled
    /// row inside "Graphics & window" rather than plopped in bare.
    pub(crate) fn ui_fullscreen_mode(&mut self, ui: &mut egui::Ui) {
        ui.label("Fullscreen mode").on_hover_text(
            "How F11 fullscreen is realised. `Inset` (default) is undecorated \
             but pulled in one pixel per edge, which avoids the display \
             re-sync that blanks some GPU/driver/monitor combinations. \
             `Safe` stays decorated so nothing ever blanks.",
        );
        ui.add_space(4.0);
        egui::ComboBox::from_id_salt("fullscreen_mode")
            .selected_text(self.config.fullscreen_mode.label())
            .show_ui(ui, |ui| {
                for mode in [
                    image_viewer::FsMode::Inset,
                    image_viewer::FsMode::Real,
                    image_viewer::FsMode::Cover,
                    image_viewer::FsMode::Resize,
                    image_viewer::FsMode::Safe,
                ] {
                    if ui
                        .selectable_label(self.config.fullscreen_mode == mode, mode.label())
                        .clicked()
                    {
                        self.config.fullscreen_mode = mode;
                        self.config.save(&self.config_path);
                    }
                }
            });
        ui.add_space(8.0);
    }
}

impl eframe::App for MyApp {
    /// Persist the last-viewed image per folder on app close, then clean up
    /// any extracted archive temp dir.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
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

        // The window is created hidden (`with_visible(false)` in `main.rs`) so
        // the compositor never composites a blank frame before the first
        // texture uploads. Show it once the first frame is painted.
        if self.first_frame {
            self.first_frame = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }

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
        // rather than a hold. `Option<bool>` because `step` takes a `bool`
        // (forwards is `true`); `nav` is passed straight through with no
        // arithmetic on it anywhere in the dispatch.
        let step_dir = nav;

        let inputs = self.read_inputs(ctx);
        let typing = inputs.typing;
        let f11 = inputs.f11;
        let esc = inputs.esc;
        let kb = inputs.kb;

        // The grid's arrows do not go through the direction rule: a grid has one
        // cell per file with no reading order. `Space` is in here because it has
        // always moved the highlight, and a key that stops working because manga
        // mode is on would be a regression nothing in the tree can see.
        let (grid_right, grid_left) = ctx.input(|i| {
            (
                binding_down(i, &kb, "grid_right") || binding_down(i, &kb, "grid_right_alt"),
                binding_down(i, &kb, "grid_left"),
            )
        });
        // Backspace goes up one level in the grid: parent folder, or out of an
        // archive. Mirroring the up-arrow cell in the grid.
        let backspace = ctx.input(|i| i.key_pressed(egui::Key::Backspace));

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
            let fs = self.is_fullscreen(ctx);
            match self.config.fullscreen_mode {
                // Real: `ViewportCommand::Fullscreen` — egui-winit turns it into
                // `Fullscreen::Borderless`, which winit's Windows backend marks
                // with `ITaskbarList2::MarkFullscreenWindow`. That is the shell
                // being told the window owns the display, and taking or giving
                // that ownership back can blank the output for seconds on some
                // GPU/driver/monitor combinations. Read once per press so the
                // held-arrows wait-for-load mode sees one edge.
                FsMode::Real => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
                    self.suppress_resize_redecode = true;
                }
                // Safe: decorated, maximized, panels hidden — the fullscreen
                // *feel* without an undecorated window. On GPU/drivers that
                // re-sync the display output when a window goes undecorated and
                // covers the whole output, this is the arm that stays clean:
                // nothing blanks, the title bar and taskbar remain.
                FsMode::Safe => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!fs));
                    self.suppress_resize_redecode = true;
                    self.covered = !fs;
                }
                // Cover/Inset/Resize: undecorated window at monitor size, no
                // fullscreen request. The saved placement carries maximized
                // state, so leaving restores the window exactly as it was.
                mode => {
                    if fs {
                        image_viewer::uncover_monitor();
                        self.covered = false;
                    } else {
                        self.covered = image_viewer::cover_monitor(mode);
                    }
                    self.suppress_resize_redecode = true;
                }
            }
            // Panel handling is the same whichever mode is chosen: entering
            // stashes the master, leaving restores it. One take() here, one
            // take() on the Esc exit below — that's the whole count, so the
            // source scan in unit_memory.rs sees two.
            if fs {
                self.show_panels = self.panels_before_fullscreen.take().unwrap_or(true);
            } else {
                self.panels_before_fullscreen = Some(self.show_panels);
                self.show_panels = false;
            }
        }
        if esc {
            //
            // Its path box holds focus when it opens, so `typing` is true while
            // the user is naming a file — and Esc landed in the search box's
            // arm, which cleared the *filter* and left the panel standing.
            // One Esc later it fell through to the quit branch and closed the
            // app with the panel still up.
            //
            // So the panel is checked before `typing`, and the rule it follows
            // is the same one the archive arm states below: Esc is "go back one
            // level", and a modal panel is a level above the viewer. It is also
            // the only modal here that *writes* — Esc on a panel holding a
            // half-typed destination must not be a quit.
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
                // A behaviour change to `esc_closes_app`: the setting decides
                // whether Esc quits *the viewer*, and inside an archive Esc is
                // answering "leave the archive" — the same relationship crop
                // mode and the grid already have above it. One level down and
                // Esc quits exactly as before.
                if !self.leave_archive(ctx, false) {
                    self.notice = Some("No folder to go back to (archive opened directly)".into());
                }
            } else {
                let in_fullscreen = self.is_fullscreen(ctx);
                if in_fullscreen {
                    match self.config.fullscreen_mode {
                        FsMode::Real => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                        }
                        FsMode::Safe => {
                            ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(false));
                            self.covered = false;
                        }
                        _ => {
                            image_viewer::uncover_monitor();
                            self.covered = false;
                        }
                    }
                    // What the master stood at *before this fullscreen*, not
                    // hard `true` — same reason the `f11` exit restores the
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
        // `M` (move) and `C` (copy) used to be bare single-key shortcuts. They
        // are not any more: both **write to the filesystem** on a keystroke
        // with nothing standing between the key and the file. `C` is the wrong
        // letter for a mutation because it is also the first thing anyone
        // reaches for in a text box. `C` is now Crop, which touches nothing on
        // disk until `Ctrl+S`.
        //
        // The single-key budget goes to actions that cannot lose data. Triage
        // is still one keystroke for Delete; Move and Copy are buttons.
        //
        // `Ctrl+C` is **not** a key event: `egui-winit` intercepts clipboard
        // chords and converts them, dropping the key. So `Ctrl+C` is read from
        // the converted `Event::Copy`, not from a `Key { key: C }` press.
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
                } else if backspace {
                    // Backspace goes up one level: parent folder, or out of an
                    // archive. Mirrors the up-arrow cell in the grid.
                    if self.zip_archive.is_some() {
                        self.leave_archive(ctx, false);
                    } else if let Some(parent) = self.folder_stack.pop() {
                        self.open_folder(ctx, parent);
                    }
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
        if changed && !self.covered && self.last_geometry_save.elapsed() > Duration::from_secs(1) {
            self.config.window_size = new_size;
            self.config.window_pos = new_pos;
            self.config.window_maximized = new_maximized;
            self.config.save(&self.config_path);
            self.last_geometry_save = Instant::now();
        }

        self.ui_status(ui, ctx, focus_search);

        self.ui_menu(ui, ctx, last, toggle);

        self.ui_exif(ui, ctx, first, toggle);

        let (resort, refilter) = self.ui_settings(ui, ctx, first, toggle);
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
