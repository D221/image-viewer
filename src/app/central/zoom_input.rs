//! Zoom, pan and shortcut input handling for the central render pass.
//!
//! Extracted out of `MyApp::central` when `src/app/central.rs` passed the
//! 1000-line target.

use crate::*;

impl MyApp {
    /// Read zoom/pan input and return the new `(scale, offset)`.
    ///
    /// Handles the wheel navigator, pinch/scroll zoom, preset keys, fit /
    /// fit-width / fill toggles, double-click, the colour picker, and pointer
    /// dragging. Mutates `self.wheel_acc`, `self.fit_wrap_pending`,
    /// `self.fit_iters` and `self.lat` as side effects.
    pub(crate) fn central_zoom_input(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        ctx: &egui::Context,
        panel: egui::Rect,
        ppp: f32,
        fit_scale: f32,
        fit_width_scale: f32,
        max_scale: f32,
        pages: &[(ImgKey, (u32, u32), Xform)],
        spread_dims: &[(f32, f32)],
        lone_spread_fills_turn: bool,
        display_px: egui::Vec2,
        img_px: egui::Vec2,
    ) -> (f32, egui::Vec2) {
        let mut scale = if self.config.fit_width_mode && self.view.fit {
            fit_width_scale
        } else if self.view.fit {
            fit_scale
        } else {
            self.view.scale
        };
        let mut offset = if self.view.fit { Vec2::ZERO } else { self.view.offset };
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
        // Colour picker: click on the picture to sample the pixel under the
        // cursor, copied to the clipboard. Reads from the full-res buffer;
        // reports when it isn't available (zoom to 1:1 first).
        if self.color_picker_active && resp.clicked() {
            if let Some(hover) = resp.hover_pos()
                && let Some(key) = self.cur.as_ref()
                && let Some(pv) = self.previews.get(key)
            {
                if let Some(full) = &pv.full {
                    let center = panel.center();
                    let sx =
                        ((hover.x - center.x - offset.x) / scale + img_px.x / 2.0).round() as i32;
                    let sy =
                        ((hover.y - center.y - offset.y) / scale + img_px.y / 2.0).round() as i32;
                    if (0..full.width as i32).contains(&sx) && (0..full.height as i32).contains(&sy)
                    {
                        let c =
                            &full.rgba[((sy as usize * full.width as usize + sx as usize) * 4)..];
                        self.notice = Some(format!(
                            "#{:02X}{:02X}{:02X}  rgb({}, {}, {})  at {}x{}",
                            c[0], c[1], c[2], c[0], c[1], c[2], sx, sy
                        ));
                        if let Ok(mut cb) = arboard::Clipboard::new() {
                            let _ = cb.set_text(&format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]));
                        }
                    } else {
                        self.notice = Some("Click outside the picture".into());
                    }
                } else {
                    self.notice = Some(
                        "Full-res buffer not available — view at 1:1 for pixel-accurate sampling"
                            .into(),
                    );
                }
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
        (scale, offset)
    }
}
