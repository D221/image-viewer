//! Module `app_central`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    pub(crate) fn central(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let panel = ui.available_rect_before_wrap();
        let ppp = ctx.pixels_per_point();

        // Track the display area in physical pixels; previews are decoded to
        // match it. If the window grew, below-target textures are dropped so
        // schedule() re-decodes them at the new, larger target.
        let panel_px = (
            (panel.width() * ppp).round() as u32,
            (panel.height() * ppp).round() as u32,
        );
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
                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                        egui::Pos2::new(
                            ((m.x - want.0) / 2.0).max(0.0),
                            ((m.y - want.1) / 2.0).max(0.0),
                        ),
                    ));
                }
            } else {
                self.pending_pos = Some((want, tries - 1));
            }
        }
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
        let panel_px = image_viewer::decode_panel_px(
            panel_px,
            self.config.manga_mode,
            lone_spread_fills_turn,
        );
        // Resize. The texture is re-fitted to the new area by the drawing code
        // below, which happens on this very frame and costs nothing — so the
        // picture is never blanked and never soft *as a choice*. The only thing
        // a resize needs to decide is whether to decode something sharper
        // behind the existing texture.
        //
        // A window drag delivers a stream of sizes, and one decode costs the
        // same whatever the size, so the refine is issued only once the size
        // stops moving: "unchanged since the previous frame", not a wall-clock
        // settle. A large preview takes ~800ms to decode, which means frames
        // are ~800ms apart, and any timer shorter than that expires between
        // every pair of them and never fires.
        let target = image_viewer::preview_target(panel_px);
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
        let cursor_on_archive = self
            .cur
            .as_ref()
            .is_some_and(|k| is_archive_path(&k.path))
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

        let (orig, tex_id, tex_w, pv_has_alpha) = {
            let Some(pv) = self.previews.get_mut(&shown) else {
                // The wanted image has no texture yet: this frame shows nothing.
                // Counted as a regression guard - it should stay at 0 except
                // for the very first frames of a cold open.
                perf::count(perf::C_DISPLAY_GAP_FRAMES, 1);
                return;
            };
            // Stamped here, and this is where the **cursor** page's `used` clock
            // moves — the partner's is stamped where it is painted below, for the
            // same reason. `evict_previews` is an LRU on `used`, so a page on
            // screen whose clock never advances is a page the cache is free to
            // drop, and a spread whose second half evaporates and comes back
            // re-decoded is the symptom.
            pv.used = self.clock;
            (
                pv.orig,
                pv.tex.id(),
                pv.tex.size()[0] as f32,
                pv.has_alpha,
            )
        };
        let (tex_id, tex_w) = if let Some(av) = self.anim.as_mut().filter(|a| a.path == shown.path) {
            // Paused: hold the chosen frame and schedule nothing. Playing:
            // advance to the current frame and repaint at its boundary.
            if let Some(held) = av.paused {
                if held != av.last_index {
                    if let Some(f) = av.anim.frames.get(held) {
                        av.tex.set(
                            ColorImage::from_rgba_unmultiplied(
                                [f.width as usize, f.height as usize],
                                &f.rgba,
                            ),
                            content_texture_options(self.config.pixel_art),
                        );
                        av.last_index = held;
                    }
                }
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
            (av.tex.id(), av.anim.frames[0].width as f32)
        } else {
            (tex_id, tex_w)
        };

        let img_px = Vec2::new(orig.0 as f32, orig.1 as f32);
        // Quarter turns and mirrors, clockwise. Only the eight lattice symmetries,
        // so the image stays axis-aligned and 1:1 stays pixel-sharp.
        let xform = self.shown_xform();
        // What the image *occupies* on screen: four of the eight swap the axes.
        let (dw, dh) = xformed_footprint(img_px.x, img_px.y, xform);
        let display_px = Vec2::new(dw, dh);

        // The turn's pages, in manga mode: the cursor page and, when the turn
        // holds two, the other one. **Empty with the mode off**, so every branch
        // below falls through to the single-image path unchanged — that is the
        // shape of the gate, not a coincidence.
        let pages: Vec<(ImgKey, (u32, u32), Xform)> = if self.config.manga_mode {
            self.turn_pages(&shown, orig)
        } else {
            Vec::new()
        };
        // **Transformed footprints, not stored dimensions** — and this is the one
        // line the whole turned-page behaviour turns on. `spread_rects` sizes a
        // page's slot by the dims it is handed and `paint_tex` rotates about
        // `rect.center()`, so a slot laid out from *stored* dims is the wrong
        // *shape* for a turned page and the rotated picture lands on the swapped
        // rectangle centred on the same point, spilling `|w - h| * scale / 2` past
        // the seam. Handing over each page's `xformed_footprint` instead makes the
        // slot the right shape, and then the rotation fits inside it — which is
        // exactly how the single-image path already works, where `rect` is in
        // unrotated image space and `footprint` is the on-screen rect after
        // rotation. Four of the eight `Xform`s swap the axes, so this is what a
        // turned page in a turn looks like rather than an edge case.
        //
        // Page 0's entry is `display_px` exactly — `turn_pages` was handed `orig`
        // for that page and looked up its own `shown_xform` — so the cursor page's
        // slot and the `footprint` the view already had are the same rectangle.
        let spread_dims: Vec<(f32, f32)> = pages
            .iter()
            .map(|(_, (w, h), xf)| xformed_footprint(*w as f32, *h as f32, *xf))
            .collect();

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
            (panel.width() / display_px.x)
                .min(panel.height() / display_px.y)
                .clamp(1e-4, 8.0)
        } else {
            image_viewer::spread_fit(
                (panel.width(), panel.height()),
                &spread_dims,
                lone_spread_fills_turn,
            )
        };
        // Fit-width: the picture's width fills the panel and the height
        // overflows, so you pan vertically — the natural mode for tall pages.
        // For a turn the whole spread's width fills the panel.
        let fit_width_scale = if pages.is_empty() {
            (panel.width() / display_px.x).clamp(1e-4, 8.0)
        } else {
            let total_width: f32 = spread_dims.iter().map(|&(w, _)| w).sum();
            if total_width > 0.0 {
                (panel.width() / total_width).clamp(1e-4, 8.0)
            } else {
                1.0
            }
        };
        let max_scale = MAX_SCALE.max(fit_scale).max(fit_width_scale);

        // Fit window to image: reset (`T`, arrivals in the mode) sets
        // 100%-if-fits and sizes the window; wrap (user zooms in the mode)
        // keeps the zoom and only re-hugs. Every resize centers on the
        // monitor, always — old-window centering was asked for and dropped:
        // polite in theory, drifting in practice. Converges on the symptom
        // itself: while the drawn picture and the panel disagree, re-measure
        // and re-send (panels move after the measure — notices, wraps, late
        // widths — so one shot lands wherever they leave it). `wrap` doubles
        // as the settle flag: cleared only on convergence, so the loop
        // actually repeats instead of running once and stranding `fit_iters`.
        // Bounded by `fit_iters`, and each pass only sends when off target.
        // Placed before `scale` is read so a reset draws what the window was
        // sized for on the same frame. Arrival fits wait for adoption
        // (`shown` still displays the previous image until its preview lands
        // — fitting then sizes the window for the picture just left); an
        // explicit `T` fits whatever is on screen by definition. Waiting
        // burns no iterations.
        if (self.fit_window_pending || self.fit_arrive_pending || self.fit_wrap_pending)
            && self.fit_iters > 0
            && (!self.fit_arrive_pending || self.fit_window_pending || self.shown == self.cur)
        {
            self.fit_iters -= 1;
            // Arrival carries the reset too (it just waits longer to run):
            // both arms mean "this picture starts at 100%", only the timing
            // differs. Forgetting the arrival half is how zoomed-out views
            // survived navigation.
            let reset = self.fit_window_pending || self.fit_arrive_pending;
            self.fit_arrive_pending = false;
            if !ui.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
                // Twin of `clamp_px` below: the content's source footprint,
                // so the two cannot disagree about how big the picture is.
                let natural = if pages.is_empty() {
                    display_px
                } else {
                    let mut w = 0.0;
                    let mut h: f32 = 0.0;
                    for &(pw, ph) in &spread_dims {
                        w += pw;
                        h = h.max(ph);
                    }
                    Vec2::new(w, h)
                } / ppp;
                if let Some(inner) = ui.input(|i| i.viewport().inner_rect.map(|r| r.size())) {
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
                        let max_scale = (available_x / natural.x)
                            .min(available_y / natural.y)
                            .min(1.0)
                            .max(0.01);
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
                    self.fit_debug = Some((
                        ratio,
                        drawn.0,
                        drawn.1,
                        panel.width(),
                        panel.height(),
                        inner.x,
                        inner.y,
                    ));
                    // Converge on the symptom (drawn vs panel), not the proxy
                    // (window vs target): a window can match its target while
                    // the picture still disagrees with the panel, and that
                    // disagreement is the bars.
                    if (drawn.0 - panel.width()).abs() > 2.0
                        || (drawn.1 - panel.height()).abs() > 2.0
                    {
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
            }
        }

        let mut scale = if self.config.fit_width_mode {
            fit_width_scale
        } else if self.view.fit {
            fit_scale
        } else {
            self.view.scale
        };
        let mut offset = if self.config.fit_width_mode || self.view.fit {
            Vec2::ZERO
        } else {
            self.view.offset
        };
        // Free zoom-out: the floor is effectively zero rather than the fit
        // scale, so the picture can shrink below the window. Centering below
        // fit comes from the pan bounds, not from snapping back.
        scale = scale.clamp(1e-4, max_scale);

        // --- input: zoom (wheel / pinch), pan (drag), shortcuts ---
        let hover = resp.hover_pos();
        // Fixed zoom presets, each a configurable source-pixel ratio (`1`
        // keeps the shipped meaning: 100%). Read as one option so the
        // dispatch below has a single branch rather than four.
        let kb = build_bindings(&self.config);
        let (scroll, pinch, key_fit, key_fit_width, key_fill, preset) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                binding_down(i, &kb, "fit") || binding_down(i, &kb, "fit_alt"),
                binding_down(i, &kb, "fit_width"),
                binding_down(i, &kb, "fill"),
                ["preset_1", "preset_2", "preset_3", "preset_4"]
                    .iter()
                    .position(|a| binding_down(i, &kb, a)),
            )
        });

        // Wheel navigation mode: accumulate wheel deltas and flip images
        // instead of zooming. Ctrl+wheel always zooms.
        let wheel_nav_active = self.config.wheel_navigation && !ui.input(|i| i.modifiers.ctrl);
        if wheel_nav_active && scroll != 0.0 {
            // **The wheel asks the rule too, and the rule says the same answer in
            // both directions** — it is the one input with no side to preserve, so
            // scrolling down is "onward" whether the book opens left or right.
            // Spelling `true` here would be correct today and would be the one
            // branch that could drift away from the setting without looking wrong:
            // Mihon excludes the wheel for the same reason.
            let down = nav_is_forward(NavInput::WheelDown, self.config.manga_rtl);
            // Scroll down (negative) = next image, scroll up = previous.
            self.wheel_acc += scroll;
            if self.wheel_acc <= -THRESH_WHEEL {
                self.wheel_acc = 0.0;
                self.step(&ctx, down);
            } else if self.wheel_acc >= THRESH_WHEEL {
                self.wheel_acc = 0.0;
                self.step(&ctx, !down);
            }
        }

        let mut factor = 1.0f32;
        if resp.hovered() {
            factor *= pinch;
            if scroll != 0.0 && !wheel_nav_active {
                factor *= (scroll * 0.0025).exp();
            }
        }
        let mut target_scale = scale * factor;
        let anchor = hover.unwrap_or(panel.center());
        // `1` toggles the 1:1 mode (sticky); `2`–`4` stay one-shot jumps.
        // `F`/`Space` toggle fit-view; double-click still fits once without
        // touching any flag. Keyboard shortcuts stand down while typing
        // (filter box) like the ui() dispatch does — typing "1" must not
        // toggle anything.
        if !ctx.egui_wants_keyboard_input() {
            if let Some(i) = preset {
                // Absolute ratio, not a multiplier: 1.0 is one source pixel per
                // physical pixel, whatever the fit is.
                target_scale = image_viewer::zoom_preset(&self.config.zoom_presets, i) / ppp;
            }
            if key_fit {
                self.toggle_fit_mode();
                // Applies immediately, same frame: on fits this image now,
                // off drops to absolute — neither waits for the next
                // arrival to make the mode visible.
                target_scale = if self.config.fit_mode {
                    fit_scale
                } else {
                    self.view.scale = 1.0 / ppp;
                    1.0 / ppp
                };
            }
            if key_fit_width {
                self.toggle_fit_width_mode();
                target_scale = if self.config.fit_width_mode {
                    fit_width_scale
                } else {
                    self.view.scale = 1.0 / ppp;
                    1.0 / ppp
                };
            }
        }
        if resp.double_clicked() {
            // In T mode, double-click should return to the T-mode initial scale
            // (1:1 or shrink-to-fit monitor), not fit-to-panel. Set the pending
            // flag so the T-mode reset runs on the next frame.
            if self.config.fit_window_mode {
                self.fit_window_pending = true;
                self.fit_iters = 6;
                target_scale = 1.0;
            } else {
                target_scale = fit_scale;
            }
        }
        if key_fill && pages.is_empty() {
            // Single-image only: a turn's layout (`spread_fit`, pan bounds,
            // seam) assumes the fit scale, so filling there is a layout change,
            // not a zoom. Here it is just a larger target on the same clamp.
            target_scale =
                fill_scale((display_px.x, display_px.y), (panel.width(), panel.height()));
        }
        // Sticky fit-window mode follows user zooms: re-hug the new drawn
        // size next frame. Input-driven, never layout-driven — a window
        // resize changing the fit scale must not retrigger it, which is the
        // feedback loop this placement exists to prevent. Wheel-flips are
        // image switches, not zooms, so the wheel only counts in zoom mode.
        if self.config.fit_window_mode
            && !self.slideshow
            && !self.gallery_view
            && !self.view.fit
            && ((!wheel_nav_active && scroll != 0.0)
                || pinch != 1.0
                || preset.is_some()
                || key_fit
                || key_fill
                || resp.double_clicked())
        {
            self.fit_wrap_pending = true;
            self.fit_iters = 6;
        }
        // Free zoom-out, input side: matches the floor above.
        let target_scale = target_scale.clamp(1e-4, max_scale);
        if (target_scale - scale).abs() > f32::EPSILON {
            // Zooming in: the visible content gets sharper. Time it from the
            // input, not from when the tiles happen to finish.
            self.lat.t_zoom = Some(Instant::now());
            self.lat.want_viewport_tiles = true;
            // Keep the point under the cursor fixed — measured from the
            // **content's** centre, not the panel's.
            //
            // `anchor - panel.center()` is right only when the content is centred on
            // the panel, which a turn is not: `spread_rects` puts the seam on the
            // panel's horizontal centre and extends each page outward, so the
            // content's centre is the seam plus half the difference of the widths,
            // and being top-aligned its vertical centre is the panel's top plus half
            // the tallest page. Both are zero for a single image — which is the case
            // that expression was written for, and the assumption that hid it.
            //
            // `spread_centre` derives its answer from `spread_rects`, so the anchor
            // cannot drift from the layout the same frame draws.
            //
            // **The content's centre is asked twice, at two different scales**, and
            // that is the whole of the remaining subtlety. A point under the cursor
            // stays under the cursor if `offset' = anchor - C(s') - (s'/s)·(anchor -
            // C(s) - offset)`, where `C` is the content's centre *at that scale*:
            // the two terms bracket the same content point, and `C(s)` is a function
            // of the scale because the pages are laid out at it. Reusing one `C` for
            // both is exactly the old line again with a different centre — it agrees
            // when `C` does not move, which a single image's does not and a turn's
            // does (`C(s) = (seam ± Δw·s/2, top + h·s/2)`, so `Δ` is 0 only for two
            // equal-width pages on the horizontal axis and the vertical `Δ` is
            // non-zero for every turn). With manga mode off both calls answer the
            // panel centre and the expression collapses to `c - (c - offset)·k`
            // term for term, so **the single-image path is unchanged**, and
            // `a_single_page_turn_anchors_on_the_panel_centre` is what says so.
            let panel4 = [panel.min.x, panel.min.y, panel.width(), panel.height()];
            let centre_at = |s: f32| {
                let (cx, cy) = image_viewer::spread_centre(
                    panel4,
                    &spread_dims,
                    s,
                    self.config.manga_rtl,
                    lone_spread_fills_turn,
                );
                Pos2::new(cx, cy)
            };
            let c_from = anchor - centre_at(scale);
            let c_to = anchor - centre_at(target_scale);
            offset = c_to - (c_from - offset) * (target_scale / scale);
            scale = target_scale;
        }
        if resp.dragged() && !self.crop_mode {
            offset += resp.drag_delta();
            self.lat.t_pan = Some(Instant::now());
            self.lat.want_viewport_tiles = true;
        }

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
        //
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
                [
                    panel.min.x + offset.x,
                    panel.min.y + offset.y,
                    panel.width(),
                    panel.height(),
                ],
                &spread_dims,
                scale,
                self.config.manga_rtl,
                lone_spread_fills_turn,
            )
            .into_iter()
            // `x` and `y` snapped, and only those: 1:1 has to stay pixel-sharp,
            // and snapping the far edge as well would move the size the tiles and
            // the source-pixel mapping below are told to expect.
            .map(|r| {
                Rect::from_min_size(
                    Pos2::new(snap(r[0]), snap(r[1])),
                    Vec2::new(r[2], r[3]),
                )
            })
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
                Pos2::new(
                    snap(centre.x - src_size.x * 0.5),
                    snap(centre.y - src_size.y * 0.5),
                ),
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
                if partner_alpha {
                    if let Some(checker) = &self.checker {
                        // From **this** page's rect, so the backdrop is under its
                        // own page. A checkerboard sized for one page and drawn
                        // under the other is a visible misalignment, not a subtle
                        // one: the squares are a fixed size in page space.
                        let ps = partner_rect.size();
                        paint_tex(
                            &painter,
                            checker.id(),
                            *partner_rect,
                            Rect::from_min_max(
                                Pos2::ZERO,
                                Pos2::new(ps.x / 32.0, ps.y / 32.0),
                            ),
                            partner_xform,
                        );
                    }
                }
                paint_tex(&painter, partner_tex, *partner_rect, uv, partner_xform);
            }
        }

        // Always paint the preview first; tiles refine it where available.
        // Transparent images get a checkerboard backdrop underneath.
        if pv_has_alpha {
            if let Some(checker) = &self.checker {
                let uv = Rect::from_min_max(
                    Pos2::ZERO,
                    Pos2::new(src_size.x / 32.0, src_size.y / 32.0),
                );
                paint_tex(&painter, checker.id(), rect, uv, xform);
            }
        }
        paint_tex(&painter, tex_id, rect, uv, xform);
        // The wanted image is now on screen. The *latency* is committed next
        // frame, once this one has actually been presented.
        if self.cur.as_ref() == Some(&shown) {
            self.lat.painted(Instant::now());
        }

        // --- full-resolution tiles ---
        //
        // **Over every page of the turn, not the cursor's alone.** This loop used to
        // be filtered to `self.full` matching `shown.path` - the cursor - and that
        // threw away a buffer which was *already in memory*: `sync_shown` adopts the
        // cursor's full image by `Arc::clone` rather than moving it, and the
        // single-pass decode hands every cached preview one, so a turn's second half
        // has its own full-resolution buffer sitting on `previews[partner].full`. The
        // expensive decode had already happened; nothing was looking at the result.
        //
        // For a source over `FAST_FULL_MAX_PIXELS` there is no such buffer - the
        // preview is a DCT-scaled decode - so the plan asks for one
        // (`Plan::partner_needs_full`) and `Msg::Full` keeps it on that page's own
        // preview. Those are ordinary manga scans, so this is the common case and not
        // a corner one. Until that decode lands the page is simply not sharpened, and
        // its preview is on screen underneath.
        //
        // **`MAX_TILES` is still the single bound**, and it has to be counted across
        // pages: cuts are on demand (`TILES_PER_FRAME`, and only over what is
        // visible), so the steady state is "a zoomed spread" - 64 tiles between two
        // pages, not 64 each - and the eviction pass below sums every page's map
        // before dropping anything. What a page is *not* is a page that keeps its
        // tiles after it leaves the screen, which is why the count is global rather
        // than per page: one map per cached page, each with its own 64, is 16 x 256 MB
        // and the ceiling would have been raised by accident.
        let screen_density = scale * ppp; // physical px per source px
        self.frame += 1;
        let frame = self.frame;

        // What each page of the turn can be sharpened from, gathered **before** any
        // mutable borrow and owned: an `Arc` clone (a refcount) plus that page's own
        // density and its two rectangles. `None` is a page with no full buffer.
        //
        // **Page 0 is the cursor, and `pages` is cursor-first**, so the entry the old
        // code computed for it is the one at index 0 - `pv.px.0 / ow` is the same
        // `tex_w / img_px.x` (`px` *is* the texture's size, and `ow` is `orig`), and
        // the single-image path below is spelled out separately so it does not depend
        // on that being true.
        // The cursor page's source, whether or not a turn is on screen: the adopted
        // `FullImage`, at the density the single-image path has always used.
        //
        // **Page 0 of a turn reads this too**, and not `previews[shown].full` which
        // holds the same buffer whenever the single-pass decode produced one. Two
        // reasons, and the second is the load-bearing one: a source over
        // `FAST_FULL_MAX_PIXELS` never gets a `pv.full` *at all* - its full decode
        // lands in `self.full` - so the preview slot is empty for exactly the pages
        // that need tiles most; and the cursor page's tiles are kept in
        // `FullImage.tiles`, which `sync_shown` clears whenever the cursor moves.
        // Two maps for one page would be two budgets' worth of GPU memory and a
        // re-cut on every navigation.
        let cursor_page = || {
            self.full.as_ref().and_then(|f| {
                // **The key, not the path.** This closure hands `f.data` — one page's
                // pixels — to `make_tile`, and the rect and density beside it are
                // `shown`'s. With the page missing, page 0's buffer satisfied the check
                // on every page of a document and the loop cut **page 0's tiles** with
                // page N's `uv`: page 0 drawn over page N, no error and no counter.
                (f.is(&shown)).then(|| TilePage {
                    data: Arc::clone(&f.data),
                    // texture px per source px
                    preview_density: tex_w / img_px.x,
                    paint: if pages.is_empty() { rect } else { page_rects[0] },
                    slot: if pages.is_empty() { footprint } else { rects[0] },
                    xform,
                    // `img_px` is the source's own size (`orig`), whatever the
                    // buffer decoded to: for a DCT-scaled buffer this is where
                    // the pitch-up ratio comes from.
                    src: img_px,
                })
            })
        };
        let tile_pages: Vec<Option<TilePage>> = if pages.is_empty() {
            // Manga mode off. `self.full` and `rect`/`footprint` are the whole of
            // what this has ever been given, and they are spelled that way on purpose.
            Vec::from_iter([cursor_page()])
        } else {
            pages
                .iter()
                .enumerate()
                .map(|(i, (key, (ow, oh), xf))| {
                    if i == 0 {
                        return cursor_page();
                    }
                    let pv = self.previews.get(key)?;
                    let data = Arc::clone(pv.full.as_ref()?);
                    Some(TilePage {
                        data,
                        // **This page's** texture against **this page's** source. The
                        // cursor's numbers on the partner's would decide whether to cut
                        // tiles from a buffer that is a different size from the one the
                        // decision was made about.
                        preview_density: pv.px.0 as f32 / (*ow).max(1) as f32,
                        // The picture in unrotated image space - the tile grid is in
                        // this page's own source pixels - and the same rectangle after
                        // `xform`, which is what can be on screen.
                        paint: page_rects[i],
                        slot: rects[i],
                        xform: *xf,
                        // `ow`/`oh` are this page's source size, as `img_px` is the
                        // cursor's: the ratio against the buffer is per page.
                        src: egui::Vec2::new(*ow as f32, *oh as f32),
                    })
                })
                .collect()
        };

        // The map each page keeps its tiles in, **moved out** for the length of the
        // loop and put back below: the cursor page's on the adopted `FullImage`
        // (rebuilt, and so cleared, whenever the cursor moves - the rule that has
        // always bounded it) and every other page's on its own preview.
        //
        // Moved rather than borrowed because two pages' worth have to be held **at
        // once**, and the borrow checker will not lend `self.full` and
        // `self.previews` mutably inside one loop even though they are different
        // fields. `mem::take` leaves an empty map with no allocation, so this is a
        // pointer move rather than a copy of anything.
        enum TileMapOwner {
            Full,
            Page(ImgKey),
        }
        let mut maps: Vec<(HashMap<(u32, u32), Tile>, TileMapOwner)> =
            Vec::with_capacity(tile_pages.len());
        for (i, tp) in tile_pages.iter().enumerate() {
            if tp.is_none() {
                continue;
            }
            if i == 0 {
                if let Some(f) = self.full.as_mut().filter(|f| f.is(&shown)) {
                    maps.push((std::mem::take(&mut f.tiles), TileMapOwner::Full));
                }
            } else if let Some(pv) = self.previews.get_mut(&pages[i].0) {
                let key = pages[i].0.clone();
                maps.push((std::mem::take(&mut pv.tiles), TileMapOwner::Page(key)));
            }
        }

        let mut budget = TILES_PER_FRAME;
        let mut pending = false;
        let mut sharpened = false;
        // Decided once per frame, not per tile: every tile this frame cuts
        // should carry the same sampling, and reading the config once keeps
        // that true even if a settings arm ran between two `make_tile` calls.
        let tile_options = content_texture_options(self.config.pixel_art);
        for (tp, (tiles, _owner)) in tile_pages.iter().zip(maps.iter_mut()) {
            let Some(tp) = tp else { continue };
            let preview_density = tp.preview_density;
            let (paint, slot, page_xform) = (tp.paint, tp.slot, tp.xform);
            if screen_density > preview_density {
                let (fw, fh) = (tp.data.width, tp.data.height);
                let vis = panel.intersect(slot);
                if vis.width() > 0.0 && vis.height() > 0.0 {
                    sharpened = true;
                    // Source pixels per buffer pixel, per axis: 1.0 when the
                    // buffer is the full decode, 2.0 when it is a half-res DCT
                    // decode of a source past `MAX_TEXTURE_DIM`. The range below
                    // is named in *buffer* pixels and the rect is pitched back
                    // up — see `tile_axis_range` / `tile_draw_rect`, whose tests
                    // pin both halves against the ghost this fixes.
                    let kx = tp.src.x / fw.max(1) as f32;
                    let ky = tp.src.y / fh.max(1) as f32;
                    let (x0, x1) = image_viewer::tile_axis_range(
                        vis.min.x, vis.max.x, paint.min.x, scale, kx, fw,
                    );
                    let (y0, y1) = image_viewer::tile_axis_range(
                        vis.min.y, vis.max.y, paint.min.y, scale, ky, fh,
                    );

                    if x1 > x0 && y1 > y0 {
                        for ty in (y0 / TILE)..=((y1 - 1) / TILE) {
                            for tx in (x0 / TILE)..=((x1 - 1) / TILE) {
                                if !tiles.contains_key(&(tx, ty)) {
                                    if budget == 0 {
                                        pending = true;
                                        continue;
                                    }
                                    budget -= 1;
                                    let tex = make_tile(&ctx, &tp.data, tx, ty, tile_options);
                                    perf::count(perf::C_TILES_CREATED, 1);
                                    tiles.insert(
                                        (tx, ty),
                                        Tile {
                                            tex,
                                            last_drawn: frame,
                                        },
                                    );
                                }
                                let tile = tiles.get_mut(&(tx, ty)).unwrap();
                                tile.last_drawn = frame;
                                let (px, py) = (tx * TILE, ty * TILE);
                                let (tw, th) = (TILE.min(fw - px), TILE.min(fh - py));
                                let q = image_viewer::tile_draw_rect(
                                    (paint.min.x, paint.min.y),
                                    px,
                                    py,
                                    tw,
                                    th,
                                    scale,
                                    kx,
                                    ky,
                                );
                                let r = Rect::from_min_size(
                                    Pos2::new(q[0], q[1]),
                                    Vec2::new(q[2], q[3]),
                                );
                                paint_tex(&painter, tile.tex.id(), r, uv, page_xform);
                            }
                        }
                    }
                }
            } else if screen_density < preview_density * 0.5 {
                tiles.clear(); // zoomed well out: release tile textures
            }
        }
        // Put every page's tiles back where they came from, **before** the bound is
        // counted - the count is over `self`, not over the locals above.
        //
        // A map whose owner has gone is dropped rather than put back anywhere else.
        // `self.full` cannot be dropped between here and there (nothing in the loop
        // touches it) and `previews` cannot lose an entry to `evict_previews` in the
        // same breath, so this is belt-and-braces: a tile texture with no owner is
        // released, which is the safe direction to be wrong in.
        for (tiles, owner) in maps {
            match owner {
                TileMapOwner::Full => {
                    if let Some(f) = self.full.as_mut() {
                        f.tiles = tiles;
                    }
                }
                TileMapOwner::Page(key) => {
                    if let Some(pv) = self.previews.get_mut(&key) {
                        pv.tiles = tiles;
                    }
                }
            }
        }

        // Bound GPU memory: drop the least recently drawn tiles, **counting every
        // page's map**. One page's map used to be the whole of it, and it is the
        // whole of it in the single-image case still - so with manga mode off this is
        // the rule that was always here, with an empty sum added.
        //
        // Counted over the whole cache rather than the pages on screen, because a
        // page that leaves the screen keeps its tiles until it is evicted: counting
        // only the visible pages would let each new turn start a fresh budget and the
        // total would climb with every turn the reader flipped through.
        let held: usize = self.full.as_ref().map_or(0, |f| f.tiles.len())
            + self.previews.values().map(|pv| pv.tiles.len()).sum::<usize>();
        if held > MAX_TILES {
            enum Owner {
                Full,
                Page(ImgKey),
            }
            let mut old: Vec<(Owner, (u32, u32), u64)> = Vec::new();
            if let Some(f) = self.full.as_ref() {
                old.extend(
                    f.tiles
                        .iter()
                        .filter(|(_, t)| t.last_drawn < frame)
                        .map(|(k, t)| (Owner::Full, *k, t.last_drawn)),
                );
            }
            for (key, pv) in self.previews.iter() {
                old.extend(
                    pv.tiles
                        .iter()
                        .filter(|(_, t)| t.last_drawn < frame)
                        .map(|(k, t)| (Owner::Page(key.clone()), *k, t.last_drawn)),
                );
            }
            old.sort_by_key(|(_, _, last)| *last);
            for (owner, k, _) in old.into_iter().take(held - MAX_TILES) {
                match owner {
                    Owner::Full => {
                        if let Some(f) = self.full.as_mut() {
                            f.tiles.remove(&k);
                        }
                    }
                    Owner::Page(key) => {
                        if let Some(pv) = self.previews.get_mut(&key) {
                            pv.tiles.remove(&k);
                        }
                    }
                }
            }
        }

        // The two outcomes, once every borrow of `self` is done: `self.lat` cannot be
        // touched while `self.previews` is lent out, which is why they are flags.
        if pending {
            ctx.request_repaint();
        } else if sharpened {
            // Everything the viewport needs is on screen, on every page of the turn.
            self.lat.viewport_tiles_ready(Instant::now());
        }

        // --- crop ---
        //
        // egui already tracks where the current press started
        // (`InputState::pointer.press_origin`), so the frame needs no gesture
        // state of its own. Using egui's rather than accumulating
        // `resp.drag_delta()` matters: the rectangle is rebuilt from the start
        // point every frame, so it cannot drift, and egui's press origin already
        // applies the drag threshold.
        let pvx = self.previews.get(&shown);
        if self.crop_mode {
            let (down, pressed, origin, ptr) = ui.input(|i| {
                (
                    i.pointer.button_down(egui::PointerButton::Primary),
                    i.pointer.primary_pressed(),
                    i.pointer.press_origin(),
                    resp.interact_pointer_pos(),
                )
            });
            if let (true, Some(start), Some(p), Some(pv), Some(cur)) =
                (down, origin, ptr, pvx, self.cur.clone())
            {
                // **Once per gesture, on the press** — not at the `set_crop` below,
                // which runs every frame the pointer moves. Recording per frame
                // would push hundreds of entries for one drag, and undo would then
                // need hundreds of presses to cross it. This arm is entered on every
                // frame the button is held, so the press frame is tested explicitly.
                if pressed {
                    self.history.record(&cur.path, self.edits.get(&cur.path));
                }
                // Shift is a temporary 1:1 whatever the setting says, because it
                // is the one shape that is always wanted and never worth a trip to
                // the settings panel. Read per frame rather than latched on press:
                // the shape a drag ends up with should be the one your hand is
                // doing now, and letting go of Shift mid-drag is how you get out
                // of a shape you did not want.
                let aspect = self.crop_aspect(&ctx, display_px);
                if let Some((crop, _)) =
                    self.crop_from_screen(start, p, aspect, footprint, display_px, pv)
                {
                    // Edits are per *file*, not per page: a turn or a crop frame
                    // you set while looking at page 2 belongs to the document, and
                    // that is the thing `Ctrl+S` writes.
                    self.edits.set_crop(&cur.path, Some(crop));
                }
            }
        }

        // Draw the frame from the *stored* crop mapped back out, so what is on
        // screen is the rectangle the save will cut — block-aligned, and correct
        // through any EXIF tag or turn or mirror.
        if let Some(pv) = pvx {
            if let Some(cr) = self.crop_screen_rect(footprint, display_px, pv) {
                let cr = cr.intersect(footprint);
                if cr.width() > 1.0 && cr.height() > 1.0 {
                    // Dim what is being cut away, as four bands rather than a
                    // full-panel wash with a hole: no blending, so the picture
                    // outside the frame is unchanged and only the frame is new ink.
                    // The geometry — which includes not dimming the middle of the
                    // top and bottom edges, which is what the first version did —
                    // is `dim_bands`, and it is tested there.
                    let band = Color32::from_black_alpha(140);
                    let outer = [
                        footprint.min.x,
                        footprint.min.y,
                        footprint.right_bottom().x,
                        footprint.right_bottom().y,
                    ];
                    let inner = [cr.min.x, cr.min.y, cr.max.x, cr.max.y];
                    for [x0, y0, x1, y1] in image_viewer::dim_bands(outer, inner) {
                        if x1 > x0 && y1 > y0 {
                            painter.rect_filled(
                                egui::Rect::from_min_max(
                                    egui::Pos2::new(x0, y0),
                                    egui::Pos2::new(x1, y1),
                                ),
                                0u8,
                                band,
                            );
                        }
                    }
                    painter.rect_stroke(
                        cr,
                        0u8,
                        egui::Stroke::new(1.0, Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                    // Rule of thirds. Two lines each way, faint: enough to line a
                    // portrait up on thirds without competing with the picture.
                    let thin = egui::Stroke::new(1.0, Color32::from_white_alpha(70));
                    for i in 1..3 {
                        let t = i as f32 / 3.0;
                        let x = cr.min.x + cr.width() * t;
                        let y = cr.min.y + cr.height() * t;
                        painter.line_segment(
                            [egui::Pos2::new(x, cr.min.y), egui::Pos2::new(x, cr.max.y)],
                            thin,
                        );
                        painter.line_segment(
                            [egui::Pos2::new(cr.min.x, y), egui::Pos2::new(cr.max.x, y)],
                            thin,
                        );
                    }
                    if let Some(crop) = self.edits.crop(&shown.path) {
                        painter.text(
                            cr.left_top() + Vec2::new(4.0, -6.0),
                            egui::Align2::LEFT_BOTTOM,
                            crop.label(),
                            egui::FontId::proportional(13.0),
                            Color32::WHITE,
                        );
                    }
                }
            }
        }

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
