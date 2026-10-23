//! Module `app_output`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {

    /// Run the nth user command (Ctrl+1..9, Tier 1 #2) against the file on
    /// screen: `{file}`, `{dir}` and `{name}` expand in the program and args
    /// ([`image_viewer::expand_user_command`], tested), then the process is
    /// spawned detached — the viewer never waits on it and never reads its
    /// output, so a slow editor cannot stall the paint thread. Spawns go
    /// through `spawn_guard`, so even a repeating chord is one window a
    /// second rather than one per frame. A missing entry
    /// is a notice rather than an error: pressing Ctrl+3 with two commands is
    /// a mistyped chord, not a failure.
    pub(crate) fn run_user_command(&mut self, n: usize) {        let Some(cmd) = self.config.user_commands.get(n).cloned() else {
            self.notice = Some(format!("No user command {}", n + 1));
            return;
        };
        let Some(path) = self
            .shown
            .as_ref()
            .map(|k| k.path.clone())
            .or_else(|| self.cur.as_ref().map(|k| k.path.clone()))
        else {
            self.notice = Some("No image is open".into());
            return;
        };
        if cmd.program.trim().is_empty() {
            self.error = Some(format!("User command '{}' has no program", cmd.name));
            return;
        }
        if !self.spawn_guard("User command") {
            return;
        }
        let (prog, args) = image_viewer::expand_user_command(&cmd, &path);
        match std::process::Command::new(&prog)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => {
                self.notice = Some(format!(
                    "Ran {}",
                    if cmd.name.trim().is_empty() {
                        prog
                    } else {
                        cmd.name
                    }
                ));
            }
            Err(e) => self.error = Some(format!("Could not run '{}': {e}", cmd.name)),
        }
    }

    /// Set the current file's modification date: now, or its EXIF
    /// shooting date when the camera wrote one. A missing EXIF date is an
    /// error naming the file, not a silent touch to epoch.
    pub(crate) fn touch_current(&mut self, to_exif: bool) {
        let Some(p) = self.gallery.current() else {
            self.notice = Some("No image is open".into());
            return;
        };
        let to = if to_exif {
            let raw: Option<String> = nom_exif::read_exif(&p)
                .ok()
                .and_then(|ex| ex.get(nom_exif::ExifTag::DateTimeOriginal).map(|e| e.to_string()));
            match raw.and_then(|s| image_viewer::parse_exif_datetime(&s)) {
                Some(t) => t,
                None => {
                    self.error = Some(format!(
                        "No EXIF date in {}",
                        p.file_name().unwrap_or_default().to_string_lossy()
                    ));
                    return;
                }
            }
        } else {
            std::time::SystemTime::now()
        };
        match image_viewer::touch_file_mtime(&p, to) {
            Ok(()) => {
                self.notice = Some(format!(
                    "Touched {}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
            Err(e) => self.error = Some(format!("Touch failed: {e}")),
        }
    }

    /// Set every visible file's date to its EXIF shooting date. Files
    /// without one are counted, not failed — a folder of screenshots has no
    /// EXIF dates at all, and that is ordinary, not an error per file.
    pub(crate) fn touch_folder_to_exif_dates(&mut self) {
        // Touched through the filtered gallery (what you see), and cloned up
        // front: touching changes mtimes the watcher stamps, so iterating a
        // borrowed list while the poll below may rescan is asking for trouble.
        let files: Vec<std::path::PathBuf> = self.gallery.files.clone();
        if files.is_empty() {
            self.notice = Some("No images to touch".into());
            return;
        }
        let (mut ok, mut skipped) = (0, 0);
        for p in &files {
            let raw: Option<String> = nom_exif::read_exif(p)
                .ok()
                .and_then(|ex| ex.get(nom_exif::ExifTag::DateTimeOriginal).map(|e| e.to_string()));
            match raw.and_then(|s| image_viewer::parse_exif_datetime(&s)) {
                Some(t) => match image_viewer::touch_file_mtime(p, t) {
                    Ok(()) => ok += 1,
                    Err(_) => skipped += 1,
                },
                None => skipped += 1,
            }
        }
        self.notice = Some(if skipped == 0 {
            format!("Touched {ok} files")
        } else {
            format!("Touched {ok} files, {skipped} without EXIF dates")
        });
    }

    /// Copy the current file's path as text (Ctrl+Shift+C).
    ///
    /// Shares `Event::Copy` with the image copy: egui-winit's
    /// `is_copy_command` is `modifiers.command && key == C` with no shift
    /// exclusion, and it returns before pushing the key — so Ctrl+Shift+C
    /// arrives as the same event as Ctrl+C, and the arm reads the shift
    /// state to tell them apart. Text goes through `ctx.copy_text`, and a
    /// notice names what went, so a Shift held by accident reads as what it
    /// did rather than as a failed image copy.
    pub(crate) fn copy_path_to_clipboard(&mut self, ctx: &Context) {
        if let Some(p) = self.gallery.current() {
            let s = p.to_string_lossy().into_owned();
            ctx.copy_text(s.clone());
            self.notice = Some(format!("Copied path: {s}"));
        }
    }

    /// Copy the current image (full-res when in memory) to the clipboard.
    /// Write a new file beside the current one with the on-screen transform baked
    /// in (Ctrl+S).
    ///
    /// Always a *new* file: the original is never modified, so the action is
    /// reversible and saving twice accumulates rather than compounding. The
    /// view-only transform stays a view transform — this is the one path that
    /// touches the disk, and it does so only when asked.
    /// Ask the launch loop to rebuild the window with a different backend.
    ///
    /// Saves first, so the choice survives even if the new backend turns out to
    /// crash — the config is the way back. `None` clears the probe cache, so
    /// switching back to Automatic re-probes rather than trusting an answer
    /// obtained before the user had an opinion.
    pub(crate) fn request_renderer_restart(&mut self, ctx: &Context, backend: image_viewer::WgpuBackend) {
        if backend == self.renderer && self.config.renderer.choice != image_viewer::WgpuBackend::Auto
        {
            return;
        }
        self.config.renderer.choice = backend;
        self.config.renderer.probed = None;
        let _ = self.config.save(&self.config_path);
        if let Ok(mut slot) = self.restart.lock() {
            *slot = Some(backend);
        }
        self.notice = Some(format!("Switching to {}…", backend.label()));
        // Closing the viewport is what returns control to the launch loop;
        // there is no other way for the app to ask for a new event loop.
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    /// Run the backend probe and report what it found, without changing
    /// anything. Useful when Automatic picked something surprising.
    pub(crate) fn probe_renderer_now(&mut self, ctx: &Context) {
        self.notice = Some("Probing graphics backends…".into());
        ctx.request_repaint();
        let results = probe_backends();
        let mut lines = Vec::new();
        for (b, ok) in &results {
            lines.push(format!("{}: {}", b.name(), if *ok { "works" } else { "unavailable" }));
        }
        // Reuse the launch-time decision so there is one implementation of
        // "which one do we pick".
        let picked = image_viewer::pick_probed(&results);
        self.notice = Some(format!("{} — would use {}", lines.join(", "), picked.name()));
    }

    pub(crate) fn save_transformed_copy(&mut self) {
        let Some(cur) = self.cur.clone() else {
            return;
        };let p = cur.path.clone();
        if self.anim.as_ref().is_some_and(|a| a.path == p) {
            self.error = Some("Cannot save a transformed animation: it has many frames".into());
            return;
        }
        // "Save what I see" is a statement about a *file*, and a page of a
        // multi-page document is not a file. The lossless JPEG path rewrites the
        // whole file's coefficients and the re-encode fallback starts from page 1,
        // so saving while looking at page 3 would write page 1's pixels under the
        // name of the document and call it a lossless save of what you were
        // looking at. Refusing is the only honest answer until the save path
        // learns to carry a page.
        if cur.page > 0 {
            self.error = Some(format!(
                "Cannot save a single page of a multi-page document ({}/{}). \
                 Saving works on the whole file.",
                cur.page + 1,
                self.gallery.pages_in_current()
            ));
            return;
        }
        let edit = self.edits.get(&p);
        match image_viewer::save_transformed(&p, edit.xform, edit.crop) {
            Ok(dest) => {
                let name = dest
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.notice = Some(if edit.is_identity() {
                    format!("Saved {name}")
                } else {
                    format!("Saved {name} ({} , lossless)", edit.label())
                });
            }
            Err(e) => self.error = Some(format!("Save failed: {e}")),
        }
    }

    /// Write the same edit `save_transformed_copy` would, downscaled to the
    /// measured display size (Ctrl+Shift+S) instead of full-res.
    ///
    /// Same guards — animations and document pages are refused for the same
    /// reasons — and the notice names "re-encoded" for JPEG, because a
    /// screen-size JPEG is a decode and a fresh encode, not moved
    /// coefficients. With no measured panel yet there is no screen size to
    /// save at, so that is an error rather than a guess.
    pub(crate) fn save_scaled_copy(&mut self) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let p = cur.path.clone();
        if self.anim.as_ref().is_some_and(|a| a.path == p) {
            self.error = Some("Cannot save a transformed animation: it has many frames".into());
            return;
        }
        if cur.page > 0 {
            self.error = Some(format!(
                "Cannot save a single page of a multi-page document ({}/{}). \
                 Saving works on the whole file.",
                cur.page + 1,
                self.gallery.pages_in_current()
            ));
            return;
        }
        let Some((dw, dh)) = self.display_dim else {
            self.error = Some("No display size measured yet — try again in a moment".into());
            return;
        };
        let edit = self.edits.get(&p);
        match image_viewer::save_transformed_scaled(&p, edit.xform, edit.crop, dw.max(dh)) {
            Ok(out) => {
                let name = out
                    .dest
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                self.notice = Some(if out.lossless {
                    format!("Saved {name} ({}x{}, lossless)", out.width, out.height)
                } else {
                    format!("Saved {name} ({}x{}, re-encoded)", out.width, out.height)
                });
            }
            Err(e) => self.error = Some(format!("Save failed: {e}")),
        }
    }

    /// Put the image on the clipboard **as it is framed**: the same crop and the
    /// same turn `Ctrl+S` would write, as pixels.
    ///
    /// The edit is looked up by the path being copied, for the same reason the
    /// save looks it up by the path being saved: a crop and a turn belong to the
    /// *document*, and `ImageEdits` is the one store for both. Reading it off the
    /// current view instead would make the clipboard a statement about whatever
    /// was on screen rather than about this file.
    ///
    /// Which buffer the pixels come from differs by path, and each one is in a
    /// different coordinate space — see [`image_viewer::region_for_save`], which is
    /// where the space arithmetic lives and is tested.
    pub(crate) fn copy_image_to_clipboard(&mut self, unedited: bool) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let p = cur.path.clone();
        // **A page of a multi-page document is refused, and the refusal is the
        // feature.** `Ctrl+S` refuses it because the lossless path rewrites the whole
        // file and the fallback starts at page 1, so a save would write page 1's pixels
        // under the document's name. That reason is about *saving*; the clipboard has no
        // such write, and the fast path below now compares the page as well as the file,
        // so it hands over the right pixels. What is still not a thing this app can do
        // is put **a page's** pixels on the clipboard and call it "the image as it is
        // framed": the crop and the turn live in `ImageEdits`, which is keyed by *path*
        // because they are an edit to the document — so a frame drawn on page 1 would be
        // applied to page 3's picture, and the result is a valid picture of the wrong
        // part of the scan, which is the one outcome with no error anywhere. Refusing
        // until the save/clipboard path carries a page is the honest answer, and it is
        // the same answer `Ctrl+S` already gives on the same keypress family.
        if cur.page > 0 {
            self.error = Some(format!(
                "Cannot copy a single page of a multi-page document ({}/{}). \
                 The clipboard copies a whole file, and the crop and turn are the \
                 document's.",
                cur.page + 1,
                self.gallery.pages_in_current()
            ));
            return;
        }
        let edit = self.edits.get(&p);
        // Unedited ("copy original"): the stored picture EXIF-oriented, no
        // crop, no turn — the same buffers through `region_for_save` with an
        // identity edit, so this shares every source below instead of
        // re-decoding its own.
        let (xform, crop, edited) = if unedited {
            (Xform::Identity, None, false)
        } else {
            (edit.xform, edit.crop, !edit.is_identity())
        };

        // Fast path: full-res pixels already in memory, EXIF orientation applied.
        // **The key, not the path** — see `FullImage::is`. With the page missing this
        // filter was true for every page of a document, so `Ctrl+C` on page 2 pasted
        // page 1's pixels with no error and no counter.
        if let Some(f) = self.full.as_ref().filter(|f| f.is(&cur)) {
            match image_viewer::region_for_save(&f.data, f.exif_tag, xform, crop) {
                Some(out) => match set_clipboard_rgba(out.width, out.height, out.rgba) {
                    Ok(()) => {
                        self.notice = Some(if unedited {
                            format!("Copied {}x{} original to clipboard", out.width, out.height)
                        } else {
                            copied_notice(out.width, out.height, &edit)
                        })
                    }
                    Err(e) => self.error = Some(format!("Clipboard failed: {e}")),
                },
                None => self.error = Some("The crop does not fit this image".into()),
            }
            return;
        }
        // Animation: copy the frame currently on screen.
        //
        // The tag is **0**, and that is a fact rather than a shrug: animation
        // frames are handed over in stored pixels — the animation path reports
        // `exif_tag` but never calls `apply_exif_orientation` — so the frame and
        // the crop are already in the same space and applying the tag here would
        // transpose the frame a second time. Passing 0 is also exactly what the
        // save does to a GIF, because `exif_orientation` finds no tag in one.
        if let Some(av) = self.anim.as_ref().filter(|a| a.path == p) {
            let idx = av.last_index.min(av.anim.frames.len().saturating_sub(1));
            match image_viewer::region_for_save(&av.anim.frames[idx], 0, xform, crop) {
                Some(out) => {
                    let msg = if edited {
                        copied_notice(out.width, out.height, &edit)
                    } else {
                        format!(
                            "Copied {}x{} animation frame to clipboard",
                            out.width, out.height
                        )
                    };
                    match set_clipboard_rgba(out.width, out.height, out.rgba) {
                        Ok(()) => self.notice = Some(msg),
                        Err(e) => self.error = Some(format!("Clipboard failed: {e}")),
                    }
                }
                None => self.error = Some("The crop does not fit this image".into()),
            }
            return;
        }
        // Fallback (large JPEG before the full decode): decode off-thread. The
        // page is carried so the clipboard gets the page you are looking at, not
        // page 1 of it, and so is the edit: `edits` lives on the UI thread and is
        // not shared, so everything the worker needs is resolved here.
        let page = cur.page;
        let exif_tag = self.previews.get(&cur).map_or(0, |pv| pv.exif_tag);
        std::thread::spawn(move || match image_viewer::load_image_full(&p, page) {
            Ok(d) => match image_viewer::region_for_save(&d, exif_tag, xform, crop) {
                Some(out) => {
                    if let Err(e) = set_clipboard_rgba(out.width, out.height, out.rgba) {
                        eprintln!("clipboard: {e}");
                    }
                }
                None => eprintln!("clipboard: the crop does not fit this image"),
            },
            Err(e) => eprintln!("clipboard decode failed: {e}"),
        });
        self.notice = Some("Copying image to clipboard…".into());
    }

    /// Stamp a wallpaper press.
    ///
    /// Returns the value this press got, plus the Arc the worker checks against:
    /// the token moves when a **second** press lands, and the first press's
    /// worker — a thread that cannot be stopped — then aborts at its own checks
    /// instead of writing a file the desktop is about to be pointed at. The
    /// same "the last action wins" rule `pending_drop` records.
    pub(crate) fn next_wallpaper_order(&mut self) -> (u64, Arc<std::sync::atomic::AtomicU64>) {
        let order =
            self.wallpaper_order
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        (order, self.wallpaper_order.clone())
    }

    /// Set the current image as the desktop wallpaper.
    ///
    /// **All of the work is on a worker**, despite being a one-off action,
    /// because the reason a paste or a save is off-thread weighs for wallpaper
    /// too: with the conversion setting on this reads the file whole (the
    /// colour check is wallpaper's own read), decodes at texture size,
    /// converts every pixel and re-encodes — hundreds of milliseconds on a big
    /// photograph and seconds on a network file, all of it on the paint thread
    /// before this moved. The shape is the rating read's: one thread, the
    /// shared channel, a notice while in flight (a wallpaper request is
    /// otherwise invisible until the desktop changes), and the result back as
    /// a message.
    pub(crate) fn set_wallpaper(&mut self) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        // The two cheap UI facts, decided here: an archive member only exists
        // in a temp dir that dies with the app, and the destination folder
        // comes from the config.
        let in_archive = self.zip_archive.is_some();
        let folder = self
            .config_path
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(std::env::temp_dir);
        let (order, issued) = self.next_wallpaper_order();
        self.notice = Some("Setting wallpaper…".into());
        let tx = self.drop_tx.clone();
        let wake = self.drop_wake.clone();
        std::thread::spawn(move || {
            // An immediately-called closure keeps the body one phrasing over
            // the two early-abort checks and the fall-through set.
            let result: Result<(), String> = (|| {
                // Superseded before the first read: a press that arrived
                // while this was queued does none of the work.
                if issued.load(std::sync::atomic::Ordering::SeqCst) != order {
                    return Err("superseded".into());
                }
                // The conversion question is part of the *worker's* work now:
                // `color_info` reads the whole file, and that read belongs
                // wherever the rest of the decode lives.
                let converted = image_viewer::wide_gamut_conversion()
                    && image_viewer::color_info(&p).space
                        == image_viewer::ColorSpace::WideGamut;
                // Two reasons the wallpaper cannot always be the original
                // file, recorded here because the UI decision above decided
                // one of them for it: an archive member only exists in a
                // temp dir that dies with the app — the UI side already decided
                // that as `in_archive` — and a converted picture has to exist
                // as a file at all, because the shell reads the file and never
                // goes near our decode path.
                let target: PathBuf = if in_archive || converted {
                    let ext = p
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_else(|| "jpg".into());
                    let dest = folder.join(format!("wallpaper.{ext}"));
                    let wrote = if converted {
                        match image_viewer::load_image(&p, image_viewer::MAX_TEXTURE_DIM) {
                            Ok(mut d) => {
                                image_viewer::convert_wide_gamut_rgba(&mut d.rgba);
                                let img = image::RgbaImage::from_raw(d.width, d.height, d.rgba)
                                    .unwrap_or_else(|| image::RgbaImage::new(d.width, d.height));
                                image::DynamicImage::ImageRgba8(img)
                                    .save(&dest)
                                    .map_err(|e| std::io::Error::other(e.to_string()))
                            }
                            Err(e) => Err(std::io::Error::other(e.to_string())),
                        }
                    } else {
                        std::fs::copy(&p, &dest).map(|_| ())
                    };
                    if let Err(e) = wrote {
                        return Err(e.to_string());
                    }
                    // Superseded again **at the set**: the decode, conversion
                    // and encode above are exactly the window a second press
                    // can arrive in, and the set is the half that must not
                    // show the wrong action's picture.
                    if issued.load(std::sync::atomic::Ordering::SeqCst) != order {
                        return Err("superseded".into());
                    }
                    dest
                } else {
                    p
                };
                match wallpaper::set_from_path(&target.to_string_lossy()) {
                    Ok(()) => Ok(()),
                    Err(e) => Err(e.to_string()),
                }
            })();
            let _ = tx.send(Msg::Wallpaper { order, result });
            wake.request_repaint();
        });
    }

    /// Wallpaper from the picture as displayed (Shift+W): the crop and the
    /// turn baked into a PNG beside the config, because the shell reads a
    /// file and never our buffers. Refuses animations and document pages
    /// past the first for the same path-keyed-edit reason save and copy
    /// refuse them (a frame drawn for page 1 applied to page 3 is a valid
    /// picture of the wrong scan); unedited images go through the
    /// original-file path, which handles those fine.
    pub(crate) fn set_wallpaper_shown(&mut self) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let p = cur.path.clone();
        let edit = self.edits.get(&p);
        if edit.is_identity() {
            self.set_wallpaper();
            return;
        }
        if cur.page > 0 {
            self.error = Some(format!(
                "Cannot wallpaper a single page of a multi-page document ({}/{}).",
                cur.page + 1,
                self.gallery.pages_in_current()
            ));
            return;
        }
        if self.anim.as_ref().is_some_and(|a| a.path == p) {
            self.error = Some("Cannot wallpaper a transformed animation: it has many frames".into());
            return;
        }
        // Same composition the save uses (stored pixels, EXIF part folded
        // in), re-encoded losslessly: the desktop is display-only, so a
        // second JPEG generation would be pure loss for nothing.
        //
        // And **the whole composition is on a worker**, like `set_wallpaper`:
        // it is one whole-file read, one full decode, a lossless transform
        // and one PNG encode of a multi-megapixel picture, none of which the
        // paint thread does. `edit` is cloned out; `self.edits` would be
        // borrowed across the thread. The same notice, token and message
        // shape as the plain path — the two run through one mechanism so a
        // `W`-then-`Shift+W` race resolves by the same rule.
        let dest = self
            .config_path
            .parent()
            .map(|d| d.to_path_buf())
            .unwrap_or_else(std::env::temp_dir)
            .join("wallpaper.png");
        let (order, issued) = self.next_wallpaper_order();
        self.notice = Some("Setting wallpaper…".into());
        let tx = self.drop_tx.clone();
        let wake = self.drop_wake.clone();
        std::thread::spawn(move || {
            let result: Result<(), String> = (|| {
                // Superseded before the first read, like the plain path.
                if issued.load(std::sync::atomic::Ordering::SeqCst) != order {
                    return Err("superseded".into());
                }
                let result = (|| {
                    let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
                    let tag = if image_viewer::is_jpeg_bytes(&bytes) {
                        image_viewer::exif_orientation(&p)
                    } else {
                        0
                    };
                    let data = image_viewer::load_image_full(&p, 0).map_err(|e| e.to_string())?;
                    let out = image_viewer::region_for_save(&data, tag, edit.xform, edit.crop)
                        .ok_or_else(|| "the crop fell outside the image".to_string())?;
                    let img = image::RgbaImage::from_raw(out.width, out.height, out.rgba)
                        .ok_or_else(|| "transformed buffer was malformed".to_string())?;
                    image::DynamicImage::ImageRgba8(img)
                        .save(&dest)
                        .map_err(|e| e.to_string())
                })();
                match result {
                    Ok(()) => {}
                    Err(e) => return Err(e),
                }
                // Superseded again **at the set**: the read, decode, transform
                // and encode above are the window a second press can arrive
                // in, and the set is the half that must not show the wrong
                // action's picture.
                if issued.load(std::sync::atomic::Ordering::SeqCst) != order {
                    return Err("superseded".into());
                }
                wallpaper::set_from_path(&dest.to_string_lossy())
                    .map_err(|e| e.to_string())
            })();
            let _ = tx.send(Msg::Wallpaper { order, result });
            wake.request_repaint();
        });
    }

}
