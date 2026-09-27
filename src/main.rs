//! Speed-first image viewer.
//!
//! How it stays fast without losing quality:
//!   * All decoding happens on a small worker pool; the UI thread never blocks.
//!   * Stage 1 (PREVIEW): JPEGs are decoded with DCT scaling to ~4K. Enough for a
//!     pixel-sharp fit-to-window on a 4K screen, ready in ~1/3 s for a 95 MP file.
//!     Formats with no reduced-decode shortcut (PNG, ...) and small files are
//!     fully decoded once instead; the preview is downscaled from that result
//!     and the full-res image is reused, never decoded twice.
//!   * Stage 2 (FULL): if the source is bigger than the preview, the untouched
//!     full-resolution pixels are decoded in the background (after a short dwell,
//!     so holding an arrow key doesn't start useless decodes).
//!   * Full-res pixels are never uploaded as one giant texture. They are cut into
//!     1024 px tiles which are converted + uploaded lazily, only when zoomed in
//!     past the preview's density, and only for tiles that are actually on screen.
//!   * Neighbouring images get their previews preloaded, so arrow-key navigation
//!     is usually instant.

// No console window in release builds (GUI app). Debug keeps the console so
// eprintln! diagnostics are visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod fileassoc;

use egui::{Color32, ColorImage, Context, Pos2, Rect, TextureHandle, TextureOptions, Vec2};
use nom_exif::{read_exif, ExifTag};
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use image_viewer::perf;
use image_viewer::{
    buffer_has_alpha, extract_zip, fit_scale_no_upscale, grid_cols, is_archive_path, load_image_full,
    load_image_preview_full, rotated_footprint, set_downscale_filter, set_linear_resample,
    scan_folder_sorted, Animation, Config, DownscaleFilter, Gallery, ImageData, SortMode,
    GRID_CELL, GRID_LABEL_H, GRID_ROW_H, MAX_FULL_PIXELS, PREVIEW_TEXTURE_DIM,
};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Wheel delta accumulated before a flip triggers in wheel-navigation mode.
const THRESH_WHEEL: f32 = 60.0;

/// How many images on each side of the current one get their preview preloaded.
const PRELOAD_RADIUS: usize = 3;
/// Max cached preview textures (each is at most ~8192 px wide, typically ~4K).
const PREVIEW_CACHE_CAP: usize = 16;
/// Full-res tile edge in source pixels.
const TILE: u32 = 1024;
/// Max tiles converted+uploaded per frame (keeps frames smooth while zooming).
const TILES_PER_FRAME: usize = 8;
/// Max full-res tile textures kept on the GPU (64 x 4 MB = 256 MB).
const MAX_TILES: usize = 64;
/// Wait this long on an image before starting its full-res decode.
const FULL_DWELL: Duration = Duration::from_millis(120);
/// Poll the folder for new/deleted files this often (watcher).
const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Highest zoom, in screen points per source pixel.
const MAX_SCALE: f32 = 32.0;

// Job priorities (lower runs first).
const PRIO_CURRENT_PREVIEW: u32 = 0;
const PRIO_CURRENT_EXIF: u32 = 5;
const PRIO_FULL: u32 = 15;
const PRIO_THUMB: u32 = 45;

fn main() -> eframe::Result<()> {
    // CLI: `--register` / `--unregister` manage file associations and exit;
    // any other argument is a file or folder to open.
    let args: Vec<String> = std::env::args().skip(1).collect();
    // `--perf` / `PERF=1` turns on latency sampling; counters are always on.
    let perf_on = args.iter().any(|a| a == "--perf")
        || std::env::var("PERF").map(|v| v != "0").unwrap_or(false);
    image_viewer::perf::mark_boot();
    image_viewer::perf::set_enabled(perf_on);
    match args.first().map(|s| s.as_str()) {
        Some("--register") => {
            return match fileassoc::register() {
                Ok(id) => {
                    println!("Registered as '{id}'. Use Explorer's 'Open with > Choose another app' to set it as default.");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("Registration failed: {e}");
                    std::process::exit(1);
                }
            };
        }
        Some("--unregister") => {
            return match fileassoc::unregister() {
                Ok(true) => {
                    println!("Unregistered.");
                    Ok(())
                }
                Ok(false) => {
                    println!("Was not registered.");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("Unregistration failed: {e}");
                    std::process::exit(1);
                }
            };
        }
        _ => {}
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Image Viewer",
        options,
        Box::new(|cc| Box::new(MyApp::new(cc))),
    )
}

// ---------------------------------------------------------------------------
// Background loader: a tiny priority queue served by a few worker threads
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Preview,
    Full,
    Exif,
    Thumb,
}

/// Long side of grid thumbnails.
const THUMB_DIM: u32 = 256;
/// Max grid thumbnails kept on the GPU (256 x ~0.3 MB ≈ 80 MB).
const THUMB_CACHE_CAP: usize = 512;
/// Decode attempts per file before a grid cell gives up (permanent
/// placeholder). Enough for transient failures, bounded so a corrupt file
/// can't keep the worker pool busy.
const THUMB_TRIES: u8 = 3;

struct Job {
    path: PathBuf,
    kind: Kind,
    prio: u32,
    /// Display target long side in physical pixels (preview jobs only).
    target_dim: u32,
}

#[derive(Default)]
struct QueueState {
    jobs: Vec<Job>,
    busy: HashSet<(PathBuf, Kind)>,
}

/// A decoded gallery-grid thumbnail.
///
/// `texels` is the *decoded* pixel size of the uploaded texture, recorded at
/// upload time so the cell layout never has to ask the texture manager.
struct Thumb {
    tex: TextureHandle,
    texels: [usize; 2],
    /// LRU stamp (frame clock).
    used: u64,
}

/// Tracks one "user intent -> useful pixels" transition so the perf module can
/// report real latencies instead of decode times.
///
/// The tricky part is *first visible pixels*: a preview arriving on the worker
/// is not on screen until the frame that paints it has been presented. We only
/// know a frame was presented when the next one starts, so a paint is recorded
/// as pending and committed in [`MyApp::frame_prelude`].
#[derive(Default)]
struct Latency {
    /// When the user asked for the current image.
    t_request: Option<Instant>,
    /// False for a fresh open, true for next/prev (reported separately).
    is_nav: bool,
    /// Set when the requested image was painted during the last frame.
    painted: Option<Instant>,
    recorded_first: bool,
    /// A preview for the requested image exists (fit-to-window quality).
    t_preview: Option<Instant>,
    /// A full-resolution decode of it has landed.
    t_full: Option<Instant>,
    /// A stall was already reported for this request (report once, not per
    /// frame).
    stall_logged: bool,
    /// Zoom / pan / gallery / scan in flight.
    t_zoom: Option<Instant>,
    t_pan: Option<Instant>,
    t_gallery: Option<Instant>,
    t_scan: Option<Instant>,
    /// Tiles wanted by a zoom/pan, satisfied when the visible set is complete.
    want_viewport_tiles: bool,
}

impl Latency {
    fn begin(&mut self, now: Instant, is_nav: bool) {
        self.t_request = Some(now);
        self.is_nav = is_nav;
        self.stall_logged = false;
        self.painted = None;
        self.recorded_first = false;
        self.t_preview = None;
        self.t_full = None;
        self.want_viewport_tiles = false;
    }

    /// The requested image reached fit-to-window quality.
    fn preview(&mut self, now: Instant) {
        if self.t_preview.is_none() {
            self.t_preview = Some(now);
        }
    }

    fn full(&mut self, now: Instant) {
        if self.t_full.is_none() {
            self.t_full = Some(now);
            if let (Some(req), Some(first)) = (self.t_request, self.painted) {
                let _ = first;
                perf::record(
                    if self.is_nav {
                        perf::NAV_TO_FULL
                    } else {
                        perf::SELECT_TO_FULL
                    },
                    now.duration_since(req).as_micros() as u64,
                );
            }
        }
    }

    /// The requested image was painted this frame; commit next frame.
    fn painted(&mut self, now: Instant) {
        self.painted = Some(now);
    }

    /// Called at the start of every frame: the previous frame has been shown.
    fn commit_frame(&mut self) {
        if let (Some(painted), Some(req), false) =
            (self.painted.take(), self.t_request, self.recorded_first)
        {
            self.recorded_first = true;
            perf::record(
                if self.is_nav {
                    perf::NAV_TO_FIRST_PIXELS
                } else {
                    perf::SELECT_TO_FIRST_PIXELS
                },
                painted.duration_since(req).as_micros() as u64,
            );
        }
        if let (Some(prev), Some(req)) = (self.t_preview, self.t_request) {
            perf::record(perf::SELECT_TO_PREVIEW, prev.duration_since(req).as_micros() as u64);
            self.t_preview = None;
        }
    }

    /// The viewport's tiles are all present: zoom/pan is satisfied.
    fn viewport_tiles_ready(&mut self, now: Instant) {
        if !self.want_viewport_tiles {
            return;
        }
        self.want_viewport_tiles = false;
        if let Some(t) = self.t_zoom.take() {
            perf::record_since(perf::ZOOM_TO_HIRES, t);
        } else if let Some(t) = self.t_pan.take() {
            perf::record_since(perf::PAN_TO_TILES, t);
        }
        let _ = now;
    }
}

struct PreviewData {
    image: ColorImage, // already converted on the worker thread
    orig: (u32, u32),  // source dimensions
    load_ms: u64,
    // Full-res image when it was already decoded in the same pass
    // (small files, and formats like PNG with no fast preview path).
    full: Option<Arc<ImageData>>,
    // Decoded frames when the file is an animated GIF.
    anim: Option<Arc<Animation>>,
    // Any pixel is translucent (draw checkerboard behind it).
    has_alpha: bool,
    // Texture is below source density (re-decode if display grows).
    below_target: bool,
}

enum Msg {
    Preview {
        path: PathBuf,
        result: Result<PreviewData, String>,
    },
    Thumb {
        path: PathBuf,
        result: Result<PreviewData, String>,
    },
    Full {
        path: PathBuf,
        result: Result<Arc<ImageData>, String>,
    },
    Exif {
        path: PathBuf,
        info: Option<String>,
    },
}

struct Loader {
    shared: Arc<(Mutex<QueueState>, Condvar)>,
}

