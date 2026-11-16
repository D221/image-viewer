//! `exif` panel, extracted from `MyApp::ui` when `src/app/ui.rs` passed the
//! 1000-line target.

use crate::*;

impl MyApp {
    pub(crate) fn ui_exif(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        _first: bool,
        _toggle: bool,
    ) {
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
                if let Some(preview) = self.shown.as_ref().and_then(|k| self.previews.get(k))
                    && let Some(line) = preview.colour.panel_line()
                {
                    ui.label(line);
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
                            if ui.link("Open in Google Maps").clicked()
                                && let Err(e) = open::that_detached(url)
                            {
                                self.error = Some(format!("Open link failed: {e}"));
                            }
                        } else {
                            ui.label(line);
                        }
                    }
                } else {
                    ui.label("No EXIF available.");
                }
                if let Some(p) = self.cur.as_ref()
                    && let Ok(md) = std::fs::metadata(&p.path)
                {
                    ui.separator();
                    ui.label(format!("Size: {:.1} MB", md.len() as f64 / (1024.0 * 1024.0)));
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
    }
}
