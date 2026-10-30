//! Module `app_nav`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// Install `files` as the gallery order, keeping the current image on
    /// screen: the cursor is re-pointed by path, not by index, so a permuted
    /// list does not jump to whatever now sits at the old slot.
    ///
    /// One funnel for both slideshow transitions (start shuffles, stop
    /// restores), because the re-point is the part that is easy to forget and
    /// a show that jumps on start or on stop is the symptom.
    pub(crate) fn set_gallery_order(&mut self, files: Vec<PathBuf>) {
        let current = self.gallery.current().map(|p| p.to_path_buf());
        self.gallery.files = files;
        if let Some(c) = current {
            if let Some(i) = self.gallery.files.iter().position(|p| p == &c) {
                self.gallery.index = i;
            }
        }
    }

    /// Begin a slideshow. With shuffle on, the folder's own order is saved
    /// and the gallery walks a shuffled copy instead — same set of files,
    /// fresh order every show. Stopping puts the saved order back, so this
    /// changes nothing outside a running show.
    pub(crate) fn start_slideshow(&mut self) {
        self.slideshow = true;
        self.slideshow_paused = false;
        self.slideshow_last = Instant::now();
        if self.config.slideshow_shuffle && self.slideshow_saved.is_none() {
            let saved = self.gallery.files.clone();
            let mut order = saved.clone();
            image_viewer::shuffle(&mut order);
            self.set_gallery_order(order);
            self.slideshow_saved = Some(saved);
        }
    }

    /// End a slideshow from either off-ramp (the toggle or the natural
    /// finish): a shuffled show puts the folder's own order back first.
    pub(crate) fn stop_slideshow(&mut self) {
        self.slideshow = false;
        self.slideshow_paused = false;
        if let Some(saved) = self.slideshow_saved.take() {
            self.set_gallery_order(saved);
        }
    }

    /// Pause or resume a running slideshow. Resuming restarts the interval so
    /// the pause never steals time from the next slide. No-op when the show
    /// is not running.
    pub(crate) fn toggle_slideshow_pause(&mut self) {
        if !self.slideshow {
            return;
        }
        self.slideshow_paused = !self.slideshow_paused;
        if !self.slideshow_paused {
            self.slideshow_last = Instant::now();
        }
    }

    /// The [`PageCtx`] the pairing is asked through: the gallery plus the
    /// reader's stored shape decisions.
    ///
    /// Built in one place because that is what keeps the overrides in the pairing
    /// everywhere it is asked for - navigation here, and the second half of a turn in
    /// [`Self::turn_partner`], which is also what the plan is handed as its
    /// `partner` - and a `Gallery` asked on its own answers `None` for
    /// `override_shape`. See [`AppPageCtx`].
    pub(crate) fn page_ctx(&self) -> AppPageCtx<'_> {
        AppPageCtx {
            gallery: &self.gallery,
            spreads: &self.spreads,
        }
    }

    /// Put the cursor on the anchor of the turn it is already on. Nothing unless
    /// manga mode is on, and nothing at all when it is already there.
    ///
    /// **This is the invariant the whole pairing rests on, enforced in one place.**
    /// `lone` is only correct at an anchor - it decides "is this page alone?" with a
    /// look at `anchor + 1`, which is wrong for a second half - so a cursor off an
    /// anchor means the turn painted beside the cursor, the `partner` the planner is
    /// given and the preload window are all one page right of the turn on screen.
    ///
    /// Called from [`Self::goto`], which every way into the app passes through, and
    /// **not** from the walks: `image_viewer::next_turn` and
    /// [`image_viewer::prev_turn`] snap defensively themselves, so a caller that
    /// navigates without a `goto` is still right. The two are deliberately
    /// independent rather than one calling the other - a `goto` snap would hide a
    /// broken walk behind a working snap, and the walk is the thing with the trap in
    /// it.
    pub(crate) fn snap_to_turn_anchor(&mut self) {
        if !self.config.manga_mode {
            return;
        }
        let here = self.gallery.page_pos();
        let at = image_viewer::anchor_of(here, self.spreads.lead_lone, &self.page_ctx());
        if at != here {
            self.gallery.goto_page(at);
        }
    }

    /// One turn forwards: the anchor the next turn opens on, and whether the cursor
    /// moved.
    ///
    /// Two lines and no rule. Which anchor the next turn opens on is
    /// [`image_viewer::next_turn`] - which snaps to the current turn's anchor first,
    /// wraps when `loop_back` is on, and refuses past the end with it off - and the
    /// move is [`Self::goto_turn`]. Both are library rules tested in
    /// `tests/spreads.rs`, which is what lets this method be two lines without being
    /// untested logic.
    pub(crate) fn turn_forward(&mut self) -> bool {
        let to = image_viewer::next_turn(self.gallery.page_pos(), self.spreads.lead_lone, &self.page_ctx());
        self.goto_turn(to)
    }

    /// One turn backwards: the anchor of the turn before this one, or the gallery's
    /// last turn when `loop_back` wraps, and whether the cursor moved.
    ///
    /// [`image_viewer::prev_turn`] rather than `prev_anchor`, for two reasons that
    /// are the same reason: it snaps to the current turn's anchor (so a cursor left on
    /// a second half cannot answer "there is no turn before me" about the wrong turn),
    /// and the wrap is the navigation layer's decision, which `prev_anchor`
    /// deliberately does not make. See that function's doc comment.
    pub(crate) fn turn_backward(&mut self) -> bool {
        let to = image_viewer::prev_turn(self.gallery.page_pos(), self.spreads.lead_lone, &self.page_ctx());
        self.goto_turn(to)
    }

    /// Move the cursor onto a turn anchor the walk produced, reporting whether there
    /// was one.
    ///
    /// **What the `bool` means: the walk produced a position, not that the view
    /// moved.** They differ in exactly one case - a one-page gallery with `loop_back`
    /// on, where the walk wraps to the position the cursor is already on - and this
    /// reports `true` there. That is [`Gallery::next`]'s and [`Gallery::prev`]'s
    /// behaviour too (both return `true` from a one-page looping gallery), so it is
    /// parity rather than a new quirk, and parity is the right answer here: the
    /// alternative would be a rule that holds for turns and not for pages.
    ///
    /// Separate from the two walks above so "which anchor" and "move the cursor"
    /// cannot drift apart, and so `None` has one spelling: a refusal changes nothing
    /// at all, which is what lets [`Self::step`] skip `goto` for it and leave the
    /// full-res dwell timer and the queue plan alone.
    pub(crate) fn goto_turn(&mut self, to: Option<PagePos>) -> bool {
        match to {
            Some(pos) => {
                self.gallery.goto_page(pos);
                true
            }
            None => false,
        }
    }

    /// `Home` / `End`: the first or last **turn** in manga mode, the first or last
    /// *file* otherwise.
    ///
    /// Manga mode asks the library for the anchor rather than assigning a file
    /// index, and the two ends are deliberately not spelled the same way:
    ///
    /// * **The first turn is `(0, 0)`, and that is arithmetic rather than a
    ///   special case.** `anchor_of` starts its walk there and returns the position
    ///   unchanged when it is asked about it, so the gallery's first page is its own
    ///   anchor and always was. `snap_to_turn_anchor` in `goto` would put the cursor
    ///   there even if this got it wrong, which is why `goto` is still called.
    /// * **The last turn is a walk, not a subtraction.** `last_anchor` reads forward
    ///   from the front, because which turn is last is a fact about the whole tiling
    ///   and nothing asked *at* the last page can answer it: `lone` says "alone" there
    ///   either way, and `prev_anchor` answers `None` at the front or on a second
    ///   half. The controller's original "walk `prev_anchor` back from the final
    ///   page" was found unimplementable for the same reason; `last_anchor` is the
    ///   rule that works, and it is the one tested in `tests/spreads.rs`.
    ///
    /// An empty gallery does nothing at all, which is the old `!is_empty()` guard
    /// moved in here so the two modes cannot disagree about it.
    pub(crate) fn goto_edge(&mut self, end: bool, ctx: &Context) {
        if self.gallery.is_empty() {
            return;
        }
        if self.config.manga_mode {
            let to = if end {
                last_anchor(self.spreads.lead_lone, &self.page_ctx())
            } else {
                Some(PagePos { index: 0, page: 0 })
            };
            if self.goto_turn(to) {
                self.goto(ctx);
            }
        } else {
            self.gallery.index = if end { self.gallery.len() - 1 } else { 0 };
            self.goto(ctx);
        }
    }

    /// `M`: turn manga mode on or off.
    ///
    /// **Re-`goto`s, because the mode changes what a page is.** With it off a step
    /// walks a page and with it on it walks a whole turn, and the cursor is only ever
    /// on an anchor in one of those - so turning the mode on while the cursor sits on
    /// the second half of what is now a pair would leave it in the middle of a turn,
    /// which is the state every pairing rule is written not to be asked from.
    /// `goto`'s own `snap_to_turn_anchor` is what actually fixes it; this calls it.
    ///
    /// View-only, which is what makes a bare letter safe here: one bool in
    /// `config.json` and a repaint.
    pub(crate) fn toggle_manga_mode(&mut self, ctx: &Context) {
        self.config.manga_mode = !self.config.manga_mode;
        self.config.save(&self.config_path);
        self.goto(ctx);
        ctx.request_repaint();
    }

    /// `D`: swap which side a turn opens on, and which way the arrows go.
    ///
    /// **No re-`goto`, deliberately, and the asymmetry with `M` is the point.** The
    /// turn's *membership* does not change - the same pages are on screen in the same
    /// pairs - only the order they are laid out in and the direction the keys move,
    /// both of which are read fresh every frame by `central` and by the `nav` tuple.
    /// Calling `goto` here would reset the full-res dwell timer and re-plan the
    /// loader queue for a picture that did not change.
    ///
    /// Also view-only: one bool in `config.json`.
    pub(crate) fn toggle_manga_rtl(&mut self, ctx: &Context) {
        self.config.manga_rtl = !self.config.manga_rtl;
        self.config.save(&self.config_path);
        ctx.request_repaint();
    }

    /// `Alt+Left`: go back to the previously viewed image. Pushes the current
    /// position onto the forward stack so `go_forward` can return here.
    ///
    /// Nothing happens at the back edge: the button and the chord are always
    /// enabled so the Settings panel can show them, but they are no-ops when
    /// the stack is empty.
    pub(crate) fn go_back(&mut self, ctx: &Context) {
        if let Some(entry) = self.history_back.pop() {
            // Push where we are now so forward can return here.
            if let Some(cur) = self.cur.clone() {
                self.history_forward.push(HistoryEntry {
                    folder: self.folder.clone(),
                    index: self.gallery.index,
                    page: cur.page,
                });
                if self.history_forward.len() > self.history_cap {
                    self.history_forward.remove(0);
                }
            }
            // Jump to the recorded position and re-enter goto, which will
            // re-snap to turn anchor and re-plan decodes.
            self.history_navigating = true;
            self.gallery.goto_page(PagePos {
                index: entry.index.min(self.gallery.files.len().saturating_sub(1)),
                page: entry.page,
            });
            self.folder = entry.folder.clone();
            self.goto(ctx);
        }
    }

    /// Record where the reader was before `goto` moves the cursor: normal
    /// navigation pushes the old position onto the back stack and clears the
    /// forward stack; back/forward navigation does neither.
    pub(crate) fn record_history(&mut self) {
        if !self.history_navigating {
            if let Some(cur) = self.cur.clone() {
                self.history_back.push(HistoryEntry {
                    folder: self.folder.clone(),
                    index: self.gallery.index,
                    page: cur.page,
                });
                if self.history_back.len() > self.history_cap {
                    self.history_back.remove(0);
                }
            }
            self.history_forward.clear();
        }
        self.history_navigating = false;
    }
    pub(crate) fn go_forward(&mut self, ctx: &Context) {
        if let Some(entry) = self.history_forward.pop() {
            if let Some(cur) = self.cur.clone() {
                self.history_back.push(HistoryEntry {
                    folder: self.folder.clone(),
                    index: self.gallery.index,
                    page: cur.page,
                });
                if self.history_back.len() > self.history_cap {
                    self.history_back.remove(0);
                }
            }
            self.history_navigating = true;
            self.gallery.goto_page(PagePos {
                index: entry.index.min(self.gallery.files.len().saturating_sub(1)),
                page: entry.page,
            });
            self.folder = entry.folder.clone();
            self.goto(ctx);
        }
    }

    /// `O` / `Shift+O`: step the leading-lone offset by `by`, clamped, and persist it.
    ///
    /// Writes only `spreads.json` under `%LOCALAPPDATA%` - **a file this app owns**,
    /// under a key naming this gallery - which is what makes a bare letter safe: there
    /// is nothing in it that is not a pairing decision, and a reader who dislikes the
    /// result deletes the file or presses `O` back. Clamping is
    /// `GallerySpreads::nudge_lead_lone`'s, so a `lead_lone` of 99 is not reachable
    /// from the keyboard and the notice reports the value that was actually landed on
    /// rather than the one that was asked for.
    ///
    /// **Re-`goto`s**, like `M`: the offset changes which pages open alone, so the
    /// tiling under the cursor can change, and `goto` is what re-learns the shapes
    /// and re-plans against them.
    pub(crate) fn nudge_lead_lone(&mut self, by: i32, ctx: &Context) {
        self.spreads.nudge_lead_lone(by);
        self.save_spreads();
        self.goto(ctx);
        self.notice = Some(format!(
            "{} pages open alone at the front",
            self.spreads.lead_lone
        ));
    }

    /// `X`: flip the cursor page between a page that takes a turn to itself and one
    /// that shares.
    ///
    /// The shape it starts from is **what the page is judged to be now**, which is an
    /// existing override if there is one and the aspect ratio otherwise -
    /// `image_viewer::shape_of` already answers exactly that question, asking the
    /// override first. So two presses flip and unflip, and `X` on a page the detector
    /// is confidently wrong about still works, which is the entire reason a manual
    /// override exists.
    ///
    /// Only the **cursor page**, never the turn: a wide scan in the middle of a pair
    /// splits that turn and pushes everything after it along, so the user sees the
    /// consequence of the override immediately rather than having to work out which
    /// page they hit.
    ///
    /// Writes only `spreads.json`, the same file `O` writes, and re-`goto`s for
    /// [`Self::nudge_lead_lone`]'s reason.
    pub(crate) fn toggle_cursor_shape(&mut self, ctx: &Context) {
        let Some(key) = self.cur.clone() else {
            self.notice = Some("No image is open".into());
            return;
        };
        let detected = image_viewer::shape_of(self.gallery.page_pos(), &self.page_ctx());
        let landed = self.spreads.toggle(&key.path, key.page, detected);
        self.save_spreads();
        self.goto(ctx);
        let page = key.page + 1;
        self.notice = Some(if landed == Shape::Double {
            format!("Page {page} now takes a whole turn to itself")
        } else {
            format!("Page {page} shares a turn with its neighbour")
        });
    }

    // ---- file list and filtering ------------------------------------------

    /// Rebuild `gallery.files` from `all_files` and the current filter. The
    /// filtering itself lives in [`Gallery::set_filtered`] so it is testable.
    pub(crate) fn apply_filter(&mut self) {
        self.gallery.set_filtered(&self.all_files, &self.filter);
    }

    /// Install a new **unfiltered** file list and re-apply the filter.
    ///
    /// This is the single place `gallery.files` is rebuilt from a whole-folder
    /// scan, so the filter cannot be forgotten on any path.
    pub(crate) fn set_files(&mut self, files: Vec<PathBuf>, ctx: &Context) {
        let current = self.gallery.current().map(|p| p.to_path_buf());
        self.all_files = files;
        self.apply_filter();
        if self.gallery.current() != current.as_deref() {
            self.reset_display();
            self.goto(ctx);
        } else {
            self.schedule();
        }
    }

    /// Adopt a freshly built gallery (folder / archive / single file), honouring
    /// any active filter and the looping setting.
    pub(crate) fn adopt_gallery(&mut self, g: Gallery, ctx: &Context) {
        self.gallery = g;
        self.gallery.loop_back = self.config.loop_back;
        self.all_files = self.gallery.files.clone();
        self.apply_filter();
        self.reset_display();
        // Loaded before `goto`, so whatever the navigation plans sees this gallery's
        // own overrides rather than the last folder's. `adopt_gallery` is the
        // single site every open goes through, which is why it is here and not in
        // each caller.
        self.refresh_spreads();
        self.goto(ctx);
    }

    /// Forget what is on screen (used when switching folders).
    pub(crate) fn reset_display(&mut self) {
        self.shown = None;
        self.shown_partner = None;
        self.full = None;
        self.anim = None;
    }

    /// Whether the current image still needs a full-resolution decode.
    ///
    /// Takes the *key*, not the path, because "is the full decode done" is a
    /// question about a page: page 3 of a document has its own full decode and its
    /// own failure, and asking about the file would answer for page 1. **The first
    /// line was the one that broke that**: it compared paths, so on a turn inside a
    /// document — every turn in a manga volume at the default `lead_lone`, where the
    /// partner is `(index, page + 1)` of the *same file* — it answered "done" from
    /// the cursor page's buffer, `plan_jobs` was handed `partner_needs_full == false`,
    /// no `Kind::Full` job was ever planned for the partner, and the second half of a
    /// zoomed spread stayed at preview resolution for the session while the expensive
    /// decode sat in the queue never being asked for.
    pub(crate) fn needs_full(&self, key: &ImgKey) -> bool {
        if self.full.as_ref().is_some_and(|f| f.is(key)) {
            return false;
        }
        if self.failed.contains(&(key.clone(), Kind::Full)) {
            return false;
        }
        let Some(pv) = self.previews.get(key) else {
            return false;
        };
        // The preview pass already decoded and kept the full image (adopted by
        // sync_shown) — nothing more to decode.
        if pv.full.is_some() {
            return false;
        }
        // Playing animations never get tiles: the texture advances over time.
        if pv.anim.is_some() {
            return false;
        }
        // Otherwise a full decode is wanted unless the source is too big to hold.
        (pv.orig.0 as u64 * pv.orig.1 as u64) <= MAX_FULL_PIXELS
    }

    pub(crate) fn schedule(&mut self) {
        // Preview texture target: the display area in physical pixels, so the
        // texture is exactly as sharp as the screen can show.
        //
        // **And no target at all until there is one.** `display_dim` is measured in
        // `central`, and the first `schedule()` of a session runs from `goto` in
        // `frame_prelude` — before `central` has ever drawn a frame. This used to
        // answer `PREVIEW_TEXTURE_DIM` for that gap, which is the animation ceiling
        // and not a decode request: **the current page, the turn's other half and
        // the entire preload window were then all decoded at 8192**, and for every
        // source under 8192 px `downscale_image` is a no-op, so those textures are
        // the *full-resolution* page (measured byte-identical to the source decode
        // on `32.jpg`) with no prefilter and no kernel pass anywhere in the
        // pipeline. The GPU then minifies them with a bilinear sampler and no
        // mipmaps: **3.397x the drawn rectangle in normal mode and 4.645x in manga
        // mode** at a 957x914 panel, against 1.045 and 1.365 for a texture decoded
        // for that panel. That is the reported lattice, and because whether a page
        // was decoded in that first window or a later one is a race with start-up,
        // it reads as "fine until I press right".
        //
        // `None` is a value, so this has to answer for it rather than invent a
        // number: an empty plan, re-made by `central` on the frame it finally
        // measures the display area. That costs one frame of "Loading..." — which
        // is what the first frame already was — and it removes seven full-resolution
        // decodes from every cold open (27 MB each on these pages, against 2 MB).
        let Some(target_dim) = image_viewer::decode_target(self.display_dim) else {
            self.loader.replace(Vec::new());
            return;
        };

        // The cache is keyed on (file, page), so this is the set of *images*
        // already decoded rather than of files. That is what lets the planner ask
        // "is page 3 cached" and get an honest answer instead of page 1's.
        let cached: HashSet<ImgKey> = self.previews.keys().cloned().collect();
        let i = self.gallery.index.min(self.gallery.files.len().saturating_sub(1));
        let cur = self.gallery.files.get(i).cloned();
        // The turn's second half, when there is one. `None` with manga mode off,
        // so every plan below is the one this function has always produced.
        let partner = self.turn_partner();
        // The preload budget spends **file lengths**, and the watcher's stamp
        // registry is where they already live: one lookup per file in the
        // plan, no stat on a path that runs per frame. A file the scanner has
        // not stamped yet (just appeared, or a fresh filter) costs `0` —
        // admitted — which is the fail-open reading: the budget is calibrated
        // to what the session has seen, not to a guess.
        let file_bytes: Vec<u64> = self
            .gallery
            .files
            .iter()
            .map(|p| self.stamps.get(p).map_or(0, |s| s.len))
            .collect();
        // The plan is computed by the library so it can be tested; all this does
        // is describe the current state to it.
        //
        // `partner_needs_full` asks the *partner's* `needs_full`, which is the
        // cursor's own question asked of a different key: `pv.full` is set by the
        // single-pass decode, so for most sources this is `false` and the partner's
        // full buffer is already in memory to cut tiles from. It is `true` for a
        // source over `FAST_FULL_MAX_PIXELS`, whose preview is DCT-scaled and
        // carries no full buffer — the common case for a manga scan.
        let jobs = match &cur {
            None => Vec::new(),
            Some(cur) => image_viewer::plan_jobs(&image_viewer::Plan {
                files: &self.gallery.files,
                index: i,
                current_page: self.gallery.current_page(),
                partner_needs_full: partner
                    .as_ref()
                    .is_some_and(|k| self.full_due.is_none() && self.needs_full(k)),
                partner,
                cached: &cached,
                failed: &self.failed,
                // Hidden panel: nobody can read the result, and the read is a
                // whole-file `fs::read` on a worker a decode is waiting for. The
                // panel asks for its own data when it is drawn, so this being
                // false costs nothing but the work it saves. Effective
                // visibility: the master *and* the left pane.
                wants_exif: image_viewer::wants_exif(
                    self.show_panels && self.show_left,
                    self.exif_loading,
                ),
                wants_full: self.full_due.is_none()
                    && self.needs_full(&ImgKey {
                        path: cur.clone(),
                        page: self.gallery.current_page(),
                    }),
                current_stale: self.resize.pending(),
                target_dim,
                radius: PRELOAD_RADIUS,
                file_bytes: &file_bytes,
                preload_budget: PRELOAD_BUDGET_BYTES,
            }),
        };
        self.loader.replace(jobs);
    }

    /// True when the image on screen has a cached preview that is softer than
    /// the display can now show, and a re-decode would actually be sharper.
    pub(crate) fn current_needs_refine(&self, target: u32) -> bool {
        let Some(pv) = self.cur.as_ref().and_then(|p| self.previews.get(p)) else {
            return false;
        };
        image_viewer::needs_refine(
            pv.px.0.max(pv.px.1),
            pv.orig.0.max(pv.orig.1),
            pv.target,
            target,
            image_viewer::RESIZE_PX_FLOOR,
        )
    }

    // ---- results from workers --------------------------------------------

}