impl Loader {
    /// Queue a job without dropping existing queued work (used by the grid for
    /// on-demand thumbnail requests). A job that is already queued or in
    /// flight is left alone — this dedup is the *only* one the grid needs.
    ///
    /// Note `push` does not survive `replace()`: `schedule()` calls `replace()`
    /// to drop stale work, which throws queued thumbnails away mid-grid. The
    /// grid therefore re-requests every frame instead of keeping a
    /// "requested" set, which could desync from the queue and strand a cell as
    /// a permanent placeholder.
    fn push(&self, job: Job) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        if q.busy.contains(&(job.path.clone(), job.kind)) {
            return;
        }
        if q.jobs.iter().any(|j| j.path == job.path && j.kind == job.kind) {
            return;
        }
        q.jobs.push(job);
        cv.notify_all();
    }

    fn new(ctx: &Context, tx: Sender<Msg>) -> Self {
        let shared: Arc<(Mutex<QueueState>, Condvar)> =
            Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .clamp(2, 4);

        for _ in 0..workers {
            let shared = shared.clone();
            let tx = tx.clone();
            let ctx = ctx.clone();
            std::thread::spawn(move || loop {
                let job = {
                    let (lock, cv) = &*shared;
                    let mut q = lock.lock().unwrap();
                    loop {
                        let best = q
                            .jobs
                            .iter()
                            .enumerate()
                            .min_by_key(|(_, j)| j.prio)
                            .map(|(i, _)| i);
                        if let Some(i) = best {
                            let job = q.jobs.swap_remove(i);
                            q.busy.insert((job.path.clone(), job.kind));
                            break job;
                        }
                        q = cv.wait(q).unwrap();
                    }
                };

                // A decoder panic on a corrupt file must not kill the worker.
                let msg = match std::panic::catch_unwind(AssertUnwindSafe(|| run_job(&job))) {
                    Ok(m) => m,
                    Err(payload) => {
                        // Keep the message: a bare "decoder panicked" hid the
                        // most likely cause (allocation failure under memory
                        // pressure) behind an unreproducible symptom.
                        perf::count(perf::C_DECODE_PANICS, 1);
                        let detail = payload
                            .downcast_ref::<&str>()
                            .map(|s| (*s).to_string())
                            .or_else(|| payload.downcast_ref::<String>().cloned())
                            .unwrap_or_else(|| "unknown".to_string());
                        fail_msg(&job, format!("decoder panicked: {detail}"))
                    }
                };

                let alive = tx.send(msg).is_ok();
                shared.0.lock().unwrap().busy.remove(&(job.path, job.kind));
                if !alive {
                    return; // UI is gone
                }
                ctx.request_repaint();
            });
        }
        Self { shared }
    }

    /// Replace everything that is still *queued* with `jobs` (in-flight work is left
    /// alone and de-duplicated). This is what makes stale requests disappear when
    /// the user navigates quickly.
    fn replace(&self, jobs: Vec<Job>) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        let busy = q.busy.clone();
        let before = q.jobs.len();
        q.jobs = jobs
            .into_iter()
            .filter(|j| !busy.contains(&(j.path.clone(), j.kind)))
            .collect();
        // Coalescing: how much queued work a navigation threw away. High values
        // mean the user outran the decoder, which is the thing to tune.
        if before > q.jobs.len() {
            perf::count(perf::C_JOBS_DROPPED, (before - q.jobs.len()) as u64);
        }
        cv.notify_all();
    }
}

fn fail_msg(job: &Job, err: String) -> Msg {
    match job.kind {
        Kind::Preview => Msg::Preview {
            path: job.path.clone(),
            result: Err(err),
        },
        Kind::Full => Msg::Full {
            path: job.path.clone(),
            result: Err(err),
        },
        Kind::Thumb => Msg::Thumb {
            path: job.path.clone(),
            result: Err(err),
        },
        Kind::Exif => Msg::Exif {
            path: job.path.clone(),
            info: None,
        },
    }
}

fn run_job(job: &Job) -> Msg {
    let path = job.path.clone();
    match job.kind {
        Kind::Preview => Msg::Preview {
            result: decode_preview(&path, job.target_dim),
            path,
        },
        Kind::Thumb => Msg::Thumb {
            result: decode_preview(&path, job.target_dim),
            path,
        },
        Kind::Full => Msg::Full {
            result: load_image_full(&path)
                .map(Arc::new)
                .map_err(|e| e.to_string()),
            path,
        },
        Kind::Exif => Msg::Exif {
            info: extract_exif(&path),
            path,
        },
    }
}

/// Decode a preview sized for the current display area (in physical pixels).
/// The result is the full-res image when it fits the target, otherwise a
/// texture capped at the target long side — always sharp at fit-to-window.
fn decode_preview(path: &Path, target_dim: u32) -> Result<PreviewData, String> {
    // Single pass: reads the file once. Large JPEGs get a cheap DCT-scaled
    // preview; everything else is fully decoded once and the preview is
    // derived from that, so the full image is already available.
    let r = load_image_preview_full(path, target_dim).map_err(|e| e.to_string())?;
    perf::count(perf::C_DECODE_CALLS, 1);
    let image = ColorImage::from_rgba_unmultiplied(
        [r.preview.width as usize, r.preview.height as usize],
        &r.preview.rgba,
    );
    let orig = r.orig.unwrap_or((r.preview.width, r.preview.height));
    Ok(PreviewData {
        image,
        orig,
        load_ms: r.preview.load_time.as_millis() as u64,
        full: r.full.map(Arc::new),
        anim: r.anim.map(Arc::new),
        has_alpha: buffer_has_alpha(&r.preview.rgba),
        below_target: r.below_target,
    })
}

fn extract_exif(path: &Path) -> Option<String> {
    let exif = read_exif(path).ok()?;

    let mut info = Vec::new();

    if let Some(entry) = exif.get(ExifTag::Make) {
        info.push(format!("Camera: {}", entry));
    }
    if let Some(entry) = exif.get(ExifTag::Model) {
        info.push(format!("Model: {}", entry));
    }
    if let Some(entry) = exif.get(ExifTag::LensModel) {
        info.push(format!("Lens: {}", entry));
    }
    if let Some(entry) = exif.get(ExifTag::Software) {
        info.push(format!("Software: {}", entry));
    }
    if let Some(entry) = exif.get(ExifTag::DateTimeOriginal) {
        info.push(format!("Date: {}", entry));
    }

    // Photo settings, joined on one line like a camera EXIF summary.
    let mut shot = Vec::new();
    if let Some(entry) = exif.get(ExifTag::FNumber) {
        if let Some(v) = entry.as_urational().and_then(|r| r.to_f64()) {
            shot.push(format!("f/{v:.1}"));
        }
    }
    if let Some(entry) = exif.get(ExifTag::ExposureTime) {
        if let Some(r) = entry.as_urational() {
            let (n, d) = (r.numerator(), r.denominator());
            let text = if d == 0 {
                format!("{n} s")
            } else if n >= d {
                format!("{:.1} s", n as f64 / d as f64)
            } else {
                format!("1/{} s", (d as f64 / n.max(1) as f64).round() as u64)
            };
            shot.push(text);
        }
    }
    if let Some(entry) = exif.get(ExifTag::ISOSpeedRatings) {
        if let Some(iso) = entry
            .as_u16()
            .map(|iso| iso as u32)
            .or_else(|| entry.as_u32().map(|iso| iso as u32))
            .map(|iso| iso as u16)
        {
            shot.push(format!("ISO {iso}"));
        }
    }
    if let Some(entry) = exif.get(ExifTag::FocalLength) {
        if let Some(v) = entry.as_urational().and_then(|r| r.to_f64()) {
            shot.push(format!("{v:.0} mm"));
        }
    }
    if let Some(entry) = exif.get(ExifTag::ExposureBiasValue) {
        if let Some(v) = entry.as_irational().and_then(|r| r.to_f64()) {
            if v != 0.0 {
                shot.push(format!("{v:+.1} EV"));
            }
        }
    }
    if let Some(entry) = exif.get(ExifTag::DigitalZoomRatio) {
        if let Some(v) = entry.as_urational().and_then(|r| r.to_f64()) {
            if v > 1.0 {
                shot.push(format!("zoom x{v:.1}"));
            }
        }
    }
    if !shot.is_empty() {
        info.push(format!("Shot: {}", shot.join(" · ")));
    }

    if let Some(entry) = exif.get(ExifTag::MeteringMode) {
        if let Some(v) = entry.as_u16() {
            if v > 0 {
                info.push(format!("Metering: {}", metering_label(v)));
            }
        }
    }
    if let Some(entry) = exif.get(ExifTag::Flash) {
        if let Some(v) = entry.as_u16() {
            info.push(format!("Flash: {}", flash_label(v)));
        }
    }
    if let Some(entry) = exif.get(ExifTag::ColorSpace) {
        if let Some(v) = entry.as_u16() {
            info.push(format!("Color: {}", color_space_label(v)));
        }
    }
    if let Some(entry) = exif.get(ExifTag::Orientation) {
        if let Some(v) = entry.as_u16() {
            if v > 1 {
                info.push(format!("Orientation: {}", orientation_label(v)));
            }
        }
    }
    if let Some(entry) = exif.get(ExifTag::ImageWidth) {
        if let Some(h_entry) = exif.get(ExifTag::ImageHeight) {
            info.push(format!("Dimensions: {}x{} px", entry, h_entry));
        }
    }

    // GPS: decimal degrees + altitude, and a Google Maps link.
    if let Some((lat, lon)) = image_viewer::gps_coords(&exif) {
        info.push(format!("Location: {:.5}, {:.5}", lat, lon));
        if let Some(alt) = image_viewer::gps_altitude(&exif) {
            info.push(format!("Altitude: {alt:.0} m"));
        }
        info.push(format!(
            "Maps: https://maps.google.com/?q={:.6},{:.6}",
            lat, lon
        ));
    }

    if info.is_empty() {
        None
    } else {
        Some(info.join("\n"))
    }
}

fn orientation_label(v: u16) -> &'static str {
    match v {
        1 => "Normal",
        2 => "Mirrored",
        3 => "Rotated 180°",
        4 => "Flipped",
        5 => "Transposed",
        6 => "Rotated 90° CW",
        7 => "Transverse",
        8 => "Rotated 270° CW",
        _ => "Unknown",
    }
}

fn metering_label(v: u16) -> &'static str {
    match v {
        1 => "Average",
        2 => "Center-weighted",
        3 => "Spot",
        4 => "Multi-spot",
        5 => "Multi-segment",
        6 => "Partial",
        _ => "Other",
    }
}

fn color_space_label(v: u16) -> &'static str {
    match v {
        1 => "sRGB",
        0xFFFF => "Uncalibrated",
        _ => "Other",
    }
}

