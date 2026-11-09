//! Module `app_struct`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::app::gallery_ops::UndoDeleteResult;
use crate::*;

/// A position in a gallery: folder + file index + page. This is everything
/// needed to restore "where the reader was" when they go back or forward.
#[derive(Clone, Debug)]
pub(crate) struct HistoryEntry {
    pub folder: PathBuf,
    pub index: usize,
    pub page: u32,
}

pub(crate) struct Preview {
    pub(crate) tex: TextureHandle,
    pub(crate) orig: (u32, u32),
    pub(crate) load_ms: u64,
    pub(crate) used: u64, // LRU stamp
    // Full-res image produced by the same decode pass (single-pass files).
    // Kept on the cached preview so it can be adopted when the user
    // navigates here, instead of being decoded again.
    pub(crate) full: Option<Arc<ImageData>>,
    // Decoded animation frames when the file is an animated GIF.
    pub(crate) anim: Option<Arc<Animation>>,
    // Any pixel is translucent (draw checkerboard behind it).
    pub(crate) has_alpha: bool,
    // The `target_dim` this texture was decoded for. With `px` and `orig` this
    // is everything `needs_refine` needs, so staleness is a pure function of the
    // cache and the current display size — no bookkeeping to keep in sync.
    // (This replaces the old `below_target` flag, which could only say "the
    // source is bigger than what we asked for" and so could not tell a source
    // that is simply smaller than the display from one worth re-decoding.)
    pub(crate) target: u32,
    // Pixel dimensions of the texture, for the same check.
    pub(crate) px: (u32, u32),
    // Full-resolution tiles cut from `full` **for this page**, keyed in this
    // page's own source pixels.
    //
    // **Why a page other than the cursor's keeps its own map.** The cursor page's
    // tiles live on the adopted `FullImage`, which is rebuilt - and so cleared -
    // whenever the cursor moves, and that is what bounds them. A turn's *other*
    // half has no such slot, and its buffer is on this preview already (see
    // `full` above), so without a map here a zoomed spread would have nowhere to
    // keep what it cut. Every map in the app is counted against one `MAX_TILES`
    // budget by the eviction pass in `central`, so this cannot raise the ceiling -
    // see the comment there, because the obvious reading of "one map per cached
    // page" is 16 x 64 tiles and that is not what this is.
    pub(crate) tiles: HashMap<(u32, u32), Tile>,
    // The file's EXIF orientation tag and the block grid a lossless JPEG crop
    // aligns to. Cached with the preview so the crop UI never has to touch the
    // file: a rectangle drawn on the *displayed* picture must be expressed in
    // *stored* coordinates, and that composition needs the tag.
    pub(crate) exif_tag: u16,
    pub(crate) crop_grid: (u32, u32),
    // The file's colour classification, from the preview decode's own bytes.
    // The panel's colour line reads this directly, so a cached preview answers
    // it with no read of any kind.
    pub(crate) colour: image_viewer::ColorInfo,
    // Levels and clipped-pixel counts from the decode worker: 3KB that rides
    // the preview for free, so the panel draws numbers rather than pixels.
    pub(crate) hist: image_viewer::Histogram,
}

/// Playback state for the animation currently on screen.
pub(crate) struct AnimView {
    pub(crate) path: PathBuf,
    pub(crate) anim: Arc<Animation>,
    pub(crate) tex: TextureHandle,
    pub(crate) start: Instant,
    pub(crate) last_index: usize,
    /// Paused on this frame (`,` / `.` / `K`). `None` while playing, in
    /// which case the clock rules. Set, not derived, so stepping twice never
    /// re-asks the clock for a frame it just left.
    pub(crate) paused: Option<usize>,
}

pub(crate) struct Tile {
    pub(crate) tex: TextureHandle,
    pub(crate) last_drawn: u64,
}

