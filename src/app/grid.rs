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
        // One extra cell when inside an archive (the "go back" cell at index 0),
        // or when we have a parent folder to go "up" to (the up-arrow cell).
        // Both are mutually exclusive: an archive has no subfolders to navigate.
        let in_archive = self.zip_archive.is_some();
        let in_folder_nav = !in_archive && !self.folder_stack.is_empty();
        let extra = if in_archive || in_folder_nav { 1 } else { 0 };
        let total = self.gallery.files.len() + extra;
        let total_rows = total.div_ceil(cols);
        let clock = self.clock;
        let sel = self.grid_sel;
        // (index, path) — path is None for the back/up cells, Some for file/folder cells.
        let mut clicked: Option<(usize, PathBuf)> = None;

        // Split the borrows so cells can be drawn without cloning the file
        // list (a big folder is thousands of PathBufs per frame otherwise).
        // `folder_stack` is NOT destructured here — it is accessed via `self`
        // inside the closure and in the click handler below, which need
        // mutable access to `self` after the closure returns.
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
                        let is_up_cell = in_folder_nav && idx == 0;
                        if is_back_cell {
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
                            if resp.clicked() && clicked.is_none() {
                                clicked = Some((idx, PathBuf::new()));
                            }
                            resp
                        } else if is_up_cell {
                            // "Up" cell: go back to the parent folder.
                            let parent = self.folder_stack.last().unwrap_or(&self.folder);
                            let parent_name = parent
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| parent.to_string_lossy().into_owned());
                            let name = format!("^ {}", parent_name);
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
                            if resp.clicked() && clicked.is_none() {
                                clicked = Some((idx, PathBuf::new()));
                            }
                            resp
                        } else {
                            // Adjust index for the extra cell offset (go back or up arrow).
                            let file_idx = idx - if extra > 0 { 1 } else { 0 };
                            let Some(path) = gallery.files.get(file_idx) else {
                                continue;
                            };
                            let is_folder = path.is_dir();
                            // Fill the ratings map in the same pass that
                            // queues thumbnails: one worker read per newly
                            // visible cell, never a sidecar stat per cell
                            // per frame. The pending mark is what makes it
                            // once per cell — without it a cell whose
                            // answer has not landed yet would spawn a
                            // thread every frame. Archives get no ratings,
                            // and folders have no ratings either.
                            if !is_folder
                                && !is_archive_path(path)
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
                            let resp = if is_folder {
                                // Folder cell: no thumbnail, just a folder icon
                                // and name. Clicking descends into the folder.
                                grid_folder_cell(ui, path, cell, idx == sel)
                            } else {
                                grid_cell(
                                    ui,
                                    path,
                                    cell,
                                    idx == sel,
                                    clock,
                                    thumbs,
                                    thumb_tries,
                                    loader,
                                    grid_ratings,
                                )
                            };
                            if resp.clicked() && clicked.is_none() {
                                clicked = Some((idx, path.clone()));
                            }
                            resp
                        };
                    }
                });
            }
        });
        // Measure every frame, decide only on selection change: the stored
        // offset and viewport are what the follow decision reads next frame.
        self.grid_scroll = grid_out.state.offset.y;
        self.grid_view_h = grid_out.inner_rect.height();

        if let Some((i, path)) = clicked {
            if zip_archive.is_some() && i == 0 {
                // Clicked the "go back" cell
                self.leave_archive(ctx, false);
            } else if in_folder_nav && i == 0 {
                // Clicked the "up" cell: go back to the parent folder.
                if let Some(parent) = self.folder_stack.pop() {
                    self.open_folder(ctx, parent);
                }
            } else if path.is_dir() {
                // Folder cell: descend into it. Remember where we came
                // from so the up arrow can take us back.
                self.folder_stack.push(self.folder.clone());
                self.open_folder(ctx, path);
            } else {
                let file_idx = i.saturating_sub(if extra > 0 { 1 } else { 0 });
                self.grid_sel = file_idx;
                self.pick_grid_selection(ctx);
            }
        }
    }
}
