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

use image_viewer::{
    buffer_has_alpha, load_image_full, load_image_preview_full, set_linear_resample,
    scan_folder_sorted, Animation, Config, Gallery, ImageData, SortMode, MAX_FULL_PIXELS,
    PREVIEW_TEXTURE_DIM,
};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

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

fn main() -> eframe::Result<()> {
    // CLI: `--register` / `--unregister` manage file associations and exit;
    // any other argument is a file or folder to open.
    let args: Vec<String> = std::env::args().skip(1).collect();
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
}

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
                let msg = std::panic::catch_unwind(AssertUnwindSafe(|| run_job(&job)))
                    .unwrap_or_else(|_| fail_msg(&job, "decoder panicked".into()));

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
        q.jobs = jobs
            .into_iter()
            .filter(|j| !busy.contains(&(j.path.clone(), j.kind)))
            .collect();
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
}

impl MyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let config_path = Config::default_path();
        let config = Config::load(&config_path);

        // Optional CLI argument: a file or a folder ("Open with…").
        let arg = std::env::args_os().nth(1).map(PathBuf::from);
        let explicit_file = arg.as_ref().is_some_and(|a| a.is_file());
        let (folder, mut gallery) = match arg {
            Some(a) if a.is_dir() => {
                let g = Gallery::new(&a);
                (a, g)
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
                    let last = PathBuf::from(last);
                    if let Some(i) = files.iter().position(|f| *f == last) {
                        gallery.index = i;
                    }
                }
            }
            gallery.files = files;
        }

        set_linear_resample(config.linear_resample);

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
        };
                app.goto(ctx);
        app
    }

    // ---- navigation -------------------------------------------------------

    /// Point the app at `gallery.current()` and (re)plan all background work.
    fn goto(&mut self, ctx: &Context) {
        self.cur = self.gallery.current().map(|p| p.to_path_buf());
        self.error = None;
        self.notice = None;
        self.exif_info = None;
        self.exif_loading = self.cur.is_some();
        self.full_due = Some(Instant::now() + FULL_DWELL);

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
                    }
                    Err(e) => {
                        eprintln!("Failed to load {}: {}", path.display(), e);
                        if Some(&path) == self.cur.as_ref() {
                            self.error = Some(format!("Failed to load {}: {}", path.display(), e));
                        }
                        self.failed.insert((path, Kind::Preview));
                    }
                },
                Msg::Full { path, result } => match result {
                    Ok(data) => {
                        // Only keep it if the user is still on this image.
                        if Some(&path) == self.cur.as_ref() {
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

    fn open_file(&mut self, ctx: &Context, file: PathBuf) {
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
                    "jpg", "jpeg", "png", "webp", "gif", "jxl", "avif", "heic", "heif",
                ],
            )
            .pick_file()
        {
            self.open_file(ctx, file);
        }
    }

    fn load_dropped(&mut self, ctx: &Context, path: PathBuf) {
        if path.is_dir() {
            self.open_folder(ctx, path);
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
        if !force && !self.config.load_all_on_startup {
            return;
        }
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
        match wallpaper::set_from_path(&p.to_string_lossy()) {
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
        let mut s = format!(
            "{}\\{} - {}/{}",
            self.folder.display(),
            filename,
            idx,
            self.gallery.len()
        );
        if let Some(pv) = self.shown.as_ref().and_then(|p| self.previews.get(p)) {
            s += &format!(" - {}x{} - preview {} ms", pv.orig.0, pv.orig.1, pv.load_ms);
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
        let resp = ui.allocate_rect(panel, egui::Sense::click_and_drag());
        let painter = ui.painter_at(panel);
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
            if grew {
                let stale: Vec<PathBuf> = self
                    .previews
                    .iter()
                    .filter(|(_, pv)| pv.below_target)
                    .map(|(p, _)| p.clone())
                    .collect();
                if !stale.is_empty() {
                    for p in &stale {
                        self.previews.remove(p);
                        self.failed.remove(&(p.clone(), Kind::Preview));
                    }
                    self.schedule();
                }
            }
        }

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

        self.clock += 1;
        let (orig, tex_id, tex_w, pv_has_alpha) = {
            let Some(pv) = self.previews.get_mut(&shown) else {
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
        let fit_scale = (panel.width() / img_px.x)
            .min(panel.height() / img_px.y)
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

        let mut factor = 1.0f32;
        if resp.hovered() {
            factor *= pinch;
            if scroll != 0.0 {
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
            // Keep the point under the cursor fixed.
            let c = anchor - panel.center();
            offset = c - (c - offset) * (target_scale / scale);
            scale = target_scale;
        }
        if resp.dragged() {
            offset += resp.drag_delta();
        }

        // Keep the image from being lost off-screen; centre it on axes where it fits.
        let size = img_px * scale;
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
        let snap = |v: f32| (v * ppp).round() / ppp;
        let centre = panel.center() + offset;
        let min = Pos2::new(snap(centre.x - size.x * 0.5), snap(centre.y - size.y * 0.5));
        let rect = Rect::from_min_size(min, size);
        let uv = Rect::from_min_max(Pos2::new(0.0, 0.0), Pos2::new(1.0, 1.0));

        // Always paint the preview first; tiles refine it where available.
        // Transparent images get a checkerboard backdrop underneath.
        if pv_has_alpha {
            if let Some(checker) = &self.checker {
                let uv = Rect::from_min_max(
                    Pos2::ZERO,
                    Pos2::new(size.x / 32.0, size.y / 32.0),
                );
                painter.image(checker.id(), rect, uv, Color32::WHITE);
            }
        }
        painter.image(tex_id, rect, uv, Color32::WHITE);

        // --- full-resolution tiles ---
        let preview_density = tex_w / img_px.x; // texture px per source px
        let screen_density = scale * ppp; // physical px per source px
        self.frame += 1;
        let frame = self.frame;
        if let Some(full) = self.full.as_mut().filter(|f| f.path == shown) {
            if screen_density > preview_density {
                let (fw, fh) = (full.data.width, full.data.height);
                let vis = panel.intersect(rect);
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
                                painter.image(tile.tex.id(), r, uv, Color32::WHITE);
                            }
                        }
                    }
                    if pending {
                        ctx.request_repaint();
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

    /// Per-frame housekeeping that must run before anything is drawn.
    fn frame_prelude(&mut self, ctx: &Context) {
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
    /// Persist the last-viewed image per folder on app close.
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(cur) = self.gallery.current() {
            self.config.last_by_folder.insert(
                self.folder.to_string_lossy().into_owned(),
                cur.to_string_lossy().into_owned(),
            );
        }
        self.config.save(&self.config_path);
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.frame_prelude(ctx);

        // Keyboard navigation + file triage
        let (next, prev, first, last, toggle) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowRight)
                    || i.key_pressed(egui::Key::PageDown)
                    || i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
                i.key_pressed(egui::Key::I),
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
        if next {
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
            self.show_panels = !self.show_panels;
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
        }

        // Drag-and-drop: files or folders
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.first().and_then(|f| f.path.clone()) {
            self.load_dropped(ctx, path);
        }

        egui::TopBottomPanel::top("status").show(ctx, |ui| {
            ui.label(self.status_text());
        });

        egui::TopBottomPanel::bottom("menu").show(ctx, |ui| {
            ui.horizontal(|ui| {
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
                if ui.button("Panels (I)").clicked() {
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

        if self.show_panels {
            egui::SidePanel::left("exif").show(ctx, |ui| {
                ui.heading("EXIF");
                if self.exif_loading {
                    ui.label("Loading EXIF…");
                } else if let Some(info) = &self.exif_info {
                    for line in info.lines() {
                        ui.label(line);
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
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(Color32::from_gray(24)))
            .show(ctx, |ui| {
                self.central(ui);
            });
    }
}