/// One page of a turn, as the tile loop needs to see it: which buffer, how sharp
/// that page's own preview already is, and where the page is.
///
/// Gathered as an owned value before the loop borrows the tile maps, so the loop can
/// hold one page's tiles and the other's at the same time - two fields of `MyApp`,
/// which one `&mut self` could not be.
pub(crate) struct TilePage {
    /// Full-resolution pixels for **this** page. An `Arc` clone, so holding it costs
    /// a refcount.
    pub(crate) data: Arc<ImageData>,
    /// Texture px per source px, for this page's own preview: the density below
    /// which the preview is already as sharp as the display and tiles are wasted.
    pub(crate) preview_density: f32,
    /// The page's picture in **unrotated** image space. The tile grid is in this
    /// page's own source pixels, so this is the rectangle they are laid out from.
    pub(crate) paint: egui::Rect,
    /// The same rectangle after `xform` - the page's slot - which is what can be on
    /// screen. Kept separate because painting the *slot* would apply the transform
    /// twice; see the `page_rects` comment.
    pub(crate) slot: egui::Rect,
    /// The page's own transform, for `paint_tex`.
    pub(crate) xform: Xform,
    /// The page's dimensions in **source** pixels — what the tile pitch is laid
    /// out against. Usually the buffer's own size, and then the grid below is
    /// exactly what it has always been; but a source past `MAX_TEXTURE_DIM`
    /// decodes its "full" buffer DCT-scaled (a 17042px JPEG keeps an 8521px
    /// buffer, since a wider texture would not fit the GPU), and a grid laid
    /// out in buffer pixels at source pitch composes a small copy of the whole
    /// image over its own top-left. The loop pitches every tile back up by
    /// `src / buffer` instead.
    pub(crate) src: egui::Vec2,
}

pub(crate) struct FullImage {
    pub(crate) path: PathBuf,
    /// Which **page** of `path` these pixels are. Not decoration:
    /// `decode_page` returns `full: None` for every page past the first of a
    /// multi-page TIFF, so a document's `self.full` is whichever page's decode
    /// landed — and a struct without this field hands page 1's pixels to page 3's
    /// rect. Keyed by `PathBuf` only, the three sites that read it were each a
    /// silent wrong-picture bug: the tile loop cutting page 0 with page N's `uv`,
    /// `Ctrl+C` pasting page 1 of a document past page 1, and `needs_full`
    /// answering "already done" for a partner that is another page of the same
    /// document — which is *every turn inside a manga volume*, and left the second
    /// half of a zoomed spread permanently soft. See [`FullImage::is`].
    pub(crate) page: u32,
    pub(crate) data: Arc<ImageData>,
    pub(crate) tiles: HashMap<(u32, u32), Tile>,
    pub(crate) load_ms: u64,
    /// The file's EXIF orientation, which the decode has **already applied** to
    /// `data`. Carried here so the clipboard can place a stored-space crop into
    /// an oriented buffer without reaching back into the preview cache — and
    /// because the value that describes a buffer is a property of the buffer.
    pub(crate) exif_tag: u16,
}

impl FullImage {
    /// Whether these are the pixels of `key` — **file and page**, the whole
    /// [`ImgKey`].
    ///
    /// Every site that asks "is this full buffer the one for this image" goes
    /// through here rather than spelling `f.path == key.path`, because that
    /// spelling is the bug: it is `true` for every page of a document, and the
    /// answer "yes, this is that page's full decode" is then given by a buffer
    /// holding a different page's pixels. A `page`-field comparison rather than a
    /// built `ImgKey` so the check allocates nothing on a path that runs per frame.
    pub(crate) fn is(&self, key: &ImgKey) -> bool {
        self.path == key.path && self.page == key.page
    }
}

/// `scale` / `offset` only matter while `fit` is false.
pub(crate) struct View {
    pub(crate) fit: bool,
    pub(crate) scale: f32,   // screen points per source pixel
    pub(crate) offset: Vec2, // image centre relative to panel centre, in points
}

/// What the save-as dropdown offers.
///
/// **The formats [`image_viewer::save_transformed_to`] can actually encode**, not
/// every extension the app can open. A destination the user can pick in a
/// dropdown and then be refused at save time is a trap, and the two sets differ:
/// the app reads GIF and AVIF and writes neither.
pub(crate) const SAVE_AS_FORMATS: &[&str] = &["jpg", "png", "webp", "bmp", "tiff"];

/// The save-as panel's contents, held in [`MyApp::save_as`].
///
/// **A path the user is typing, plus two numbers, and nothing else.** The source
/// picture is deliberately *not* in here: it is read from `cur` at save time.
/// A panel that captured a path would keep saving the picture you were looking
/// at when you opened it, after you had navigated somewhere else — and the
/// notice would name a file whose pixels came from a different image.
pub(crate) struct SaveAsState {
    pub(crate) open: bool,
    /// Pre-filled with the `_rotated` / `_cropped` unique name at the moment the
    /// panel opens, so pressing Save with no edits does exactly what `Ctrl+S`
    /// does. That is the "defaults auto-selected as now" requirement, and the
    /// reason the panel never opens empty: an empty path is not a default, it is
    /// a question.
    pub(crate) dest: String,
    /// JPEG quality, 1..=100. Only meaningful for a JPEG destination, and the
    /// panel says so rather than offering a slider that does nothing.
    pub(crate) quality: u8,
    /// Bound on the long side in pixels; blank means "do not resize".
    pub(crate) max_long: String,
    pub(crate) formats: &'static [&'static str],
}

