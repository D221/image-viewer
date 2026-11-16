//! Crop gesture and crop-frame drawing for the central render pass.
//!
//! Extracted out of `MyApp::central` when `src/app/central.rs` passed the
//! 1000-line target.

use crate::*;

impl MyApp {
    /// Handle the crop gesture and draw the crop frame with its dim bands.
    // The render pass threads this many values through; a params struct would
    // be a larger refactor than the function.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn central_crop(
        &mut self,
        ui: &mut egui::Ui,
        resp: &egui::Response,
        ctx: &egui::Context,
        shown: &ImgKey,
        footprint: egui::Rect,
        display_px: egui::Vec2,
        painter: &egui::Painter,
    ) {
        // --- crop ---
        //
        // The gesture is decided **once, on the press**, and carried by
        // `self.crop_drag` after that. `primary_pressed()` is true for exactly
        // one frame, so re-deciding every frame is how moving a frame ended up
        // painting a new one on every held frame. `image_viewer::CropDrag` is
        // that decision, and it is tested.
        let pvx = self.previews.get(shown);
        if self.crop_mode {
            let (down, pressed, ptr) = ui.input(|i| {
                (
                    i.pointer.button_down(egui::PointerButton::Primary),
                    i.pointer.primary_pressed(),
                    resp.interact_pointer_pos(),
                )
            });
            if !down {
                self.crop_drag = None;
            }
            if let (true, Some(p), Some(pv), Some(cur)) = (down, ptr, pvx, self.cur.clone()) {
                let screen = |r: egui::Rect| [r.min.x, r.min.y, r.max.x, r.max.y];
                // **Once per gesture, on the press** — not at the `set_crop`
                // below, which runs every frame the pointer moves. Recording per
                // frame would push hundreds of entries for one drag, and undo
                // would then need hundreds of presses to cross it. This arm is
                // entered on every frame the button is held, so the press frame
                // is tested explicitly.
                if pressed {
                    self.history.record(&cur.path, self.edits.get(&cur.path));
                    // The frame that was under the press. Only if it belongs to
                    // the file being edited: `crop_screen_rect` reads the
                    // *shown* picture, which is `cur` except in the frame or two
                    // before a requested preview is adopted.
                    let under = self
                        .crop_screen_rect(footprint, display_px, pv)
                        .filter(|r| r.contains(p))
                        .filter(|_| self.shown.as_ref().is_some_and(|s| s.path == cur.path))
                        .map(screen);
                    self.crop_drag = Some(CropDragState {
                        path: cur.path.clone(),
                        drag: image_viewer::CropDrag::at_press(under, (p.x, p.y)),
                    });
                }
                let drag = self
                    .crop_drag
                    .as_ref()
                    .filter(|s| s.path == cur.path)
                    .map(|s| s.drag.frame((p.x, p.y), screen(footprint)));
                // Shift is a temporary 1:1 whatever the setting says, because it
                // is the one shape that is always wanted and never worth a trip to
                // the settings panel. Read per frame rather than latched on press:
                // the shape a drag ends up with should be the one your hand is
                // doing now, and letting go of Shift mid-drag is how you get out
                // of a shape you did not want.
                match drag {
                    Some(image_viewer::CropFrame::Draw { anchor, corner }) => {
                        let aspect = self.crop_aspect(ctx, display_px);
                        if let Some((crop, _)) = self.crop_from_screen(
                            egui::Pos2::new(anchor.0, anchor.1),
                            egui::Pos2::new(corner.0, corner.1),
                            aspect,
                            footprint,
                            display_px,
                            pv,
                        ) {
                            // Edits are per *file*, not per page: a turn or a crop
                            // frame you set while looking at page 2 belongs to the
                            // document, and that is the thing `Ctrl+S` writes.
                            self.edits.set_crop(&cur.path, Some(crop));
                        }
                    }
                    // Translation only: the frame keeps the shape it had when the
                    // button went down, so no `aspect` is applied here.
                    Some(image_viewer::CropFrame::Move(frame)) => {
                        let frame = egui::Rect::from_min_max(
                            egui::Pos2::new(frame[0], frame[1]),
                            egui::Pos2::new(frame[2], frame[3]),
                        );
                        if let Some((crop, _)) =
                            self.crop_from_screen_rect(frame, footprint, display_px, pv)
                        {
                            self.edits.set_crop(&cur.path, Some(crop));
                        }
                    }
                    None => {}
                }
            }
        }
        // Draw the frame from the *stored* crop mapped back out, so what is on
        // screen is the rectangle the save will cut — block-aligned, and correct
        // through any EXIF tag or turn or mirror.
        if let Some(pv) = pvx
            && let Some(cr) = self.crop_screen_rect(footprint, display_px, pv)
        {
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
}
