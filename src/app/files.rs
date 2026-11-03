//! Module `app_files`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// Stable temp dir for an archive; reused when the archive is unchanged.
    pub(crate) fn zip_temp_for(archive: &Path) -> PathBuf {
        use std::hash::{Hash, Hasher};
        let meta = std::fs::metadata(archive);
        let mtime = meta
            .as_ref()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        archive.hash(&mut h);
        mtime.hash(&mut h);
        size.hash(&mut h);
        std::env::temp_dir().join("image-viewer").join(format!("{:016x}", h.finish()))
    }

    /// Open a zip/cbz/7z/rar image archive: extract supported entries to a
    /// stable temp dir and treat it as a folder gallery.
    ///
    /// **The parent is remembered here, and only here.** An archive can only be
    /// entered from a folder — an archive holds no browsable archive — so this
    /// is entered at most once per session per depth, and `archive_parent` has
    /// exactly one writer. The record is taken *after* `close_transients`,
    /// which is the call that would otherwise clear it.
    pub(crate) fn open_archive(&mut self, ctx: &Context, archive: &PathBuf) {
        self.close_transients();
        let from_folder = self.folder.clone();
        let from_name = archive.clone();
        let temp = Self::zip_temp_for(archive);
        match extract_archive(archive, &temp) {
            Ok((count, skipped)) if count > 0 => {
                // Only a real folder earns a way back. Opening an archive from
                // the command line or a dialog has no parent to return to, and
                // `Esc` is the way out there.
                if self.zip_archive.is_none() && from_folder != temp {
                    self.archive_parent = Some((from_folder, from_name));
                }
                self.zip_archive = Some(archive.clone());
                self.zip_temp = Some(temp.clone());
                self.folder = temp.clone();
                self.gallery = Gallery::new(&temp);
                self.gallery.loop_back = self.config.loop_back;
                self.all_files = self.gallery.files.clone();
                self.apply_filter();
                // Resume the last-viewed member for this archive. Looked up in
                // the *filtered* list, so a resume can never select something
                // the user cannot see.
                let key = archive.to_string_lossy().to_string();
                if let Some(last) = self.config.last_by_folder.get(&key) {
                    let last = std::path::Path::new(last)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                    if let Some(name) = last {
                        if let Some(i) =
                            self.gallery.files.iter().position(|f| file_name_of(f) == *name)
                        {
                            self.gallery.index = i;
                        }
                    }
                }
                self.reset_display();
                // `zip_archive` was set above, so this keys the gallery by the
                // archive's own path and not by `self.folder`, which is the temp
                // directory `extract_archive` just wrote — a name that changes on
                // every re-extract, so keying by it would lose every override the
                // first time the archive was edited.
                self.refresh_spreads();
                self.goto(ctx);
                // `skipped` is not always encrypted: the rar reader cannot
                // decompress, so for it the skipped members are compressed ones.
                // The wording names what was dropped rather than guessing why.
                let n = if skipped > 0 {
                    format!("Archive: {count} images ({skipped} unreadable, skipped)")
                } else {
                    format!("Archive: {count} images")
                };
                // **Says how to get out**, because `←` working and nobody
                // knowing it is the same as it not working: the only way out of
                // an archive was the arrow the reader has no reason to try,
                // while Esc — the key every archive tool uses for "back" — closed
                // the app. A status line that names the exit is the cheapest fix
                // there is and it is not a substitute for Esc leaving, which is
                // handled in its own arm.
                self.notice = Some(format!(
                    "{n} — press Left or Esc to go back to {}",
                    file_name_of(&archive)
                ));
            }
            Ok((_, skipped)) => {
                let msg = if skipped > 0 {
                    format!("{skipped} entries could not be read")
                } else {
                    "No images inside the archive".to_string()
                };
                self.error = Some(msg);
            }
            Err(e) => self.error = Some(format!("Archive failed: {e}")),
        }
    }

    /// Is the cursor at either end of the open archive's listing?
    ///
    /// `zip_archive` is the only thing that says the current listing came out of
    /// a container — the same gallery type is used for a plain folder — so it is
    /// asked here rather than assumed.
    pub(crate) fn archive_edge(&self, forward: bool) -> bool {
        self.zip_archive.is_some()
            && image_viewer::leave_archive_at(
                forward,
                self.gallery.at_front(),
                self.gallery.at_back(),
                // When inside an archive and loop_back is on, don't leave
                // at the back edge on forward press — let gallery.next() wrap
                // within the archive instead.
                !(forward && self.gallery.at_back() && self.config.loop_back),
            )
    }

    /// Enter the archive the cursor has just landed on, if it is one.
    ///
    /// **Called from the places where a person asked for something**, not from
    /// `goto`. `goto` used to enter, and `goto` is also where opening a folder
    /// ends — so a folder whose first entry is a `.cbz` opened that cbz, and
    /// "Browse folder" opened something nobody chose. Now arriving at an archive
    /// *positions* the cursor on it, and opening it takes an arrow or a grid
    /// pick: the same distinction as an image, where opening a folder selects
    /// its first image rather than opening it.
    ///
    /// Returns whether it opened one, so the caller can report the press as a
    /// navigation rather than a no-op.
    pub(crate) fn enter_archive_if_present(&mut self, ctx: &Context) -> bool {
        let Some(cur) = self.gallery.current().map(|p| p.to_path_buf()) else {
            return false;
        };
        if !is_archive_path(&cur) {
            return false;
        }
        self.open_archive(ctx, &cur);
        true
    }

    /// Leave the open archive for the folder it was entered from, with the
    /// cursor back on the archive.
    ///
    /// `forward` is what the keypress was: `←` returns to the position the
    /// archive was opened from and stays there, while `→` *continues* — it moves
    /// on to the next entry in the folder, which is very often the next archive
    /// and so enters it. Without that second case a folder of archives reads as
    /// a folder of one, because `→` at an archive's last page had nowhere to go
    /// but back to its own first page.
    ///
    /// Returns false when there is no parent to go back to — an archive opened
    /// from the command line or a dialog has none, and the caller must fall
    /// through to the normal previous-image behaviour.
    ///
    /// **The folder is rescanned rather than restored from a saved list**, so a
    /// file added or deleted while the reader was inside is reflected, and the
    /// cursor is placed by *name* rather than by index: the saved index was
    /// taken before the reader may have sorted a newly arrived file ahead of
    /// it.
    pub(crate) fn leave_archive(&mut self, ctx: &Context, forward: bool) -> bool {
        let Some((folder, archive)) = self.archive_parent.take() else {
            return false;
        };
        self.close_transients();
        self.folder = folder.clone();
        let mut g = Gallery::new(&folder);
        if self.config.load_all_on_startup {
            Self::load_folder_into(
                &folder,
                self.config.recursive_folders && self.zip_archive.is_none(),
                &mut g,
            );
        }
        if let Some(i) = g.files.iter().position(|f| file_name_of(f) == file_name_of(&archive)) {
            g.index = i;
        }
        self.adopt_gallery(g, ctx);
        // **Says how to get out**, because `←` working and nobody
        // knowing it is the same as it not working. The status line that names
        // the exit is the cheapest fix there is, and it is not a substitute for
        // Esc leaving, which is handled in its own arm.
        self.notice = Some(format!(
            "Back in {} — press Left or Esc to go back to {}",
            folder
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| folder.to_string_lossy().into_owned()),
            file_name_of(&archive)
        ));

        // Continuing forwards: one step in the parent from the archive's own
        // position, which lands on the next entry — and `goto` enters it when
        // that is another archive, so walking a shelf of volumes is one press
        // per volume rather than an unreachable list.
        //
        // Asked of the parent's own `next`, so `loop_back` still decides what
        // "past the end" means: with it off, an archive that is the folder's
        // last stays put instead of dropping the reader into the folder by a
        // press with nowhere to land.
        if forward && self.gallery.next() {
            self.goto(ctx);
            // After moving to the next entry in the parent, enter it if it's
            // an archive. This mirrors what `step` does after `goto`.
            self.enter_archive_if_present(ctx);
        }
        true
    }

    /// Open one file. `transient` says the caller does **not** vouch for the
    /// source outliving the session, which is true of a drag-and-drop from
    /// inside an archive and of nothing else.
    ///
    /// It is a parameter rather than a second function because the two differ in
    /// exactly one step and a second function is where that step would be
    /// forgotten: there is no default, so a new caller has to answer.
    pub(crate) fn open_file(&mut self, ctx: &Context, file: PathBuf, transient: bool) {
        self.close_transients();
        if transient {
            // **After** `close_transients`, not before. That call releases the
            // previous drop copy, so a copy taken above it would be removed by the
            // very call that goes on to open it.
            //
            // And *requested*, not performed: this returns without opening
            // anything, and `pump` opens the copy when the worker reports in. That
            // is what keeps a 500 MB copy off the thread that paints.
            self.start_drop_copy(file);
            return;
        }
        self.folder = file.parent().unwrap_or(&file).to_path_buf();
        let mut g = Gallery::from_file(file);
        if self.config.load_all_on_startup {
            Self::load_folder_into(
                &self.folder,
                self.config.recursive_folders && self.zip_archive.is_none(),
                &mut g,
            );
        }
        self.adopt_gallery(g, ctx);
    }

    pub(crate) fn pick_folder(&mut self, ctx: &Context) {
        if let Some(folder) = rfd::FileDialog::new().set_title("Select image folder").pick_folder()
        {
            self.close_transients();
            self.config.folder = Some(folder.to_string_lossy().to_string());
            self.config.save(&self.config_path);
            self.open_folder(ctx, folder);
        }
    }

    pub(crate) fn pick_file(&mut self, ctx: &Context) {
        if let Some(file) = rfd::FileDialog::new()
            .set_title("Select image file")
            .add_filter(
                "Images",
                &["jpg", "jpeg", "png", "webp", "gif", "jxl", "avif", "heic", "heif", "cbz", "zip"],
            )
            .pick_file()
        {
            if is_archive_path(&file) {
                self.open_archive(ctx, &file);
            } else {
                // A dialog pick is a file the user chose off their own disk: not
                // transient, so no copy. Only a drop is.
                self.open_file(ctx, file, false);
            }
        }
    }

    pub(crate) fn load_dropped(&mut self, ctx: &Context, path: PathBuf) {
        if path.is_dir() {
            self.close_transients();
            self.open_folder(ctx, path);
            return;
        }
        if is_archive_path(&path) {
            self.open_archive(ctx, &path);
            return;
        }
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
        if image_viewer::SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            // **`true`**: Explorer's extraction of a zip member is a temp file it
            // deletes after the drop returns, and a drag out of *any* container
            // (7-Zip, WinRAR, a shell extension) has the same shape. Opening the
            // path in place means the 2 s poll finds a changed file - it is a
            // different stamp, because the file is gone - and blanks the picture
            // it is showing, which is the reported symptom.
            //
            // **`true` now means "copy it first", and the copy is requested
            // rather than performed**: `open_file` hands the work to a thread and
            // returns, so nothing is on screen until that thread reports. The
            // branch below is the only place the file reaches the app, which is
            // why it does not have a "the copy failed, open it anyway" path of
            // its own - `pump` owns that fallback.
            self.open_file(ctx, path, true);
        }
    }

    // ---- folder watcher ----------------------------------------------------
}