/// Swap a path's extension, keeping the folder and the stem.
///
/// A string function rather than `Path::with_extension` because the input is
/// whatever the user has typed so far, and `with_extension` on a path with a
/// trailing separator produces surprises. A path with no extension gains one.
pub(crate) fn replace_extension(dest: &str, ext: &str) -> String {
    let (dir, stem) = match dest.rfind(['/', '\\']) {
        Some(i) => (&dest[..=i], &dest[i + 1..]),
        None => ("", dest),
    };
    // The dot must come *after* the last separator for `archive.part1` to keep
    // its stem, and after index 0 for a dotfile not to lose its name.
    match stem.rfind('.') {
        Some(i) if i > 0 => format!("{dir}{}{ext}", &stem[..i]),
        _ => format!("{dir}{stem}.{ext}"),
    }
}

/// The crop drag in progress, and the file it belongs to.
///
/// A wrapper rather than a field on the gesture itself: `CropDrag` is pure
/// screen geometry in `lib.rs` (where it can be tested), and a path is not.
pub(crate) struct CropDragState {
    pub(crate) path: PathBuf,
    pub(crate) drag: image_viewer::CropDrag,
}

pub(crate) struct MyApp {
    pub(crate) folder: PathBuf,
    pub(crate) gallery: Gallery,
    pub(crate) config: Config,
    pub(crate) config_path: PathBuf,

    pub(crate) loader: Loader,
    pub(crate) rx: Receiver<Msg>,

    /// Decoded previews, keyed by **(file, page)** rather than by file. Page 1 and
    /// page 3 of one document are different pictures, and a path-keyed cache hands
    /// back the wrong one — silently, and only for multi-page files.
    pub(crate) previews: HashMap<ImgKey, Preview>,
    /// `(key, kind)` pairs that failed to decode, permanent for the session.
    /// Per *image*, so a document with one unreadable page keeps its other pages.
    pub(crate) failed: HashSet<(ImgKey, Kind)>,
    /// What a failed preview said, by image key — so navigating back to it
    /// re-states the error instead of showing "Loading…" for a decode that
    /// will never be re-issued. Written on the same arm that records the
    /// `failed` entry and cleared everywhere that entry is cleared; the two
    /// cannot drift apart because there is exactly one of each site.
    pub(crate) load_errors: HashMap<ImgKey, String>,
    pub(crate) clock: u64,
    pub(crate) frame: u64,

    /// What the user asked for: a file and a page of it.
    pub(crate) cur: Option<ImgKey>,
    /// A rename dialog is open: `Some(current file name)` prefilled, `None`
    /// when closed. Confirming renames the file on disk and re-points the
    /// gallery at the new path; cancelling drops it.
    pub(crate) rename_input: Option<String>,
    /// What is on screen: a file and a page of it. Lags `cur` until the wanted
    /// preview has been adopted by `sync_shown`.
    ///
    /// A key rather than a path, even though the two differ only by a page number
    /// that is 0 almost always. During the lag — which is every uncached
    /// navigation, and therefore most of them — `shown` names the *previous*
    /// image, and if it remembered only the path then a document's old page would
    /// be lost the moment you moved to another one. The page has to be part of
    /// what "what is on screen" means.
    pub(crate) shown: Option<ImgKey>,
    /// The turn's **other** page, in manga mode: `None` for a lone turn, and for
    /// the whole feature off.
    ///
    /// **A second field rather than a second meaning on `shown`, and the reason
    /// is the lag `shown` exists for.** `shown` lags `cur` until the wanted
    /// preview has been adopted, so a spread needs its own lagging partner:
    /// during the lag — every uncached navigation, so most of them — `shown`
    /// names the previous turn's cursor page, and a partner read from `cur`
    /// would be the new turn's. The reader would see one page of the turn they
    /// left beside one page of the turn they asked for.
    ///
    /// **Set in the same act that adopts `shown`, and nowhere else.** Adopting
    /// the two separately is how that happens: the partner lands first, or the
    /// cursor does, and there is a frame where one is from this turn and one from
    /// the last. So `sync_shown` assigns both inside its existing "adopt the
    /// wanted preview" block, and `reset_display` / `invalidate_path` drop both
    /// beside the existing `shown = None`.
    ///
    /// **Only the paint loop reads it.** Every view-shaped path — `footprint`,
    /// the crop frame, `dim_bands`, the full-res tiles, `anim`, EXIF, the
    /// latency commit — reads `shown` and is unchanged, which is why no crop,
    /// tile or save code moves for this feature at all.
    pub(crate) shown_partner: Option<ImgKey>,
    pub(crate) full: Option<FullImage>,
    pub(crate) full_due: Option<Instant>,
    pub(crate) view: View,
    pub(crate) error: Option<String>,
    pub(crate) notice: Option<String>, // transient feedback for file actions
    /// Files deleted this session, most recent first, capped at ten. `Del` is
    /// the one bare letter allowed to lose data; this stack makes it
    /// recoverable instead of the sole exception. Keys are the original paths,
    /// so re-adding restores the file beside its neighbours.
    pub(crate) deleted_stack: Vec<PathBuf>,
    pub(crate) anim: Option<AnimView>, // playing animation (animated GIF)
    /// Colour picker is active: click on the picture to sample the pixel under
    /// the cursor and show its colour. `Ctrl+K`; view-only, one bool.
    pub(crate) color_picker_active: bool,

