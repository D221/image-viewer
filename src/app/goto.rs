//! Module `app_goto`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    pub(crate) fn new_with_renderer(
        cc: &eframe::CreationContext<'_>,
        restart: RestartSlot,
        mut prefetched: Option<(PathBuf, std::sync::mpsc::Receiver<Vec<PathBuf>>)>,
    ) -> Self {
        let ctx = &cc.egui_ctx;
        let config_path = Config::default_path();
        let config = Config::load(&config_path);
        // The launch loop already resolved this (probing if it needed to), and
        // the answer is in the config, so reading it back keeps the two in step.
        let renderer = config.renderer.resolve(None);

        // **Before the gallery is built below**, unlike the other settings the
        // library reads, which are applied further down. `Gallery::new` scans
        // the folder, and `is_browsable` asks this flag on every entry — so a
        // setter placed with the rest would arrive after the session's first
        // listing had already been built from the default. The saved setting
        // would then take effect only on the next rescan, which is a session
        // later and reads as the toggle not working.
        image_viewer::set_show_archives(config.show_archives);

        // Optional CLI argument: a file or a folder ("Open with…"). Must be the
        // first *non-flag* argument: `nth(1)` would take `--perf` as the path
        // and silently open the persisted folder instead.
        let arg = image_viewer::path_arg(std::env::args().skip(1));
        // Which file, if any, the user named. Taken before the `match` below
        // consumes `arg`, and excluding archives because those are extracted to a
        // temp folder and opened as a directory instead.
        let named_file: Option<PathBuf> =
            arg.clone().filter(|a| a.is_file() && !is_archive_path(a));
        // An archive argument is extracted up front and opened as its temp dir.
        let mut zip_archive: Option<PathBuf> = None;
        let mut zip_temp: Option<PathBuf> = None;
        let (folder, mut gallery) = match arg {
            Some(a) if a.is_dir() => {
                // `load_all_on_startup` re-lists this folder below, with the
                // configured sort order, and *replaces* whatever is here — so
                // building it now is a second full walk of the same directory.
                // The default-folder arm below already skips it for that reason;
                // this one did not, and on a cold cache a 1400-image folder paid
                // ~300 ms for a list that was thrown away before the first frame.
                let g =
                    if config.load_all_on_startup { Gallery::empty() } else { Gallery::new(&a) };
                (a, g)
            }
            Some(a) if a.is_file() && is_archive_path(&a) => {
                let temp = Self::zip_temp_for(&a);
                match extract_archive(&a, &temp) {
                    Ok((count, _)) if count > 0 => {
                        zip_archive = Some(a.clone());
                        zip_temp = Some(temp.clone());
                        // Same reason as the folder arm above: the sort pass
                        // below re-lists this directory and replaces the result.
                        let g = if config.load_all_on_startup {
                            Gallery::empty()
                        } else {
                            Gallery::new(&temp)
                        };
                        (temp.clone(), g)
                    }
                    _ => (
                        a.parent().map(|p| p.to_path_buf()).unwrap_or_default(),
                        Gallery::from_file(a),
                    ),
                }
            }
            Some(a) if a.is_file() => {
                let dir = a.parent().map(|p| p.to_path_buf()).unwrap_or_default();
                (dir, Gallery::from_file(a))
            }
            _ => {
                let folder = image_viewer::startup_folder(config.folder.as_deref());
                // `Gallery::new` scans the folder, and with `load_all_on_startup`
                // (the default) the sorted scan below immediately replaces that
                // list. So the first walk is thrown away. Measured at ~50 ms on a
                // 1001-image folder, and it is pure waste: there is one scan here
                // now instead of three.
                let g = if config.load_all_on_startup {
                    Gallery::empty()
                } else {
                    Gallery::new(&folder)
                };
                (folder, g)
            }
        };
        if config.load_all_on_startup {
            // One scan, sorted. `load_all_from_folder` used to be called here as
            // well, which scanned a second time and had its result overwritten by
            // this line - ~100 ms of directory walking to produce a list nobody
            // read.
            //
            // Sort per config (natural name order by default) and pick what to show.
            // The rule itself lives in `image_viewer::startup_index` so it can be
            // tested; see its doc comment for the bug it fixes.
            //
            // The background prefetch (started in `main` before the window exists)
            // lands here if it matches this folder; otherwise a synchronous scan
            // is the fallback.
            let recursive = config.recursive_folders && zip_archive.is_none();
            let files = match prefetched.take() {
                Some((dir, rx)) if dir == folder => match rx.recv() {
                    Ok(files) => files,
                    Err(_) => {
                        Self::sorted_scan(&folder, config.sort, config.sort_descending, recursive)
                    }
                },
                _ => {
                    let files =
                        Self::sorted_scan(&folder, config.sort, config.sort_descending, recursive);
                    files
                }
            };
            gallery.index = image_viewer::startup_index(
                &files,
                named_file.as_deref(),
                config
                    .last_by_folder
                    .get(&folder.to_string_lossy().to_string())
                    .map(|s| s.as_str()),
            );
            gallery.files = files;
        }

        set_linear_resample(config.linear_resample);
        set_downscale_filter(config.downscale_filter);
        set_downscale_smoothing(config.smooth_downscale);
        image_viewer::set_wide_gamut_conversion(config.convert_wide_gamut);
        set_thumbnail_cache(config.thumbnail_cache);
        image_viewer::set_preview_store(config.preview_store);
        // The preview store's thread, started once and only when the store is
        // on: the encode it does must not land in front of the first pixels, so
        // it cannot happen on the worker that decoded the picture.
        if config.preview_store {
            image_viewer::start_preview_store();
        }
        set_nav_overrides(&config.keybindings);

        if !config_path.exists() {
            config.save(&config_path);
        }

        let (tx, rx) = mpsc::channel();
        // `tx` is cloned rather than moved: the loader owns one sender and the
        // drop copy owns another, because a drop copy cannot go through the
        // loader's queue (see `drop_tx`).
        let loader = new_loader(ctx, tx.clone(), config.workers);

        // Taken before `gallery` is moved into the struct below.
        let all_files = gallery.files.clone();
        gallery.loop_back = config.loop_back;
        // In-memory copy of the persisted master; the field in the config is
        // the one `I` writes to. Owned by the session only in the sense that
        // fullscreen borrows it (see `panels_before_fullscreen`).
        let show_panels = config.show_panels;
        let mut app = Self {
            folder,
            gallery,
            config,
            config_path,
            loader,
            rx,
            previews: HashMap::new(),
            failed: HashSet::new(),
            load_errors: HashMap::new(),
            clock: 0,
            frame: 0,
            cur: None,
            rename_input: None,
            shown: None,
            shown_partner: None,
            full: None,
            full_due: None,
            view: View { fit: true, scale: 1.0, offset: Vec2::ZERO },
            error: None,
            notice: None,
            deleted_stack: Vec::new(),
            anim: None,
            color_picker_active: false,
            exif_info: None,
            exif_loading: false,
            rating: image_viewer::Ratings::default(),
            rating_for: None,
            show_panels,
            show_left: true,
            show_right: true,
            panels_before_fullscreen: None,
            pan_key: None,
            views: image_viewer::ViewMemory::new(),
            fit_window_pending: false,
            fit_wrap_pending: false,
            fit_window_key: None,
            fit_arrive_pending: false,
            fit_iters: 0,
            pending_pos: None,
            capturing: None,
            fit_debug: None,
            slideshow: false,
            slideshow_last: Instant::now(),
            slideshow_saved: None,
            slideshow_paused: false,
            last_geometry_save: Instant::now() - Duration::from_secs(2),
            next_scan: Instant::now() + RESCAN_INTERVAL,
            watcher: None,
            watcher_key: None,
            pending_rescan: false,
            next_rescan_allowed: Instant::now(),
            pending_content: std::collections::HashMap::new(),
            watch_events_seen: 0,
            rx_rescan: None,
            rx_undo_delete: None,
            last_external_spawn: None,
            checker: Some(make_checker_texture(ctx)),
            display_dim: None,
            resize: image_viewer::ResizeTracker::new(),
            renderer,
            restart,
            suppress_resize_redecode: false,
            waiting_load: false,
            grid_sel: 0,
            grid_cols: 1,
            edits: image_viewer::ImageEdits::default(),
            history: image_viewer::EditHistory::default(),
            crop_mode: false,
            crop_drag: None,
            grid_sel_scrolled: usize::MAX,
            grid_scroll: 0.0,
            grid_view_h: 0.0,
            lat: Latency::default(),
            t_boot: perf::boot(),
            t_prev_frame: None,
            pending_advance: None,
            history_back: Vec::new(),
            history_forward: Vec::new(),
            history_cap: 50,
            history_navigating: false,
            favourite_folders: Vec::new(),
            zip_archive,
            zip_temp,
            archive_parent: None,

            save_as: SaveAsState {
                open: false,
                dest: String::new(),
                quality: image_viewer::DEFAULT_JPEG_QUALITY,
                max_long: String::new(),
                formats: SAVE_AS_FORMATS,
            },
            // A file only ever arrives here from the command line or the file
            // dialog, neither of which is a transient hand-off, so there is never
            // a copy to release at start-up. `load_dropped` owns one instead.
            drop_copy: None,
            pending_drop: None,
            drop_tx: tx,
            drop_wake: ctx.clone(),
            wallpaper_order: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            spreads: GallerySpreads::default(),
            spreads_key: None,
            wheel_acc: 0.0,
            thumbs: HashMap::new(),
            thumb_tries: HashMap::new(),
            grid_ratings: HashMap::new(),
            grid_rating_pending: HashSet::new(),
            all_files,
            filter: String::new(),
            stamps: HashMap::new(),
            grid_page_rows: 1,
            gallery_view: false,
        };
        // Loaded before the first `goto`, so the gallery opened at start-up has its
        // own overrides in hand before anything is planned. `zip_archive` is already
        // set above for a `.cbz` named on the command line, so this keys such a
        // gallery by the archive rather than by the directory it was extracted into.
        app.refresh_spreads();
        app.goto(ctx);
        app
    }

    /// Point `spreads` at the gallery now on screen: its key first, then whatever
    /// that key has stored.
    ///
    /// **One function, because key and contents cannot be set independently.** The
    /// load names a key and the save writes under whatever `spreads_key` says, so
    /// two sites each doing half of this would put one gallery's decisions under
    /// another gallery's entry — silently, and only once the user had switched.
    ///
    /// Every site that opens a gallery calls this: `adopt_gallery` (which every
    /// folder and file open already goes through, for the same reason the filter
    /// cannot be forgotten), `open_archive` (which builds its own gallery instead of
    /// adopting one), and the constructor.
    ///
    /// **It reads `zip_archive` first, so it depends on `close_transients()` having run
    /// when leaving an archive.** Every caller that can follow one does: `open_folder`
    /// and `open_file` close before adopting, and `open_archive` closes before
    /// extracting. (The constructor has no archive to close.) A caller that opened a
    /// gallery without closing would key it by a temp directory that is about to be
    /// deleted, which is the exact mistake this function exists to prevent.
    pub(crate) fn refresh_spreads(&mut self) {
        self.spreads_key = spread_root_key(self.zip_archive.as_deref(), &self.folder);
        self.spreads = match (self.spreads_key.as_deref(), spreads_root()) {
            (Some(key), Some(store)) => load_spread_overrides(&store, Path::new(key)),
            // No root, or nowhere to persist: the decisions stay in memory for the
            // session, which is a smaller loss than refusing to browse.
            _ => GallerySpreads::default(),
        };
    }

    /// Write the overrides out.
    ///
    /// **The error is swallowed on purpose.** A read-only or full disk must not
    /// break browsing, and the decisions are still in memory for the session — the
    /// same rule the thumbnail cache follows, for the same reason. There is no
    /// retry and no warning: the next override saves again, and a pairing the user
    /// has to press twice is a far smaller problem than an app that stops
    /// responding because a file could not be written.
    pub(crate) fn save_spreads(&self) {
        let (Some(store), Some(key)) = (spreads_root(), self.spreads_key.as_deref()) else {
            return;
        };
        let _ = save_spread_overrides(&store, Path::new(key), &self.spreads);
    }

    // ---- navigation -------------------------------------------------------

    /// Point the app at `gallery.current()` and (re)plan all background work.
    ///
    /// **An archive is positioned on here, never opened and never decoded.**
    /// Both halves were once in this one branch and both were wrong:
    ///
    ///  * *Entered* — `goto` is where opening a folder ends, so entering here
    ///    made "Browse folder" open whichever archive happened to sort first, an
    ///    action nobody asked for. It is also the funnel for a rescan, a filter
    ///    change and a resume.
    ///  * *Decoded* — a `.cbz` has no image bytes, so the alternative would be a
    ///    decode job that sniffs, fails, and writes a `failed` entry permanent for
    ///    the session: an archive that can never load, with no error to act on.
    ///    `plan_jobs` refuses archives for that reason.
    ///
    /// So opening one is [`Self::enter_archive_if_present`], asked by the arrow
    /// and grid paths — the places where a person asked for something. Arriving
    /// here on an archive is the same as arriving on any other file: the cursor
    /// is on it, the panel draws a placeholder for it, and nothing is decoded.
    pub(crate) fn goto(&mut self, ctx: &Context) {
        // A goto that follows a keypress is navigation; a fresh open/click is a
        // selection. They are reported separately because the preloaded case
        // (0 ms) is the common one for navigation.
        let is_nav = self.lat.t_request.is_some();
        self.lat.begin(Instant::now(), is_nav);
        // Manga mode only: the cursor is only ever on a turn anchor, and this is the
        // one place every way into the app passes through - `step`, the grid pick,
        // and `Del`'s reschedule all end in a `goto`. The three of them assign the
        // cursor by file index, and at the default `lead_lone` of 1 an ordinary folder
        // of single pages tiles `{0} {1,2} {3,4} ...`, so file 3 is page 2: a second
        // half, where every pairing rule is written on the cursor *not* being
        // (lone's own doc calls this out), and where a backward step used to jump to
        // the last spread of the volume. Snapping here rather than at those three call
        // sites is what keeps a fourth from forgetting.
        self.snap_to_turn_anchor();
        // Learn how many pages the file we are arriving at has, *before* the page
        // is read, so `next` knows whether there is a page 2 to go to. This is
        // four bytes for anything that is not a TIFF, and the answer is cached, so
        // it is paid once per file rather than once per visit.
        if let Some(p) = self.gallery.current().map(|p| p.to_path_buf()) {
            self.gallery.learn_pages(&p);
        }
        // ...and how many pages the **turn's other half** has, when that half names a
        // different file.
        //
        // **Without this a whole document is stepped over, and the content manga mode
        // is for is exactly what it is stepped over.** `pages_in` answers 1 for a file
        // nobody has asked about, `step` applies `turn_len` one page at a time, and so
        // a step of two pages from a portrait page in front of an unlearned
        // multi-page document lands *past* it: in a gallery
        // `[a, b, vol.tiff(200pp), c]` the step from `b` pairs optimistically (the
        // document's page 1 has no measured dimensions, and `pairable` treats unknown
        // as "pairs"), consumes the document's first page as its own second half, and
        // lands on `c` — and because the count is still unlearned, the tiling keeps
        // saying "one page" and every further step treats 199 printed pages as one
        // file. Pressing `→` never reaches them. The non-manga path is immune because
        // `Gallery::next` steps one page and lands on it.
        //
        // **Same trade `learn_shapes`' read-ahead makes, and the same answer**: one
        // four-byte read, `learn_pages` is cache-first so it is paid once per *file*
        // for the session, and per navigation the cost is one `HashMap` lookup on a
        // path that is going to be walked anyway. Manga mode only, so a photo folder —
        // where there is no second half and every file is one page — is untouched.
        self.learn_partner_pages();
        // How large this page *displays* at, and the next one's, which is what the
        // manga pairing judges a page by: a turn is decided by asking about the page
        // after it, so learning only this one would let a spread re-flow under the
        // reader. Beside `learn_pages` because both are "measure what we are arriving
        // at" and both answer from a cache, and `learn_shapes` reads the page count
        // `learn_pages` just filled in, so it must come second. May already be
        // answered: `pump` records a page's dimensions as its preview lands, so a
        // page in the preload window has them before the user gets there.
        self.gallery.learn_shapes(self.gallery.page_pos());
        self.record_history();
        self.cur = self
            .gallery
            .current()
            .map(|p| ImgKey { path: p.to_path_buf(), page: self.gallery.current_page() });
        // Panning belongs to the picture it panned: a carried-over offset
        // shifts the new image by the old one's drag — black on one side,
        // clipped on the other — and no zoom readout looks wrong while it
        // does, because the scale was never the problem. Zooming merely
        // recomputes the offset around the cursor, which is why any zoom
        // seemed to "fix" the bars while the scale stayed "bad". Keyed on
        // the cursor (not every goto: toggles and rescans must not steal
        // the pan).
        if self.cur != self.pan_key {
            // **With per-image remembering on, the whole view is what a
            // cursor change trades**: leaving remembers the outgoing image's
            // view as it was, arriving restores a remembered view exactly, and
            // the un-remembered case answers the carry — `arriving_view`'s
            // whole job, left pure in `lib.rs` so the two rules stay testable
            // side by side. The fit modes are respected upstream of this
            // (their own arrival rules run after), so this only ever answers
            // where no mode already does.
            if self.config.remember_view {
                if let Some(out) = self.pan_key.clone() {
                    self.views.remember(
                        out,
                        SavedView {
                            fit: self.view.fit,
                            scale: self.view.scale,
                            offset: self.view.offset.into(),
                        },
                    );
                }
            }
            self.pan_key = self.cur.clone();
            self.view.offset = Vec2::ZERO;
            if self.config.remember_view {
                if let Some(key) = self.cur.clone() {
                    let arriving = image_viewer::arriving_view(
                        self.views.restore(&key),
                        self.view.fit,
                        self.view.scale,
                    );
                    self.view.fit = arriving.fit;
                    self.view.scale = arriving.scale;
                    self.view.offset = arriving.offset.into();
                }
            }
        }
        // Preloading hit rate: the new image already had a preview, so the
        // first pixel is free. If this counter stays low, the preload window
        // is aimed wrong.
        if is_nav && self.cur.as_ref().is_some_and(|c| self.previews.contains_key(c)) {
            perf::count(perf::C_PREVIEW_REUSED, 1);
        }
        self.error = None;
        self.notice = None;
        // A revisit to an already-failed image issues no new decode, so say
        // what the first attempt said rather than showing "Loading…" for a
        // job that will never run. The entry is cleared everywhere the
        // `failed` entry is, so this cannot outlive the failure it reports.
        if let Some(msg) = self.cur.as_ref().and_then(|c| self.load_errors.get(c)).cloned() {
            self.error = Some(msg);
        }
        self.exif_info = None;
        self.exif_loading = self.cur.is_some();
        // Reset the write base with the read marker: until the new sidecar
        // read lands, `rating_for != cur` and the mutators below must not
        // write the previous image's stars and tags onto this one.
        self.rating = image_viewer::Ratings::default();
        self.rating_for = None;
        self.request_rating();
        self.full_due = Some(Instant::now() + FULL_DWELL);
        // Blocking mode: only wait when the preview isn't already cached.
        // Archives have no preview, so don't wait for them.
        self.waiting_load = self.config.block_while_loading
            && self
                .cur
                .as_ref()
                .is_some_and(|c| !is_archive_path(&c.path) && !self.previews.contains_key(c));

        if let Some(name) =
            self.cur.as_ref().and_then(|p| p.path.file_name()).and_then(|n| n.to_str())
        {
            // "page 3 of 12" only when the file has more than one page, so a
            // normal photo's title is exactly what it always was.
            let title = match self.gallery.pages_in_current() {
                n if n > 1 => {
                    format!("Image Viewer - {name}  ({}/{})", self.gallery.current_page() + 1, n)
                }
                _ => format!("Image Viewer - {name}"),
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
        // Sticky fit-window mode: a newly arrived image gets the 100%-or-fit
        // treatment like an explicit `T`. Keyed on the cursor, so toggles and
        // rescans re-`goto`ing the same image do not refit; skipped while a
        // slideshow advances, which would dance the window every interval.
        if self.config.fit_window_mode && !self.slideshow && self.cur != self.fit_window_key {
            self.fit_window_key = self.cur.clone();
            self.fit_arrive_pending = true;
            self.fit_iters = 6;
        }
        self.schedule();
    }

    /// The other page of the turn the cursor is on, in manga mode, or `None`
    /// otherwise.
    ///
    /// **The mode gate is the whole of this method.** It is app state — a setting —
    /// and the rule it guards is [`image_viewer::turn_partner`], which is a pure
    /// function next to the rest of the pairing and is tested there. Splitting it
    /// that way is what makes the rule falsifiable: the plan once specified this
    /// as `next_anchor`, which is the turn *after* this one, and a test is what
    /// would have caught that.
    ///
    /// `None` also when the turn holds one page or the gallery is empty — see the
    /// library function for why those need no separate case here.
    pub(crate) fn turn_partner(&self) -> Option<ImgKey> {
        self.turn_partner_at(self.gallery.page_pos())
    }

    /// [`Self::turn_partner`] for **any** position, not just the cursor's.
    ///
    /// The position is a parameter because "the turn the reader is looking at" and
    /// "the turn the cursor is on" are two different questions during the lag
    /// between them — `shown` lags `cur` for the length of a decode — and answering
    /// the second while painting the first is how a spread ends up showing a page
    /// from this turn beside one from the last. `shown_partner` is derived through
    /// this; the plan is not, because the plan is about what is *wanted*.
    ///
    /// **A position that is not an anchor is not snapped here, and that is
    /// deliberate.** Snapping is [`image_viewer::anchor_of`], an O(distance from the
    /// front) walk, and this runs every frame. It does not need to: the only position
    /// it is asked about is `shown`'s, which was adopted from a `cur` that `goto`'s
    /// `snap_to_turn_anchor` had already put on an anchor, and every way the pairing
    /// can change afterwards makes turns **shorter or leaves them alone** —
    /// `lone(anchor)`'s three arms are `is_lead`, `shape_of(anchor) == Double` and
    /// `!pairable(anchor, next)`, and a dimension only ever resolves from unknown
    /// (`Shape::Single`) to `Single` or `Double`, so `lone` can only go true and a
    /// turn can only shrink. A turn that shrinks frees its second half, so a position
    /// that was an anchor cannot stop being one, and cannot become the second half of
    /// a turn behind it either (that would need some *earlier* anchor to stop being
    /// lone). The four things that re-pair by choice — `M`, `O`, `X` and loading a
    /// spread store — all re-`goto`, which re-snaps.
    pub(crate) fn turn_partner_at(&self, pos: PagePos) -> Option<ImgKey> {
        if !self.config.manga_mode {
            return None;
        }
        image_viewer::turn_partner(pos, self.spreads.lead_lone, &self.page_ctx())
    }

    /// Where "what is on screen" sits in the flat page sequence, as a position.
    ///
    /// **`shown` is a key and the pairing is asked in positions**, and this is the
    /// bridge. It exists because `ImgKey` does not know its file index and the
    /// gallery is a `Vec`, so turning one back into the other is a scan — see
    /// [`Gallery::pos_of`], which this defers to.
    ///
    /// **The fast path is the whole of the cost.** `shown` almost always *is* `cur`,
    /// and `cur` was built from [`Gallery::page_pos`], so the position is already in
    /// hand and the scan never runs. The scan is for the lag — the frames between a
    /// navigation and its decode landing — which are the frames where a full-res
    /// decode is in flight and frames are hundreds of milliseconds apart, so the few
    /// hundred path comparisons are not what the frame is spending its time on.
    ///
    /// `None` when nothing is on screen, and when the file is no longer in the
    /// gallery at all (removed from under the picture, or the folder was filtered
    /// or reloaded). Both mean "no turn", which is the honest answer: `turn_partner`
    /// of an unknown position cannot be a turn of anything.
    pub(crate) fn shown_pos(&self) -> Option<PagePos> {
        let shown = self.shown.as_ref()?;
        if self.cur.as_ref() == Some(shown) {
            return Some(self.gallery.page_pos());
        }
        self.gallery.pos_of(shown)
    }

    /// Re-derive `shown_partner` from where `shown` is. **Never a latch, and the
    /// ordering in [`Self::sync_shown`] is the property** — the call sits after the
    /// adopt gate, not inside it.
    ///
    /// It was a latch, and the gate is why: the only assignment sat inside
    /// `if previews.contains_key(&cur) && shown != cur`, which is false every frame
    /// the cursor is standing still — so `M` (which re-`goto`s without moving the
    /// cursor), `O` at `(0, 0)` and `X` all changed what a page is and the screen
    /// kept the old answer. The repro is four keystrokes: `→ → → M` in a folder of
    /// single pages leaves `pages` with **one** entry, so `spread_fit` fits a page
    /// into the now-half-width panel and the reader gets a half-width picture beside
    /// an empty half, until the next `→` produces a correct spread and the bug reads
    /// as "mostly working".
    ///
    /// The other reason it has to be unconditional is the same defect seen from the
    /// other side: a partner whose decode reveals it is a two-page scan collapses the
    /// *plan* (`arrival_reschedules` re-runs `schedule`), and nothing re-derived the
    /// pair, so three printed pages stayed squashed into one viewport until the reader
    /// navigated. Deriving every frame closes that as a class rather than one site.
    pub(crate) fn sync_shown_partner(&mut self) {
        let want = self.shown_pos().and_then(|pos| self.turn_partner_at(pos));
        self.shown_partner = want;
    }

    /// Learn the page count of the file the turn's second half names, when that half
    /// is a **different file** from the one the cursor is on.
    ///
    /// Called from [`Self::goto`] and nothing else. The gap it closes is a whole
    /// multi-page document being stepped over: `pages_in` answers 1 for a file nobody
    /// has asked about, `pairable` treats an unmeasured page as pairing optimistically,
    /// so a turn anchored just before a 200-page document claims its first page as the
    /// turn's second half, the step lands on the *file after* it, and the tiling then
    /// says "one page" for the rest of the session — so `→` never reaches pages 2..200.
    /// `goto`'s own comment has the traced version.
    ///
    /// **`None` — and so no read — for a lone turn, and for a partner inside the same
    /// document**, whose count `learn_pages` has already answered because it is the
    /// cursor's own file. So the added work per navigation is one `HashMap` lookup for
    /// a cached count and a four-byte read the first time a file is visited.
    ///
    /// Manga mode only, and it takes the partner from [`Self::turn_partner`] rather
    /// than "the next page": a turn that holds one page has no second half to learn
    /// anything about, and asking about a page the turn does not contain would learn a
    /// count nothing is asking about.
    pub(crate) fn learn_partner_pages(&mut self) {
        let Some(partner) = self.turn_partner() else {
            return;
        };
        // The cursor's own **file**, from the gallery and not from `self.cur`: `goto`
        // calls this before `cur` is rebuilt, so `cur` is still the position the
        // navigation is leaving and the "is it the same file" test below would
        // compare the wrong two.
        let Some(here) = self.gallery.current().map(|p| p.to_path_buf()) else {
            return;
        };
        if partner.path == here {
            return;
        }
        self.gallery.learn_pages(&partner.path);
    }

    /// Step one image (or turn) forwards or back. Returns whether to keep
    /// going: false only at the end of the gallery with looping off, which is
    /// what stops a slideshow. A throttled step returns true — the image is
    /// still coming, so waiting is right and stopping is wrong.
    pub(crate) fn step(&mut self, ctx: &Context, forward: bool) -> bool {
        // "Wait for load" mode: throttle stepping to decode completion.
        if self.config.block_while_loading && self.waiting_load {
            return true;
        }
        // **Both ends of an archive step out of it.** At the front, `prev` would loop
        // to its last page (with `loop_back` on) or do nothing (off), and both
        // are wrong: the thing before the archive's first page is the folder
        // entry that opened it. At the back, `next` would wrap to the archive's
        // *own* first member — which is what made a folder of archives behave
        // like a folder of one, the reader cycling through the first volume
        // forever and never reaching the second.
        //
        // Asked *before* the move rather than after it, because "we did not
        // move" and "we are leaving" are otherwise the same observation and the
        // first one is the right answer for `loop_back: false` at the end of any
        // folder. Deliberately not gated on `loop_back` for the way out: with
        // looping off, pressing `←` inside an archive should still get out, and
        // a viewer that traps the reader inside a 200-page archive with a setting
        // that exists to stop the opposite is not honouring the setting.
        //
        // Returns true — the press did something — so `step`'s caller records a
        // navigation rather than a no-op.
        if self.archive_edge(forward) && self.leave_archive(ctx, forward) {
            return true;
        }
        // Manga mode moves a whole turn, which is one or two pages; single-image
        // mode moves one page. With looping off, stepping at either end goes
        // nowhere in both. Returning without `goto` matters: it avoids resetting
        // the full-res dwell timer and re-planning the queue for an image that did
        // not change.
        //
        // **Right arrow on an archive in the folder listing enters it, rather than
        // moving past it.** This check is before the move, so we enter the archive
        // we're standing on. Only applies when NOT inside an archive (zip_archive
        // is None) because inside an archive the forward edge is handled by
        // `archive_edge` above.
        if forward
            && self.zip_archive.is_none()
            && self.gallery.current().map(|p| is_archive_path(p)).unwrap_or(false)
        {
            return self.enter_archive_if_present(ctx);
        }

        let moved = if self.config.manga_mode {
            if forward { self.turn_forward() } else { self.turn_backward() }
        } else if forward {
            self.gallery.next()
        } else {
            self.gallery.prev()
        };
        if moved {
            self.goto(ctx);
            // Landing on an archive by arrowing is the request to open it, and
            // this is where that request is made. `goto` deliberately does not:
            // it is also where opening a folder ends, and entering there opened
            // whichever archive sorted first without anyone asking.
            //
            // **After** the `goto`, so the panel has already planned for the
            // archive and `open_archive` replaces that plan wholesale rather
            // than leaving a stale one behind.
            if self.enter_archive_if_present(ctx) {
                return true;
            }
        }
        moved
    }
}
