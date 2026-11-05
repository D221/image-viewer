//! Module `app_central`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.
use crate::*;

mod crop;
mod tiles;
mod zoom_input;

impl MyApp {
    /// Panel geometry for the current frame: the available rect, pixels per
    /// point, and the display area in physical pixels. Also settles any
    /// pending window position (fit-window centering) and returns the
    /// computed `(panel, ppp, panel_px)`.
    ///
    /// `pending_pos` is a `(width, height, tries)` triple left by the
    /// fit-window code. Each frame it checks whether the viewport has reached
    /// the requested size; once settled (or out of tries) it centres the
    /// window on the monitor and clears the triple.
    fn panel_geometry(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
    ) -> (egui::Rect, f32, (u32, u32)) {
        let panel = ui.available_rect_before_wrap();
        let ppp = ctx.pixels_per_point();
        // Track the display area in physical pixels; previews are decoded to
        // match it. If the window grew, below-target textures are dropped so
        // schedule() re-decodes them at the new, larger target.
        let panel_px =
            ((panel.width() * ppp).round() as u32, (panel.height() * ppp).round() as u32);
        // Second phase of fit-window centering (see the consumption below):
        // place once the inner size matches what was asked, retry a few
        // frames while the resize is in flight, then place with whatever is
        // known rather than never. Always the monitor center — old-window
        // centering drifted in practice. Without monitor info there is
        // nothing to center on, so that case clears silently.
        if let Some((want, tries)) = self.pending_pos {
            let settled = ui
                .input(|i| i.viewport().inner_rect.map(|r| r.size()))
                .is_some_and(|s| (s.x - want.0).abs() < 3.0 && (s.y - want.1).abs() < 3.0);
            if settled || tries == 0 {
                self.pending_pos = None;
                if let Some(m) = ui.input(|i| i.viewport().monitor_size) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::Pos2::new(
                        ((m.x - want.0) / 2.0).max(0.0),
                        ((m.y - want.1) / 2.0).max(0.0),
                    )));
                }
            } else {
                self.pending_pos = Some((want, tries - 1));
            }
        }
        (panel, ppp, panel_px)
    }

    /// First measurement and resize action for the frame.
    ///
    /// The first measurement (when `display_dim` is `None`) is not a resize:
    /// it is the frame the plan has been waiting for, so `schedule()` is called
    /// once to decode the current page at the panel's real size. Later changes
    /// go through `ResizeTracker`, which coalesces window-drag frames into a
    /// single `Refine` decision once the size stops moving.
    fn handle_resize(&mut self, panel_px: (u32, u32), target: u32, ctx: &egui::Context) {
        if self.display_dim != Some(panel_px) {
            // **The first measurement is not a resize, and it is the frame the plan
            // has been waiting for.** `schedule` answers no target at all until the
            // display area exists (see its doc), so the plan built in
            // `frame_prelude` was empty and *nothing* is queued. Re-planning here is
            // what makes the current page decode at the panel's real size instead of
            // `PREVIEW_TEXTURE_DIM`.
            //
            // **Inside this block, and that is the property rather than its
            // presence.** `display_dim` is what `schedule` reads, so a call one line
            // above the assignment would queue the plan a second time against the
            // same empty `display_dim` and change nothing at all — which is the
            // defect shape this feature has already produced once
            // (`the_turns_second_half_is_derived_not_latched`). The scan
            // `the_plan_is_empty_until_the_display_area_is_measured` brace-matches
            // this block and checks the call is inside it.
            //
            // Deliberately *only* the first measurement. Every later change of the
            // display area is a resize, and re-planning on each of those would issue
            // a decode per frame of a window drag — the exact storm `ResizeTracker`
            // exists to coalesce, and the reason `Wait` is not `Refine` below.
            let first_measurement = self.display_dim.is_none();
            self.display_dim = Some(panel_px);
            // Hiding/showing panels resizes the display area but must not
            // reload the image: consume exactly one transition silently.
            if self.suppress_resize_redecode {
                self.suppress_resize_redecode = false;
                self.resize.suppress();
            }
            if first_measurement {
                self.schedule();
            }
        }
        match self.resize.frame(target, self.current_needs_refine(target)) {
            image_viewer::ResizeAction::Refine => {
                perf::count(perf::C_RESIZE_REDECODES, 1);
                self.schedule();
            }
            image_viewer::ResizeAction::Wait => {
                // Still moving. Ask for one more frame so the decision gets made
                // once it stops; without this the final size of a drag would
                // never be examined, because nothing else asks for a repaint.
                ctx.request_repaint_after(RESIZE_SETTLE_REPAINT);
            }
            image_viewer::ResizeAction::Idle => {}
        }
    }

    /// Fit-window calculations and convergence: reset (`T`, arrivals in the
    /// mode) sets 100%-if-fits and sizes the window; wrap keeps the zoom and
    /// only re-hugs. Converges on the symptom itself — while the drawn picture
    /// and the panel disagree, re-measure and re-send. `wrap` doubles as the
    /// settle flag: cleared only on convergence, so the loop actually repeats
    /// instead of running once and stranding `fit_iters`.
    ///
    /// `shown_is_cur` is `true` when the shown page is the current page, which
    /// gates arrival fits so they wait for adoption rather than fitting a
    /// picture the reader has already left.
    ///
    /// Ten parameters is more than clippy likes, but they are all naturally
    /// part of the fit-window computation: UI/context access, panel geometry,
    /// density, content size, fit scale, and convergence state. Grouping them
    /// into a struct would add an unnecessary abstraction just to satisfy a
    /// lint count.
    #[allow(clippy::too_many_arguments)]
    fn handle_fit_window(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        panel: egui::Rect,
        ppp: f32,
        display_px: egui::Vec2,
        spread_dims: &[(f32, f32)],
        fit_scale: f32,
        shown: &ImgKey,
        cur: Option<ImgKey>,
    ) {
        if !(self.fit_window_pending || self.fit_arrive_pending || self.fit_wrap_pending)
            || self.fit_iters == 0
            || (self.fit_arrive_pending && !self.fit_window_pending && cur.as_ref() != Some(shown))
        {
            return;
        }
        self.fit_iters -= 1;
        // Arrival carries the reset too (it just waits longer to run):
        // both arms mean "this picture starts at 100%", only the timing
        // differs. Forgetting the arrival half is how zoomed-out views
        // survived navigation.
        let reset = self.fit_window_pending || self.fit_arrive_pending;
        self.fit_arrive_pending = false;
        if ui.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
            return;
        }
        // Twin of `clamp_px` below: the content's source footprint,
        // so the two cannot disagree about how big the picture is.
        let natural = if spread_dims.is_empty() {
            display_px
        } else {
            let mut w = 0.0;
            let mut h: f32 = 0.0;
            for &(pw, ph) in spread_dims {
                w += pw;
                h = h.max(ph);
            }
            Vec2::new(w, h)
        } / ppp;
        let Some(inner) = ui.input(|i| i.viewport().inner_rect.map(|r| r.size())) else {
            return;
        };
        // No monitor info: fall back to the window itself, which
        // only affects the clamp — the zoom above never shrinks.
        let work = ui
            .input(|i| i.viewport().monitor_size)
            .map(|m| (m.x, m.y))
            .unwrap_or((inner.x, inner.y));
        let mut ratio = if self.view.fit { fit_scale * ppp } else { self.view.scale * ppp };
        if reset {
            self.fit_window_pending = false;
            // T mode: fit the window to the image at 1:1, but if the
            // image is larger than the monitor, scale it down to fit
            // (maintaining aspect ratio). This matches JPEGView
            // behaviour — 1:1 only when it fits, otherwise shrink to
            // fit the monitor.
            self.view.fit = false;
            self.view.offset = Vec2::ZERO;
            // Available space for the image on the monitor (monitor
            // minus window chrome). Chrome = inner - panel (panels,
            // title bar, etc.).
            let chrome_x = (inner.x - panel.width()).max(0.0);
            let chrome_y = (inner.y - panel.height()).max(0.0);
            let available_x = work.0 - chrome_x;
            let available_y = work.1 - chrome_y;
            // Max scale that fits the image in the available space,
            // capped at 1.0 (never upscale).
            let max_scale =
                (available_x / natural.x).min(available_y / natural.y).min(1.0).max(0.01);
            self.view.scale = max_scale / ppp;
            self.view.offset = Vec2::ZERO;
            ratio = max_scale;
        }
        // `wrap` clears only here, on convergence: it doubles as the
        // settle flag, so clearing it up front would run the loop
        // exactly once no matter what `fit_iters` allows.
        let drawn = (natural.x * ratio, natural.y * ratio);
        let (nw, nh) = image_viewer::fit_window_size(
            drawn,
            (inner.x, inner.y),
            (panel.width(), panel.height()),
            Some(work),
        );
        self.fit_debug =
            Some((ratio, drawn.0, drawn.1, panel.width(), panel.height(), inner.x, inner.y));
        // Converge on the symptom (drawn vs panel), not the proxy
        // (window vs target): a window can match its target while
        // the picture still disagrees with the panel, and that
        // disagreement is the bars.
        if (drawn.0 - panel.width()).abs() > 2.0 || (drawn.1 - panel.height()).abs() > 2.0 {
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(Vec2::new(nw, nh)));
            // Position still goes through `pending_pos` (below):
            // it must land after this resize settles.
            self.pending_pos = Some(((nw, nh), 10));
            self.fit_wrap_pending = true;
        } else {
            self.fit_wrap_pending = false;
            self.fit_iters = 0;
        }
    }

    /// Preview lookup and animation frame advance for the cursor page.
    ///
    /// Returns `((orig_w, orig_h), tex_id, tex_w, has_alpha)` or `None` when the
    /// wanted image has no texture yet (this frame shows nothing). On `None`,
    /// the caller counts a display-gap frame and returns.
    ///
    /// When an animation is playing on the shown path, its texture is advanced
    /// to the current frame and a repaint is scheduled at the frame boundary.
    /// When paused, the held frame is held.
    fn preview_and_animation(
        &mut self,
        ctx: &egui::Context,
        shown: &ImgKey,
    ) -> Option<((u32, u32), egui::TextureId, f32, bool)> {
        let Some(pv) = self.previews.get_mut(shown) else {
            return None;
        };
        // Stamped here, and this is where the **cursor** page's `used` clock
        // moves — the partner's is stamped where it is painted below, for the
        // same reason. `evict_previews` is an LRU on `used`, so a page on
        // screen whose clock never advances is a page the cache is free to
        // drop, and a spread whose second half evaporates and comes back
        // re-decoded is the symptom.
        pv.used = self.clock;
        let mut tex_id = pv.tex.id();
        let mut tex_w = pv.tex.size()[0] as f32;
        let pv_has_alpha = pv.has_alpha;
        let orig = pv.orig;
        if let Some(av) = self.anim.as_mut().filter(|a| a.path == shown.path) {
            // Paused: hold the chosen frame and schedule nothing. Playing:
            // advance to the current frame and repaint at its boundary.
            if let Some(held) = av.paused
                && held != av.last_index
                && let Some(f) = av.anim.frames.get(held)
            {
                av.tex.set(
                    ColorImage::from_rgba_unmultiplied(
                        [f.width as usize, f.height as usize],
                        &f.rgba,
                    ),
                    content_texture_options(self.config.pixel_art),
                );
                av.last_index = held;
            } else {
                let (idx, next_in) = image_viewer::anim_frame_state(
                    &av.anim,
                    av.start.elapsed(),
                    self.config.anim_speed,
                );
                if idx != av.last_index {
                    let f = &av.anim.frames[idx];
                    av.tex.set(
                        ColorImage::from_rgba_unmultiplied(
                            [f.width as usize, f.height as usize],
                            &f.rgba,
                        ),
                        TextureOptions::LINEAR,
                    );
                    av.last_index = idx;
                }
                ctx.request_repaint_after(next_in);
            }
            tex_id = av.tex.id();
            tex_w = av.anim.frames[0].width as f32;
        }
        Some((orig, tex_id, tex_w, pv_has_alpha))
    }

    /// Image transform and display dimensions: applies EXIF orientation and
    /// computes the transformed footprint, which is what the image occupies
    /// on screen (four of the eight transforms swap the axes).
    fn compute_transform_and_display(&self, orig: (u32, u32)) -> (Vec2, Xform, Vec2) {
        let img_px = Vec2::new(orig.0 as f32, orig.1 as f32);
        let xform = self.shown_xform();
        let (dw, dh) = xformed_footprint(img_px.x, img_px.y, xform);
        let display_px = Vec2::new(dw, dh);
        (img_px, xform, display_px)
    }

    /// Spread layout: manga turn pages and their transformed footprints.
    ///
    /// `spread_dims` uses transformed footprints, not stored dimensions — a
    /// slot laid out from stored dims is the wrong shape for a turned page;
    // transformed footprints make the rotation fit inside the slot.
    ///
    /// The return type is a pair of vectors; factoring it into a struct would
    /// add an unnecessary abstraction just to satisfy a lint count.
    #[allow(clippy::type_complexity)]
    fn compute_spread_layout(
        &self,
        shown: &ImgKey,
        orig: (u32, u32),
    ) -> (Vec<(ImgKey, (u32, u32), Xform)>, Vec<(f32, f32)>) {
        // The turn's pages, in manga mode: the cursor page and, when the turn
        // holds two, the other one. Empty with the mode off, so every branch
        // below falls through to the single-image path unchanged.
        let pages: Vec<(ImgKey, (u32, u32), Xform)> =
            if self.config.manga_mode { self.turn_pages(shown, orig) } else { Vec::new() };
        let spread_dims: Vec<(f32, f32)> = pages
            .iter()
            .map(|(_, (w, h), xf)| xformed_footprint(*w as f32, *h as f32, *xf))
            .collect();
        (pages, spread_dims)
    }

    pub(crate) fn central(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (panel, ppp, panel_px) = self.panel_geometry(ui, &ctx);

        // **The half panel in manga mode**, and both halves of the change hang off the
        // same number. `preview_target` takes the *long* side, so halving the width is
        // what makes `display_dim`, `schedule`'s `target_dim` and the resize refine ask
        // for half-width textures — and a page in a **two-page** turn can never need
        // more than half the panel's width, so decoding for the whole panel would make
        // every turn cost twice what it needs to. It is the width that is halved and
        // not the height because the seam runs vertically: the panel's full height is
        // what one page is fitted into. `decode_panel_px` is that rule, moved into the
        // library so it can be tested, and with one exception:
        //
        // **A lone spread is laid out as the single-image layout, so it is decoded as
        // one too** — that is the whole of the exception, and it takes the *same*
        // flag the four layout calls below are handed. Halving unconditionally here
        // while `spread_fit` fitted that page against the panel's whole width was the
        // reported defect: a picture drawn 1518.75 points wide out of a texture asked
        // for half that. **Measured on a 2700x1920 page on a 1920x1080 panel**, the
        // request went 1080 → 1920 at `ppp` 1 and 2160 → 3840 at `ppp` 2, either way
        // from 0.711 of the drawn long side to 1.264 of it; and because
        // `decode_jpeg_scaled` picks the smallest of 1/8, 1/4, 1/2, 1 that covers the
        // request, the texture at `ppp` 1 went 1350x960 → 2700x1920, so the picture
        // was painted from a texture 0.889 of the width it was drawn at.
        //
        // **Which rung a source lands on is the source's, not the rule's**, and that
        // is why the fix is stated about the request: at `ppp` 2 both requests are past
        // this page's long side, both land on 1/1, and this fixture's texture is the
        // same 2700 wide either way. A larger page leaves 1/2 at `ppp` 2 as well, and a
        // page under 2160 never leaves it at all. The invariant is the one
        // `the_decode_target_covers_the_picture_the_layout_draws` pins: **the request
        // is never smaller than the picture it will be drawn as.**
        //
        // **One decision, asked once, read twice.** `lone_spread_fills_turn` is a
        // query over the position and the pairing, not over the paint: `turn_len` is
        // what `turn_partner` is built from and `shape_of` is the shape the fit is
        // judged by, so it can be answered at the top of the frame — before `pages`
        // and `spread_dims` exist — and is the same answer the layout reaches later.
        // That is why the two cannot disagree, and it is why the count is not
        // `spread_dims.len()` or `shown_partner.is_none()` any more: both are readings
        // of a rule, and a reading is free to be stale where the rule is not.
        //
        // Two honest costs, both bounded and both correct. **Toggling the mode is now a
        // no-op for a lone spread** — on is the full panel and off is the full panel,
        // because the layout is the single-image layout either way, so it costs no
        // refine at all where the half-panel answer used to charge one for a picture
        // that did not change. And **arriving at a spread nobody has measured** is a
        // target change once, when `learn_shapes` reads its header from `goto` and the
        // flag becomes true: `needs_refine` fires once, the decoded preview is
        // replaced once, and the target is then stable because a shape only ever
        // resolves from unknown to measured.
        //
        // `self.config.manga_mode` is read here, on the frame, rather than snapshotted
        // at construction, so toggling it takes effect without a restart.
        let lone_spread_fills_turn = image_viewer::lone_spread_fills_turn(
            self.shown_pos(),
            self.spreads.lead_lone,
            &self.page_ctx(),
        );
        let panel_px =
            image_viewer::decode_panel_px(panel_px, self.config.manga_mode, lone_spread_fills_turn);
        let target = image_viewer::preview_target(panel_px);
        // First measurement and resize action. The first measurement (when
        // `display_dim` is `None`) is not a resize: it is the frame the plan has
        // been waiting for, so `schedule()` is called once to decode the current
        // page at the panel's real size. Later changes go through `ResizeTracker`,
        // which coalesces window-drag frames into a single `Refine` decision once
        // the size stops moving.
        self.handle_resize(panel_px, target, &ctx);
        // The save-as panel, drawn as a window over whatever is behind it and
        // **before the early returns below**, so it also appears over the grid
        // and over the archive placeholder. Placed after those returns it would
        // be invisible in exactly the two states where a reader is most likely
        // to want it: looking at a folder (the grid), and standing on a
        // container.
        if self.save_as.open {
            let still_open = self.save_as_panel(ui);
            self.save_as.open = still_open;
        }
        // Gallery grid view (G): scrollable thumbnail grid, click to open.
        // This must run *before* the panel rect is allocated below — an
        // allocated rect consumes the whole panel, leaving the grid's
        // ScrollArea with no space (it would render offscreen).
        if self.gallery_view {
            self.gallery_grid(ui, &ctx, panel);
            return;
        }
        let resp = ui.allocate_rect(panel, egui::Sense::click_and_drag());
        let painter = ui.painter_at(panel);
        // **Standing on an archive (not inside one) draws a placeholder.**
        // Previously the previous image stayed painted while the status line named
        // a zip, and that mismatch is what made this feel broken. The cursor is
        // on an archive iff `self.cur` is an archive and `self.zip_archive` is
        // None (inside an archive, zip_archive is Some and the gallery *is* the
        // archive's pages).
        let cursor_on_archive = self.cur.as_ref().is_some_and(|k| is_archive_path(&k.path))
            && self.zip_archive.is_none();
        if cursor_on_archive {
            let cur_path = self.cur.as_ref().unwrap().path.clone();
            let name = cur_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cur_path.to_string_lossy().into_owned());
            let n = image_viewer::archive_image_count(&cur_path)
                .map(|c| format!("{c} images"))
                .unwrap_or_else(|| "unreadable container".to_string());
            let msg = format!("{name}\n{n}\npress right to open");
            painter.text(
                panel.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(14.0),
                ui.visuals().text_color(),
            );
            return;
        }
        let Some(shown) = self.shown.clone() else {
            let msg = if let Some(e) = &self.error {
                e.clone()
            } else if self.gallery.is_empty() {
                "No images loaded. Drop a file or folder here.".to_owned()
            } else {
                "Loading…".to_owned()
            };
            painter.text(
                panel.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(16.0),
                ui.visuals().text_color(),
            );
            return;
        };
        let Some((orig, tex_id, tex_w, pv_has_alpha)) = self.preview_and_animation(&ctx, &shown)
        else {
            // The wanted image has no texture yet: this frame shows nothing.
            // Counted as a regression guard - it should stay at 0 except
            // for the very first frames of a cold open.
            perf::count(perf::C_DISPLAY_GAP_FRAMES, 1);
            return;
        };
        let (img_px, xform, display_px) = self.compute_transform_and_display(orig);
        let (pages, spread_dims) = self.compute_spread_layout(&shown, orig);
        // **A turn of one page that is a spread fills the turn, and the page _is_ the
        // whole spread** — so it is fitted against the panel's whole width and centred
        // on the seam, rather than confined to a half with an empty one beside it.
        //
        // **The flag was decided at the top of this frame**, by
        // `image_viewer::lone_spread_fills_turn`, and this is only reading it — the
        // same value `decode_panel_px` was handed, which is what keeps the picture from
        // being drawn at one size out of a texture asked for another. The *why* it is
        // a position query rather than a reading of `spread_dims` is at that call:
        // `turn_len` is what `turn_partner` is built from and `shape_of` is the shape
        // the fit is judged by, so it is answerable before `pages` exists and cannot be
        // a frame out of date with the paint the way a latched count can.
        // One shared scale for the whole turn, and the cursor page's own fit when
        // the mode is off. `spread_fit` clamps to the same `(1e-4, 8.0)` the
        // single-image fit uses, and is used as this frame's `fit_scale` so the
        // zoom clamp below, the 1:1 shortcut and the pan anchor all behave
        // **exactly** as they do for one image — there is one `scale` and one
        // `view`, and a turn has to share them or the two halves would drift
        // apart as you zoom.
        let fit_scale = if pages.is_empty() {
            (panel.width() / display_px.x).min(panel.height() / display_px.y).clamp(1e-4, 8.0)
        } else {
            image_viewer::spread_fit(
                (panel.width(), panel.height()),
                &spread_dims,
                lone_spread_fills_turn,
            )
        };
        // Fit-width: picture width fills the panel, height overflows for vertical pan.
        let fit_width_scale = if pages.is_empty() {
            (panel.width() / display_px.x).clamp(1e-4, 8.0)
        } else {
            let tw: f32 = spread_dims.iter().map(|&(w, _)| w).sum();
            if tw > 0.0 { (panel.width() / tw).clamp(1e-4, 8.0) } else { 1.0 }
        };
        let max_scale = MAX_SCALE.max(fit_scale).max(fit_width_scale);
        self.handle_fit_window(
            ui,
            &ctx,
            panel,
            ppp,
            display_px,
            &spread_dims,
            fit_scale,
            &shown,
            self.cur.clone(),
        );
        let (scale, mut offset) = self.central_zoom_input(
            ui,
            &resp,
            &ctx,
            panel,
            ppp,
            fit_scale,
            fit_width_scale,
            max_scale,
            &pages,
            &spread_dims,
            lone_spread_fills_turn,
            display_px,
            img_px,
        );
        // Keep the image from being lost off-screen; centre it on axes where it fits.
        // Uses the *transformed* footprint, so panning limits and fit are correct
        // whichever way up the image is.
        let size = display_px * scale;
        // **The turn's own footprint in manga mode**, so panning stops at the
        // spread's edge rather than at the panel's. The panel is one page wide in
        // the other direction and the turn is two, so clamping against the panel
        // would let a reader drag a spread into empty space with no way back.
        //
        // Folded from `spread_dims` rather than recomputed, and that is not just
        // tidiness: `spread_dims` is what `spread_rects` sized the actual slots
        // with, so the clamp and the picture cannot disagree — including about a
        // page the user has turned, whose slot is its *transformed* footprint. The
        // previous spelling recomputed the cursor's `xformed_footprint` here and
        // added the partner's **stored** width to it, which was a second statement
        // of the same footprint and wrong for a turned partner.
        //
        // Widths **sum** and heights **max**, which is what `spread_rects` builds:
        // the pages sit on opposite sides of the seam at the same scale, sharing
        // its top edge.
        let clamp_px = if pages.is_empty() {
            display_px
        } else {
            let mut w = 0.0;
            let mut h: f32 = 0.0;
            for &(pw, ph) in &spread_dims {
                w += pw;
                h = h.max(ph);
            }
            Vec2::new(w, h)
        };
        // Keep the image from being lost off-screen, bounded by **the content's own
        // edges** against the panel's - not by a range symmetric about the panel's
        // centre, which is right only when the content is centred there.
        //
        // `spread_pan_bounds` derives the range from the same rectangles
        // `spread_rects` hands the paint path a few lines below, so the clamp and the
        // picture are one geometry. A turn's content is displaced from the panel's
        // centre by half the width difference and, being top-aligned, by half the tallest
        // page - and panning moves content the *other* way, so the range is the symmetric
        // one **translated by minus that displacement**: not narrower, just moved.
        //
        // What that is worth, measured on the running app (a two-page spread of a
        // 2000-wide and a 2800-wide page, zoomed in 40 steps): the old expression
        // truncated **36 of 41** steps that still had room to pan, by up to **67
        // points**; this one truncates **1 of 40**, and that one is the content being
        // 11 points taller than the panel, where any further pan would open a gap.
        // Truncation is not itself the bug - a zoom that wants more pan than the
        // content has must be refused - which is exactly what makes "the bound is wrong"
        // and "the bound is right and binding" look the same from the outside.
        //
        // **Both axes, and the bounds are not symmetric**, so this is a per-axis clamp
        // and not the `Vec2` one it replaced. `f32::clamp` is `max` then `min`, which is
        // what `Vec2::clamp` did per component, so a single image - which takes the
        // library's gated branch and answers the same expression, term for term - is
        // bit for bit unchanged. `f32::clamp` also panics on an inverted range, which
        // `spread_pan_bounds` cannot return (`a_pan_range_is_always_ordered_and_finite`).
        let (off_lo, off_hi) = image_viewer::spread_pan_bounds(
            [panel.min.x, panel.min.y, panel.width(), panel.height()],
            (clamp_px.x, clamp_px.y),
            &spread_dims,
            scale,
            self.config.manga_rtl,
            lone_spread_fills_turn,
        );
        offset.x = offset.x.clamp(off_lo[0], off_hi[0]);
        offset.y = offset.y.clamp(off_lo[1], off_hi[1]);
        // Fit mode is explicit only (F key, fit-window shrink, turns): never
        // re-engaged by proximity, which used to forget the zoom on the next
        // frame and read as a rubberband blocking zoom-out.
        let at_fit = false;
        if at_fit {
            offset = Vec2::ZERO;
        }
        self.view.fit = at_fit;
        self.view.scale = scale;
        self.view.offset = offset;
        // Snap the image origin to the physical pixel grid so 1:1 is razor sharp.
        // `rect` stays in *unrotated* image space — that is what the tiles and
        // the source-pixel mapping below expect — and is rotated about its
        // centre at paint time, which makes it exactly cover `size`.
        let snap = |v: f32| (v * ppp).round() / ppp;
        let centre = panel.center() + offset;
        let src_size = img_px * scale;
        // Where each page of the turn goes: one rect per page, in the order
        // `turn_pages` gave them, so `rects[0]` is the **cursor** page.
        // The panel handed in is **already translated by `self.view.offset`** — set
        // a few lines above, this frame's clamped value — so the seam pans with
        // the image rather than sitting still while the turn slides under it. It
        // is the panel's *size* that is untouched, so `spread_fit`'s half-width
        // rule is about the visible area and not about where the picture happens
        // to be.
        let mut rects: Vec<egui::Rect> = if pages.is_empty() {
            Vec::new()
        } else {
            image_viewer::spread_rects(
                [panel.min.x + offset.x, panel.min.y + offset.y, panel.width(), panel.height()],
                &spread_dims,
                scale,
                self.config.manga_rtl,
                lone_spread_fills_turn,
            )
            .into_iter()
            // `x` and `y` snapped, and only those: 1:1 has to stay pixel-sharp,
            // and snapping the far edge as well would move the size the tiles and
            // the source-pixel mapping below are told to expect.
            .map(|r| Rect::from_min_size(Pos2::new(snap(r[0]), snap(r[1])), Vec2::new(r[2], r[3])))
            .collect()
        };
        // Close the seam. Snapping each page's `x` **independently** leaves a
        // sub-pixel gap at the seam, because the two pages' shared edge is one
        // number rounded twice: measured on the probe fixture at ppp 1.25, the
        // cursor page's `x` snapped to 711.20 while the partner's inner edge
        // landed on 710.81 — **0.39 points of bare panel between two pages of
        // artwork that are supposed to touch**. A gap there is not a rounding
        // detail, it is a hairline down the middle of every spread.
        //
        // **The cursor page owns the seam** and the partner is slid onto it, not
        // the other way round, because the cursor page's `rect` is what
        // `footprint`, the tiles and the crop frame are all derived from: letting
        // the partner move it would shift the crop frame and re-cut the tiles by
        // up to one physical pixel the first time a partner's texture arrived.
        // The partner absorbs at most the sub-pixel the snap moved, which is
        // invisible on a page edge and invisible in the page's own aspect.
        //
        // A lone turn has no second page, so there is nothing to close and the
        // condition cannot fire — which is why it is an `if let` on two rects and
        // not a guard on a value.
        if let [cursor, partner] = rects.as_mut_slice() {
            // A rigid slide, by the same amount on **both** edges — not one edge
            // moved onto the seam. `Rect::width()` is `max.x - min.x`, so setting
            // one edge and leaving the other *resizes* the rect, and a resized slot
            // is a slot the painted picture no longer fits: measured on the probe
            // fixture, moving only the partner's outer edge widened its slot from
            // 571.31 to 571.42 points while the picture stayed 571.31.
            //
            // The *outer* edge is what absorbs the sub-pixel, which is the whole
            // point of sliding rather than resizing: the page's own dimensions are
            // untouched, so the two halves keep one shared scale exactly.
            let d = if self.config.manga_rtl {
                // Page 0 hangs to the *right* of the seam, so the partner's `max.x`
                // is the edge that has to land on the cursor's `min.x`.
                cursor.min.x - partner.max.x
            } else {
                // Page 0 hangs to the *left*: the partner's `min.x` is the edge.
                cursor.right_bottom().x - partner.min.x
            };
            partner.min.x += d;
            partner.max.x += d;
        }
        // **Two rectangles per page, and conflating them is the whole trap.** The
        // **slot** is where the page goes on screen — its *transformed* footprint,
        // which is what `spread_rects` placed and what has to fit the panel. The
        // **paint rect** is what goes into `paint_tex`, and it stays in *unrotated
        // image space*: the page's stored dimensions at `scale`, centred on the
        // slot. `paint_tex` then rotates it about that shared centre into exactly
        // the slot. This is the single-image path's own construction — `rect` is
        // unrotated image space and `footprint` is the rect after rotation — and
        // it is easy to get wrong in a way that looks fine, because handing
        // `paint_tex` the *slot* paints the transform **twice**: the slot is
        // already the transformed shape, so a `Rot90` page's slot comes out
        // swapped back and spilling `|w - h| * scale / 2` off the panel. That was
        // measured on this box before it was fixed — a `Rot90` page reported a
        // painted rect of `380.87x571.20` inside a slot of `571.20x380.88`.
        let page_rects: Vec<egui::Rect> = pages
            .iter()
            .zip(&rects)
            .map(|((_, (w, h), _), slot)| {
                Rect::from_center_size(
                    slot.center(),
                    Vec2::new(*w as f32 * scale, *h as f32 * scale),
                )
            })
            .collect();
        // The cursor page's paint rect — page 0's, because `pages` is cursor first.
        // Everything view-shaped below (the tiles, the crop frame, `dim_bands`,
        // `crop_from_screen`) reads `rect`, so in manga mode all of it describes
        // the cursor page and none of it needed to change.
        let rect = match page_rects.first() {
            Some(r) => *r,
            None => Rect::from_min_size(
                Pos2::new(snap(centre.x - src_size.x * 0.5), snap(centre.y - src_size.y * 0.5)),
                src_size,
            ),
        };
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
        // The on-screen footprint, i.e. `rect` turned by `quarter`.
        //
        // **Centred on `centre` when the mode is off, on `rect`'s own centre when
        // it is on** — and the difference is deliberate rather than tidiness. With
        // one image the rect's centre is the panel's centre *before* the snap, and
        // `footprint` has always been the unsnapped one; deriving it from `rect`
        // instead would move the crop frame and the four `dim_bands` by up to half
        // a physical pixel on a path that is supposed to be untouched. In a spread
        // there is no panel centre to speak of — the cursor page sits off at the
        // seam — so it has to come from `rect`.
        //
        // **Concentric with `rect`, and that is what makes the rotation cover
        // `size` exactly.** `paint_tex` rotates about `rect.center()`, so
        // `footprint` being concentric with it is the whole of the guarantee — and
        // `size` is `display_px * scale`, the cursor page's transformed footprint,
        // which is its slot's size.
        let footprint = if pages.is_empty() {
            Rect::from_center_size(centre, size)
        } else {
            Rect::from_center_size(rect.center(), size)
        };
        // The turn's other half. Painted **before** the cursor page's own, so a
        // missing partner texture costs a blank half rather than the whole turn.
        //
        // **With the partner's own `xform`,** which is the same rule the cursor
        // page follows: a turn is a set of pictures and each carries its own edit.
        // `turn_pages` looks that xform up by the partner's own path, and the slot
        // it was given in `spread_rects` is that page's *transformed* footprint, so
        // rotating about the rect's centre lands inside the slot rather than
        // spilling across the seam.
        //
        // **The checkerboard takes the same xform as the page above it,** and that
        // is not a detail: it is painted into the same rect, so leaving it
        // `Identity` would rotate the squares with the page's slot but not with
        // the page's picture, and a turned transparent page would show a backdrop
        // that does not line up with the artwork on it.
        //
        // **A missing partner texture is not a display gap.** The counter further
        // up is a regression guard on the *cursor* page, the one the reader asked
        // for; the partner is a neighbour that is nearly always already decoded —
        // it is planned at the current priority precisely so a turn paints whole —
        // and counting it would make the guard cry wolf on every cold spread.
        if let (Some(partner_rect), Some(partner)) = (page_rects.get(1), pages.get(1)) {
            let partner_xform = partner.2;
            let painted = self
                .previews
                .get_mut(&partner.0)
                // Stamped for the same reason the cursor's is: `evict_previews` is
                // an LRU on `used`, and a page on screen whose clock never moves is
                // a page the cache is allowed to drop.
                .map(|pv| {
                    pv.used = self.clock;
                    (pv.tex.id(), pv.has_alpha)
                });
            if let Some((partner_tex, partner_alpha)) = painted {
                if partner_alpha && let Some(checker) = &self.checker {
                    // From **this** page's rect, so the backdrop is under its
                    // own page. A checkerboard sized for one page and drawn
                    // under the other is a visible misalignment, not a subtle
                    // one: the squares are a fixed size in page space.
                    let ps = partner_rect.size();
                    paint_tex(
                        &painter,
                        checker.id(),
                        *partner_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(ps.x / 32.0, ps.y / 32.0)),
                        partner_xform,
                    );
                }
                paint_tex(&painter, partner_tex, *partner_rect, uv, partner_xform);
            }
        }
        // Always paint the preview first; tiles refine it where available.
        // Transparent images get a checkerboard backdrop underneath.
        if pv_has_alpha && let Some(checker) = &self.checker {
            let uv =
                Rect::from_min_max(Pos2::ZERO, Pos2::new(src_size.x / 32.0, src_size.y / 32.0));
            paint_tex(&painter, checker.id(), rect, uv, xform);
        }
        paint_tex(&painter, tex_id, rect, uv, xform);
        // The wanted image is now on screen. The *latency* is committed next
        // frame, once this one has actually been presented.
        if self.cur.as_ref() == Some(&shown) {
            self.lat.painted(Instant::now());
        }
        let (pending, sharpened) = self.central_tiles(
            &ctx,
            &painter,
            panel,
            scale,
            ppp,
            xform,
            rect,
            &pages,
            &rects,
            &page_rects,
            footprint,
            uv,
            tex_w,
            img_px,
            &shown,
        );
        // The two outcomes, once every borrow of `self` is done: `self.lat` cannot be
        // touched while `self.previews` is lent out, which is why they are flags.
        if pending {
            ctx.request_repaint();
        } else if sharpened {
            // Everything the viewport needs is on screen, on every page of the turn.
            self.lat.viewport_tiles_ready(Instant::now());
        }
        self.central_crop(ui, &resp, &ctx, &shown, footprint, display_px, &painter);
        if self.cur.is_some() && self.cur != self.shown {
            // Never for a target that already failed: its decode will never
            // be re-issued, so "Loading…" is a promise nothing keeps, and the
            // stored error in the status bar is the honest readout instead.
            let failed = self
                .cur
                .as_ref()
                .is_some_and(|k| self.failed.contains(&(k.clone(), Kind::Preview)));
            if !failed {
                painter.text(
                    panel.left_top() + Vec2::new(10.0, 10.0),
                    egui::Align2::LEFT_TOP,
                    "Loading…",
                    egui::FontId::proportional(14.0),
                    Color32::from_white_alpha(200),
                );
            }
        }
    }
}