    pub(crate) exif_info: Option<String>,
    pub(crate) exif_loading: bool,
    /// The sidecar rating for the image the cursor is on. Read off the UI
    /// thread through the same worker+message shape as every other read, and
    /// deliberately NOT part of the `wants_exif` gate: that gate exists to stop
    /// a 7-12 ms whole-file read, and a 300-byte sidecar must not vanish from
    /// the UI just because EXIF is not wanted.
    pub(crate) rating: image_viewer::Ratings,
    /// Which image `rating` was read for, or `None` when no read is in hand —
    /// cleared in `goto` beside `exif_info`, so a stale rating is never shown
    /// for the wrong image.
    pub(crate) rating_for: Option<PathBuf>,
    /// The side-panel master (`I`), **persisted in the config**
    /// (`Config::show_panels`); this field is its in-memory copy, read on
    /// startup so a launch with chrome off opens with chrome off.
    pub(crate) show_panels: bool,
    /// Left (EXIF) and right (settings) panes individually. `show_panels`
    /// (`I`) is the master: a pane paints only when both its flag and the
    /// master are on. Toggled by `L` and `P`, view-only like every other
    /// bare letter. Session state — deliberately not like the master any
    /// more: the master persists between launches, the per-pane fine-tuning
    /// does not, and a restart re-shows both panes.
    pub(crate) show_left: bool,
    pub(crate) show_right: bool,
    /// The side-panel master as it stood **before entering fullscreen**, so
    /// leaving can restore it: the master is a persisted preference and the
    /// fullscreen dance is a transient overlay, and `on_exit` saves the whole
    /// config with whatever the field holds — a hardcoded "panels come back"
    /// on exit would silently rewrite the user's minimal state.
    pub(crate) panels_before_fullscreen: Option<bool>,
    /// A cover experiment holds the window at monitor size. **The viewport
    /// cannot report this one** — no fullscreen request was made, so
    /// `viewport().fullscreen` stays false and this flag is the only record of
    /// it. Every `viewport().fullscreen` read goes through `is_fullscreen`.
    pub(crate) covered: bool,
    /// Cursor whose pan the view holds: see the reset in `goto`.
    pub(crate) pan_key: Option<ImgKey>,
    /// What each visited image's view was when the user left it, for the
    /// per-image remembering setting. Filled in `goto`'s cursor-change block
    /// and read by nobody else; `invalidate_path` drops a file's entries, for
    /// the same reason it drops the pages' shapes — a remembered view for
    /// pixels that no longer exist is a value that looks like state and is
    /// wrong. Session-only, one entry per visited image, kilobytes.
    pub(crate) views: image_viewer::ViewMemory,
    /// Fit-window-to-image armed (`T`): consumed by `central` on its next
    /// frame, where the drawn size is known. A flag rather than doing it in
    /// the dispatch, because the dispatch cannot see the laid-out content.
    pub(crate) fit_window_pending: bool,
    /// Follow-up wrap for the sticky mode: re-hug the current zoom without
    /// touching it. Separate from the reset above, which also sets the view.
    pub(crate) fit_wrap_pending: bool,
    /// Cursor the window was last fitted to: the mode refits on arrival, not
    /// on every `goto` (toggles and rescans re-`goto` the same image).
    pub(crate) fit_window_key: Option<ImgKey>,
    /// Arrival fit armed by `goto` (mode only): unlike an explicit `T`,
    /// which fits whatever is on screen, this must wait until the new
    /// image is actually displayed — central paints `shown`, which lags
    /// `cur` for a decode, and fitting early sizes the window for the
    /// picture just left (every status line then reads one image behind).
    pub(crate) fit_arrive_pending: bool,
    /// Size iterations left: the window is re-measured and re-sent until it
    /// matches (or this runs out), because panels move after the measure —
    /// notices, wraps, late EXIF widths — and one shot lands wherever they
    /// leave it. Bounded, so this always terminates.
    pub(crate) fit_iters: u8,
    /// Window size a centering command is waiting on: the position must land
    /// after the resize settles, or it anchors a window that then resizes out
    /// from under it. `(want_size, frames_left)`.
    pub(crate) pending_pos: Option<((f32, f32), u8)>,
    /// Action id currently capturing a replacement chord in Settings (click
    /// a chord, press keys). While set, the key dispatch stands down so the
    /// capture press cannot act. Session-only, never persisted.
    pub(crate) capturing: Option<String>,
    /// Last fit-window computation, for the status readout while the mode is
    /// on: ratio, drawn, panel, inner, ppp. Debugging bars means comparing
    /// these four numbers, so they are shown rather than logged.
    pub(crate) fit_debug: Option<(f32, f32, f32, f32, f32, f32, f32)>,
    /// Slideshow running (advance every `Config::slideshow_secs`). Session
    /// state — always off on launch — with the last advance time so the
    /// first step waits a full interval rather than firing at once.
    pub(crate) slideshow: bool,
    pub(crate) slideshow_last: Instant,
    /// The folder's own order while a shuffled show runs, so stopping the
    /// show puts it back. `None` whenever no shuffled show is running — the
    /// flag and the saved order are set and cleared together, never apart.
    pub(crate) slideshow_saved: Option<Vec<PathBuf>>,
    /// Slideshow paused mid-show. The clock stops and no frames advance;
    /// resuming restarts the interval so a pause never steals time from a
    /// slide. Session-only like the show itself.
    pub(crate) slideshow_paused: bool,