fn flash_label(v: u16) -> &'static str {
    if v & 0x1 == 1 {
        "Fired"
    } else {
        "No flash"
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

struct Preview {
    tex: TextureHandle,
    orig: (u32, u32),
    load_ms: u64,
    used: u64, // LRU stamp
    // Full-res image produced by the same decode pass (single-pass files).
    // Kept on the cached preview so it can be adopted when the user
    // navigates here, instead of being decoded again.
    full: Option<Arc<ImageData>>,
    // Decoded animation frames when the file is an animated GIF.
    anim: Option<Arc<Animation>>,
    // Any pixel is translucent (draw checkerboard behind it).
    has_alpha: bool,
    // Texture is below source density (re-decode if display grows).
    below_target: bool,
}

/// Playback state for the animation currently on screen.
struct AnimView {
    path: PathBuf,
    anim: Arc<Animation>,
    tex: TextureHandle,
    start: Instant,
    last_index: usize,
}

struct Tile {
    tex: TextureHandle,
    last_drawn: u64,
}

struct FullImage {
    path: PathBuf,
    data: Arc<ImageData>,
    tiles: HashMap<(u32, u32), Tile>,
    load_ms: u64,
}

/// `scale` / `offset` only matter while `fit` is false.
struct View {
    fit: bool,
    scale: f32,   // screen points per source pixel
    offset: Vec2, // image centre relative to panel centre, in points
}

struct MyApp {
    folder: PathBuf,
    gallery: Gallery,
    config: Config,
    config_path: PathBuf,

    loader: Loader,
    rx: Receiver<Msg>,

    previews: HashMap<PathBuf, Preview>,
    failed: HashSet<(PathBuf, Kind)>,
    clock: u64,
    frame: u64,

    cur: Option<PathBuf>,   // what the user asked for
    shown: Option<PathBuf>, // what is on screen (lags `cur` until its preview is ready)
    full: Option<FullImage>,
    full_due: Option<Instant>,
    view: View,
    error: Option<String>,
    notice: Option<String>, // transient feedback for file actions
    anim: Option<AnimView>, // playing animation (animated GIF)

    exif_info: Option<String>,
    exif_loading: bool,
    show_panels: bool,
    next_scan: Instant, // periodic folder rescan (watcher)
    checker: Option<TextureHandle>, // transparency backdrop
    display_dim: Option<(u32, u32)>, // central panel size in physical px
    suppress_resize_redecode: bool, // panel toggle must not re-decode
    waiting_load: bool, // block_while_loading: current image not decoded yet
    /// Index highlighted in the grid (G). Browsing the grid does *not* change
    /// the shown image — only Enter or a click does — so arrowing around a
    /// 2000-image folder costs no re-decodes.
    grid_sel: usize,
    /// Columns the grid last laid out with; Up/Down move by this much.
    grid_cols: usize,
    /// Quarter turns clockwise applied to the shown image (0..4).
    rot: u8,
    /// Selection index the grid's scroll offset was last set for. The offset is
    /// written only when this differs, so manual scrolling is never fought.
    grid_sel_scrolled: usize,
    /// "User intent -> useful pixels" tracking for the perf report.
    lat: Latency,
    /// First frame's timestamp, for startup latency.
    t_boot: Option<Instant>,
    pending_advance: Option<bool>, // queued step, consumed after one painted frame
    zip_archive: Option<PathBuf>, // archive currently open (cbz/zip)
    zip_temp: Option<PathBuf>, // its extracted temp dir
    wheel_acc: f32, // accumulated wheel delta in wheel-navigation mode
    thumbs: HashMap<PathBuf, Thumb>, // grid thumbnails
    thumb_tries: HashMap<PathBuf, u8>, // per-path thumb attempts (bounded retry)
    gallery_view: bool, // grid browser open
}

impl MyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let config_path = Config::default_path();
        let config = Config::load(&config_path);

        // Optional CLI argument: a file or a folder ("Open with…").
        let arg = std::env::args_os().nth(1).map(PathBuf::from);
        let explicit_file = arg.as_ref().is_some_and(|a| a.is_file());
        // An archive argument is extracted up front and opened as its temp dir.
        let mut zip_archive: Option<PathBuf> = None;
        let mut zip_temp: Option<PathBuf> = None;
        let (folder, mut gallery) = match arg {
            Some(a) if a.is_dir() => {
                let g = Gallery::new(&a);
                (a, g)
            }
            Some(a) if a.is_file() && is_archive_path(&a) => {
                let temp = Self::zip_temp_for(&a);
                match extract_zip(&a, &temp) {
                    Ok((count, _)) if count > 0 => {
                        zip_archive = Some(a.clone());
                        zip_temp = Some(temp.clone());
                        (temp.clone(), Gallery::new(&temp))
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
                let folder = config
                    .folder
                    .as_ref()
                    .map(PathBuf::from)
                    .filter(|p| p.is_dir())
                    .unwrap_or_else(|| PathBuf::from("test-images"));
                let g = Gallery::new(&folder);
                (folder, g)
            }
        };
        if config.load_all_on_startup {
            gallery.load_all_from_folder(&folder);
            // Sort per config (natural name order by default) and resume the
            // last-viewed image for this folder — but only when the user
            // didn't explicitly open a file; an explicit file wins.
            let files = scan_folder_sorted(&folder, config.sort);
            if !explicit_file {
                if let Some(last) = config.last_by_folder.get(&folder.to_string_lossy().to_string()) {
                    // Stored as a file name (so archives and folders share the
                    // format); match on the name.
                    let last = Path::new(last).file_name().map(|n| n.to_string_lossy().into_owned());
                    if let Some(i) = last.and_then(|name| {
                        files.iter().position(|f| {
                            f.file_name().map(|n| n.to_string_lossy().into_owned()) == Some(name.clone())
                        })
                    }) {
                        gallery.index = i;
                    }
                }
            }
            gallery.files = files;
        }

        set_linear_resample(config.linear_resample);
        set_downscale_filter(config.downscale_filter);

        if !config_path.exists() {
            config.save(&config_path);
        }

        let (tx, rx) = mpsc::channel();
        let loader = Loader::new(ctx, tx);

        let mut app = Self {
            folder,
            gallery,
            config,
            config_path,
            loader,
            rx,
            previews: HashMap::new(),
            failed: HashSet::new(),
            clock: 0,
            frame: 0,
            cur: None,
            shown: None,
            full: None,
            full_due: None,
            view: View {
                fit: true,
                scale: 1.0,
                offset: Vec2::ZERO,
            },
                        error: None,
            notice: None,
            anim: None,
            exif_info: None,
            exif_loading: false,
            show_panels: true,
            next_scan: Instant::now() + RESCAN_INTERVAL,
            checker: Some(make_checker_texture(ctx)),
            display_dim: None,
                        suppress_resize_redecode: false,
            waiting_load: false,
            grid_sel: 0,
            grid_cols: 1,
            rot: 0,
            grid_sel_scrolled: usize::MAX,
            lat: Latency::default(),
            t_boot: perf::boot(),
            pending_advance: None,
            zip_archive,
            zip_temp,
            wheel_acc: 0.0,
            thumbs: HashMap::new(),
            thumb_tries: HashMap::new(),
            gallery_view: false,
        };
                app.goto(ctx);
        app
    }

    // ---- navigation -------------------------------------------------------

    /// Point the app at `gallery.current()` and (re)plan all background work.
    fn goto(&mut self, ctx: &Context) {
        // A goto that follows a keypress is navigation; a fresh open/click is a
        // selection. They are reported separately because the preloaded case
        // (0 ms) is the common one for navigation.
        let is_nav = self.lat.t_request.is_some();
        self.lat.begin(Instant::now(), is_nav);
        self.cur = self.gallery.current().map(|p| p.to_path_buf());
        // Preloading hit rate: the new image already had a preview, so the
        // first pixel is free. If this counter stays low, the preload window
        // is aimed wrong.
        if is_nav
            && self
                .cur
                .as_ref()
                .is_some_and(|c| self.previews.contains_key(c))
        {
            perf::count(perf::C_PREVIEW_REUSED, 1);
        }
        self.error = None;
        self.notice = None;
        self.exif_info = None;
        self.exif_loading = self.cur.is_some();
        self.full_due = Some(Instant::now() + FULL_DWELL);
        // Blocking mode: only wait when the preview isn't already cached.
        self.waiting_load = self.config.block_while_loading
            && self
                .cur
                .as_ref()
                .is_some_and(|c| !self.previews.contains_key(c));

        if let Some(name) = self
            .cur
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "Image Viewer - {name}"
            )));
        }
        self.schedule();
    }

    fn step(&mut self, ctx: &Context, forward: bool) {
        // "Wait for load" mode: throttle stepping to decode completion.
        if self.config.block_while_loading && self.waiting_load {
            return;
        }
        if forward {
            self.gallery.next();
        } else {
            self.gallery.prev();
        }
        self.goto(ctx);
    }

    /// Forget what is on screen (used when switching folders).
    fn reset_display(&mut self) {
        self.shown = None;
        self.full = None;
        self.anim = None;
    }

    fn needs_full(&self, p: &PathBuf) -> bool {
        if self.full.as_ref().is_some_and(|f| &f.path == p) {
            return false;
        }
        if self.failed.contains(&(p.clone(), Kind::Full)) {
            return false;
        }
        let Some(pv) = self.previews.get(p) else {
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

    fn schedule(&mut self) {
        let mut jobs = Vec::new();
        let files = &self.gallery.files;
        let n = files.len();
        if n == 0 {
            self.loader.replace(jobs);
            return;
        }
        let i = self.gallery.index.min(n - 1);
        let cur = &files[i];

        let need_preview = |p: &PathBuf| {
            !self.previews.contains_key(p) && !self.failed.contains(&(p.clone(), Kind::Preview))
        };

        // Preview texture target: the display area in physical pixels, so the
        // texture is exactly as sharp as the screen can show.
        let target_dim = self
            .display_dim
            .map(|(w, h)| w.max(h))
            .unwrap_or(PREVIEW_TEXTURE_DIM)
            .clamp(256, PREVIEW_TEXTURE_DIM);

        if need_preview(cur) {
            jobs.push(Job {
                path: cur.clone(),
                kind: Kind::Preview,
                prio: PRIO_CURRENT_PREVIEW,
                target_dim,
            });
        }
        if self.exif_loading {
            jobs.push(Job {
                path: cur.clone(),
                kind: Kind::Exif,
                prio: PRIO_CURRENT_EXIF,
                target_dim: 0,
            });
        }
        if self.full_due.is_none() && self.needs_full(cur) {
            jobs.push(Job {
                path: cur.clone(),
                kind: Kind::Full,
                prio: PRIO_FULL,
                target_dim: 0,
            });
        }

        // Neighbours: "next" slightly ahead of "previous" at each distance.
        let mut seen: HashSet<usize> = HashSet::from([i]);
        for d in 1..=PRELOAD_RADIUS {
            for (idx, prio) in [
                ((i + d) % n, 10 + 2 * d as u32),
                ((i + n - d % n) % n, 11 + 2 * d as u32),
            ] {
                if seen.insert(idx) && need_preview(&files[idx]) {
                    jobs.push(Job {
                        path: files[idx].clone(),
                        kind: Kind::Preview,
                        prio,
                        target_dim,
                    });
                }
            }
        }
        self.loader.replace(jobs);
    }

    // ---- results from workers --------------------------------------------

    fn pump(&mut self, ctx: &Context) {
        let msgs: Vec<Msg> = self.rx.try_iter().collect();
        if msgs.is_empty() {
            return;
        }
        let mut resched = false;

        for m in msgs {
            match m {
                Msg::Preview { path, result } => match result {
                    Ok(p) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let tex = ctx.load_texture(name.as_ref(), p.image, TextureOptions::LINEAR);
                        perf::count(perf::C_TEXTURE_UPLOADS, 1);
                        perf::record(perf::JOB_PREVIEW_MS, p.load_ms * 1000);
                        if Some(&path) == self.cur.as_ref() {
                            self.lat.preview(Instant::now());
                        }
                        self.clock += 1;
                        self.previews.insert(
                            path.clone(),
                            Preview {
                                tex,
                                orig: p.orig,
                                load_ms: p.load_ms,
                                used: self.clock,
                                full: p.full,
                                anim: p.anim,
                                has_alpha: p.has_alpha,
                                below_target: p.below_target,
                            },
                        );
                                                                        resched |= Some(&path) == self.cur.as_ref();
                        // "Wait for load" mode: arrived preview for the
                        // current image unblocks; if the key is still held,
                        // queue one more advance — consumed next frame so the
                        // image is guaranteed to have been painted on screen.
                        if Some(&path) == self.cur.as_ref() && self.waiting_load {
                            self.waiting_load = false;
                            if self.config.block_while_loading {
                                let (held_fwd, held_prev) = ctx.input(|i| {
                                    (
                                        i.key_down(egui::Key::ArrowRight)
                                            || i.key_down(egui::Key::PageDown)
                                            || i.key_down(egui::Key::Space),
                                        i.key_down(egui::Key::ArrowLeft)
                                            || i.key_down(egui::Key::PageUp),
                                    )
                                });
                                if held_fwd {
                                    self.pending_advance = Some(true);
                                } else if held_prev {
                                    self.pending_advance = Some(false);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to load {}: {}", path.display(), e);
                        if Some(&path) == self.cur.as_ref() {
                            self.error = Some(format!("Failed to load {}: {}", path.display(), e));
                            self.waiting_load = false;
                        }
                        self.failed.insert((path, Kind::Preview));
                    }
                },
                Msg::Thumb { path, result } => match result {
                    Ok(p) => {
                        let name = path.file_name().unwrap_or_default().to_string_lossy();
                        let texels = p.image.size;
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
                Msg::Full { path, result } => match result {
                    Ok(data) => {
                        perf::record(perf::JOB_FULL_MS, data.load_time.as_micros() as u64);
                        // Only keep it if the user is still on this image.
                        if Some(&path) == self.cur.as_ref() {
                            self.lat.full(Instant::now());
                            let load_ms = data.load_time.as_millis() as u64;
                            self.full = Some(FullImage {
                                path,
                                data,
                                tiles: HashMap::new(),
                                load_ms,
                            });
                        }
                    }
                    Err(e) => {
                        eprintln!("Full-res load failed for {}: {}", path.display(), e);
                        self.failed.insert((path, Kind::Full));
                    }
                },
                Msg::Exif { path, info } => {
                    if Some(&path) == self.cur.as_ref() {
                        self.exif_info = info;
                        self.exif_loading = false;
                    }
                }
            }
        }
        if resched {
            self.schedule();
        }
        self.evict_previews();
    }

    fn evict_previews(&mut self) {
        while self.previews.len() > PREVIEW_CACHE_CAP {
            let n = self.gallery.files.len();
            let i = self.gallery.index;
            let protected: HashSet<&PathBuf> = (0..=2 * PRELOAD_RADIUS)
                .filter(|_| n > 0)
                .map(|k| (i + n * 2 + k - PRELOAD_RADIUS) % n)
                .filter_map(|idx| self.gallery.files.get(idx))
                .chain(self.shown.iter())
                .collect();
            let victim = self
                .previews
                .iter()
                .filter(|(p, _)| !protected.contains(p))
                .min_by_key(|(_, v)| v.used)
                .map(|(p, _)| p.clone());
            match victim {
                Some(v) => {
                    self.previews.remove(&v);
                }
                None => break,
            }
        }
    }

    /// Drop the least-recently-painted grid thumbnails once the cache is full.
    /// Cells re-request on demand, so eviction only costs a re-decode.
    fn evict_thumbs(&mut self) {
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
    fn sync_shown(&mut self, ctx: &Context) {
        let Some(cur) = self.cur.clone() else {
            self.shown = None;
            self.anim = None;
            return;
        };
        if self.anim.as_ref().is_some_and(|a| &a.path != &cur) {
            self.anim = None;
        }
        if self.previews.contains_key(&cur) && self.shown.as_ref() != Some(&cur) {
            self.shown = Some(cur.clone());
            self.view.fit = true;
        }
        // Adopt the full image that came with the preview's decode pass
        // (single-pass files), whether it just arrived or was preloaded
        // while the user was on a neighbouring image.
        if let Some(pv) = self.previews.get(&cur) {
            if let Some(data) = &pv.full {
                if self.full.as_ref().map_or(true, |f| f.path != cur) {
                    let load_ms = data.load_time.as_millis() as u64;
                    self.full = Some(FullImage {
                        path: cur.clone(),
                        data: Arc::clone(data),
                        tiles: HashMap::new(),
                        load_ms,
                    });
                }
            }
        }
        // Adopt the animation that came with the preview's decode pass
        // (animated GIFs), including preloaded neighbours.
        if let Some(pv) = self.previews.get(&cur) {
            if let Some(anim) = &pv.anim {
                if self.anim.as_ref().map_or(true, |a| &a.path != &cur) {
                    let f0 = &anim.frames[0];
                    let image = ColorImage::from_rgba_unmultiplied(
                        [f0.width as usize, f0.height as usize],
                        &f0.rgba,
                    );
                    let tex = ctx.load_texture("anim", image, TextureOptions::LINEAR);
                    self.anim = Some(AnimView {
                        path: cur.clone(),
                        anim: Arc::clone(anim),
                        tex,
                        start: Instant::now(),
                        last_index: usize::MAX,
                    });
                }
            }
        }
        if let Some(s) = &self.shown {
            if self.full.as_ref().is_some_and(|f| &f.path != s) {
                self.full = None;
            }
        }
    }

    // ---- file pickers -----------------------------------------------------

    fn open_folder(&mut self, ctx: &Context, folder: PathBuf) {
        self.folder = folder.clone();
        self.gallery = Gallery::new(&folder);
        if self.config.load_all_on_startup {
            self.gallery.load_all_from_folder(&folder);
        }
        self.reset_display();
        self.goto(ctx);
    }

    /// Release the extracted temp dir of a previously open archive.
    fn close_zip(&mut self) {
        if let Some(t) = self.zip_temp.take() {
            let _ = std::fs::remove_dir_all(t);
        }
        self.zip_archive = None;
    }

    /// Stable temp dir for an archive; reused when the archive is unchanged.
    fn zip_temp_for(archive: &Path) -> PathBuf {
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
        std::env::temp_dir()
            .join("image-viewer")
            .join(format!("{:016x}", h.finish()))
    }

    /// Open a cbz/zip image archive: extract supported entries to a stable
    /// temp dir and treat it as a folder gallery.
    fn open_archive(&mut self, ctx: &Context, archive: &PathBuf) {
        self.close_zip();
        let temp = Self::zip_temp_for(archive);
        match extract_zip(archive, &temp) {
            Ok((count, encrypted)) if count > 0 => {
                self.zip_archive = Some(archive.clone());
                self.zip_temp = Some(temp.clone());
                self.folder = temp.clone();
                self.gallery = Gallery::new(&temp);
                // Resume the last-viewed member for this archive.
                let key = archive.to_string_lossy().to_string();
                if let Some(last) = self.config.last_by_folder.get(&key) {
                    let last = std::path::Path::new(last)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned());
                    if let Some(name) = last {
                        if let Some(i) = self
                            .gallery
                            .files
                            .iter()
                            .position(|f| {
                                f.file_name().map(|n| n.to_string_lossy().into_owned())
                                    == Some(name.clone())
                            })
                        {
                            self.gallery.index = i;
                        }
                    }
                }
                self.reset_display();
                self.goto(ctx);
                let mut n = format!("Archive: {count} images");
                if encrypted > 0 {
                    n += &format!(" ({encrypted} encrypted skipped)");
                }
                self.notice = Some(n);
            }
            Ok((_, encrypted)) => {
                let msg = if encrypted > 0 {
                    format!("{encrypted} encrypted entries — unsupported")
                } else {
                    "No images inside the archive".to_string()
                };
                self.error = Some(msg);
            }
            Err(e) => self.error = Some(format!("Archive failed: {e}")),
        }
    }

    fn open_file(&mut self, ctx: &Context, file: PathBuf) {
        self.close_zip();
        self.folder = file.parent().unwrap_or(&file).to_path_buf();
        self.gallery = Gallery::from_file(file);
        if self.config.load_all_on_startup {
            self.gallery.load_all_from_folder(&self.folder);
        }
        self.reset_display();
        self.goto(ctx);
    }

    fn pick_folder(&mut self, ctx: &Context) {
        if let Some(folder) = rfd::FileDialog::new()
            .set_title("Select image folder")
            .pick_folder()
        {
            self.close_zip();
            self.config.folder = Some(folder.to_string_lossy().to_string());
            self.config.save(&self.config_path);
            self.open_folder(ctx, folder);
        }
    }

    fn pick_file(&mut self, ctx: &Context) {
        if let Some(file) = rfd::FileDialog::new()
            .set_title("Select image file")
            .add_filter(
                "Images",
                &[
                    "jpg", "jpeg", "png", "webp", "gif", "jxl", "avif", "heic", "heif", "cbz",
                    "zip",
                ],
            )
            .pick_file()
        {
            if is_archive_path(&file) {
                self.open_archive(ctx, &file);
            } else {
                self.close_zip();
                self.open_file(ctx, file);
            }
        }
    }

    fn load_dropped(&mut self, ctx: &Context, path: PathBuf) {
        if path.is_dir() {
            self.close_zip();
            self.open_folder(ctx, path);
            return;
        }
        if is_archive_path(&path) {
            self.open_archive(ctx, &path);
            return;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if image_viewer::SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            self.open_file(ctx, path);
        }
    }

    // ---- folder watcher ----------------------------------------------------

    /// Poll the folder on a timer so files that appear/disappear while the
    /// viewer is open (e.g. an active download folder) show up automatically.
    fn maybe_rescan(&mut self, ctx: &Context) {
        let now = Instant::now();
        if now >= self.next_scan {
            self.next_scan = now + RESCAN_INTERVAL;
            self.rescan(ctx, false);
        }
        ctx.request_repaint_after(self.next_scan.saturating_duration_since(Instant::now()));
    }

    /// Re-scan the current folder and merge the result, keeping the user's
    /// position. `force` bypasses the load_all setting (manual reload key).
    fn rescan(&mut self, ctx: &Context, force: bool) {
        if self.zip_archive.is_some() {
            return; // archives: the extracted temp dir is stable
        }
        if !force && !self.config.load_all_on_startup {
            return;
        }
        self.lat.t_scan = Some(Instant::now());
        let mut files = scan_folder_sorted(&self.folder, self.config.sort);
        // Random must not reshuffle on every poll: keep the user's order,
        // append new files, drop vanished ones (manual reload reshuffles).
        if self.config.sort == SortMode::Random && !force && !self.gallery.files.is_empty() {
            let fresh: HashSet<&PathBuf> = files.iter().collect();
            self.gallery.files.retain(|p| fresh.contains(p));
            for f in &files {
                if !self.gallery.files.contains(f) {
                    self.gallery.files.push(f.clone());
                }
            }
            files = self.gallery.files.clone();
        }
        let current = self.gallery.current().map(|p| p.to_path_buf());
        self.gallery.files = files;
        self.gallery.index = current
            .as_ref()
            .and_then(|p| self.gallery.files.iter().position(|f| f == p))
            .unwrap_or_else(|| self.gallery.index.min(self.gallery.len().saturating_sub(1)));
        if self.gallery.current() != current.as_deref() {
            self.reset_display();
            self.goto(ctx);
        } else {
            self.schedule();
        }
    }

    // ---- file triage -------------------------------------------------------

    /// Remove the current file from the gallery (file already gone from disk)
    /// and advance to the next image.
    fn remove_current_from_gallery(&mut self, ctx: &Context) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        self.previews.remove(&p);
        self.failed.remove(&(p.clone(), Kind::Preview));
        self.failed.remove(&(p.clone(), Kind::Full));
        if !self.gallery.is_empty() {
            let i = self.gallery.index.min(self.gallery.len() - 1);
            self.gallery.files.remove(i);
            self.gallery.index = i.min(self.gallery.len().saturating_sub(1));
        }
        self.reset_display();
        self.goto(ctx);
    }

    /// Delete the current file to the recycle bin and advance.
    fn delete_current(&mut self, ctx: &Context) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        if self.zip_archive.is_some() {
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
            }
            Err(e) => self.error = Some(format!("Delete failed: {e}")),
        }
    }

    /// Copy or move the current file to a folder picked by the user.
    fn transfer_current(&mut self, ctx: &Context, copy: bool) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        if self.zip_archive.is_some() {
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

    /// Show the current file's folder in the system's default file browser
    /// (respects folder associations, e.g. Directory Opus instead of Explorer).
    fn explore_current(&mut self) {
        if let Some(p) = self.gallery.current() {
            let folder = p
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| p.to_path_buf());
            if let Err(e) = open::that_detached(&folder) {
                self.error = Some(format!("Open folder failed: {e}"));
            }
        }
    }

    /// Copy the current image (full-res when in memory) to the clipboard.
    fn copy_image_to_clipboard(&mut self) {
        let Some(p) = self.cur.clone() else {
            return;
        };
        // Fast path: full-res pixels already in memory.
        if let Some(data) = self
            .full
            .as_ref()
            .filter(|f| f.path == p)
            .map(|f| Arc::clone(&f.data))
        {
            match set_clipboard_rgba(&data) {
                Ok(()) => {
                    self.notice =
                        Some(format!("Copied {}x{} image to clipboard", data.width, data.height))
                }
                Err(e) => self.error = Some(format!("Clipboard failed: {e}")),
            }
            return;
        }
        // Animation: copy the frame currently on screen.
        if let Some(av) = self.anim.as_ref().filter(|a| a.path == p) {
            let idx = av.last_index.min(av.anim.frames.len().saturating_sub(1));
            if let Err(e) = set_clipboard_rgba(&av.anim.frames[idx]) {
                self.error = Some(format!("Clipboard failed: {e}"));
            } else {
                self.notice = Some("Copied animation frame to clipboard".into());
            }
            return;
        }
        // Fallback (large JPEG before the full decode): decode off-thread.
        std::thread::spawn(move || match image_viewer::load_image_full(&p) {
            Ok(d) => {
                if let Err(e) = set_clipboard_rgba(&d) {
                    eprintln!("clipboard: {e}");
                }
            }
            Err(e) => eprintln!("clipboard decode failed: {e}"),
        });
        self.notice = Some("Copying image to clipboard…".into());
    }

    /// Set the current image as the desktop wallpaper.
    fn set_wallpaper(&mut self) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        // For archive members (temp extraction), copy to a stable location
        // first — the temp dir dies with the app.
        let target: PathBuf = if self.zip_archive.is_some() {
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_string())
                .unwrap_or_else(|| "jpg".into());
            let dest = self
                .config_path
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(std::env::temp_dir)
                .join(format!("wallpaper.{ext}"));
            if let Err(e) = std::fs::copy(&p, &dest) {
                self.error = Some(format!("Wallpaper failed: {e}"));
                return;
            }
            dest
        } else {
            p
        };
        match wallpaper::set_from_path(&target.to_string_lossy()) {
            Ok(()) => self.notice = Some("Set as wallpaper".into()),
            Err(e) => self.error = Some(format!("Wallpaper failed: {e}")),
        }
    }

    // ---- drawing ----------------------------------------------------------

    fn status_text(&self) -> String {
        let idx = if self.gallery.is_empty() {
            0
        } else {
            self.gallery.index + 1
        };
        let filename = self
            .shown
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let location = match (&self.zip_archive, self.shown.as_ref()) {
            (Some(a), Some(p)) => format!(
                "{}!{}",
                a.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                p.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
            ),
            _ => format!("{}\\{}", self.folder.display(), filename),
        };
        let mut s = format!(
            "{} - {}/{}",
            location,
            idx,
            self.gallery.len()
        );
        if let Some(pv) = self.shown.as_ref().and_then(|p| self.previews.get(p)) {
            s += &format!(" - {}x{} - preview {} ms", pv.orig.0, pv.orig.1, pv.load_ms);
            // Show the dimensions as displayed once rotated, so the readout
            // matches what is on screen.
            if self.rot & 3 != 0 {
                let (w, h) = rotated_footprint(pv.orig.0 as f32, pv.orig.1 as f32, self.rot);
                s += &format!(" (rotated {}x{})", w as u32, h as u32);
            }
            if let Some(anim) = &pv.anim {
                s += &format!(" - anim {} frames", anim.frames.len());
            } else {
                match &self.full {
                    Some(f) if Some(&f.path) == self.shown.as_ref() => {
                        s += &format!(" - full {} ms", f.load_ms)
                    }
                    _ => {
                        if let Some(sh) = &self.shown {
                            if self.needs_full(sh) {
                                s += " - full-res loading…";
                            }
                        }
                    }
                }
            }
        }
        if let Some(e) = &self.error {
            s += &format!(" — {e}");
        } else if let Some(n) = &self.notice {
            s += &format!(" — {n}");
        }
        s
    }

    fn central(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let panel = ui.available_rect_before_wrap();
        let ppp = ctx.pixels_per_point();

        // Track the display area in physical pixels; previews are decoded to
        // match it. If the window grew, below-target textures are dropped so
        // schedule() re-decodes them at the new, larger target.
        let panel_px = (
            (panel.width() * ppp).round() as u32,
            (panel.height() * ppp).round() as u32,
        );
        if self.display_dim != Some(panel_px) {
            let grew = self
                .display_dim
                .map_or(true, |(w, h)| panel_px.0.max(panel_px.1) > w.max(h));
            self.display_dim = Some(panel_px);
            // Hiding/showing panels resizes the display area but must not
            // reload the image: consume exactly one transition silently.
            if self.suppress_resize_redecode {
                self.suppress_resize_redecode = false;
            } else if grew {
                let stale: Vec<PathBuf> = self
                    .previews
                    .iter()
                    .filter(|(_, pv)| pv.below_target)
                    .map(|(p, _)| p.clone())
                    .collect();
                if !stale.is_empty() {
                    perf::count(perf::C_RESIZE_REDECODES, 1);
                    for p in &stale {
                        self.previews.remove(p);
                        self.failed.remove(&(p.clone(), Kind::Preview));
                    }
                    self.schedule();
                }
            }
        }

        // Gallery grid view (G): scrollable thumbnail grid, click to open.
        // This must run *before* the panel rect is allocated below — an
        // allocated rect consumes the whole panel, leaving the grid's
        // ScrollArea with no space (it would render offscreen).
        if self.gallery_view {
            self.gallery_grid(ui, &ctx, panel);
            return;
        }

        let resp = ui.allocate_rect(panel, egui::Sense::click_and_drag());
        let painter = ui.painter_at(panel);

        let Some(shown) = self.shown.clone() else {
            let msg = if let Some(e) = &self.error {
                e.clone()
            } else if self.gallery.is_empty() {
                "No images loaded. Drop a file or folder here.".to_owned()
            } else {
                "Loading…".to_owned()
            };
            painter.text(
                panel.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                egui::FontId::proportional(16.0),
                ui.visuals().text_color(),
            );
            return;
        };

        let (orig, tex_id, tex_w, pv_has_alpha) = {
            let Some(pv) = self.previews.get_mut(&shown) else {
                // The wanted image has no texture yet: this frame shows nothing.
                // Counted as a regression guard - it should stay at 0 except
                // for the very first frames of a cold open.
                perf::count(perf::C_DISPLAY_GAP_FRAMES, 1);
                return;
            };
            pv.used = self.clock;
            (
                pv.orig,
                pv.tex.id(),
                pv.tex.size()[0] as f32,
                pv.has_alpha,
            )
        };
        let (tex_id, tex_w) = if let Some(av) = self.anim.as_mut().filter(|a| a.path == shown) {
            // Advance to the current frame and repaint at its boundary.
            let (idx, next_in) = anim_frame_state(&av.anim, av.start.elapsed());
            if idx != av.last_index {
                let f = &av.anim.frames[idx];
                av.tex.set(
                    ColorImage::from_rgba_unmultiplied(
                        [f.width as usize, f.height as usize],
                        &f.rgba,
                    ),
                    TextureOptions::LINEAR,
                );
                av.last_index = idx;
            }
            ctx.request_repaint_after(next_in);
            (av.tex.id(), av.anim.frames[0].width as f32)
        } else {
            (tex_id, tex_w)
        };

        let img_px = Vec2::new(orig.0 as f32, orig.1 as f32);
        // Quarter turns clockwise. Only 90° steps, so the image stays
        // axis-aligned and 1:1 stays pixel-sharp.
        let quarter = self.rot & 3;
        // What the image *occupies* on screen: odd quarter turns swap the axes.
        let (dw, dh) = rotated_footprint(img_px.x, img_px.y, quarter);
        let display_px = Vec2::new(dw, dh);
        let fit_scale = (panel.width() / display_px.x)
            .min(panel.height() / display_px.y)
            .clamp(1e-4, 8.0);
        let max_scale = MAX_SCALE.max(fit_scale);

        let mut scale = if self.view.fit {
            fit_scale
        } else {
            self.view.scale
        };
        let mut offset = if self.view.fit {
            Vec2::ZERO
        } else {
            self.view.offset
        };
        scale = scale.clamp(fit_scale, max_scale);

        // --- input: zoom (wheel / pinch), pan (drag), shortcuts ---
        let hover = resp.hover_pos();
        let (scroll, pinch, key_fit, key_100) = ui.input(|i| {
            (
                i.smooth_scroll_delta.y,
                i.zoom_delta(),
                i.key_pressed(egui::Key::F),
                i.key_pressed(egui::Key::Num1),
            )
        });

        // Wheel navigation mode: accumulate wheel deltas and flip images
        // instead of zooming. Ctrl+wheel always zooms.
        let wheel_nav_active = self.config.wheel_navigation && !ui.input(|i| i.modifiers.ctrl);
        if wheel_nav_active && scroll != 0.0 {
            // Scroll down (negative) = next image, scroll up = previous.
            self.wheel_acc += scroll;
            if self.wheel_acc <= -THRESH_WHEEL {
                self.wheel_acc = 0.0;
                self.step(&ctx, true);
            } else if self.wheel_acc >= THRESH_WHEEL {
                self.wheel_acc = 0.0;
                self.step(&ctx, false);
            }
        }

        let mut factor = 1.0f32;
        if resp.hovered() {
            factor *= pinch;
            if scroll != 0.0 && !wheel_nav_active {
                factor *= (scroll * 0.0025).exp();
            }
        }
        let mut target_scale = scale * factor;
        let anchor = hover.unwrap_or(panel.center());
        if key_100 {
            target_scale = 1.0 / ppp; // 1 source pixel = 1 physical pixel
        }
        if key_fit || resp.double_clicked() {
            target_scale = fit_scale;
        }
        let target_scale = target_scale.clamp(fit_scale, max_scale);
        if (target_scale - scale).abs() > f32::EPSILON {
            // Zooming in: the visible content gets sharper. Time it from the
            // input, not from when the tiles happen to finish.
            self.lat.t_zoom = Some(Instant::now());
            self.lat.want_viewport_tiles = true;
            // Keep the point under the cursor fixed.
            let c = anchor - panel.center();
            offset = c - (c - offset) * (target_scale / scale);
            scale = target_scale;
        }
        if resp.dragged() {
            offset += resp.drag_delta();
            self.lat.t_pan = Some(Instant::now());
            self.lat.want_viewport_tiles = true;
        }

        // Keep the image from being lost off-screen; centre it on axes where it fits.
        // Uses the *rotated* footprint, so panning limits and fit are correct
        // whichever way up the image is.
        let size = display_px * scale;
        let max_off = ((size - panel.size()) * 0.5).max(Vec2::ZERO);
        offset = offset.clamp(-max_off, max_off);

        let at_fit = scale <= fit_scale * 1.0001;
        if at_fit {
            offset = Vec2::ZERO;
        }
        self.view.fit = at_fit;
        self.view.scale = scale;
        self.view.offset = offset;

        // Snap the image origin to the physical pixel grid so 1:1 is razor sharp.
        // `rect` stays in *unrotated* image space — that is what the tiles and
        // the source-pixel mapping below expect — and is rotated about its
        // centre at paint time, which makes it exactly cover `size`.
        let snap = |v: f32| (v * ppp).round() / ppp;
        let centre = panel.center() + offset;
        let src_size = img_px * scale;
        let min = Pos2::new(
            snap(centre.x - src_size.x * 0.5),
            snap(centre.y - src_size.y * 0.5),
        );
        let rect = Rect::from_min_size(min, src_size);
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));
        // The on-screen footprint, i.e. `rect` turned by `quarter`.
        let footprint = Rect::from_center_size(centre, size);

        // Always paint the preview first; tiles refine it where available.
        // Transparent images get a checkerboard backdrop underneath.
        if pv_has_alpha {
            if let Some(checker) = &self.checker {
                let uv = Rect::from_min_max(
                    Pos2::ZERO,
                    Pos2::new(src_size.x / 32.0, src_size.y / 32.0),
                );
                paint_tex(&painter, checker.id(), rect, uv, quarter);
            }
        }
        paint_tex(&painter, tex_id, rect, uv, quarter);
        // The wanted image is now on screen. The *latency* is committed next
        // frame, once this one has actually been presented.
        if self.cur.as_ref() == Some(&shown) {
            self.lat.painted(Instant::now());
        }

        // --- full-resolution tiles ---
        let preview_density = tex_w / img_px.x; // texture px per source px
        let screen_density = scale * ppp; // physical px per source px
        self.frame += 1;
        let frame = self.frame;
        if let Some(full) = self.full.as_mut().filter(|f| f.path == shown) {
            if screen_density > preview_density {
                let (fw, fh) = (full.data.width, full.data.height);
                let vis = panel.intersect(footprint);
                if vis.width() > 0.0 && vis.height() > 0.0 {
                    let x0 = (((vis.min.x - rect.min.x) / scale).floor().max(0.0) as u32).min(fw);
                    let y0 = (((vis.min.y - rect.min.y) / scale).floor().max(0.0) as u32).min(fh);
                    let x1 = (((vis.max.x - rect.min.x) / scale).ceil().max(0.0) as u32).min(fw);
                    let y1 = (((vis.max.y - rect.min.y) / scale).ceil().max(0.0) as u32).min(fh);
                    let mut budget = TILES_PER_FRAME;
                    let mut pending = false;

                    if x1 > x0 && y1 > y0 {
                        for ty in (y0 / TILE)..=((y1 - 1) / TILE) {
                            for tx in (x0 / TILE)..=((x1 - 1) / TILE) {
                                if !full.tiles.contains_key(&(tx, ty)) {
                                    if budget == 0 {
                                        pending = true;
                                        continue;
                                    }
                                    budget -= 1;
                                    let tex = make_tile(&ctx, &full.data, tx, ty);
                                    perf::count(perf::C_TILES_CREATED, 1);
                                    full.tiles.insert(
                                        (tx, ty),
                                        Tile {
                                            tex,
                                            last_drawn: frame,
                                        },
                                    );
                                }
                                let tile = full.tiles.get_mut(&(tx, ty)).unwrap();
                                tile.last_drawn = frame;
                                let (px, py) = (tx * TILE, ty * TILE);
                                let (tw, th) = (TILE.min(fw - px), TILE.min(fh - py));
                                let r = Rect::from_min_size(
                                    rect.min + Vec2::new(px as f32, py as f32) * scale,
                                    Vec2::new(tw as f32, th as f32) * scale,
                                );
                                paint_tex(&painter, tile.tex.id(), r, uv, quarter);
                            }
                        }
                    }
                    if pending {
                        ctx.request_repaint();
                    } else {
                        // Everything the viewport needs is on screen.
                        self.lat.viewport_tiles_ready(Instant::now());
                    }
                }

                // Bound GPU memory: drop the least recently drawn tiles.
                if full.tiles.len() > MAX_TILES {
                    let mut old: Vec<((u32, u32), u64)> = full
                        .tiles
                        .iter()
                        .filter(|(_, t)| t.last_drawn < frame)
                        .map(|(k, t)| (*k, t.last_drawn))
                        .collect();
                    old.sort_by_key(|(_, l)| *l);
                    let excess = full.tiles.len() - MAX_TILES;
                    for (k, _) in old.into_iter().take(excess) {
                        full.tiles.remove(&k);
                    }
                }
            } else if screen_density < preview_density * 0.5 {
                full.tiles.clear(); // zoomed well out: release tile textures
            }
        }

        if self.cur.is_some() && self.cur != self.shown {
            painter.text(
                panel.left_top() + Vec2::new(10.0, 10.0),
                egui::Align2::LEFT_TOP,
                "Loading…",
                egui::FontId::proportional(14.0),
                Color32::from_white_alpha(200),
            );
        }
    }

    /// Show/hide the thumbnail grid (G key and the bottom-panel button).
    fn toggle_gallery(&mut self) {
        self.gallery_view = !self.gallery_view;
        if self.gallery_view {
            // Start the highlight on the image currently shown.
            self.grid_sel = self.gallery.index.min(self.gallery.len().saturating_sub(1));
            // Always re-focus on open, even if the selection index happens to
            // be the same: the offset is per-folder state.
            self.grid_sel_scrolled = usize::MAX;
            self.lat.t_gallery = Some(Instant::now());
        }
    }

    /// Open the grid's highlighted image and leave the grid (Enter or click).
    fn pick_grid_selection(&mut self, ctx: &Context) {
        if self.gallery.is_empty() {
            return;
        }
        self.gallery.index = self.grid_sel.min(self.gallery.len() - 1);
        self.gallery_view = false;
        self.goto(ctx);
    }

    /// Gallery grid view (G): scrollable thumbnails of every image in the
    /// queue. Thumbs decode lazily for visible cells.
    ///
    /// Browsing here does *not* change the shown image: arrows/Home/End only
    /// move `grid_sel`, and Enter (or a click) opens it. Arrowing around a
    /// 2000-image folder therefore costs no re-decodes.
    fn gallery_grid(&mut self, ui: &mut egui::Ui, ctx: &Context, panel: egui::Rect) {
        let cols = grid_cols(panel.width());
        self.grid_cols = cols; // Up/Down in update() step by this
        let row_h = GRID_ROW_H;
        let total = self.gallery.files.len();
        let total_rows = (total + cols - 1) / cols;
        let clock = self.clock;
        let sel = self.grid_sel;
        let mut clicked: Option<usize> = None;

        // Split the borrows so cells can be drawn without cloning the file
        // list (a big folder is thousands of PathBufs per frame otherwise).
        let Self {
            gallery,
            thumbs,
            thumb_tries,
            loader,
            ..
        } = self;
        let files = &gallery.files;
        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, false]);
        // Scroll to the selection, but only when the selection actually moved.
        //
        // Two mechanisms were tried and both had a failure mode:
        //
        //  * `Response::scroll_to_me` cannot reach a row that is not laid out,
        //    and `show_rows` only lays out the viewport — so opening the grid
        //    on an off-screen image did nothing.
        //  * Re-issuing it every frame (or forcing the offset every frame) fights
        //    the user: any small manual scroll kept re-centring the selection.
        //
        // Writing the offset once per *selection change* handles opening,
        // arrows and Home/End uniformly, is deterministic, and leaves manual
        // scrolling completely alone. show_rows adds vertical item spacing on
        // top of row_h, hence the pitch.
        if sel != self.grid_sel_scrolled {
            let pitch = row_h + ui.spacing().item_spacing.y;
            sa = sa.vertical_scroll_offset((sel / cols) as f32 * pitch);
            self.grid_sel_scrolled = sel;
        }
        sa.show_rows(ui, row_h, total_rows, |ui, rows| {
                for row in rows {
                    ui.horizontal(|ui| {
                        for col in 0..cols {
                            let idx = row * cols + col;
                            let Some(path) = files.get(idx) else {
                                continue;
                            };
                            let resp = grid_cell(
                                ui,
                                path,
                                GRID_CELL,
                                idx == sel,
                                clock,
                                thumbs,
                                thumb_tries,
                                loader,
                            );
                            if resp.clicked() && clicked.is_none() {
                                clicked = Some(idx);
                            }
                        }
                    });
                }
            });

        if let Some(i) = clicked {
            self.grid_sel = i;
            self.pick_grid_selection(ctx);
        }
    }

    /// Turn the shown image by `steps` quarter turns clockwise (negative =
    /// counter-clockwise). Re-fits afterwards, since the footprint changes
    /// shape. Temporary UI: the rotation is a view transform only — nothing is
    /// written back to the file.
    fn rotate(&mut self, steps: i32, ctx: &Context) {
        self.rot = (self.rot as i32 + steps).rem_euclid(4) as u8;
        self.view.fit = true;
        self.view.offset = Vec2::ZERO;
        ctx.request_repaint();
    }

    /// Per-frame housekeeping that must run before anything is drawn.
    /// Report (once) an image that has not reached the screen long after it was
    /// asked for. Without this, "the viewer hangs on some images" is
    /// undiagnosable from the outside: the state below is the whole story.
    fn check_for_stall(&mut self, _ctx: &Context) {
        const STALL_AFTER: Duration = Duration::from_secs(5);
        if self.lat.stall_logged {
            return;
        }
        // Only a request that has *not* produced pixels is a stall. Without
        // this check the watchdog fires on any request older than the
        // threshold — including images that were served instantly from cache.
        if self.lat.recorded_first {
            return;
        }
        let (Some(t), Some(cur)) = (self.lat.t_request, self.cur.as_ref()) else {
            return;
        };
        if t.elapsed() < STALL_AFTER {
            return;
        }
        self.lat.stall_logged = true;
        perf::count(perf::C_STALL, 1);
        let cached = self.previews.contains_key(cur);
        let poisoned = self.failed.contains(&(cur.clone(), Kind::Preview));
        eprintln!(
            "stall: {:?} not painted after {:?}\n  index={}/{} shown={:?} shown==cur={}\n  \
cached={} poisoned={} previews={} thumbs={} panics={}",
            cur.file_name().unwrap_or_default(),
            t.elapsed(),
            self.gallery.index,
            self.gallery.len(),
            self.shown.as_ref().and_then(|p| p.file_name()),
            self.shown == self.cur,
            cached,
            poisoned,
            self.previews.len(),
            self.thumbs.len(),
            perf::counter(perf::C_DECODE_PANICS),
        );
    }

    fn frame_prelude(&mut self, ctx: &Context) {
        // The previous frame has been presented by the time the next one
        // starts, so this is where "first visible pixels" is honestly known.
        self.lat.commit_frame();
        if let Some(t) = self.t_boot.take() {
            perf::record_since(perf::STARTUP_TO_FIRST_FRAME, t);
        }
        self.check_for_stall(ctx);
        // LRU clock for the preview/thumbnail caches. Lives here (not in
        // `central`) so it also advances while the gallery grid is open.
        self.clock += 1;

        // Consume a queued advance from "wait for load" mode — the image it
        // belongs to was painted during the previous frame.
        if let Some(dir) = self.pending_advance.take() {
            let (fwd, prev) = ctx.input(|i| {
                (
                    i.key_down(egui::Key::ArrowRight)
                        || i.key_down(egui::Key::PageDown)
                        || i.key_down(egui::Key::Space),
                    i.key_down(egui::Key::ArrowLeft) || i.key_down(egui::Key::PageUp),
                )
            });
            if dir && fwd {
                self.step(ctx, true);
            } else if !dir && prev {
                self.step(ctx, false);
            }
        }
        self.pump(ctx);
        self.sync_shown(ctx);
        self.maybe_rescan(ctx);

        // Start the full-res decode once the user has dwelled on an image.
        if let Some(due) = self.full_due {
            let now = Instant::now();
            if now >= due {
                self.full_due = None;
                self.schedule();
            } else {
                ctx.request_repaint_after(due - now);
            }
        }
    }
}

