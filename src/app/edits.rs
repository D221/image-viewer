//! Module `app_edits`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// Turn the shown image by `steps` quarter turns clockwise (negative =
    /// counter-clockwise). Re-fits afterwards, since the footprint changes
    /// shape.
    ///
    /// **Per image, not per app.** This used to be a single `u8` on `MyApp`, so
    /// turning one photo turned every photo you then navigated to, and coming
    /// back showed the original at whatever the current rotation happened to be.
    /// That is not how any real viewer behaves — Windows Photos, FastStone,
    /// IrfanView and Preview all keep it per image and keep your edit when you
    /// return — and for an app whose whole job is showing a picture faithfully
    /// it is worse than wrong, because you cannot tell "I rotated this" from
    /// "this is broken".
    ///
    /// Still a view transform: nothing is written until `save_transformed_copy`.
    pub(crate) fn rotate(&mut self, steps: i32, ctx: &Context) {
        self.apply_to_current(|r, p| r.turn(p, steps), ctx);
    }

    /// Mirror the shown image left-to-right or top-to-bottom.
    ///
    /// Goes through the same store as `rotate`, so a mirror and a rotation
    /// compose instead of living in two counters. That matters: rotating and then
    /// mirroring is a *diagonal* mirror, and two independent counters would have
    /// the save work that out at the last moment, where a sign error is invisible
    /// because the viewer re-applies the same mistake to the file it just wrote.
    pub(crate) fn mirror(&mut self, horizontal: bool, ctx: &Context) {
        self.apply_to_current(
            |r, p| {
                if horizontal { r.mirror_horizontal(p) } else { r.mirror_vertical(p) }
            },
            ctx,
        );
    }

    /// Back to the file's own orientation, discarding the view transform.
    pub(crate) fn reset_transform(&mut self, ctx: &Context) {
        self.apply_to_current(
            |r, p| {
                r.reset(p);
                Xform::Identity
            },
            ctx,
        );
    }

    /// Shared tail for every view-transform button: re-fit and repaint.
    ///
    /// `view.fit` is reset because four of the eight transforms change the
    /// footprint's shape. A mirror does not, so re-fitting there is harmless but
    /// not required — and doing it anyway is the same code path for all of them,
    /// which is worth more than saving the reset on two buttons.
    pub(crate) fn apply_to_current(
        &mut self,
        f: impl FnOnce(&mut image_viewer::ImageEdits, &std::path::Path) -> Xform,
        ctx: &Context,
    ) {
        if let Some(p) = self.cur.clone() {
            // Per file, not per page: a turn is an edit to the document.
            //
            // **Recorded before the edit, not after** — `undo` hands back the
            // state the image was in *before* this change, so recording
            // afterwards would store the new state and make every undo a no-op
            // that looks like it worked.
            self.history.record(&p.path, self.edits.get(&p.path));
            f(&mut self.edits, &p.path);
        }
        self.after_edit_change(ctx);
    }

    /// The tail every change to the picture's transform shares, so that undo,
    /// redo and a turn cannot drift apart on whether the view re-fits.
    pub(crate) fn after_edit_change(&mut self, ctx: &Context) {
        // An explicit turn/mirror keeps the current zoom contract: only the
        // legacy no-mode state refits, the way it always has. Fit-window and
        // fit-view already answer "how big", so a turn must not second-guess
        // them.
        if !self.config.fit_window_mode {
            self.view.fit = true;
        }
        self.view.offset = Vec2::ZERO;
        ctx.request_repaint();
    }

    /// Undo the last view transform on the current image.
    ///
    /// **Reads `cur`, not `shown`**, and applies the result to `cur`'s path:
    /// the edit store is keyed by the file being edited, so restoring into a
    /// different path would restore one image's edit onto another's picture.
    pub(crate) fn undo(&mut self, ctx: &Context) {
        let Some(p) = self.cur.clone() else { return };
        let restored = self.history.undo(&p.path, self.edits.get(&p.path));
        if let Some(e) = restored {
            self.edits.set(&p.path, e);
            self.after_edit_change(ctx);
        }
    }

    pub(crate) fn can_undo_current(&self) -> bool {
        self.cur.as_ref().is_some_and(|p| self.history.can_undo(&p.path))
    }

    pub(crate) fn can_redo_current(&self) -> bool {
        self.cur.as_ref().is_some_and(|p| self.history.can_redo(&p.path))
    }

    /// Redo the view transform that the last [`Self::undo`] walked back.
    pub(crate) fn redo(&mut self, ctx: &Context) {
        let Some(p) = self.cur.clone() else { return };
        let restored = self.history.redo(&p.path, self.edits.get(&p.path));
        if let Some(e) = restored {
            self.edits.set(&p.path, e);
            self.after_edit_change(ctx);
        }
    }

    /// The stored-space crop for a rectangle the user dragged on the **displayed**
    /// picture, snapped to the block grid — that is, exactly the rectangle the
    /// save will cut.
    ///
    /// The path a dragged rectangle takes, and each step is a place this could be
    /// wrong in a way that looks fine:
    ///
    ///  1. screen points -> the displayed image's own pixels, via `footprint`,
    ///     with the frame first held to `aspect` if one is asked for. Rounded
    ///     *outwards* (floor the near edges, ceil the far ones) so a one-pixel
    ///     slip while dragging never shaves the frame.
    ///  2. displayed pixels -> stored pixels, through the inverse of the full
    ///     display transform. `Crop::from_display_rect` does this, and it is the
    ///     step that transposes if you get the direction wrong.
    ///  3. stored pixels -> the block grid TurboJPEG needs.
    ///  4. and back out again, so the frame that is *drawn* is the frame that will
    ///     be saved rather than the raw drag.
    ///
    /// Steps 3 and 4 are what stop the UI lying. Without them the user frames one
    /// rectangle and gets another, up to a block different, and never finds out.
    ///
    /// `anchor` and `ptr` rather than a finished rect, because holding a shape
    /// needs to know which corner the drag started from: the constrained frame
    /// grows out of the press point, not out of the middle. A min/max rect has
    /// already thrown that away.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn crop_from_screen(
        &self,
        anchor: egui::Pos2,
        ptr: egui::Pos2,
        aspect: Option<f32>,
        footprint: egui::Rect,
        display_px: Vec2,
        pv: &Preview,
    ) -> Option<(image_viewer::Crop, (u32, u32, u32, u32))> {
        let edit = self.edits.get(&self.cur.as_ref()?.path);
        let display = image_viewer::display_xform(pv.exif_tag, edit.xform);
        let stored = stored_dims(pv);

        let sx = footprint.width() / display_px.x.max(1.0);
        let sy = footprint.height() / display_px.y.max(1.0);
        if sx <= 0.0 || sy <= 0.0 {
            return None;
        }
        let to_px = |p: egui::Pos2| -> (f32, f32) {
            ((p.x - footprint.min.x) / sx, (p.y - footprint.min.y) / sy)
        };
        // The shape is held in the image's own pixels rather than in screen
        // points, because that is the space the rectangle is finally expressed
        // in — and because a square in image pixels is a square on screen, the
        // footprint scaling the same on both axes.
        let (ax, ay) = to_px(anchor);
        let (cx, cy) = to_px(ptr);
        let (x0, y0, x1, y1) = image_viewer::constrain_to_aspect((ax, ay), (cx, cy), aspect);
        // Outwards, so the frame never loses a row to rounding.
        let rect = (
            x0.floor().max(0.0) as u32,
            y0.floor().max(0.0) as u32,
            (x1.ceil() - x0.floor()).max(0.0) as u32,
            (y1.ceil() - y0.floor()).max(0.0) as u32,
        );

        let asked = image_viewer::Crop::from_display_rect(pv.exif_tag, edit.xform, rect, stored)?;
        // The same snap `save_transformed` will do, done here so the drawn frame
        // is the saved one. Idempotent: doing it again changes nothing.
        let snapped = asked.snapped_to_mcu(pv.crop_grid.0, pv.crop_grid.1, stored.0, stored.1)?;
        Some((snapped, snapped.to_display_rect(display, stored)))
    }

    /// The crop rectangle for the shown image, in **screen** points, or `None`.
    pub(crate) fn crop_screen_rect(
        &self,
        footprint: egui::Rect,
        display_px: Vec2,
        pv: &Preview,
    ) -> Option<egui::Rect> {
        let crop = self.edits.crop(&self.shown.as_ref()?.path)?;
        let edit = self.edits.get(&self.shown.as_ref()?.path);
        let display = image_viewer::display_xform(pv.exif_tag, edit.xform);
        let stored = stored_dims(pv);
        let (dx, dy, dw, dh) = crop.to_display_rect(display, stored);
        let sx = footprint.width() / display_px.x.max(1.0);
        let sy = footprint.height() / display_px.y.max(1.0);
        Some(egui::Rect::from_min_size(
            footprint.min + Vec2::new(dx as f32 * sx, dy as f32 * sy),
            Vec2::new(dw as f32 * sx, dh as f32 * sy),
        ))
    }

    /// The ratio a crop frame is being dragged to, from the setting and Shift.
    ///
    /// Shift wins, and it means square. The alternative — Shift meaning "ignore the
    /// setting" — is the other common convention, and it is worse here: with the
    /// default setting being `Free` there would be nothing for Shift to override,
    /// and the shortcut people reach for would do nothing at all.
    pub(crate) fn crop_aspect(&self, ctx: &Context, display_px: Vec2) -> Option<f32> {
        // The image's own size, in whole pixels: what "the same shape as the
        // image" means, and a degenerate zero cannot be divided by.
        let dims = (display_px.x.max(1.0) as u32, display_px.y.max(1.0) as u32);
        if ctx.input(|i| i.modifiers.shift) {
            return image_viewer::CropAspect::Square.ratio(dims);
        }
        self.config.crop_aspect.ratio(dims)
    }

    /// Enter or leave crop mode. Leaving it throws the frame away.
    ///
    /// The frame is cleared rather than kept so that crop mode is a single,
    /// reversible gesture: press `C`, drag, press `C` and you are back to where you
    /// were. Keeping a stale frame that reappears next time you crop is how you end
    /// up saving a crop you framed for a different picture and forgot about.
    pub(crate) fn toggle_crop_mode(&mut self, ctx: &Context) {
        self.crop_mode = !self.crop_mode;
        if !self.crop_mode {
            if let Some(p) = self.cur.clone() {
                self.history.record(&p.path, self.edits.get(&p.path));
                self.edits.clear_crop(&p.path);
            }
        }
        ctx.request_repaint();
    }

    /// The transform of the image the user last asked for.
    pub(crate) fn current_xform(&self) -> Xform {
        self.cur.as_ref().map_or(Xform::Identity, |k| self.edits.xform(&k.path))
    }

    /// The transform of the image actually *on screen*.
    ///
    /// Deliberately keyed on `shown`, not `cur`. Those are different images
    /// whenever the wanted one's preview has not been adopted yet, and the draw
    /// path paints `shown`'s texture — so reading the transform from `cur` drew one
    /// picture under another picture's transform. Whichever way the two get out of
    /// step, that is a visible flicker: with `cur` ahead you see the stale image
    /// briefly turned, and with it behind you see the new one arrive unrotated and
    /// then jump.
    ///
    /// `current_xform` is still what the *save* uses, because saving is about
    /// `cur` — the image the user last asked for — even if it is not up yet.
    pub(crate) fn shown_xform(&self) -> Xform {
        self.shown.as_ref().map_or(Xform::Identity, |k| self.edits.xform(&k.path))
    }

    /// Per-frame housekeeping that must run before anything is drawn.
    /// Report (once) an image that has not reached the screen long after it was
    /// asked for. Without this, "the viewer hangs on some images" is
    /// undiagnosable from the outside: the state below is the whole story.
    pub(crate) fn check_for_stall(&mut self, _ctx: &Context) {
        const STALL_AFTER: Duration = Duration::from_secs(5);
        if self.lat.stall_logged {
            return;
        }
        // Only a request that has *not* produced pixels is a stall. Without
        // this check the watchdog fires on any request older than the
        // threshold — including images that were served instantly from cache.
        if self.lat.recorded_first {
            return;
        }
        let (Some(t), Some(cur)) = (self.lat.t_request, self.cur.as_ref()) else {
            return;
        };
        if t.elapsed() < STALL_AFTER {
            return;
        }
        self.lat.stall_logged = true;
        perf::count(perf::C_STALL, 1);
        let cached = self.previews.contains_key(cur);
        let poisoned = self.failed.contains(&(cur.clone(), Kind::Preview));
        eprintln!(
            "stall: {:?} not painted after {:?}\n  index={}/{} shown={:?} shown==cur={}\n  \
cached={} poisoned={} previews={} thumbs={} panics={}",
            cur.path.file_name().unwrap_or_default(),
            t.elapsed(),
            self.gallery.index,
            self.gallery.len(),
            self.shown.as_ref().and_then(|k| k.path.file_name()),
            self.shown == self.cur,
            cached,
            poisoned,
            self.previews.len(),
            self.thumbs.len(),
            perf::counter(perf::C_DECODE_PANICS),
        );
    }
}