    /// When the window geometry was last written to config, so a resize or
    /// move does not hammer the disk every frame.
    pub(crate) last_geometry_save: Instant,
    pub(crate) next_scan: Instant, // fallback folder poll (only when no OS watch runs)
    /// The OS notification watch for the folder on screen, when the platform
    /// gave us one. **`Some` is what turns the 2 s poll off**: with a watch
    /// running, the watcher thread is the only thing that can tell us a file
    /// changed, and it calls `request_repaint` when it has news — which is why
    /// an idle photo now costs no frames at all.
    ///
    /// Derived every frame from `(folder, recursive, archive)` by
    /// `sync_watcher`, never set at a call site: folder opens, the recursive
    /// toggle and archive open/close all have to restart it, and deriving the
    /// answer is the only way none of them can be forgotten.
    pub(crate) watcher: Option<image_viewer::DirWatcher>,
    /// What `watcher` (or the decision not to have one) refers to. Compared
    /// against the wanted key so the start is attempted **once per folder** —
    /// a share that refuses a watch would otherwise be asked again on every
    /// frame, and each ask is a `stat` plus a thread. A watch that dies keeps
    /// its key, so a folder that cannot hold one polls instead of thrashing.
    pub(crate) watcher_key: Option<(PathBuf, bool)>,
    /// A membership change (a file appeared, vanished or was renamed away) is
    /// owed a rescan. Set by the event drain, cleared when the rescan runs;
    /// deferred rather than dropped by `RESCAN_MIN_GAP`.
    pub(crate) pending_rescan: bool,
    /// Earliest time the next notification-driven rescan may run.
    pub(crate) next_rescan_allowed: Instant,
    /// Content changes waiting for their writer to go quiet, and the last
    /// event seen for each. A path leaves this map when it is re-decoded; the
    /// deadline is `CONTENT_SETTLE` after the most recent event, so a save in
    /// several chunks is one decode of the finished file.
    pub(crate) pending_content: std::collections::HashMap<PathBuf, Instant>,
    /// `DirWatcher::events()` as of the last frame, so the perf counter counts
    /// what this session *received*.
    pub(crate) watch_events_seen: u64,
    /// A folder scan in flight on a background thread. `Some` while the scan
    /// is running; the receiver delivers the new file list when it lands.
    /// Keeping the old gallery until the new one arrives means a rescan never
    /// blanks the picture — the same rule the decode path follows.
    pub(crate) rx_rescan: Option<std::sync::mpsc::Receiver<Vec<PathBuf>>>,
    /// An undo-delete in flight on a background thread. `Some` while trash
    /// operations (list + restore) are running; the receiver delivers the
    /// result when it lands. Runs off the UI thread so a slow Recycle Bin
    /// never freezes the viewer.
    pub(crate) rx_undo_delete: Option<std::sync::mpsc::Receiver<UndoDeleteResult>>,
    /// When an external process was last spawned (user command, Explorer).
    /// Rate-limits spawns so a repeating chord can never fork-bomb the
    /// machine again — see `SPAWN_COOLDOWN`.
    pub(crate) last_external_spawn: Option<Instant>,
    pub(crate) checker: Option<TextureHandle>, // transparency backdrop
    pub(crate) display_dim: Option<(u32, u32)>, // central panel size in physical px
    pub(crate) suppress_resize_redecode: bool, // panel toggle must not re-decode
    /// Resize coalescing: issues one sharper decode per settled size instead of
    /// one per frame, and never blanks the picture. See `ResizeTracker`.
    pub(crate) resize: image_viewer::ResizeTracker,
    pub(crate) waiting_load: bool, // block_while_loading: current image not decoded yet
    /// Index highlighted in the grid (G). Browsing the grid does *not* change
    /// the shown image — only Enter or a click does — so arrowing around a
    /// 2000-image folder costs no re-decodes.
    pub(crate) grid_sel: usize,
    /// Columns the grid last laid out with; Up/Down move by this much.
    pub(crate) grid_cols: usize,
    /// Quarter turns and mirrors applied per image, for this session. Keyed by path
    /// because a transform belongs to a *picture*: turning one photo must not turn
    /// the next one, and returning to a turned photo must still find it turned.
    /// Lives in `lib.rs` so the rule can be tested — see [`image_viewer::ImageEdits`].
    pub(crate) edits: image_viewer::ImageEdits,
    /// What each transform replaced, per file, so `Ctrl+Z` can walk it back.
    /// Session-only, like `edits`: a step is not persisted, and a restart starts
    /// with nothing to undo.
    pub(crate) history: image_viewer::EditHistory,
    /// Crop mode: left-drag frames a crop instead of panning.
    ///
    /// A mode rather than a modifier, because a modifier would have to be held
    /// for the whole gesture and the frame is adjusted after the fact anyway —
    /// the crop is stored as soon as it exists and stays until cleared. Panning is
    /// unreachable while this is on, which is the one real cost; leaving crop mode
    /// is one click and the button says which mode you are in.
    pub(crate) crop_mode: bool,
    /// The crop drag in progress, latched on the press and dropped on release.
    ///
    /// **Latched rather than re-decided**, because the press is a one-frame
    /// event: see [`image_viewer::CropDrag`] for the bug that re-deciding each
    /// frame produced. The path is part of the latch because the frame on screen
    /// belongs to one *file* — if the picture changes mid-drag, the gesture has
    /// to die with it rather than write a crop to whatever arrived.
    pub(crate) crop_drag: Option<CropDragState>,
    /// Selection index the grid's scroll offset was last set for. The offset is
    /// written only when this differs, so manual scrolling is never fought.
    pub(crate) grid_sel_scrolled: usize,
    /// Last frame's grid scroll offset and viewport height, measured from the
    /// `show_rows` output — not set by us, so wheel and scrollbar drags stay
    /// in sync for free. The follow decision reads these; unmeasured (grid
    /// just opened) they are 0 and the selection pins to the top instead.
    pub(crate) grid_scroll: f32,
    pub(crate) grid_view_h: f32,
    /// "User intent -> useful pixels" tracking for the perf report.
    pub(crate) lat: Latency,
    /// First frame's timestamp, for startup latency.
    pub(crate) t_boot: Option<Instant>,
    /// Start of the previous frame, for the frame-interval metric.
    pub(crate) t_prev_frame: Option<Instant>,
    /// The window is created hidden; shown on the first frame so the user never
    /// sees a blank compositor frame before the first texture uploads.
    pub(crate) first_frame: bool,
    pub(crate) pending_advance: Option<bool>, // queued step, consumed after one painted frame
    /// Folder navigation stack: parent folders we descended from, so the grid
    /// can offer an "up arrow" to go back one level. Each entry is a folder we
    /// left to enter a subfolder. Pop to go up.
    pub(crate) folder_stack: Vec<PathBuf>,
    pub(crate) zip_archive: Option<PathBuf>, // archive currently open (cbz/zip/7z/rar)
    pub(crate) zip_temp: Option<PathBuf>,    // its extracted temp dir
    /// The folder the current archive was entered **from**, and the index the
    /// archive sat at in it. Left by `enter_archive`, spent by `leave_archive`.
    ///
    /// **One slot, not a stack, and one level is provably enough.**
    /// `archive_member_name` filters a member by `SUPPORTED_EXTENSIONS`, which
    /// does not contain `zip`, so an archive can never contain a browsable
    /// archive: there is nothing a second level could hold. A stack would be
    /// code for a case that cannot arrive.
    ///
    /// Holding the *index* rather than the whole list is deliberate: the parent
    /// folder is rescanned on the way out, so a file added or deleted while the
    /// reader was inside the archive is reflected, and the cursor lands on the
    /// archive by name rather than on whatever now occupies that slot.
    pub(crate) archive_parent: Option<(PathBuf, PathBuf)>,
    /// The save-as panel: open or closed, and everything it is holding.
    ///
    /// **Not a `Config` field.** Every other dialog-ish value in this app is
    /// either a mode that should survive a restart (fit window, sort order) or
    /// something a native file dialog owns. Save-as is neither: it is a panel
    /// opened for one save and dismissed after it, and persisting a half-typed
    /// path and a quality slider would mean a settings entry for a value nobody
    /// wants to have remembered.
    pub(crate) save_as: SaveAsState,
    /// A copy of a **dropped** file, in a directory this app owns, for as long as
    /// that file is open.
    ///
    /// **The second half of the same slot as `zip_temp`**, and released with it by
    /// `close_transients`. A drop is a hand-off rather than a reference -
    /// Explorer extracts a zip member to a temp file and deletes it after the
    /// drop returns - so opening one in place is opening a file with an expiry
    /// date on it, and the 2 s poll then finds a *changed* file and blanks it.
    pub(crate) drop_copy: Option<image_viewer::DropCopy>,
    /// The path whose copy is being made right now, or `None` when none is.
    ///
    /// **One drop in flight, and this is the identity of it rather than a
    /// flag.** A second drop overwrites the first, and the first's copy then
    /// lands against a request that is no longer the one the reader made — so it
    /// is recognised by comparing the message's `src` against this and thrown
    /// away. A counter would work the same way and would be one more thing to
    /// keep in step.
    pub(crate) pending_drop: Option<PathBuf>,
    /// Where `start_drop_copy` sends its answer, and how it asks for a repaint.
    ///
    /// **Its own channel and thread rather than the loader's queue**, and the
    /// reason is not tidiness: a drop copy is not a decode, so it is not in
    /// `plan_jobs`' answer, and `Loader::replace` makes the queue match the plan
    /// *exactly*. A copy job sitting in that queue is therefore discarded by the
    /// next `schedule()` — which runs on the next step, the next resize or the
    /// next dwell expiry — and never runs at all, so the drop silently does
    /// nothing. That is the bug this whole path exists to prevent, arrived at
    /// from the other direction. A plain thread cannot be discarded that way.
    ///
    /// One thread, not a pool: `pending_drop` is a single slot, so at most one
    /// copy is ever outstanding.
    pub(crate) drop_tx: Sender<Msg>,
    pub(crate) drop_wake: Context,
    /// The wallpaper presses' issuance token, Arc'd for its workers.
    ///
    /// A wallpaper press increments this and stamps its message with the value
    /// it got; `pump` only displays a result **equal to the counter now**, and
    /// a worker that notices the counter has moved past its stamp aborts
    /// early. That is the "last action wins" rule `pending_drop` encodes,
    /// without trying to stop a thread (it cannot be stopped — see
    /// `drop_tx`). One press at a time is the shape of the action, so one
    /// counter, not a queue, is enough.
    pub(crate) wallpaper_order: Arc<std::sync::atomic::AtomicU64>,
    /// The user's own pairing decisions for the gallery on screen: which pages take
    /// a whole turn, and how many leading pages stand alone. Loaded when a gallery
    /// is opened, and one file's entries are dropped by `invalidate_path`, because
    /// a re-export can change a file's page count and an override naming page 3 of
    /// a one-page document is a decision about pixels that no longer exist.
    pub(crate) spreads: GallerySpreads,
    /// The gallery `spreads` belongs to, as the overrides file keys it. **For an
    /// archive this is the ORIGINAL archive path**, never the `%TEMP%` extraction
    /// directory, whose name changes on every re-extract and would orphan every
    /// override the first time the `.cbz` was edited. It is a field rather than
    /// something `save_spreads` re-derives so a save can never write to a different
    /// key than the load read.
    pub(crate) spreads_key: Option<String>,
    pub(crate) wheel_acc: f32, // accumulated wheel delta in wheel-navigation mode
    pub(crate) thumbs: HashMap<PathBuf, Thumb>, // grid thumbnails
    pub(crate) thumb_tries: HashMap<PathBuf, u8>, // per-path thumb attempts (bounded retry)
    /// Sidecar ratings for grid cells, filled by the grid's thumbnail pass —
    /// one worker read per newly visible cell, never a sidecar stat per cell
    /// per frame. Separate from the cursor's `rating` field on purpose: that
    /// holds one image's rating, and reading it per cell would show every
    /// cell the current image's stars.
    pub(crate) grid_ratings: HashMap<PathBuf, image_viewer::Ratings>,
    /// Sidecar reads in flight, so a cell whose answer has not landed yet is
    /// not re-asked every frame. Cleared on arrival in `pump`, and by an edit
    /// that wrote a fresher value than the in-flight read could hold.
    pub(crate) grid_rating_pending: HashSet<PathBuf>,
    pub(crate) gallery_view: bool, // grid browser open

