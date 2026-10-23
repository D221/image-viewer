//! Module `app_save_as`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// Show/hide the thumbnail grid (G key and the bottom-panel button).
    pub(crate) fn toggle_gallery(&mut self) {
        self.gallery_view = !self.gallery_view;
        if self.gallery_view {
            // Start the highlight on the image currently shown.
            // When inside an archive, start at the "go back" cell (index 0).
            self.grid_sel = if self.zip_archive.is_some() {
                0
            } else {
                self.gallery.index.min(self.gallery.len().saturating_sub(1))
            };
            // Always re-focus on open, even if the selection index happens to
            // be the same: the offset is per-folder state. The viewport is
            // unmeasured until the first frame lays out, so the follow
            // decision pins the row to the top that once, then follows.
            self.grid_sel_scrolled = usize::MAX;
            self.grid_view_h = 0.0;
            self.lat.t_gallery = Some(Instant::now());
        }
    }

    /// Open the grid's highlighted image and leave the grid (Enter or click).
    ///
    /// **`goto_index`, not an assignment to `gallery.index`, and that is the whole
    /// of what manga mode changed here.** The grid is one cell per file and the cell
    /// names a *file*, so picking one has to mean "that file, at its first page" -
    /// and `goto_index` is what says so, by resetting the page as well as the index.
    /// Assigning `.index` leaves the page wherever the cursor happened to be, so
    /// picking file 3 while sitting on page 2 of file 7 landed on page 2 of file 3,
    /// which is a page of the picked file but not its *first* one. In manga mode that
    /// is worse than a cosmetic slip: `goto`'s `snap_to_turn_anchor` then walks the
    /// cursor to a turn anchor, and the reader arrives somewhere they did not pick.
    ///
    /// The grid is otherwise untouched, deliberately: one cell per file, page 1 only,
    /// as it has always been. When inside an archive, index 0 is the "go back" cell
    /// and is handled separately.
    /// Open the save-as panel, pre-filled with what `Ctrl+S` would write.
    ///
    /// **The defaults are the current behaviour, deliberately.** `Ctrl+S` picks a
    /// `_rotated` / `_cropped` name that does not exist and writes the full-res
    /// picture; this pre-fills exactly that name, sets quality to the number the
    /// screen-save already uses, and leaves the resize blank. A user who opens
    /// the panel and presses Save without touching anything gets a file
    /// identical to the one `Ctrl+S` would have written, which is the only way
    /// "save-as" can be an addition rather than a change.
    pub(crate) fn open_save_as(&mut self) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let p = cur.path.clone();
        let infix = self.save_as_infix(&p);
        let dest = image_viewer::unique_dest_named(&p, infix);
        self.save_as.dest = dest.to_string_lossy().into_owned();
        self.save_as.quality = image_viewer::DEFAULT_JPEG_QUALITY;
        self.save_as.max_long = String::new();
        self.save_as.open = true;
    }

    /// The infix the panel's default name carries — [`image_viewer::save_infix`]
    /// fed the *composed* transform, which is the whole point.
    ///
    /// **The EXIF tag is folded in here, and folding it in is the difference
    /// between matching `Ctrl+S` and not.** `save_transformed` names its file
    /// from `display_xform(exif_tag, user)`, so a phone photo stored sideways is
    /// `_rotated` on its own and an untouched landscape JPEG is `_copy`. Asking
    /// this question of the raw `edit.xform` instead answers "did the user press
    /// a turn key", which is a different question from "did the picture move" —
    /// and it is the one that named every unedited picture `_rotated`.
    ///
    /// It is a separate function rather than a line inside `open_save_as` only
    /// because [`Self::do_save_as`]'s confirmation has to agree with the name
    /// the panel was pre-filled with; two sites asking one question is how they
    /// drift.
    pub(crate) fn save_as_infix(&self, path: &Path) -> &'static str {
        let edit = self.edits.get(path);
        let exif = image_viewer::exif_orientation(path);
        let xform = image_viewer::display_xform(exif, edit.xform);
        image_viewer::save_infix(xform, edit.crop)
    }

    /// Perform the save the panel describes.
    ///
    /// **Reads `cur`, not a captured source.** See [`SaveAsState`]: a panel
    /// that remembered which picture it was opened on would save that picture
    /// after the reader had navigated elsewhere, and name it as though it were
    /// the one on screen.
    ///
    /// The animation and document-page refusals are the same ones `Ctrl+S`
    /// makes, duplicated rather than shared — four lines each, against a shared
    /// function that would have to take the edit and both guards as arguments.
    /// The risk of them drifting is why each refusal names itself, so a test can
    /// read the message and tell which guard fired.
    pub(crate) fn do_save_as(&mut self) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let p = cur.path.clone();
        if self.anim.as_ref().is_some_and(|a| a.path == p) {
            self.error = Some("Cannot save a transformed animation: it has many frames".into());
            return;
        }
        if cur.page > 0 {
            self.error = Some(format!(
                "Cannot save a single page of a multi-page document ({}/{}). \
                 Saving works on the whole file.",
                cur.page + 1,
                self.gallery.pages_in_current()
            ));
            return;
        }
        let dest = PathBuf::from(self.save_as.dest.trim());
        if dest.as_os_str().is_empty() {
            self.error = Some("No destination: type or pick a file name".into());
            return;
        }
        // Blank means "leave it alone", and 0 is how `save_transformed_to` is
        // told that. A non-numeric box is an error rather than a silent 0:
        // "I typed a size and it saved full-size" is a worse answer than "that
        // is not a number".
        let max_long = match self.save_as.max_long.trim() {
            "" => 0,
            s => match s.parse::<u32>() {
                Ok(n) => n,
                Err(_) => {
                    self.error = Some(format!("Max long side must be a number, got {s:?}"));
                    return;
                }
            },
        };
        let quality = self.save_as.quality;
        let edit = self.edits.get(&p);
        match image_viewer::save_transformed_to(&p, edit.xform, edit.crop, &dest, quality, max_long) {
            Ok(out) => {
                let name = dest
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                let sized = if max_long == 0 {
                    String::new()
                } else {
                    format!(" (long side <= {max_long})")
                };
                self.notice = Some(if out.lossless {
                    format!("Saved {name} ({} , lossless){sized}", edit.label())
                } else {
                    format!("Saved {name} ({} , re-encoded at quality {quality}){sized}", edit.label())
                });
                self.save_as.open = false;
            }
            Err(e) => self.error = Some(format!("Save failed: {e}")),
        }
    }

    /// Draw the save-as panel. Returns whether it is still open, because the
    /// caller draws it as an overlay and must know whether to keep doing so.
    pub(crate) fn save_as_panel(&mut self, ui: &mut egui::Ui) -> bool {
        let mut close = false;
        let mut save = false;
        egui::Window::new("Save as")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ui.ctx(), |ui| {
                ui.add_space(4.0);
                ui.label("File:");
                ui.text_edit_singleline(&mut self.save_as.dest);
                ui.add_space(4.0);
                ui.label("Format:");
                // **Replaces the extension, never the whole path.** The obvious
                // `selectable_value(&mut dest, dest.clone(), "png")` assigns the
                // entire path to the string `"png"`, so picking a format throws
                // away the folder and the name the user typed. The destination
                // box above stays editable, so a hand-typed extension is still
                // respected — the dropdown is a shortcut, not the authority.
                let current_ext = self.save_as.dest.rsplit('.').next().unwrap_or("").to_lowercase();
                egui::ComboBox::from_id_salt("save_as_format")
                    .selected_text(current_ext.clone())
                    .show_ui(ui, |ui| {
                        for f in self.save_as.formats {
                            if ui
                                .selectable_label(current_ext == *f, format!(".{f}"))
                                .clicked()
                            {
                                let dest = std::mem::take(&mut self.save_as.dest);
                                self.save_as.dest = replace_extension(&dest, f);
                            }
                        }
                    });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label("Quality:");
                    ui.add(egui::Slider::new(&mut self.save_as.quality, 1..=100).text(""));
                });
                // **The slider is told when it does nothing**, rather than left
                // silently inert. `image` 0.25 has one lossy encoder (JPEG) and
                // none for the rest, so a quality slider on a PNG is a control
                // that does not do what it says.
                let dest_ext = self.save_as.dest.rsplit('.').next().unwrap_or("").to_lowercase();
                if !matches!(dest_ext.as_str(), "jpg" | "jpeg") {
                    ui.label("Quality applies to JPEG only; this format is lossless.");
                }
                ui.horizontal(|ui| {
                    ui.label("Max long side:");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.save_as.max_long)
                            .desired_width(70.0)
                            .hint_text("original"),
                    );
                });
                ui.label("Blank = keep the original size.");
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                });
            });
        if save {
            self.do_save_as();
        }
        if close {
            self.save_as.open = false;
        }
        self.save_as.open
    }

    pub(crate) fn pick_grid_selection(&mut self, ctx: &Context) {
        if self.gallery.is_empty() {
            return;
        }
        if self.zip_archive.is_some() && self.grid_sel == 0 {
            // "Go back" cell pressed
            self.leave_archive(ctx, false);
        } else {
            let file_idx = self.grid_sel.saturating_sub(if self.zip_archive.is_some() { 1 } else { 0 });
            self.gallery.goto_index(file_idx.min(self.gallery.len().saturating_sub(1)));
            self.gallery_view = false;
            self.goto(ctx);
            // A click or Enter on an archive's cell is the request to open it — the
            // grid has no other meaning for that cell, since there is nothing to
            // display. Same rule as the arrow, for the same reason: opening is asked
            // for here, not in the funnel every arrival passes through.
            self.enter_archive_if_present(ctx);
        }
    }

}
