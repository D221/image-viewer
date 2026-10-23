//! Module `app_watch`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// Keep the OS folder watch pointed at what is being browsed, and feed its
    /// events to the change machinery.
    ///
    /// Two sources, one rule each:
    /// - **With a watch running, events are the truth.** No timer, no stat
    ///   pass, and no idle repaint — the watcher thread wakes the app when it
    ///   has news, which is why a static photo now costs no frames at all
    ///   (the 2 s poll used to be its only wake source, ~2 fps).
    /// - **With no watch** — an archive's temp dir, or a share that refused
    ///   one — the 2 s poll runs exactly as it always did.
    ///
    /// Which applies is derived every frame by `sync_watcher`, so folder
    /// opens, the recursive toggle and opening an archive need no bookkeeping
    /// of their own: a value latched at one of those events and read later is
    /// how every wrong-screen bug in this file has happened.
    pub(crate) fn maybe_rescan(&mut self, ctx: &Context) {
        self.sync_watcher(ctx);
        // A watch that stopped delivering (a share that refused the read while
        // the handle stayed open) reports nothing forever: drop it and let the
        // poll take over, or the viewer silently stops noticing changes.
        if self.watcher.as_ref().is_some_and(|w| !w.alive()) {
            perf::count(perf::C_WATCHER_FALLBACK, 1);
            self.watcher = None;
            self.pending_content.clear();
            self.next_scan = Instant::now();
        }
        if self.watcher.is_some() {
            self.drain_watch_events(ctx);
            self.flush_content_changes(ctx);
            if self.pending_rescan {
                let now = Instant::now();
                if now >= self.next_rescan_allowed {
                    self.pending_rescan = false;
                    self.next_rescan_allowed = now + RESCAN_MIN_GAP;
                    self.rescan(ctx, false);
                } else {
                    // Owed but throttled, and the writer may have just
                    // stopped — there is no further event to wake us, so ask
                    // for the frame the rescan is due.
                    ctx.request_repaint_after(self.next_rescan_allowed - now);
                }
            }
            return;
        }
        let now = Instant::now();
        if now >= self.next_scan {
            self.next_scan = now + RESCAN_INTERVAL;
            // Change detection is *not* gated on `load_all_on_startup`: that
            // setting is about listing files at startup, but noticing that the
            // file you are looking at just got re-exported should not depend on
            // it.
            self.check_changed(ctx);
            self.rescan(ctx, false);
        }
        ctx.request_repaint_after(self.next_scan.saturating_duration_since(Instant::now()));
    }

    /// Start, restart or stop the folder watch so it matches what is on
    /// screen.
    ///
    /// The wanted key is `(folder, recursive)`, and `None` for an archive.
    /// Archives are excluded for the reason `check_changed` early-returns on
    /// them: the extracted temp dir is rewritten on every open and deleted on
    /// close (`AGENTS.md`), so watching it would report the app's own work.
    pub(crate) fn sync_watcher(&mut self, ctx: &Context) {
        let want: Option<(PathBuf, bool)> = if self.zip_archive.is_some() {
            None
        } else {
            Some((self.folder.clone(), self.config.recursive_folders))
        };
        if self.watcher_key.as_ref() == want.as_ref() {
            return;
        }
        // Dropping the old watch is what cancels its blocking call; see
        // `DirWatcher`'s `Drop`. So a folder switch costs one cancellation,
        // not a leaked thread and handle.
        self.watcher = None;
        self.watcher_key = want.clone();
        // A change owed for the folder being left must not be flushed against
        // the folder being entered.
        self.pending_content.clear();
        let Some((dir, recursive)) = want else {
            return;
        };
        let wake_ctx = ctx.clone();
        match image_viewer::DirWatcher::start(dir, recursive, move || wake_ctx.request_repaint()) {
            Ok(w) => {
                // A fresh watch knows nothing, and `stamps` may be empty or
                // about a different folder: one full comparison now gives the
                // first event a baseline. It is the pass the first 2 s poll
                // would have done, moved to the moment the folder opens.
                self.stamps.clear();
                self.next_scan = Instant::now() + RESCAN_INTERVAL;
                self.watcher = Some(w);
                self.check_changed(ctx);
            }
            Err(_) => {
                // No watch: the poll is the fallback. The counter is why a
                // folder that is still polled can be explained instead of
                // looking like the feature simply not working.
                perf::count(perf::C_WATCHER_FALLBACK, 1);
                self.next_scan = Instant::now();
            }
        }
    }

    /// Act on the events the OS sent since the last frame.
    ///
    /// Per-event work here is a **map insert**; the `stat` and the re-decode
    /// happen in `flush_content_changes` once the writer has gone quiet.
    /// Anything about the *listing* is coalesced into one rescan — one
    /// directory walk for a whole bulk copy, where acting per event would be
    /// one walk per file.
    pub(crate) fn drain_watch_events(&mut self, ctx: &Context) {
        // Scoped so the immutable borrow of `watcher` ends before anything
        // below wants `&mut self`.
        let (batches, fresh) = {
            let Some(w) = &self.watcher else {
                return;
            };
            let total = w.events();
            let fresh = total.saturating_sub(self.watch_events_seen);
            let mut b: Vec<image_viewer::FsChange> = Vec::new();
            while let Some(batch) = w.try_recv() {
                b.extend(batch);
            }
            (b, fresh)
        };
        // Count what arrived, not what is acted on: a probe has to be able to
        // tell a silent OS from an ignored event.
        if fresh > 0 {
            perf::count(perf::C_WATCH_EVENTS, fresh);
            self.watch_events_seen += fresh;
        }
        if batches.is_empty() {
            return;
        }
        let mut overflow = false;
        {
            // The **unfiltered** listing, so a file hidden by the filter still
            // counts as known: its save is a content change, not an excuse to
            // rescan the folder. Scoped because the full comparison below
            // needs `&mut self`.
            let known: HashSet<&PathBuf> = self.all_files.iter().collect();
            let now = Instant::now();
            for ev in image_viewer::coalesce_changes(batches) {
                let p = self.folder.join(&ev.name);
                let image_name = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase())
                    .is_some_and(|e| image_viewer::SUPPORTED_EXTENSIONS.contains(&e.as_str()));
                match image_viewer::classify_change(ev.action, known.contains(&p), image_name) {
                    // Refreshing the deadline is the debounce: a save that is
                    // still being written keeps pushing it, and the decode
                    // happens once, on the finished file.
                    image_viewer::ChangeKind::Content => {
                        self.pending_content.insert(p, now);
                    }
                    image_viewer::ChangeKind::Membership => self.pending_rescan = true,
                    image_viewer::ChangeKind::Overflow => {
                        // The OS dropped names, so nothing else knows what
                        // changed: do the full pass the timer used to do. This
                        // is the one case where a watch has to pay a poll's
                        // work, and it is the alternative to silently missing a
                        // change.
                        overflow = true;
                        self.pending_rescan = true;
                    }
                    image_viewer::ChangeKind::Ignore => {}
                }
            }
        }
        if overflow {
            self.check_changed(ctx);
        }
    }

    /// Re-decode the files whose writer has gone quiet.
    ///
    /// **The settle is not politeness, it is the difference between one decode
    /// and a storm**: a chunked save emits an event per write, and decoding on
    /// the first one decodes a half-written file, which the next chunk throws
    /// away. The stamp is read *now*, not when the event arrived, so what is
    /// decoded is the finished bytes.
    pub(crate) fn flush_content_changes(&mut self, ctx: &Context) {
        if self.pending_content.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut due: Vec<PathBuf> = Vec::new();
        self.pending_content.retain(|p, last| {
            if now.duration_since(*last) >= CONTENT_SETTLE {
                due.push(p.clone());
                false
            } else {
                true
            }
        });
        due.sort();
        for p in due {
            let stamp = Stamp::of(&p);
            // **No baseline counts as changed.** An event is the OS saying the
            // bytes moved; the alternative is to miss the first re-export of
            // the session, which is exactly the one the old first poll (2 s
            // after open) would have caught.
            let moved = self.stamps.get(&p).is_none_or(|old| *old != stamp);
            self.stamps.insert(p.clone(), stamp);
            if moved && self.has_cached_state_for(&p) {
                // One per reload the machinery actually acts on, so a probe
                // that mutates a file under the running viewer can assert the
                // watcher both saw and served the change - see
                // tests/probe_watcher.ps1.
                perf::count(perf::C_WATCHED_CHANGES, 1);
                self.invalidate_path(&p, ctx);
            }
        }
        if let Some(earliest) = self.pending_content.values().min() {
            ctx.request_repaint_after(
                (*earliest + CONTENT_SETTLE).saturating_duration_since(Instant::now()),
            );
        }
    }

    /// Re-stat every file and invalidate anything whose contents changed since
    /// the last poll.
    ///
    /// **The fallback path**, used only while no OS watch is running. The
    /// per-event path in `drain_watch_events` does the same comparison for one
    /// named file.
    ///
    /// Only files we hold state for are worth reloading, so the common case
    /// (a big folder, one image on screen) costs a metadata call per file and
    /// no decoding at all.
    pub(crate) fn check_changed(&mut self, ctx: &Context) {
        if self.zip_archive.is_some() {
            return; // archives: the extracted temp dir is stable
        }
        let mut fresh: HashMap<PathBuf, Stamp> =
            HashMap::with_capacity(self.gallery.files.len());
        let mut changed: Vec<PathBuf> = Vec::new();
        for p in &self.gallery.files {
            let now = Stamp::of(p);
            let moved = self.stamps.get(p).is_some_and(|old| *old != now);
            fresh.insert(p.clone(), now);
            if moved && self.has_cached_state_for(p) {
                // One per reload the machinery actually acts on, so a probe
                // that mutates a file under the running viewer can assert the
                // watcher both saw and served the change — see
                // tests/probe_watcher.ps1.
                perf::count(perf::C_WATCHED_CHANGES, 1);
                changed.push(p.clone());
            }
        }
        self.stamps = fresh;
        if changed.is_empty() {
            return;
        }
        for p in changed {
            self.invalidate_path(&p, ctx);
        }
    }

    /// Do we hold anything that a content change would make stale?
    pub(crate) fn has_cached_state_for(&self, p: &Path) -> bool {
        // Every page of the file, not just page 0: a re-exported document makes
        // all of its pages stale, and `any` over the keys is the only way to ask
        // that when the cache is keyed by (file, page).
        self.previews.keys().any(|k| k.path == p)
            || self.thumbs.contains_key(p)
            || self.full.as_ref().is_some_and(|f| f.path == p)
            || self.cur.as_ref().is_some_and(|c| c.path == p)
            || self.failed.iter().any(|(q, _)| q.path == p)
    }

}