    /// Back/forward navigation history: where the reader has been, in the order
    /// they were visited. The most recent entry is the last one pushed — going
    /// back pops it and pushes the current position onto the forward stack.
    pub(crate) history_back: Vec<HistoryEntry>,
    /// Positions reachable by going forward: cleared on every normal navigation,
    /// pushed to by going back.
    pub(crate) history_forward: Vec<HistoryEntry>,
    /// Cap on each history stack so a long session cannot grow without bound.
    /// 50 entries each covers an hour of steady browsing and costs nothing.
    pub(crate) history_cap: usize,
    /// True while `goto` is being driven by back/forward, so the current
    /// position is not pushed onto the back stack a second time.
    pub(crate) history_navigating: bool,

    /// Favourite folders: bookmarked locations the reader can jump to from
    /// Settings. Stored as absolute paths; a favourite whose folder no longer
    /// exists is silently skipped when opened.
    #[allow(dead_code)]
    pub(crate) favourite_folders: Vec<PathBuf>,

    /// Every file in the folder, **unfiltered**. `gallery.files` is this list
    /// run through `filter`, so a filter is always reversible and the folder's
    /// real contents are never thrown away.
    pub(crate) all_files: Vec<PathBuf>,
    /// Case-insensitive substring match on the file name. Empty = show all.
    /// Deliberately not persisted: a filter that silently survived a restart
    /// would hide files with no obvious way back.
    pub(crate) filter: String,
    /// `Stamp` per gallery file as of the last poll. A stamp that moved means
    /// the file was edited on disk and anything cached for it is stale.
    pub(crate) stamps: HashMap<PathBuf, Stamp>,
    /// Grid rows currently on screen, so PageUp/PageDown can move a page.
    pub(crate) grid_page_rows: usize,
}

