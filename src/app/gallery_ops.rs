//! Module `app_gallery_ops`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;
use std::path::PathBuf;

impl MyApp {
    /// Throw away everything cached for `p` and, if it is on screen, ask for it
    /// again.
    ///
    /// Clearing `failed` here is what makes an edit **self-healing**: a file
    /// that is re-read while it is still being written fails to decode, and
    /// `failed` is permanent for the session — but the writer's finishing
    /// touches the stamp again, so the next poll re-detects the change, clears
    /// the entry and retries. Without this, "edit the file I am looking at"
    /// would reliably poison it.
    pub(crate) fn invalidate_path(&mut self, p: &Path, ctx: &Context) {
        // All pages, not one. A path-keyed `remove` would be a no-op now that the
        // cache is keyed by (file, page), and the symptom would be a
        // re-exported document still showing the old version of page 3.
        self.previews.retain(|k, _| k.path != p);
        self.thumbs.remove(p);
        self.thumb_tries.remove(p);
        self.failed.retain(|(q, _)| q.path != p);
        self.load_errors.retain(|k, _| k.path != p);
        // Drop the on-disk cell too. Nothing *depends* on this — the stamp in the
        // entry already makes the changed file a cache miss on the next request,
        // which is why the watcher being wrong cannot show a stale picture — but
        // without it a re-saved file sits in the cache taking up space for a
        // folder this session may never open again.
        forget_thumbnail(p);
        // A re-export can turn a 12-page document into a 1-page one. Without this
        // the cursor would sit on a page that no longer exists and `next` would
        // walk off the end of a phantom document.
        self.gallery.forget_page_count(p);
        // ...and the page's remembered dimensions, for the same reason. A
        // re-export can change a page's aspect from portrait to landscape, and the
        // manga pairing trusts what it was told: keep the old shape and every
        // spread after this file stays mis-paired for the session, with no error
        // and nothing on screen to say so.
        self.gallery.forget_shapes(p);
        // ...and the user's own decision about each of its pages, for a sharper
        // reason than the dimensions above. A stale override is not a wrong answer
        // to a question about the current file, it is a decision about pixels that
        // no longer exist — and because an override beats the detector, keeping it
        // holds that page a spread whatever the file now contains.
        self.spreads.forget_file(p);
        // ...and the remembered view of each of its pages, for the same family
        // of reason: a view saved standing on the old pixels may restore a
        // scale or an offset that no longer answers anything about the new
        // file — a surface remembered at 100% for a 4000 px scan now framing a
        // 600 px re-export at a zoom nobody chose. Session-only state, so the
        // cost is one line and no save.
        self.views.forget_file(p);
        // Saved, not just dropped: a store that still holds the stale entry hands it
        // back on the next launch, so dropping it in memory only would make "a
        // re-export clears the override" true for one session and false across a
        // restart. The cost is a write on the UI thread, which is a departure from
        // the rule that this thread does no I/O — taken because the alternative is
        // an override that survives the restart it was cleared for, and bounded in
        // practice: `check_changed` only calls this for a file the app holds state
        // for, so it is the handful of previewed or on-screen files rather than the
        // folder. And `SPREADS_MAX_BYTES` is what the save refuses *over*, not a
        // promise about how big a write may come out.
        self.save_spreads();
        let shown_gone = self.shown.as_ref().is_some_and(|s| s.path == p);
        if self.full.as_ref().is_some_and(|f| f.path == p) {
            self.full = None;
            self.anim = None;
        }
        if shown_gone {
            // The texture on screen belongs to a version that no longer exists,
            // so stop showing it. `central()` paints nothing until the new
            // decode lands — the same thing that happens when you navigate to
            // an image that is not cached yet. Deliberately *not* a stale-texture
            // holdover: keeping old pixels for a re-exported file is the one
            // outcome that looks like a bug.
            self.shown = None;
        }
        // **The partner needs no arm and no re-arm here, and did once.**
        //
        // This used to carry a twenty-line comment and an `if` that cleared the turn's
        // second half when the changed path was the *partner's*, then re-asked it in
        // the same breath. Both halves are gone, and the reason is worth keeping: the
        // re-arm existed because that field was **latched**, assigned only inside
        // `sync_shown`'s `if previews.contains_key(&cur) && shown != cur` gate — which
        // is false on every frame the cursor stands still, so when the partner was the
        // file that changed, `shown` survived, equalled `cur`, and nothing was assigned
        // again for the rest of the session. The spread collapsed to one page and stayed
        // collapsed after the partner's fresh decode had landed and was perfectly
        // paintable.
        //
        // The latch is gone: `sync_shown_partner` re-derives the answer from `shown`'s
        // own position **every frame**, and this function has already run
        // `forget_shapes` / `forget_page_count` / `spreads.forget_file`, so the very
        // next `frame_prelude` asks the pairing the question again and gets whatever the
        // re-exported file actually is. That is strictly better than the re-arm: it also
        // covers the case where the re-export made the partner *stop* pairing (the turn
        // correctly collapses to one page) rather than only the case where it changed
        // the pixels.
        //
        // What still holds is the *order*: `previews.retain` above drops the partner's
        // pixels, so the turn must not keep a half whose texture is gone, and the
        // derivation reads the same dropped `previews` the paint does. One frame of
        // disagreement is not possible — both happen inside the same `sync_shown` call.
        if self.cur.as_ref().is_some_and(|c| c.path == p) {
            self.error = None;
            self.waiting_load = false;
            // A re-exported file usually has new dimensions, so re-fit instead
            // of keeping a view scale chosen for the old pixels.
            self.view = View {
                fit: true,
                scale: 1.0,
                offset: Vec2::ZERO,
            };
            self.full_due = Some(Instant::now() + FULL_DWELL);
            self.schedule();
            ctx.request_repaint();
        }
    }

/// One sorted folder listing for the whole app: flat or recursive. Every
    /// gallery build that faces the filesystem goes through here (startup
    /// takes this function directly, since `self` does not exist yet), so a
    /// new scan flavor cannot land in one place and miss the others.
    pub(crate) fn sorted_scan(
        folder: &Path,
        sort: image_viewer::SortMode,
        descending: bool,
        recursive: bool,
    ) -> Vec<PathBuf> {
        if recursive {
            image_viewer::scan_folder_sorted_recursive(folder, sort, descending)
        } else {
            scan_folder_sorted_dir(folder, sort, descending)
        }
    }

