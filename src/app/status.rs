//! Module `app_status`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// The turn's pages — **cursor first** — as `(key, stored dimensions, xform)`.
    ///
    /// **Dimensions, not textures, and that is the whole point of the split.**
    /// A layout read from the textures on screen would reflow the turn under the
    /// reader the moment the partner's decode landed, because the two textures
    /// have different sizes: the cursor's preview is already sized to the panel
    /// and the partner's may not be, or may be an older, sharper decode. The
    /// geometry therefore comes from the same measurement the pairing itself uses
    /// (the gallery's shape cache, falling back to `orig`), and the textures are
    /// only ever drawn *into* a rect that was decided before they arrived.
    ///
    /// **Each page carries its own `Xform`, and that is the load-bearing half of
    /// this function's signature.** A turn is a *set of pictures*, and each one
    /// carries its own edit — the user turned this page, not the turn. `edits` is
    /// keyed by path precisely so that turning one image must not turn the next,
    /// and reading the partner's transform off the cursor is that rule broken one
    /// page over: two different pictures, one transform.
    ///
    /// Which is not a cosmetic difference, because `spread_rects` sizes a page's
    /// slot by the dimensions it is handed and `paint_tex` rotates about
    /// `rect.center()`: lay a slot out from **stored** dims and paint a turned
    /// page into it and the rotated picture lands on the *swapped* rectangle
    /// centred on the same point, spilling `|w - h| * scale / 2` past the seam and
    /// over the partner's half. On a 1000x1500 page at the fit scale that is
    /// ~234 points of a turned page drawn on top of the facing page.
    ///
    /// **Page 0's xform is `shown_xform()`, and it is looked up here rather than
    /// passed in** so that one expression answers for every page: `self.edits
    /// .xform(&key.path)` is what `shown_xform` computes for `shown`, so the
    /// cursor page's answer is the same value by construction and not by a second
    /// spelling that could drift from it.
    ///
    /// **This is a lookup and never a measurement**, and that is what keeps it off
    /// the UI thread's critical path: nothing here opens a file. The dimensions
    /// have been put in the cache by the two callers that already read them —
    /// `goto`'s `learn_shapes` for the page being arrived at and the page after it,
    /// and `pump`'s `note_shapes` for any page whose decode has landed. The pairing
    /// itself consumes the same entries (`pairable` reads *both* pages' dimensions
    /// on every `schedule`), so the query that decides a turn is also the query
    /// that needs them, and the pairing's read-ahead is this layout's prefetch.
    ///
    /// "Already known" is not "guaranteed": a partner nothing has measured yet
    /// answers `None`, which is why the fallback below exists rather than a `?`.
    ///
    /// `cursor_dims` is passed in rather than looked up because the caller is
    /// already holding the cursor page's `orig`, and this runs per frame.
    ///
    /// **Cursor first, and that order is load-bearing**: `spread_rects` returns
    /// rects in *input* order, so index 0 is the cursor page's rect and every
    /// view-shaped path downstream (the crop frame, the tiles, `footprint`) is
    /// built from it.
    pub(crate) fn turn_pages(
        &self,
        shown: &ImgKey,
        cursor_dims: (u32, u32),
    ) -> Vec<(ImgKey, (u32, u32), Xform)> {
        let cursor_xform = self.edits.xform(&shown.path);
        let Some(partner) = self.shown_partner.as_ref() else {
            return vec![(shown.clone(), cursor_dims, cursor_xform)];
        };
        let pos = self.gallery.page_pos();
        // **`shown`'s position, not the cursor's.** During the lag between a navigation
        // and its decode landing these are different images on different turns, and
        // stepping from the cursor to look for the partner walked the wrong turn — the
        // `filter` below then failed and the layout fell back to the cursor page's
        // dimensions. With the pair derived from `shown` (`sync_shown_partner`) this
        // lookup is the one it was written to be.
        let pos = self.shown_pos().unwrap_or(pos);
        // `step_no_wrap` rather than the wrapping step, because a turn must not
        // straddle the wrap — the same rule `PageCtx::step_no_wrap` is built on and
        // the same one `turn_partner` steps over. The `key` comparison is what
        // makes this a lookup on the page `shown_partner` actually names rather
        // than an assumption that "the page after the cursor" is still it.
        let cached = self
            .gallery
            .step_no_wrap(pos, 1)
            .filter(|p| PageCtx::key(&self.gallery, *p).as_ref() == Some(partner))
            .and_then(|p| PageCtx::dims(&self.gallery, p));
        // A partner with no cached dimensions and no preview is a page nothing has
        // ever measured. The cursor page's own dimensions are the fallback, and
        // they are a *principled* guess rather than a shrug: `pairable` treats an
        // unmeasured page as pairing optimistically, which is exactly the claim
        // that it looks like its partner.
        //
        // **What the wrong fallback would cost, precisely.** `spread_fit` takes a
        // `min` over the pages, so a *too small* placeholder does not shrink the
        // scale — `half_w / 1.0` is large and the other page wins the `min`. What
        // it costs is the partner's own **rect**: `spread_rects` sizes a page by
        // the dimensions it is handed, so a 1x1 placeholder is a 1-point sliver at
        // the seam. That is the failure this line prevents, and not the one the
        // `min` would suggest.
        let dims =
            cached.or_else(|| self.previews.get(partner).map(|pv| pv.orig)).unwrap_or(cursor_dims);
        vec![
            (shown.clone(), cursor_dims, cursor_xform),
            (partner.clone(), dims, self.edits.xform(&partner.path)),
        ]
    }

    // ---- drawing ----------------------------------------------------------

    pub(crate) fn status_text(&self, ppp: f32) -> String {
        let idx = if self.gallery.is_empty() { 0 } else { self.gallery.index + 1 };
        let filename = self
            .shown
            .as_ref()
            .and_then(|k| k.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let location = match (&self.zip_archive, self.shown.as_ref()) {
            (Some(a), Some(k)) => format!(
                "{}!{}",
                a.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                k.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
            ),
            _ => format!("{}\\{}", self.folder.display(), filename),
        };
        let mut s = format!("{} - {}/{}", location, idx, self.gallery.len());
        // Standing on an archive says what it holds and what pressing → does.
        // **The cursor, not `shown`**: an archive has no picture on screen — the
        // previous image is still painted, deliberately — so a readout built
        // from `shown` would name a file the reader is not on.
        //
        // Read once here and nowhere else. The count is a header read on the UI
        // thread, which is why it is bounded to a header; putting it in the
        // per-frame path of every panel would be a way to make start-up slow.
        if let Some(cur) = self.gallery.current()
            && is_archive_path(cur)
        {
            match archive_image_count(cur) {
                // Spelling the key rather than drawing an arrow: egui's
                // default font subset has no `→` glyph and renders it as an
                // empty square, which reads as a missing character rather
                // than as a key.
                Some(n) => s.push_str(&format!(" - {n} images, press right to open")),
                None => s.push_str(" - unreadable container"),
            }
        }
        // "page 3/12" in the readout as well as the title, because the title bar is
        // not always visible and this is where the position already is.
        let pages = self.gallery.pages_in_current();
        if pages > 1
            && let Some(k) = &self.shown
        {
            s += &format!(" - page {}/{}", k.page + 1, pages);
        }
        if let Some((shown_key, pv)) =
            self.shown.as_ref().and_then(|k| self.previews.get(k).map(|pv| (k, pv)))
        {
            s += &format!(" - {}x{} - preview {} ms", pv.orig.0, pv.orig.1, pv.load_ms);
            // Show the dimensions as displayed once turned, so the readout
            // matches what is on screen.
            let shown_xf = self.shown_xform();
            if shown_xf != Xform::Identity {
                let (w, h) = xformed_footprint(pv.orig.0 as f32, pv.orig.1 as f32, shown_xf);
                s += &format!(" ({} {}x{})", shown_xf.label(), w as u32, h as u32);
            }
            // A crop is not shown by cropping the picture — the frame is drawn over
            // the whole image instead, so that turning or mirroring still works on
            // the full frame. The readout says what the save will produce, or the
            // user has no way to find out what they framed.
            if let Some(crop) = self.edits.crop(&shown_key.path) {
                s += &format!(" - crop {}", crop.label());
            }
            if let Some(anim) = &pv.anim {
                s += &format!(" - anim {} frames", anim.frames.len());
                // Paused frame, so stepping shows where it landed. Read off
                // the player, not the preview: the preview never pauses.
                if let Some(held) =
                    self.anim.as_ref().filter(|a| a.path == shown_key.path).and_then(|a| a.paused)
                {
                    s += &format!(" (paused {}/{})", held + 1, anim.frames.len());
                }
            } else {
                match &self.full {
                    Some(f) if self.shown.as_ref().is_some_and(|k| f.path == k.path) => {
                        s += &format!(" - full {} ms", f.load_ms)
                    }
                    _ => {
                        if let Some(sh) = &self.shown
                            && self.needs_full(sh)
                        {
                            s += " - full-res loading…";
                        }
                    }
                }
            }
        }
        // Zoom as a percentage of 1:1, so it means the same whatever the window
        // is doing. `view.scale` is points per source pixel; times `ppp` that
        // is physical pixels per source pixel, which is what the percent means.
        s += &format!(" - {}%", image_viewer::zoom_percent(self.view.scale, ppp));
        if self.slideshow {
            s += &format!(" - slideshow {}s", self.config.slideshow_secs);
        }
        if self.config.fit_window_mode {
            s += " - fit-window";
            // Live fit numbers: ratio, drawn, panel, window. When the window
            // disagrees with the picture these say which half is wrong.
            if let Some((r, dw, dh, pw, ph, iw, ih)) = self.fit_debug {
                s += &format!(" r{r:.2} {dw:.0}x{dh:.0}/{pw:.0}x{ph:.0} win{iw:.0}x{ih:.0}");
            }
        }
        if self.config.fit_mode {
            s += " - fit-view";
        }
        if let Some(e) = &self.error {
            s += &format!(" — {e}");
        } else if let Some(n) = &self.notice {
            s += &format!(" — {n}");
        }
        s
    }
}