/// The app's [`PageCtx`]: the gallery, plus the pairing decisions the reader has
/// stored for it.
///
/// A wrapper rather than an `impl PageCtx for Gallery` because the gallery cannot
/// reach the overrides — they live on `MyApp`, beside the gallery they describe.
/// That matters more than it looks: an override **replaces** the aspect-ratio
/// detector, including when the detector is confidently wrong, which is the whole
/// reason the setting exists. A `Gallery` asked on its own answers `None` and the
/// user's decision is dropped from every pairing.
///
/// Private, like every other item in this binary: nothing outside the crate can
/// name it. And `main.rs` is unreachable from every integration test, which is
/// this project's stated reason for keeping the pairing *rules* in `lib.rs`
/// where `tests/spreads.rs` can drive them.
pub(crate) struct AppPageCtx<'a> {
    pub(crate) gallery: &'a Gallery,
    pub(crate) spreads: &'a GallerySpreads,
}

impl PageCtx for AppPageCtx<'_> {
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
        self.gallery.step(from, by)
    }

    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
        self.gallery.step_no_wrap(from, by)
    }

    fn key(&self, pos: PagePos) -> Option<ImgKey> {
        self.gallery.key(pos)
    }

    fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
        self.gallery.dims(pos)
    }

    /// The one method that does not delegate, for the reason the struct exists:
    /// the store is keyed by file name and page, so the key the position already
    /// resolved to is what it is looked up by.
    fn override_shape(&self, pos: PagePos) -> Option<Shape> {
        let key = self.key(pos)?;
        self.spreads.shape_for(&key.path, key.page)
    }

    fn flat(&self, pos: PagePos) -> Option<usize> {
        self.gallery.flat(pos)
    }
}