/// Current frame index and time until the next boundary for a playing
/// animation, given elapsed playback time. Zero/short delays are floored to
/// 20 ms so a broken GIF can't spin the render loop.
fn anim_frame_state(anim: &Animation, elapsed: Duration) -> (usize, Duration) {
    let total = anim.total_duration().as_millis().max(1);
    let t = elapsed.as_millis() % total;
    let mut acc = 0u128;
    for (i, d) in anim.delays.iter().enumerate() {
        let dm = d.as_millis().max(20);
        if t < acc + dm {
            return (i, Duration::from_millis((acc + dm - t) as u64));
        }
        acc += dm;
    }
    (
        anim.frames.len().saturating_sub(1),
        Duration::from_millis(20),
    )
}

/// One gallery-grid cell: thumbnail (or placeholder) + file name. Requests the
/// thumbnail when it is missing, then returns true if the cell was clicked.
///
/// Free function (not a method) so `gallery_grid` can hand out split borrows
/// and avoid cloning the whole file list every frame.
fn grid_cell(
    ui: &mut egui::Ui,
    path: &Path,
    cell: f32,
    selected: bool,
    clock: u64,
    thumbs: &mut HashMap<PathBuf, Thumb>,
    tries: &mut HashMap<PathBuf, u8>,
    loader: &Loader,
) -> egui::Response {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Ask for the thumbnail whenever it is missing. `Loader::push` dedups
    // against queued + in-flight work, so this costs one lock per visible cell
    // per frame, and it self-heals after `schedule()` wipes the queue.
    // `tries` counts *completed* attempts, so jobs lost to a queue wipe cost
    // nothing; only real decode failures count against the budget.
    if !thumbs.contains_key(path) && tries.get(path).copied().unwrap_or(0) < THUMB_TRIES {
        loader.push(Job {
            path: path.to_path_buf(),
            kind: Kind::Thumb,
            prio: PRIO_THUMB,
            target_dim: THUMB_DIM,
        });
    }
    if let Some(t) = thumbs.get_mut(path) {
        t.used = clock; // LRU stamp
    }

    let fill = if selected {
        egui::Color32::from_rgb(28, 58, 118)
    } else {
        egui::Color32::from_rgb(38, 38, 38)
    };
    let stroke = if selected {
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(120, 170, 255))
    } else {
        egui::Stroke::NONE
    };

    // Allocate the cell *ourselves* with click sense. `Frame::show` would
    // size itself to the rest of the row and only register hover, which both
    // pushed the following cells offscreen and made clicks dead.
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(cell, cell + GRID_LABEL_H),
        egui::Sense::click(),
    );
    if ui.is_rect_visible(rect) {
        ui.painter().rect(rect, 0.0, fill, stroke);
    }

    let inner = rect.shrink(4.0);
    let mut child = ui.child_ui(inner, egui::Layout::top_down(egui::Align::Center));

    // Thumbnail area: full width, cell height. Only the *rect* is computed
    // here; egui still does the painting. Widgets place at the cursor, and a
    // `Layout::top_down(Align::Center)` does not move them (a top-down flow's
    // initial cursor runs to +infinity on the main axis, so
    // `allocate_exact_size` only centres on the cross axis), hence the
    // explicit rect.
    let img_area = egui::Rect::from_min_size(
        egui::Pos2::new(inner.min.x, inner.min.y),
        egui::vec2(inner.width(), cell),
    );
    child.allocate_exact_size(img_area.size(), egui::Sense::hover());
    match thumbs.get(path) {
        Some(t) if t.texels[0] > 0 && t.texels[1] > 0 => {
            // Texels -> points, so a 256px thumb looks the same size whatever
            // the display's pixel ratio. `texels` is the decoded buffer's size,
            // recorded at upload time: the ground truth, with no dependency on
            // texture-manager state.
            let ppp = ui.ctx().pixels_per_point();
            let natural = egui::vec2(t.texels[0] as f32, t.texels[1] as f32) / ppp;
            let scale = fit_scale_no_upscale(
                (natural.x, natural.y),
                (img_area.width(), img_area.height()),
            );
            let draw_size = (natural * scale)
                .max(egui::Vec2::splat(1.0))
                .min(img_area.size());
            let draw = egui::Rect::from_center_size(img_area.center(), draw_size);
            if ui.is_rect_visible(draw) {
                // Give the widget a Ui bounded to the centred rect: that is
                // what centres it. Do NOT paint the texture by hand here
                // (`painter().image`) — that shipped once and turned every
                // thumbnail into a solid block.
                let mut img_ui =
                    child.child_ui(draw, egui::Layout::top_down(egui::Align::Min));
                img_ui.add(egui::Image::new(&t.tex).max_size(draw_size));
            }
        }
        _ => {
            // Centred like a real thumbnail would be.
            child.painter().text(
                img_area.center(),
                egui::Align2::CENTER_CENTER,
                "\u{2026}",
                egui::FontId::proportional(16.0),
                ui.visuals().weak_text_color(),
            );
        }
    }
    child.add(
        egui::Label::new(
            egui::RichText::new(&name)
                .small()
                .color(egui::Color32::from_gray(200)),
        )
        .truncate(true),
    );

    resp
}