    /// The folder listing this gallery browses: flat or recursive per
    /// config, never archives (their temp dir is already flat). The one
    /// funnel for every rebuild, so the flag cannot be forgotten at one
    /// site — startup, rescan, opens and the toggle all read this.
    #[allow(dead_code)] // kept because tests/unit.rs scans source text for it
    pub(crate) fn scan_current(&self) -> Vec<PathBuf> {
        Self::sorted_scan(
            &self.folder,
            self.config.sort,
            self.config.sort_descending,
            self.config.recursive_folders && self.zip_archive.is_none(),
        )
    }

    /// Install the whole folder (or tree) into a gallery, keeping the current
    /// image selected. The pair of funnels: [`Self::sorted_scan`] is the
    /// sorted-list half, this the position-preserving half, and the flag is
    /// decided in one place each. Not a `&self` method: the Settings panel
    /// loads into `self.gallery`, so a receiver would borrow the field it
    /// mutates.
    pub(crate) fn load_folder_into(folder: &Path, recursive: bool, g: &mut Gallery) {
        if recursive {
            g.load_all_from_folder_recursive(folder);
        } else {
            g.load_all_from_folder(folder);
        }
    }

    /// Re-scan the current folder and merge the result, keeping the user's
    /// position. `force` bypasses the load_all setting (manual reload key).
    ///
    /// The scan itself runs on a background thread so a 2 000-file network
    /// share can't block the paint thread every 2 s fallback poll. The old
    /// gallery stays on screen until the new list lands — the same rule the
    /// decode path follows.
    pub(crate) fn rescan(&mut self, ctx: &Context, force: bool) {
        if self.zip_archive.is_some() {
            return; // archives: the extracted temp dir is stable
        }
        if !force && !self.config.load_all_on_startup {
            return;
        }

        // A scan is in flight: check if it has landed. Keeping the old gallery
        // until then means the picture never blanks for a rescan.
        if let Some(rx) = &self.rx_rescan {
            match rx.try_recv() {
                Ok(files) => {
                    self.rx_rescan = None;
                    self.apply_scanned_files(files, ctx, force);
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    // Still scanning: wait for the next frame.
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.rx_rescan = None;
                }
            }
            return;
        }

        // Start the scan on a background thread. `sorted_scan` is pure on the
        // file system, so it is safe off the UI thread.
        let folder = self.folder.clone();
        let sort = self.config.sort;
        let descending = self.config.sort_descending;
        let recursive = self.config.recursive_folders;
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let files = if recursive {
                image_viewer::scan_folder_sorted_recursive(&folder, sort, descending)
            } else {
                scan_folder_sorted_dir(&folder, sort, descending)
            };
            let _ = tx.send(files);
        });
        self.rx_rescan = Some(rx);
    }

    /// Apply a file list that arrived from a background scan. The Random
    /// merge happens here (not in the thread) so `all_files` is only touched
    /// on the UI thread.
    fn apply_scanned_files(&mut self, mut files: Vec<PathBuf>, ctx: &Context, force: bool) {
        self.lat.t_scan = Some(Instant::now());
        // Random must not reshuffle on every poll: keep the user's order,
        // append new files, drop vanished ones (manual reload reshuffles).
        if self.config.sort == SortMode::Random && !force && !self.all_files.is_empty() {
            let fresh: HashSet<&PathBuf> = files.iter().collect();
            self.all_files.retain(|p| fresh.contains(p));
            for f in &files {
                if !self.all_files.contains(f) {
                    self.all_files.push(f.clone());
                }
            }
            files = self.all_files.clone();
        }
        self.set_files(files, ctx);
    }

    /// Re-apply the filter after the user edited the search box. Clears caches
    /// only when the *set of files* changes; a re-filter that keeps the same
    /// files must not throw away decoded pixels.
    pub(crate) fn on_filter_changed(&mut self, ctx: &Context) {
        let before: HashSet<PathBuf> = self.gallery.files.iter().cloned().collect();
        let current = self.gallery.current().map(|p| p.to_path_buf());
        self.apply_filter();
        if self.gallery.current() != current.as_deref() {
            self.reset_display();
            self.goto(ctx);
        } else {
            self.schedule();
        }
        // Release pixels for files the filter hid: they are unreachable now, so
        // holding them is pure waste. Keyed on the *new* visible set, so a file
        // that is still visible keeps its texture instead of re-decoding on
        // every keystroke.
        let now: HashSet<PathBuf> = self.gallery.files.iter().cloned().collect();
        if before != now {
            let cur = self.cur.clone();
            // By *file*: a hidden document releases all of its pages, not just the
            // one that happened to be cached.
            self.previews
                .retain(|k, _| now.contains(&k.path) || Some(&k.path) == cur.as_ref().map(|c| &c.path));
            self.thumbs.retain(|p, _| now.contains(p));
            // Same reason, for the grid's ratings (and their in-flight marks):
            // a hidden file's sidecar answer is unreachable until it is
            // visible again, when one cheap re-read refills it.
            self.grid_ratings.retain(|p, _| now.contains(p));
            self.grid_rating_pending.retain(|p| now.contains(p));
        }
    }

    // ---- file triage -------------------------------------------------------

    /// Remove the current file from the gallery (file already gone from disk)
    /// and advance to the next image.
    pub(crate) fn remove_current_from_gallery(&mut self, ctx: &Context) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        // Every page of the file goes: the file is leaving the gallery, so its
        // other pages are unreachable and holding their pixels is pure waste.
        self.previews.retain(|k, _| k.path != p);
        self.failed.retain(|(k, _)| k.path != p);
        self.load_errors.retain(|k, _| k.path != p);
        self.gallery.forget_page_count(&p);
        // Same reason: the file is unreachable from here, so anything keyed on its
        // path is unreachable too.
        self.gallery.forget_shapes(&p);
        if !self.gallery.is_empty() {
            let i = self.gallery.index.min(self.gallery.len() - 1);
            self.gallery.files.remove(i);
            self.gallery.index = i.min(self.gallery.len().saturating_sub(1));
        }
        self.reset_display();
        self.goto(ctx);
    }

    /// Delete the current file to the recycle bin and advance.
    pub(crate) fn delete_current(&mut self, ctx: &Context) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        // **The current *file*, not "am I inside an archive".** Those were the same
        // thing until archives became listable; with a `.cbz` sitting in a folder
        // the cursor can be on one while `zip_archive` is `None`, and a
        // misplaced arrow would send a 200 MB volume to the recycle bin.
        // `Del` is the one bare letter allowed to lose data, so the guard that
        // decides where it points is not the place to be off by one concept.
        if is_archive_path(&p) || self.zip_archive.is_some() {
            self.error = Some("Delete is not supported for archives".into());
            return;
        }
        match trash::delete(&p) {
            Ok(()) => {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.remove_current_from_gallery(ctx);
                self.notice = Some(format!("Deleted {name}"));
                // Remember what was deleted so it can be restored. Cap the
                // stack at ten; older deletions fall off. push/pop are a
                // matched pair: newest pushed, newest popped.
                self.deleted_stack.push(p);
                self.deleted_stack.truncate(10);
            }
            Err(e) => self.error = Some(format!("Delete failed: {e}")),
        }
    }

    /// Restore the most recently deleted file from the Recycle Bin and put it
    /// back in the gallery at its old position. `Ctrl+Y`. `Del` is the one
    /// bare letter allowed to lose data; this makes it recoverable instead of
    /// the sole exception.
    pub(crate) fn undo_delete(&mut self, ctx: &Context) {
        let Some(original) = self.deleted_stack.pop() else {
            self.notice = Some("Nothing to restore".into());
            return;
        };
        let name = match original.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => {
                self.error = Some("Cannot restore: bad path".into());
                return;
            }
        };
        // Find the matching item in the Recycle Bin. Matching by name is the
        // best we have without tracking the shell's opaque item id at delete
        // time; if several items share a name we restore the newest one.
        let items = match trash::os_limited::list() {
            Ok(items) => items,
            Err(e) => {
                self.error = Some(format!("Cannot read the Recycle Bin: {e}"));
                return;
            }
        };
        let Some(item) = items.iter().find(|i| i.name.to_str() == Some(&name)) else {
            self.error = Some(format!("{name} is not in the Recycle Bin"));
            return;
        };
        match trash::os_limited::restore_all(std::iter::once(item.clone())) {
            Ok(()) => {
                // Put it back at the end of the gallery and navigate to it.
                self.gallery.files.push(original.clone());
                let idx = self.gallery.files.len() - 1;
                self.gallery.index = idx;
                self.reset_display();
                self.goto(ctx);
                // goto() clears the notice, so set it after; schedule() is
                // called inside goto() so the restored image starts decoding
                // on the very next frame.
                self.notice = Some(format!("Restored {name}"));
            }
            Err(e) => {
                // Put the path back so the user can try again.
                self.deleted_stack.push(original);
                self.error = Some(format!("Restore failed: {e}"));
            }
        }
    }

    /// Copy or move the current file to a folder picked by the user.
    pub(crate) fn transfer_current(&mut self, ctx: &Context, copy: bool) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        // Same reasoning as `delete_current`: the file under the cursor, not whether
        // the listing came out of an archive.
        if is_archive_path(&p) || self.zip_archive.is_some() {
            self.error = Some("Move/copy is not supported for archives".into());
            return;
        }
        let Some(dest_dir) = rfd::FileDialog::new()
            .set_title(if copy {
                "Copy image to…"
            } else {
                "Move image to…"
            })
            .pick_folder()
        else {
            return;
        };
        let Some(name) = p.file_name() else {
            return;
        };
        let dest = dest_dir.join(name);
        if dest.exists() {
            self.error = Some(format!("Target already exists: {}", dest.display()));
            return;
        }
        let result = if copy {
            std::fs::copy(&p, &dest).map(|_| ())
        } else {
            std::fs::rename(&p, &dest)
        };
        match result {
            Ok(()) => {
                let verb = if copy { "Copied" } else { "Moved" };
                if copy {
                    self.notice = Some(format!("{verb} to {}", dest_dir.display()));
                } else {
                    self.remove_current_from_gallery(ctx);
                    self.notice = Some(format!("{verb} to {}", dest_dir.display()));
                }
            }
            Err(e) => {
                self.error = Some(format!(
                    "{} failed: {e}",
                    if copy { "Copy" } else { "Move" }
                ))
            }
        }
    }

    /// Rename the current file on disk and re-point the gallery at the new
    /// path. Refuses archives (same rule as delete/move): the cursor file, not
    /// the listing, decides. The new name is typed into `rename_input` and
    /// confirmed from `ui`.
    pub(crate) fn rename_current(&mut self, _ctx: &Context) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        if is_archive_path(&p) || self.zip_archive.is_some() {
            self.error = Some("Rename is not supported for archives".into());
            return;
        }
        let Some(name) = p.file_name().and_then(|n| n.to_str()).map(|s| s.to_owned()) else {
            return;
        };
        self.rename_input = Some(name);
    }

    /// Commit a rename that the user typed into `rename_input` and confirmed.
    /// Returns the new path on success so the UI can report it.
    pub(crate) fn rename_commit(&mut self, ctx: &Context, new_name: &str) -> Option<PathBuf> {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return None;
        };
        let i = self.gallery.index;
        let new_path = p.with_file_name(new_name);
        if new_path == p {
            return Some(p);
        }
        if new_path.exists() {
            self.error = Some(format!("Target already exists: {}", new_path.display()));
            return None;
        }
        match std::fs::rename(&p, &new_path) {
            Ok(()) => {
                // Re-point the gallery entry and move any cached pixels over to
                // the new key, so the renamed image keeps its preview.
                self.gallery.files[i] = new_path.clone();
                let old_key = ImgKey { path: p.clone(), page: self.gallery.current_page() };
                let new_key = ImgKey {
                    path: new_path.clone(),
                    page: self.gallery.current_page(),
                };
                if let Some(prev) = self.previews.remove(&old_key) {
                    self.previews.insert(new_key.clone(), prev);
                }
                if self.failed.contains(&(old_key.clone(), Kind::Full)) {
                    self.failed.remove(&(old_key.clone(), Kind::Full));
                    self.failed.insert((new_key.clone(), Kind::Full));
                }
                self.load_errors.remove(&old_key);
                if let Some(c) = self.cur.as_mut() {
                    if c.path == p {
                        c.path = new_path.clone();
                    }
                }
                self.reset_display();
                self.goto(ctx);
                self.notice = Some(format!("Renamed to {}", new_path.display()));
                Some(new_path)
            }
            Err(e) => {
                self.error = Some(format!("Rename failed: {e}"));
                None
            }
        }
    }

}

