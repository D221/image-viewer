//! Module `app_pump`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    pub(crate) fn pump(&mut self, ctx: &Context) {
        let msgs: Vec<Msg> = self.rx.try_iter().collect();
        if msgs.is_empty() {
            return;
        }
        let mut resched = false;
        // The turn's second half as the plan that queued this batch's decodes saw
        // it, captured **before** any of the messages is absorbed: a preview's
        // `note_shapes` is the very call that can collapse the turn, so a query
        // made after absorbing would already be answering about the new turn and
        // would miss its own trigger. Captured here rather than kept in a field
        // because the pairing only changes inside this loop, and `schedule` runs
        // after it — so at this point the pairing is exactly what the last
        // `schedule` saw, with no second copy of it to fall out of step.
        let turn_partner = self.turn_partner();

        for m in msgs {
            match m {
                Msg::Preview { key, result } => match result {
                    Ok(p) => {
                        let name = key
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        let px = (p.image.size[0] as u32, p.image.size[1] as u32);
                        // Sampling is decided **at upload**, naming the two
                        // sampler axes rather than a zoom level, so the arrival
                        // moment does not freeze a choice that zooming later
                        // changes — see `image_viewer::pixel_art_sampling`.
                        let tex = ctx.load_texture(
                            name.as_ref(),
                            p.image,
                            content_texture_options(self.config.pixel_art),
                        );
                        perf::count(perf::C_TEXTURE_UPLOADS, 1);
                        perf::record(perf::JOB_PREVIEW_MS, p.load_ms * 1000);
                        if Some(&key) == self.cur.as_ref() {
                            self.lat.preview(Instant::now());
                        }
                        self.clock += 1;
                        // A decode that has already happened reports its own source
                        // dimensions, so feed them to the shape cache. The neighbours
                        // in the preload window are decoded anyway, so this is the
                        // cheapest source the manga pairing has.
                        //
                        // Keyed by the `(file, page)` key, because the cache judges a
                        // *page*: a document with a landscape spread among portrait
                        // pages can only say so if each page has its own entry, and
                        // crediting page 0's decode with page 5's dimensions would be
                        // the wrong page's shape. `note_shapes` never replaces an
                        // entry that is already there, so a page measured from its
                        // own header keeps that number.
                        self.gallery.note_shapes(key.clone(), p.orig);
                        self.previews.insert(
                            key.clone(),
                            Preview {
                                tex,
                                orig: p.orig,
                                load_ms: p.load_ms,
                                used: self.clock,
                                full: p.full,
                                anim: p.anim,
                                has_alpha: p.has_alpha,
                                target: p.target,
                                exif_tag: p.exif_tag,
                                crop_grid: p.crop_grid,
                                colour: p.colour,
                                hist: p.hist,
                                px,
                                tiles: HashMap::new(),
                            },
                        );
                        // A refine that lands clears the pending flag. If the
                        // display grew again while it was decoding, the next
                        // frame re-arms it, so a resize is never left served by
                        // a texture that is still soft.
                        if Some(&key) == self.cur.as_ref() {
                            self.resize.landed();
                        }
                        // The cursor page was the only trigger; a *partner's* arrival
                        // is one too, because it brings the measurement that can
                        // collapse the turn it was planned into — see
                        // `arrival_reschedules`.
                        resched |= image_viewer::arrival_reschedules(
                            &key,
                            self.cur.as_ref(),
                            turn_partner.as_ref(),
                        );
                        // "Wait for load" mode: arrived preview for the
                        // current image unblocks; if the key is still held,
                        // queue one more advance — consumed next frame so the
                        // image is guaranteed to have been painted on screen.
                        if Some(&key) == self.cur.as_ref() && self.waiting_load {
                            self.waiting_load = false;
                            if self.config.block_while_loading {
                                // **The held keys go through the same
                                // `nav_direction` as the press did.** This is the
                                // second half of the navigation rule and the half
                                // nobody writes: read the raw keys here and this site
                                // decides the direction a second time, in a second
                                // place, with no rule in sight — so under
                                // `manga_rtl` a held arrow steps the opposite way to
                                // the press that started the hold, and holding an
                                // arrow in a manga library bounces between two turns
                                // forever. `key_down` rather than `key_pressed`
                                // because the key is being held, and no mouse button
                                // because holding a side button is not a gesture this
                                // app has.
                                let held = ctx.input(|i| {
                                    nav_direction(
                                        |key, c, s, a| {
                                            i.key_down(key)
                                                && i.modifiers.ctrl == c
                                                && i.modifiers.shift == s
                                                && i.modifiers.alt == a
                                        },
                                        |_| false,
                                        self.config.manga_rtl,
                                    )
                                });
                                if let Some(dir) = held {
                                    self.pending_advance = Some(dir);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to load {}: {}", key, e);
                        // Stored as well as shown: navigating away clears the
                        // banner, and navigating back issues no new decode
                        // (the `failed` entry is permanent), so without this
                        // the revisit shows "Loading…" for a job that will
                        // never run. `goto` re-states it from here.
                        let msg = format!("Failed to load {}: {}", key, e);
                        self.load_errors.insert(key.clone(), msg.clone());
                        if Some(&key) == self.cur.as_ref() {
                            self.error = Some(msg);
                            self.waiting_load = false;
                        }
                        // Failed is per *image*, not per file: a document whose
                        // page 3 is an unsupported colour type must not poison its
                        // other 199 pages for the session. Counted before the
                        // insert for the same reason the message above it exists:
                        // a poisoned entry otherwise looks like nothing at all.
                        perf::count(perf::C_LOAD_FAILED, 1);
                        self.failed.insert((key, Kind::Preview));
                    }
                },
                Msg::Thumb { path, result } => match result {
                    Ok(p) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let texels = p.image.size;
                        // A grid cell keeps LINEAR whatever Pixel art is set
                        // to: a cell is 150 pt of a 256-texel image drawn at
                        // most upscale by the display factor, and the grid also
                        // browses photos — the toggle is for the viewer, not
                        // the picker (`content_texture_options`).
                        let tex = ctx.load_texture(name.as_ref(), p.image, TextureOptions::LINEAR);
                        perf::count(perf::C_TEXTURE_UPLOADS, 1);
                        perf::record(perf::JOB_THUMB_MS, p.load_ms * 1000);
                        // First thumbnail after opening the grid: that is the
                        // "gallery open -> something useful on screen" latency.
                        if let Some(t) = self.lat.t_gallery.take() {
                            perf::record_since(perf::GALLERY_TO_THUMBS, t);
                        }
                        self.clock += 1;
                        self.thumbs.insert(
                            path.clone(),
                            Thumb {
                                tex,
                                texels,
                                used: self.clock,
                            },
                        );
                        self.thumb_tries.remove(&path);
                        self.evict_thumbs();
                    }
                    Err(_) => {
                        // Count the attempt (done in pump, not at queue time, so
                        // a job wiped by schedule() costs nothing). After
                        // THUMB_TRIES the cell stops asking, so one corrupt
                        // file can't keep the worker pool spinning — while a
                        // transient failure still gets its retries.
                        let e = self.thumb_tries.entry(path).or_insert(0);
                        *e = e.saturating_add(1);
                        if self.thumb_tries.len() > 4096 {
                            self.thumb_tries.clear();
                        }
                    }
                },
                Msg::Full { key, result } => match result {
                    Ok(data) => {
                        perf::record(perf::JOB_FULL_MS, data.load_time.as_micros() as u64);
                        // Only keep it if the user is still on this image.
                        if Some(&key) == self.cur.as_ref() {
                            self.lat.full(Instant::now());
                            let load_ms = data.load_time.as_millis() as u64;
                            let exif_tag =
                                self.previews.get(&key).map_or(0, |pv| pv.exif_tag);
                            self.full = Some(FullImage {
                                path: key.path.clone(),
                                page: key.page,
                                data,
                                tiles: HashMap::new(),
                                load_ms,
                                exif_tag,
                            });
                        } else if let Some(pv) = self.previews.get_mut(&key) {
                            // **Keep it anyway**, on the page it belongs to.
                            //
                            // A `Kind::Full` decode is now planned for the turn's
                            // *other half* as well (`Plan::partner_needs_full`), and
                            // dropping that result is what made the defect the user
                            // reported permanent rather than slow: the partner's half
                            // of a zoomed spread stayed at preview resolution for the
                            // whole session, with no error and nothing to see. The
                            // plan asks again on every schedule, so a discarded
                            // partner decode was not one wasted decode - it was a
                            // re-decode of a 30 MB page every time the user moved.
                            //
                            // **On the preview, which is where a single-pass decode
                            // puts the same buffer**, so the two are indistinguishable
                            // afterwards: `needs_full` answers "no" (the buffer is
                            // there), `sync_shown` adopts it when the cursor arrives,
                            // and the LRU drops it with the page it belongs to. A
                            // decode for a page that is no longer on screen lands
                            // here too, and that is the same state a neighbour's
                            // single-pass decode has always left behind - so it is
                            // kept rather than thrown away, on purpose.
                            pv.full = Some(data);
                        }
                    }
                    Err(e) => {
                        eprintln!("Full-res load failed for {}: {}", key, e);
                        // The counter, not the message: a poisoned `failed`
                        // entry otherwise looks like nothing at all.
                        perf::count(perf::C_LOAD_FAILED, 1);
                        self.failed.insert((key, Kind::Full));
                    }
                },
                Msg::Exif { path, info } => {
                    if self.cur.as_ref().is_some_and(|c| c.path == path) {
                        self.exif_info = info;
                        self.exif_loading = false;
                    }
                }
                Msg::Rating { path, result } => {
                    // A stale answer is ignored: the reader moved on while the
                    // read was in flight, and the request for the new image is
                    // already on its way. A damaged sidecar is an `Err` and
                    // shows as unrated rather than an error banner on every
                    // visit — the image itself is untouched.
                    if self.cur.as_ref().is_some_and(|c| c.path == path) {
                        self.rating = result.unwrap_or_default();
                        self.rating_for = Some(path);
                    }
                }
                Msg::Wallpaper { order, result } => {
                    // **Stale answers are dropped, not shown.** A second press
                    // while the first is still flying is "the last action
                    // wins": the result stamped with the counter's *current*
                    // value is the only one worth displaying, and the superseded
                    // one's Ok(()) — true, the desktop did show it for a
                    // moment — is not that action any more. Its worker also
                    // aborts at its own checks before ever reaching the set, so
                    // a superseded result usually never arrives at all.
                    if order == self.wallpaper_order.load(std::sync::atomic::Ordering::SeqCst) {
                        match result {
                            Ok(()) => self.notice = Some("Set as wallpaper".into()),
                            Err(e) => self.error = Some(format!("Wallpaper failed: {e}")),
                        }
                    }
                }
                Msg::GridRating { path, result } => {
                    // A stale answer is dropped, not shown: an edit wrote a
                    // fresher value into the map and cleared the pending mark
                    // first, so an in-flight read from before the edit must
                    // not clobber it. A damaged sidecar is an `Err` and shows
                    // as unrated, like the pane.
                    if self.grid_rating_pending.remove(&path) {
                        self.grid_ratings.insert(path, result.unwrap_or_default());
                    }
                }
                Msg::DropCopy { src, result } => {
                    // **Stale answers are thrown away, not opened.** The reader
                    // dropped something else while this copy was running, and
                    // opening it now would put the older file on top of the newer
                    // one — with no error, and with the newer drop's copy
                    // directory deleted by the next `close_transients`.
                    if self.pending_drop.as_deref() != Some(src.as_path()) {
                        // Whoever is no longer waiting for it owns it, and
                        // `%TEMP%` is the place that has to be reclaimed — but
                        // **not if it is the copy the app has already opened.**
                        // That is reachable: two drops of the *same unchanged*
                        // file resolve to the same directory, so the second
                        // drop's answer can be handled first and this one's
                        // directory is the one on screen.
                        if let Ok(c) = result {
                            if Some(&c.file) != self.drop_copy.as_ref().map(|d| &d.file) {
                                image_viewer::forget_drop_copy(&c);
                            }
                        }
                        continue;
                    }
                    self.pending_drop = None;
                    match result {
                        Ok(copy) => {
                            self.notice = Some(if copy.reused {
                                format!(
                                    "Dropped file already copied to {} - the source may be temporary",
                                    copy.dir.display()
                                )
                            } else {
                                format!(
                                    "Dropped file copied to {} - the source may be temporary",
                                    copy.dir.display()
                                )
                            });
                            // **`false`, and that is the answer the copy earns.**
                            // The path is one this app owns, in a directory this
                            // app deletes, so nothing else is going to remove it:
                            // the transient hand-off ended when the copy was made.
                            self.open_file(ctx, copy.file.clone(), false);
                            // **After** `open_file`, because that call's
                            // `close_transients` would delete the very copy we
                            // went on to open. At this point the slot is empty —
                            // the request released the previous copy before it
                            // started this one — so this is a release, not a
                            // deletion of anything in use.
                            self.drop_copy = Some(copy);
                        }
                        Err(e) => {
                            // **A failed copy must not refuse to show the image the
                            // user actually dropped**: a full disk, or a source
                            // that vanished between the drop and here. The watcher
                            // will do what it does with the path, exactly as it
                            // did before any of this.
                            self.notice = Some(format!("Could not copy the dropped file ({e})"));
                            self.open_file(ctx, src, false);
                        }
                    }
                }
            }
        }
        if resched {
            self.schedule();
        }
        self.evict_previews();
    }

    pub(crate) fn evict_previews(&mut self) {
        while self.previews.len() > PREVIEW_CACHE_CAP {
            let protected = protected_preview_keys(
                &self.gallery.files,
                self.gallery.index,
                self.gallery.current_page(),
                self.shown.as_ref(),
                PRELOAD_RADIUS,
            );
            let victim = self
                .previews
                .iter()
                .filter(|(k, _)| !protected.contains(k))
                .min_by_key(|(_, v)| v.used)
                .map(|(k, _)| k.clone());
            match victim {
                Some(v) => {
                    self.previews.remove(&v);
                }
                // Nothing outside the protected set, so the cap cannot be honoured
                // and the cache is allowed to stay over it rather than evict the
                // image on screen. Bounded in practice: the protected set is at
                // most `2 * radius + 2` images.
                None => break,
            }
        }
    }

    /// Drop the least-recently-painted grid thumbnails once the cache is full.
    /// Cells re-request on demand, so eviction only costs a re-decode.
    pub(crate) fn evict_thumbs(&mut self) {
        while self.thumbs.len() > THUMB_CACHE_CAP {
            let victim = self
                .thumbs
                .iter()
                .min_by_key(|(_, v)| v.used)
                .map(|(p, _)| p.clone());
            match victim {
                Some(v) => {
                    self.thumbs.remove(&v);
                }
                None => break,
            }
        }
    }

    /// Switch what's on screen as soon as the requested image's preview exists.
    ///
    /// **Two things happen here, and the second is not inside the first.** `shown` is
    /// adopted when the wanted preview lands — it lags `cur` while that decodes, and
    /// this is the only place it catches up. `shown_partner` is then **re-derived every
    /// frame**, from where `shown` is, by [`Self::sync_shown_partner`].
    ///
    /// **The separation is the fix, and it is load-bearing in both directions.**
    ///
    /// *Deriving it after the gate* is what stops it being a latch. It used to be
    /// assigned inside `if previews.contains_key(&cur) && shown != cur`, which is false
    /// on every frame the cursor is standing still — and `M`, `O` and `X` all re-`goto`
    /// **without moving the cursor**. So all three changed what a page is and the screen
    /// kept the old answer: `→ → → M` in a folder of single pages halves the panel and
    /// lays out **one** page, so the reader gets a half-width picture beside an empty
    /// half, and only the next `→` produces a correct spread. `invalidate_path` had
    /// already found this same gate and worked around it for the one case it knew
    /// about; the workaround is gone now, because there is no latch to work around.
    ///
    /// *Deriving it from `shown`'s position rather than the cursor's* is what stops the
    /// two halves disagreeing. During the lag `shown` and `cur` are different images,
    /// and `turn_partner` read the cursor's turn while this frame painted `shown`'s.
    pub(crate) fn sync_shown(&mut self, ctx: &Context) {
        let Some(cur) = self.cur.clone() else {
            self.shown = None;
            self.shown_partner = None;
            self.anim = None;
            return;
        };
        if self.anim.as_ref().is_some_and(|a| a.path != cur.path) {
            self.anim = None;
        }
        // Page 1 and page 3 of one document are the same *file* but a different
        // picture, so the re-fit has to happen when the page changes and not only
        // when the file does. Comparing the key is what catches that; a path
        // comparison cannot, because the two have the same path.
        if self.previews.contains_key(&cur) && self.shown.as_ref() != Some(&cur) {
            self.shown = Some(cur.clone());
            // The zoom follows the view modes, in priority order: fit-window
            // owns it absolutely (its arrival reset), 1:1 pins it below,
            // fit-view fits it here. Anything else leaves the zoom alone —
            // with no mode on, photos open at 100% instead of maximized, and
            // an absolute zoom survives navigation rather than snapping back.
            if self.config.fit_window_mode {
                // Arrival pending handles it; nothing to do here.
            } else if self.config.fit_mode {
                self.view.fit = true;
            }
        }
        // **Outside the gate on purpose, and this is the assertion that keeps it
        // outside** (`tests/spreads_latches.rs`,
        // `the_turns_second_half_is_derived_not_latched`). One frame, unconditionally:
        // the mode toggle, the leading-lone nudge, `X`, a learned shape that collapses
        // the turn, a learned page count that moves its far edge, a re-export that
        // changes the pairing — every one of them is a change to what the turn *is*,
        // and re-deriving is what makes them take effect without a navigation.
        self.sync_shown_partner();
        // Adopt the full image that came with the preview's decode pass
        // (single-pass files), whether it just arrived or was preloaded
        // while the user was on a neighbouring image.
        if let Some(pv) = self.previews.get(&cur) {
            if let Some(data) = &pv.full {
                if self.full.as_ref().map_or(true, |f| !f.is(&cur)) {
                    let load_ms = data.load_time.as_millis() as u64;
                    self.full = Some(FullImage {
                        path: cur.path.clone(),
                        page: cur.page,
                        data: Arc::clone(data),
                        tiles: HashMap::new(),
                        load_ms,
                        exif_tag: pv.exif_tag,
                    });
                }
            }
        }
        // Adopt the animation that came with the preview's decode pass
        // (animated GIFs), including preloaded neighbours.
        if let Some(pv) = self.previews.get(&cur) {
            if let Some(anim) = &pv.anim {
                if self.anim.as_ref().map_or(true, |a| a.path != cur.path) {
                    let f0 = &anim.frames[0];
                    let image = ColorImage::from_rgba_unmultiplied(
                        [f0.width as usize, f0.height as usize],
                        &f0.rgba,
                    );
                    let tex = ctx.load_texture(
                        "anim",
                        image,
                        content_texture_options(self.config.pixel_art),
                    );
                    self.anim = Some(AnimView {
                        path: cur.path.clone(),
                        anim: Arc::clone(anim),
                        tex,
                        start: Instant::now(),
                        last_index: usize::MAX,
                        paused: None,
                    });
                }
            }
        }
        // `self.full` holds one page, and the picture on screen is `shown`. Dropping it
        // whenever those two name different pages is what keeps a document's tile
        // buffer from being page 0's while page 3 is on screen — `decode_page`
        // returns no full buffer for a page past the first, so without the page in
        // the comparison the two are the same file and the drop never fired.
        if let Some(s) = &self.shown {
            if self.full.as_ref().is_some_and(|f| !f.is(s)) {
                self.full = None;
            }
        }
    }

    // ---- file pickers -----------------------------------------------------

}