/// A small 2x2-square checker texture used as the transparency backdrop.
/// Painted with repeat-wrapped UVs so it tiles at any image size.
fn make_checker_texture(ctx: &Context) -> TextureHandle {
    const N: usize = 64; // texture side in px
    const SQ: usize = 32; // square side in px
    let mut pixels = Vec::with_capacity(N * N);
    for y in 0..N {
        for x in 0..N {
            let even = ((x / SQ) + (y / SQ)) % 2 == 0;
            let g = if even { 82 } else { 118 };
            pixels.push(Color32::from_rgb(g, g, g));
        }
    }
    ctx.load_texture(
        "checker",
        ColorImage {
            size: [N, N],
            pixels,
        },
        TextureOptions::NEAREST_REPEAT,
    )
}

/// Put an RGBA image on the clipboard (any thread).
fn set_clipboard_rgba(d: &ImageData) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_image(arboard::ImageData {
        width: d.width as usize,
        height: d.height as usize,
        bytes: std::borrow::Cow::Owned(d.rgba.clone()),
    })
    .map_err(|e| e.to_string())
}

/// Paint a texture into `rect`, rotated by `quarter` (0..4) 90° steps clockwise
/// about the rect's centre.
///
/// `painter.image` cannot rotate, so build a mesh and rotate it — the same
/// trick egui's own `Image::paint_at` uses for `ImageOptions::rotation`. A
/// quarter turn keeps everything axis-aligned, so 1:1 stays pixel-sharp and
/// the caller can keep working in unrotated image space.
fn paint_tex(
    painter: &egui::Painter,
    tex_id: egui::TextureId,
    rect: egui::Rect,
    uv: egui::Rect,
    quarter: u8,
) {
    let quarter = quarter & 3;
    if quarter == 0 {
        painter.image(tex_id, rect, uv, Color32::WHITE);
        return;
    }
    let mut mesh = egui::epaint::Mesh::with_texture(tex_id);
    mesh.add_rect_with_uv(rect, uv, Color32::WHITE);
    mesh.rotate(
        egui::emath::Rot2::from_angle(quarter as f32 * std::f32::consts::FRAC_PI_2),
        rect.center(),
    );
    painter.add(egui::Shape::mesh(mesh));
}

