//! Module `app_grid`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// Gallery grid view (G): scrollable thumbnails of every image in the
    /// queue. Thumbs decode lazily for visible cells.
    ///
    /// Browsing here does *not* change the shown image: arrows/Home/End only
    /// move `grid_sel`, and Enter (or a click) opens it. Arrowing around a
    /// 2000-image folder therefore costs no re-decodes.
    pub(crate) fn gallery_grid(&mut self, ui: &mut egui::Ui, ctx: &Context, panel: egui::Rect) {
        // One cell size in the app: the config value, clamped so a hand-edited
        // config cannot park the grid at zero-width cells. Everything below —
        // columns, row pitch, the go-back cell and each image cell — derives
        // from it.
        let cell = self.config.grid_cell.clamp(64.0, 400.0);
        let cols = grid_cols(panel.width(), cell);
        self.grid_cols = cols; // Up/Down in update() step by this
        let row_h = grid_row_h(cell);
        // How many rows actually fit, so PageUp/PageDown move a page rather than
        // a fixed guess. `show_rows` adds vertical item spacing on top of
        // row_h, so the pitch is not row_h.
        let pitch = row_h as f32 + ui.spacing().item_spacing.y;
        self.grid_page_rows = ((panel.height() / pitch).floor() as usize).max(1);
        let extra = if self.zip_archive.is_some() { 1 } else { 0 };
        let total = self.gallery.files.len() + extra;
        let total_rows = (total + cols - 1) / cols;
        let clock = self.clock;
        let sel = self.grid_sel;
        let mut clicked: Option<usize> = None;

        // Split the borrows so cells can be drawn without cloning the file
        // list (a big folder is thousands of PathBufs per frame otherwise).
        let Self {
            gallery,
            thumbs,
            thumb_tries,
            loader,
            zip_archive,
            grid_ratings,
            grid_rating_pending,
            drop_tx,
            drop_wake,
            ..
        } = self;
        // When inside an archive, index 0 is the "go back" cell.
        let in_archive = zip_archive.is_some();
        // auto_shrink([false, true]) is what makes the scrollbar appear: with
        // [false, false] the ScrollArea demands the full width and there is no
        // room left for the bar, so a long folder had no visible way to show
        // where you were.
        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, true]);
        // Follow the selection the way file pickers do: leave the view alone
        // while the selected row is visible, scroll the minimum distance when
        // it is not — never pin the row to the top on every press. The offset
        // and viewport below are last frame's measurements, so wheel and
        // scrollbar drags stay in sync for free; unmeasured (just opened)
        // they are 0 and the selection pins to the top instead, which is
        // what opening on a deep selection has always done.
        //
        // Written only on a selection change: re-issuing it every frame (or
        // forcing the offset every frame) fights the user, and any small
        // manual scroll would snap back.
        if sel != self.grid_sel_scrolled {
            let pitch = row_h + ui.spacing().item_spacing.y;
            if let Some(offset) = image_viewer::grid_follow_offset(
                sel / cols.max(1),
                pitch,
                self.grid_view_h,
                self.grid_scroll,
            ) {
                sa = sa.vertical_scroll_offset(offset);
            }
            self.grid_sel_scrolled = sel;
        }
        let grid_out = sa.show_rows(ui, row_h, total_rows, |ui, rows| {
            for row in rows {
                ui.horizontal(|ui| {
                    for col in 0..cols {
                        let idx = row * cols + col;
                        let is_back_cell = in_archive && idx == 0;
                        let resp = if is_back_cell {
                            // "Go back" cell: draw an arrow up and the folder name
                            let folder_name = self
                                .folder
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| self.folder.to_string_lossy().into_owned());
                            let name = format!("^ {}", folder_name);
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(cell, cell + GRID_LABEL_H),
                                egui::Sense::click(),
                            );
                            let fill = if idx == sel {
                                egui::Color32::from_rgb(28, 58, 118)
                            } else {
                                egui::Color32::from_rgb(38, 38, 38)
                            };
                            let stroke = if idx == sel {
                                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(120, 170, 255))
                            } else {
                                egui::Stroke::NONE
                            };
                            if ui.is_rect_visible(rect) {
                                ui.painter().rect(
                                    rect,
                                    0.0,
                                    fill,
                                    stroke,
                                    egui::StrokeKind::Inside,
                                );
                            }
                            let inner = rect.shrink(4.0);
                            let mut child = ui.new_child(
                                egui::UiBuilder::new()
                                    .max_rect(inner)
                                    .layout(egui::Layout::top_down(egui::Align::Center)),
                            );
                            let img_area = egui::Rect::from_min_size(
                                egui::Pos2::new(inner.min.x, inner.min.y),
                                egui::vec2(inner.width(), cell),
                            );
                            child.allocate_exact_size(img_area.size(), egui::Sense::hover());
                            child.painter().text(
                                img_area.center(),
                                egui::Align2::CENTER_CENTER,
                                "^",
                                egui::FontId::proportional(24.0),
                                ui.visuals().text_color(),
                            );
                            child.add(
                                egui::Label::new(
                                    egui::RichText::new(&name)
                                        .small()
                                        .color(egui::Color32::from_gray(200)),
                                )
                                .truncate(),
                            );
                            resp
                        } else {
                            // Adjust index for the "go back" cell offset
                            let file_idx = idx - if in_archive { 1 } else { 0 };
                            let Some(path) = gallery.files.get(file_idx) else {
                                continue;
                            };
                            // Fill the ratings map in the same pass that
                            // queues thumbnails: one worker read per newly
                            // visible cell, never a sidecar stat per cell
                            // per frame. The pending mark is what makes it
                            // once per cell — without it a cell whose
                            // answer has not landed yet would spawn a
                            // thread every frame. Archives get no ratings.
                            if !is_archive_path(path)
                                && !grid_ratings.contains_key(path)
                                && !grid_rating_pending.contains(path)
                            {
                                grid_rating_pending.insert(path.clone());
                                let owned = path.clone();
                                let tx = drop_tx.clone();
                                let wake = drop_wake.clone();
                                std::thread::spawn(move || {
                                    let result = image_viewer::Ratings::read(&owned);
                                    let _ = tx.send(Msg::GridRating { path: owned, result });
                                    wake.request_repaint();
                                });
                            }
                            let resp = grid_cell(
                                ui,
                                path,
                                cell,
                                idx == sel,
                                clock,
                                thumbs,
                                thumb_tries,
                                loader,
                                grid_ratings,
                            );
                            resp
                        };
                        if resp.clicked() && clicked.is_none() {
                            clicked = Some(idx);
                        }
                    }
                });
            }
        });
        // Measure every frame, decide only on selection change: the stored
        // offset and viewport are what the follow decision reads next frame.
        self.grid_scroll = grid_out.state.offset.y;
        self.grid_view_h = grid_out.inner_rect.height();

        if let Some(i) = clicked {
            if zip_archive.is_some() && i == 0 {
                // Clicked the "go back" cell
                self.leave_archive(ctx, false);
            } else {
                // Adjust index for the "go back" cell offset
                let file_idx = i.saturating_sub(if zip_archive.is_some() { 1 } else { 0 });
                self.grid_sel = file_idx;
                self.pick_grid_selection(ctx);
            }
        }
    }
}
