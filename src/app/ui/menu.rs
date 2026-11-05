//! `menu` panel, extracted from `MyApp::ui` when `src/app/ui.rs` passed the
//! 1000-line target.

use crate::*;

impl MyApp {
    pub(crate) fn ui_menu(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        _last: bool,
        _toggle: bool,
    ) {
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
    }
}
