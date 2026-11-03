//! Module `app_open`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    pub(crate) fn open_folder(&mut self, ctx: &Context, folder: PathBuf) {
        self.folder = folder.clone();
        let mut g = Gallery::new(&folder);
        if self.config.load_all_on_startup {
            Self::load_folder_into(
                &folder,
                self.config.recursive_folders && self.zip_archive.is_none(),
                &mut g,
            );
        }
        self.adopt_gallery(g, ctx);
    }

    /// Release everything the app is holding **on the app's own behalf**: the
    /// extracted temp dir of a previously open archive, and the copy of a file
    /// that was dropped on us.
    ///
    /// **One slot for both, and that is the point.** They are the same kind of
    /// thing - a file the user showed us without giving us a stable source for -
    /// and two slots would mean two cleanup sites, and the second one is the one
    /// that gets forgotten. One `Option` each, released together, means the
    /// lifetime is one function and there is no path that leaves a copy behind.
    ///
    /// **Order inside the drop path is load-bearing and is asserted by
    /// `a_drop_copies_its_source_after_releasing_the_previous_copy`**: a drop
    /// calls this and *then* takes its copy, so a copy made before this call
    /// would be deleted by the same call that opens it - a picture that never
    /// appears, with no error anywhere.
    pub(crate) fn close_transients(&mut self) {
        // **A copy in flight is one of the things being let go of**, so it is
        // cancelled here rather than at each of the six sites: the thread cannot
        // be stopped, so the drop does not happen, and when its answer arrives
        // `pump` recognises it as stale and releases the directory it wrote.
        // Without this, dropping a file and then opening a folder would show the
        // *folder* and then have the drop arrive on top of it, because the copy
        // no longer finishes before the next action does.
        self.pending_drop = None;
        if let Some(t) = self.zip_temp.take() {
            let _ = std::fs::remove_dir_all(t);
        }
        self.zip_archive = None;
        if let Some(c) = self.drop_copy.take() {
            image_viewer::forget_drop_copy(&c);
        }
    }

    /// Ask a worker to take a copy of a dropped file.
    ///
    /// **The copy is a read and a write of a file whose size the app does not
    /// know, so it cannot happen on the thread that paints.** That is the first
    /// line of `AGENTS.md`, and the synchronous form this replaced cost 3.8 ms
    /// at 5 MB, 26 ms at 50 MB and 279 ms at 500 MB on this box — the last of
    /// which is eighteen frames at 60 Hz, and is linear in a size nothing in the
    /// app can predict.
    ///
    /// **The picture on screen does not change while the copy runs, and that is
    /// deliberate.** The alternative — blanking the central panel for the length
    /// of a 500 MB copy — replaces a stall the user might not notice with a
    /// flicker they certainly would, on an interaction as ordinary as dropping a
    /// file. The pixels already in memory are correct (they were decoded from
    /// the source, not re-read), and the previous image's *copy* is the one thing
    /// the flight does invalidate — `close_transients` released it before this
    /// call, which is the ordering `open_file` is asserted on — so a zoom tile
    /// or a save aimed at the *old* image during the flight fails, for a path
    /// the drop is abandoning anyway.
    ///
    /// What the reader does get is the status line, because **a drop is
    /// otherwise invisible**: nothing changes on screen until the copy lands, and
    /// without a line a dropped file looks like a dropped keystroke.
    ///
    /// A thread rather than a loader job, and `drop_tx` says why at length: the
    /// loader's queue is the plan's queue, and `replace` makes it match the plan
    /// exactly, so a queued copy would be thrown away by the next `schedule()`
    /// and the drop would silently never open.
    pub(crate) fn start_drop_copy(&mut self, src: PathBuf) {
        // Supersede any copy already in flight. The older one still finishes and
        // still reports, and `pump` recognises it as stale by this comparison —
        // which is also what releases the directory it wrote.
        self.pending_drop = Some(src.clone());
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| src.to_string_lossy().into_owned());
        self.notice = Some(format!("Copying {name}…"));
        let tx = self.drop_tx.clone();
        let wake = self.drop_wake.clone();
        std::thread::spawn(move || {
            let result = image_viewer::own_dropped(&src);
            // The same shape as the loader: report, then ask for a repaint, so a
            // copy that lands with nothing else happening still shows up.
            let _ = tx.send(Msg::DropCopy { src, result });
            wake.request_repaint();
        });
    }

    /// Read the sidecar rating for the cursor image, off the UI thread.
    ///
    /// **Its own thread rather than the loader's queue**, for the same reason as
    /// `start_drop_copy` above: a rating is not a decode, so it is not in
    /// `plan_jobs`' answer, and `Loader::replace` makes the queue match the plan
    /// *exactly* — a queued rating job would be discarded by the next
    /// `schedule()` and the pane would stay on "Loading rating…". A 300-byte
    /// sidecar read is sub-millisecond, but it is still filesystem I/O in a
    /// paint frame, so it goes through the same worker+message shape as every
    /// other read rather than blocking.
    ///
    /// Separate from the `wants_exif` gate on purpose: that gate exists to stop
    /// a 7-12 ms whole-file read when the panel is hidden, and joining this
    /// read to it would mean the rating vanishes from the UI whenever the pane
    /// does. Called from `goto`, which is also what clears `rating_for`.
    pub(crate) fn request_rating(&mut self) {
        let Some(path) = self.cur.as_ref().map(|k| k.path().to_path_buf()) else {
            return;
        };
        // Archives get no ratings: there is nowhere to write
        // `vol1.cbz/p003.jpg.xmp`, and a rating that disappears on leaving the
        // archive is worse than a refusal.
        if is_archive_path(&path) {
            return;
        }
        self.rating_for = None;
        // The drop channel carries this too: it is just a `Sender<Msg>` plus a
        // repaint handle, and a second channel for the same thing would be a
        // second name for one mechanism.
        let tx = self.drop_tx.clone();
        let wake = self.drop_wake.clone();
        std::thread::spawn(move || {
            let result = image_viewer::Ratings::read(&path);
            let _ = tx.send(Msg::Rating { path, result });
            wake.request_repaint();
        });
    }

    /// Add `delta` to the cursor image's star rating (clamped 0..=5), write the
    /// sidecar, and re-read so the pane shows what is on disk. A write failure
    /// lands in `self.error` with the message: it must be visible, never
    /// silent.
    pub(crate) fn set_stars(&mut self, ctx: &Context, delta: i32) {
        let Some(path) = self.cur.as_ref().map(|k| k.path().to_path_buf()) else {
            return;
        };
        if is_archive_path(&path) {
            self.error = Some(
                "Cannot rate an archive member: ratings need a sidecar beside the file".into(),
            );
            return;
        }
        let mut rating = self.rating.clone();
        rating.rating.stars = (rating.rating.stars as i32 + delta).clamp(0, 5) as u8;
        if let Err(e) = rating.write(&path) {
            self.error = Some(format!("Failed to write rating for {}: {e}", path.display()));
            return;
        }
        self.rating = rating;
        // The grid holds this file's rating too: hand it what is on disk, and
        // clear the pending mark so an in-flight read from before the edit
        // lands stale and is dropped rather than clobbering this.
        self.grid_ratings.insert(path.clone(), self.rating.clone());
        self.grid_rating_pending.remove(&path);
        self.request_rating();
        ctx.request_repaint();
    }

    /// Flip the cursor image's pick flag, then the same write + re-read as
    /// `set_stars`.
    pub(crate) fn toggle_flag(&mut self, ctx: &Context) {
        let Some(path) = self.cur.as_ref().map(|k| k.path().to_path_buf()) else {
            return;
        };
        if is_archive_path(&path) {
            self.error = Some(
                "Cannot rate an archive member: ratings need a sidecar beside the file".into(),
            );
            return;
        }
        let mut rating = self.rating.clone();
        rating.rating.flag = !rating.rating.flag;
        if let Err(e) = rating.write(&path) {
            self.error = Some(format!("Failed to write rating for {}: {e}", path.display()));
            return;
        }
        self.rating = rating;
        // Same freshness as `set_stars` above: the grid's copy is this write,
        // not whatever an older in-flight read says.
        self.grid_ratings.insert(path.clone(), self.rating.clone());
        self.grid_rating_pending.remove(&path);
        self.request_rating();
        ctx.request_repaint();
    }

    /// Set the cursor image's caption, write the sidecar, and re-read so the
    /// pane shows what is on disk. Same write + re-read shape as `set_stars`.
    pub(crate) fn set_caption(&mut self, ctx: &Context, caption: String) {
        let Some(path) = self.cur.as_ref().map(|k| k.path().to_path_buf()) else {
            return;
        };
        if is_archive_path(&path) {
            self.error =
                Some("Cannot caption an archive member: sidecars need a file beside it".into());
            return;
        }
        let mut rating = self.rating.clone();
        rating.caption = caption;
        if let Err(e) = rating.write(&path) {
            self.error = Some(format!("Failed to write caption for {}: {e}", path.display()));
            return;
        }
        self.rating = rating;
        self.grid_ratings.insert(path.clone(), self.rating.clone());
        self.grid_rating_pending.remove(&path);
        self.request_rating();
        ctx.request_repaint();
    }

    /// Set the cursor image's artist, write the sidecar, and re-read. Same
    /// shape as `set_caption`.
    pub(crate) fn set_artist(&mut self, ctx: &Context, artist: String) {
        let Some(path) = self.cur.as_ref().map(|k| k.path().to_path_buf()) else {
            return;
        };
        if is_archive_path(&path) {
            self.error =
                Some("Cannot credit an archive member: sidecars need a file beside it".into());
            return;
        }
        let mut rating = self.rating.clone();
        rating.artist = artist;
        if let Err(e) = rating.write(&path) {
            self.error = Some(format!("Failed to write artist for {}: {e}", path.display()));
            return;
        }
        self.rating = rating;
        self.grid_ratings.insert(path.clone(), self.rating.clone());
        self.grid_rating_pending.remove(&path);
        self.request_rating();
        ctx.request_repaint();
    }
}