/// Copy one tile out of the full-res RGBA buffer and upload it.
fn make_tile(ctx: &Context, data: &ImageData, tx: u32, ty: u32) -> TextureHandle {
    let (fw, fh) = (data.width as usize, data.height as usize);
    let (x0, y0) = ((tx * TILE) as usize, (ty * TILE) as usize);
    let (w, h) = ((TILE as usize).min(fw - x0), (TILE as usize).min(fh - y0));
    let stride = fw * 4;

    let mut pixels: Vec<Color32> = Vec::with_capacity(w * h);
    for y in y0..y0 + h {
        let row = &data.rgba[y * stride + x0 * 4..y * stride + (x0 + w) * 4];
        pixels.extend(
            row.chunks_exact(4)
                .map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])),
        );
    }
    let img = ColorImage {
        size: [w, h],
        pixels,
    };
    ctx.load_texture(format!("tile {tx},{ty}"), img, TextureOptions::LINEAR)
}

// ---------------------------------------------------------------------------
// eframe glue
// ---------------------------------------------------------------------------

impl eframe::App for MyApp {
    /// Persist the last-viewed image per folder on app close, then clean up
    /// any extracted archive temp dir.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Key by the archive path when one is open (the temp folder name is
        // meaningless to the user).
        let key: String = match &self.zip_archive {
            Some(a) => a.to_string_lossy().into_owned(),
            None => self.folder.to_string_lossy().into_owned(),
        };
        if let Some(cur) = self.gallery.current() {
            // Store just the file name so the same key works for archives
            // (member name) and folders (file name) alike.
            let value = cur
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| cur.to_string_lossy().into_owned());
            self.config.last_by_folder.insert(key, value);
        }
        self.config.save(&self.config_path);
        self.close_zip();
        if image_viewer::perf::enabled() {
            let report = image_viewer::perf::report();
            eprint!("{report}");
            // A release build has no console, so also leave the report where it
            // can be found: %TEMP%\image-viewer-perf.txt.
            let path = std::env::temp_dir().join("image-viewer-perf.txt");
            image_viewer::perf::write_report(&path);
        }
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.frame_prelude(ctx);

        // F11: fullscreen toggle — image + panels only, chrome hidden.
        // Esc: exit fullscreen when in it, otherwise close the app.
        let f11 = ctx.input(|i| i.key_pressed(egui::Key::F11));
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if f11 {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
            self.suppress_resize_redecode = true;
            self.show_panels = fs; // entering fullscreen hides panels; leaving restores
        }
                        if esc {
            if self.gallery_view {
                self.gallery_view = false; // close the grid first
            } else {
                let in_fullscreen = ctx
                    .input(|i| i.viewport().fullscreen.unwrap_or(false));
                if in_fullscreen {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
                    self.show_panels = true;
                    self.suppress_resize_redecode = true;
                } else {
                    self.close_zip();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }

                // Keyboard navigation + file triage
        let (next, prev, first, last, toggle, grid) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowRight)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
                i.key_pressed(egui::Key::I),
                i.key_pressed(egui::Key::G),
            )
        });
        // In grid mode the arrows drive the grid's own selection instead of
        // changing the shown image, and Enter opens the highlighted cell.
        let (up, down, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
            )
        });
        let (del, move_f, copy_f, explore, reload, clip_img, wall) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::M),
                i.key_pressed(egui::Key::C) && !i.modifiers.ctrl,
                i.key_pressed(egui::Key::E),
                i.key_pressed(egui::Key::R),
                i.key_pressed(egui::Key::C) && i.modifiers.ctrl,
                i.key_pressed(egui::Key::W) && !i.modifiers.any(),
            )
        });
        // Mouse side buttons: back = previous image, forward = next image.
        let (mouse_back, mouse_fwd) = ctx.input(|i| {
            (
                i.pointer
                    .button_clicked(egui::PointerButton::Extra1),
                i.pointer
                    .button_clicked(egui::PointerButton::Extra2),
            )
        });
        if self.gallery_view {
            // Grid: arrows move the highlight, Enter opens it. Clamped, so
            // walking off an edge stops instead of jumping to the far end.
            let cols = self.grid_cols.max(1);
            let last_idx = self.gallery.len().saturating_sub(1);
            let sel = &mut self.grid_sel;
            if next || down {
                *sel = (*sel + if down { cols } else { 1 }).min(last_idx);
            } else if prev || up {
                *sel = sel.saturating_sub(if up { cols } else { 1 });
            } else if first {
                *sel = 0;
            } else if last {
                *sel = last_idx;
            } else if enter {
                self.pick_grid_selection(ctx);
            }
        } else if next {
            self.step(ctx, true);
        } else if prev {
            self.step(ctx, false);
        } else if first && !self.gallery.is_empty() {
            self.gallery.index = 0;
            self.goto(ctx);
        } else if last && !self.gallery.is_empty() {
            self.gallery.index = self.gallery.len() - 1;
            self.goto(ctx);
        }
        if toggle {
            self.suppress_resize_redecode = true;
            self.show_panels = !self.show_panels;
        }
        if grid {
            self.toggle_gallery();
        }
        if del {
            self.delete_current(ctx);
        } else if move_f {
            self.transfer_current(ctx, false);
        } else if copy_f {
            self.transfer_current(ctx, true);
        } else if explore {
            self.explore_current();
        } else if reload {
            self.rescan(ctx, true);
        } else if clip_img {
            self.copy_image_to_clipboard();
        } else if wall {
            self.set_wallpaper();
        } else if mouse_back {
            self.step(ctx, false);
        } else if mouse_fwd {
            self.step(ctx, true);
        }

        // Drag-and-drop: files or folders
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.first().and_then(|f| f.path.clone()) {
            self.load_dropped(ctx, path);
        }

        if self.show_panels {
            egui::TopBottomPanel::top("status").show(ctx, |ui| {
                ui.label(self.status_text());
            });
        }

        if self.show_panels {
            egui::TopBottomPanel::bottom("menu").show(ctx, |ui| {
                // Wrapped: a single row of buttons overflows a narrow
                // window and the tail (incl. the hints) gets clipped.
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Browse Folder…").clicked() {
                        self.pick_folder(ctx);
                    }
                    if ui.button("Browse File…").clicked() {
                        self.pick_file(ctx);
                    }
                    if ui.button("Prev (←)").clicked() {
                        self.step(ctx, false);
                    }
                    if ui.button("Next (→)").clicked() {
                        self.step(ctx, true);
                    }
                    if ui.button("Fit (F)").clicked() {
                        self.view.fit = true;
                    }
                    if ui.button("Gallery (G)").clicked() {
                        self.toggle_gallery();
                    }
                    if ui.button("Rotate ⟲").clicked() {
                        self.rotate(-1, ctx);
                    }
                    if ui.button("Rotate ⟳").clicked() {
                        self.rotate(1, ctx);
                    }
                    if ui.button("Panels (I)").clicked() {
                        self.suppress_resize_redecode = true;
                        self.show_panels = !self.show_panels;
                    }
                    if ui.button("Delete (Del)").clicked() {
                        self.delete_current(ctx);
                    }
                    if ui.button("Move… (M)").clicked() {
                        self.transfer_current(ctx, false);
                    }
                    if ui.button("Copy… (C)").clicked() {
                        self.transfer_current(ctx, true);
                    }
                    if ui.button("Explore (E)").clicked() {
                        self.explore_current();
                    }
                    if ui.button("Reload (R)").clicked() {
                        self.rescan(ctx, true);
                    }
                    if ui.button("Copy IMG (Ctrl+C)").clicked() {
                        self.copy_image_to_clipboard();
                    }
                    if ui.button("Wallpaper (W)").clicked() {
                        self.set_wallpaper();
                    }
                    ui.label("wheel: zoom · drag: pan · 1: 100% · double-click: fit");
                });
            });
        }

        if self.show_panels {
            egui::SidePanel::left("exif").show(ctx, |ui| {
                ui.heading("EXIF");
                if self.exif_loading {
                    ui.label("Loading EXIF…");
                } else if let Some(info) = &self.exif_info {
                    for line in info.lines() {
                        if let Some(url) = line.strip_prefix("Maps: ") {
                            // GPS link: clickable, opens the system browser.
                            if ui.link("Open in Google Maps ↗").clicked() {
                                if let Err(e) = open::that_detached(url) {
                                    self.error = Some(format!("Open link failed: {e}"));
                                }
                            }
                        } else {
                            ui.label(line);
                        }
                    }
                } else {
                    ui.label("No EXIF available.");
                }
                if let Some(p) = self.cur.as_ref() {
                    if let Ok(md) = std::fs::metadata(p) {
                        ui.separator();
                        ui.label(format!(
                            "Size: {:.1} MB",
                            md.len() as f64 / (1024.0 * 1024.0)
                        ));
                    }
                }
            });

            let mut resort = false;
            let mut refilter = false;
            egui::SidePanel::right("settings").show(ctx, |ui| {
                ui.heading("Settings");
                if ui
                    .checkbox(
                        &mut self.config.load_all_on_startup,
                        "Load all images in folder",
                    )
                    .changed()
                {
                    self.config.save(&self.config_path);
                    if self.config.load_all_on_startup {
                        self.gallery.load_all_from_folder(&self.folder);
                    }
                    self.goto(ctx);
                }
                ui.add_space(6.0);
                if ui
                    .checkbox(
                        &mut self.config.linear_resample,
                        "Linear-light downsampling",
                    )
                    .changed()
                {
                    set_linear_resample(self.config.linear_resample);
                    self.config.save(&self.config_path);
                }
                                let mut new_filter = self.config.downscale_filter;
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    egui::ComboBox::from_id_source("ds_filter")
                        .selected_text(format!("{:?}", self.config.downscale_filter))
                        .show_ui(ui, |ui| {
                            for f in [
                                DownscaleFilter::Hermite,
                                DownscaleFilter::Catrom,
                                DownscaleFilter::Lanczos3,
                            ] {
                                ui.selectable_value(&mut new_filter, f, format!("{f:?}"));
                            }
                        });
                });
                if ui
                    .checkbox(
                        &mut self.config.block_while_loading,
                        "Wait for image when holding arrows",
                    )
                    .changed()
                {
                    self.config.save(&self.config_path);
                }
                if ui
                    .checkbox(
                        &mut self.config.wheel_navigation,
                        "Mouse wheel flips images (Ctrl+wheel zooms)",
                    )
                    .changed()
                {
                    self.config.save(&self.config_path);
                }
                if new_filter != self.config.downscale_filter {
                    self.config.downscale_filter = new_filter;
                    set_downscale_filter(new_filter);
                    self.config.save(&self.config_path);
                    refilter = true;
                }
                let mut new_sort = self.config.sort;
                ui.horizontal(|ui| {
                    ui.label("Sort:");
                    egui::ComboBox::from_id_source("sort_mode")
                        .selected_text(format!("{new_sort:?}"))
                        .show_ui(ui, |ui| {
                            for m in [
                                SortMode::Name,
                                SortMode::Date,
                                SortMode::Size,
                                SortMode::Random,
                            ] {
                                ui.selectable_value(&mut new_sort, m, format!("{m:?}"));
                            }
                        });
                });
                if new_sort != self.config.sort {
                    self.config.sort = new_sort;
                    self.config.save(&self.config_path);
                    resort = true;
                }
                ui.add_space(6.0);
                ui.heading("File associations");
                let registered = fileassoc::is_registered();
                if registered {
                    ui.label(format!("Registered as '{}'", fileassoc::progid()));
                    if ui.button("Unregister file types").clicked() {
                        match fileassoc::unregister() {
                            Ok(_) => self.notice = Some("File associations removed".into()),
                            Err(e) => self.error = Some(format!("Unregister failed: {e}")),
                        }
                    }
                } else if ui.button("Register as image viewer…").clicked() {
                    match fileassoc::register() {
                        Ok(_) => self.notice = Some("Registered — set default via Explorer's 'Open with'".into()),
                        Err(e) => self.error = Some(format!("Register failed: {e}")),
                    }
                }
                ui.small("Registers in the user registry (no admin). To make it the default, right-click an image > Open with > Choose another app > Image Viewer > Always.");
                ui.label(format!("Config: {}", self.config_path.display()));
            });
            if resort {
                self.rescan(ctx, true);
            }
            if refilter {
                // Realtime filter preview: drop every preview and re-queue at
                // the current target; old textures remain on screen until the
                // replacements arrive (no blank flash).
                self.previews.clear();
                self.full = None;
                self.anim = None;
                self.failed.clear();
                self.reset_display();
                self.goto(ctx);
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::BLACK))
            .show(ctx, |ui| {
                self.central(ui);
            });
    }
}
