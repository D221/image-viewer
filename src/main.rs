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
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use image_viewer::perf;
use image_viewer::{
    buffer_has_alpha, extract_zip, fit_scale_no_upscale, forget_thumbnail, grid_cols,
    is_archive_path, load_grid_thumbnail, load_image_full, load_page_preview_full,
    load_spread_overrides, protected_preview_keys, save_spread_overrides, set_downscale_filter,
    set_linear_resample, set_thumbnail_cache, scan_folder_sorted, spread_root_key, spreads_root,
    thumb_cache_root, xformed_footprint, Animation, Config, DownscaleFilter, Gallery, GallerySpreads,
    ImgKey, ImageData, SortMode, Xform, GRID_CELL, GRID_LABEL_H, GRID_ROW_H, MAX_FULL_PIXELS,
    PREVIEW_TEXTURE_DIM,
};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Wheel delta accumulated before a flip triggers in wheel-navigation mode.
const THRESH_WHEEL: f32 = 60.0;

/// How many images on each side of the current one get their preview preloaded.
const PRELOAD_RADIUS: usize = 3;

/// How long to wait for a follow-up frame after the display size changed, to
/// decide whether a drag has stopped. One frame is enough — the test is whether
/// the size is unchanged since the previous frame, not how long it has been.
const RESIZE_SETTLE_REPAINT: std::time::Duration = std::time::Duration::from_millis(16);

/// Source size for the window icon, in pixels. winit scales this to whatever the
/// shell asks for, so this only sets the ceiling: 256 is the largest size
/// `icon::render` produces for the `.ico`, and a taskbar button wants 24-32 even
/// on a HiDPI display, so there is nothing to gain from generating larger.
const WINDOW_ICON_PX: u32 = 256;

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

// Job priorities (lower runs first) live in the library, next to `plan_jobs`,
// so the planner and its priorities cannot drift apart.

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
                    // Explorer caches icons per user and does not notice a
                    // changed registration on its own, so without this the
                    // obvious conclusion is that the fix did not work.
                    match fileassoc::default_icon() {
                        Some(icon) => println!("Icon: {icon}"),
                        None => println!("Warning: no DefaultIcon was registered; files will show a generic icon."),
                    }
                    println!("If icons have not changed, run: ie4uinit.exe -show  (then restart Explorer).");
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
        Some("--icc-scan") => return icc_scan(&args),
        _ => {}
    }

    // `--probe-renderer <name>` is the child half of the backend probe: it does
    // adapter enumeration for one backend and exits, so that a driver which
    // access-violates takes this process down and nothing else. Never reached
    // unless the parent asked for it.
    if let Some(i) = args.iter().position(|a| a == "--probe-renderer") {
        let name = args.get(i + 1).and_then(|s| image_viewer::WgpuBackend::parse(s));
        let Some(backend) = name else {
            eprintln!("--probe-renderer needs a backend name (dx12, gl, vulkan)");
            std::process::exit(2);
        };
        std::process::exit(probe_one(backend));
    }

    // The taskbar keys its icon cache on an AppUserModelID, and caches whatever
    // it found the first time. Declared here, before any window exists, because
    // after the window is created the call is ignored. See
    // `fileassoc::declare_taskbar_identity` for why neither the embedded icon
    // resource nor `WM_SETICON` is sufficient on its own.
    if let Err(e) = fileassoc::declare_taskbar_identity() {
        eprintln!("note: could not declare a taskbar identity: {e}");
    }

    // The renderer can change while the app is running, which means re-entering
    // the eframe event loop. The app cannot hand that back through its return
    // value, so it leaves a request in this slot on its way out and the loop
    // below picks it up.
    let restart: RestartSlot = std::sync::Arc::new(std::sync::Mutex::new(None));

    let config_path = image_viewer::Config::default_path();
    let mut config = image_viewer::Config::load(&config_path);
    let (mut backend, probed) = resolve_renderer(&config.renderer);
    if let Some(p) = probed {
        // Remember the answer so the next launch does not probe again.
        config.renderer.probed = Some(p);
        let _ = config.save(&config_path);
    }

    // One iteration per renderer the user runs. The app is rebuilt from the
    // config each time, so a restart keeps the folder, sort mode and settings
    // but loses the in-memory preview cache and has to decode again.
    loop {
        let options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1280.0, 800.0])
                // The window icon, set at creation rather than afterwards.
                //
                // This is the whole fix, and it took four attempts to find the
                // obvious API. Everything before it was the icon reaching the
                // window *late*: `WM_SETICON` only works if the window procedure
                // forwards it to `DefWindowProc` (winit's does not, and it serves
                // icons from its own state instead), and DWM caches the title
                // bar's icon when the non-client area is built — which is before
                // any of our code runs. Setting it here is the only point where
                // the window does not exist yet, so nothing has to be corrected
                // afterwards.
                //
                // `icon::render` is the same generator that produces
                // `assets/app.ico` for the embedded resource, so the window icon
                // and the icon in the exe are the same artwork by construction
                // rather than by two files agreeing.
                .with_icon(std::sync::Arc::new(egui::IconData {
                    rgba: image_viewer::icon::render(WINDOW_ICON_PX).into_raw(),
                    width: WINDOW_ICON_PX,
                    height: WINDOW_ICON_PX,
                })),
            wgpu_options: wgpu_options(backend),
            ..Default::default()
        };
        let slot = restart.clone();
        let result = eframe::run_native(
            "Image Viewer",
            options,
            Box::new(move |cc| {
                Ok(Box::new(MyApp::new_with_renderer(cc, slot)) as Box<dyn eframe::App>)
            }),
        );

        // Whatever happened, get the request out before deciding. The perf
        // report is written by `on_exit`, not here: that is the clean-exit path,
        // and writing it from both places would produce it twice.
        let requested = restart.lock().ok().and_then(|mut s| s.take());

        match (result, requested) {
            (_, Some(next)) if next != backend => {
                eprintln!("Restarting with the {} backend...", next.name());
                backend = next;
                // Loop, rather than fall through: the user asked for this
                // backend and has not been shown it yet.
            }
            (Ok(_), _) => return Ok(()),
            (Err(e), _) => {
                eprintln!("Failed to start the viewer: {e}");
                return Err(e.into());
            }
        }
    }
}

/// Where a running app leaves "restart me with this backend" on its way out.
///
/// A `run_native` loop cannot see inside the `App` it is handed, and a hard
/// crash is not something to recover from in-process, so the only two channels
/// available are a process exit code (too coarse) and shared state. This is the
/// shared state.
type RestartSlot = std::sync::Arc<std::sync::Mutex<Option<image_viewer::WgpuBackend>>>;

/// wgpu configuration, pinned to exactly one backend.
///
/// **One backend per process, and that is not a simplification — it is the whole
/// design.** wgpu enumerates every backend in the set during adapter discovery,
/// *before* it has chosen anything, and on some drivers that enumeration is a
/// hard access violation (`0xC0000005`) before any window exists. Measured, not
/// assumed, on the development machine:
///
/// - `vulkaninfo` reports a healthy device — Intel UHD 620, API 1.2.187. So
///   **Vulkan itself works.**
/// - wgpu with `VULKAN | DX12` access-violates, 3/3 trials, nothing on stderr.
/// - wgpu with `DX12` alone starts every time.
///
/// So "try DX12, and fall back to Vulkan if it fails" cannot be written in one
/// process: enabling Vulkan is what kills it. Hence the probe in a *child*
/// process, which is the only place a hard crash can be survived. See
/// `probe_backends`.
///
/// A registry probe for `Khronos\Vulkan\Drivers` was tried and is a false signal
/// in both directions — the key is *absent* on this machine while Vulkan works
/// (the loader finds the Intel ICD with no `Drivers` key and no `VK_*` variables
/// set), and a key being present does not mean wgpu can use the driver behind it.
///
/// The chosen backend is printed so a crash is never a mystery.
fn wgpu_options(backend: image_viewer::WgpuBackend) -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{
        wgpu::{Backends, InstanceDescriptor},
        WgpuSetup, WgpuSetupCreateNew,
    };

    // The bit values live in `lib.rs` so the policy is testable without pulling
    // wgpu into the library; the conversion to the real flags happens here.
    let bits = image_viewer::wgpu_backend_set(backend);
    let backends = match backend {
        image_viewer::WgpuBackend::Gl => Backends::GL,
        image_viewer::WgpuBackend::Vulkan => Backends::VULKAN,
        // `Auto` is resolved before it gets here, and falls back to DX12.
        _ => Backends::DX12,
    };
    debug_assert_eq!(bits, backends.bits(), "backend bit value drifted from lib.rs");

    let mut instance = InstanceDescriptor::new_without_display_handle();
    instance.backends = backends;
    eprintln!("wgpu backend: {backends:?} ({} requested)", backend.name());

    eframe::egui_wgpu::WgpuConfiguration {
        // `surface` is deliberately left at the default (HIGH_THROUGHPUT, frame
        // latency 2). `SurfaceConfig::LOW_LATENCY` is documented as the right
        // choice for a GUI with little GPU work, which is this app, so it looks
        // like a free improvement — but it was tried and could not be shown to be
        // one. Don't flip it on the strength of the docs: re-measure on a *local
        // console* session first. An earlier attempt at this produced a confident
        // wrong answer by measuring over RDP.
        wgpu_setup: WgpuSetup::CreateNew(WgpuSetupCreateNew {
            instance_descriptor: instance,
            ..WgpuSetupCreateNew::without_display_handle()
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Backend probing
// ---------------------------------------------------------------------------

/// Probe one backend, in *this* process. Prints a verdict and returns the exit
/// code the parent reads.
///
/// This is the child half of [`probe_backends`]. It does the minimum that
/// actually crashes: create the instance and enumerate adapters. No window, no
/// surface, no app — because a failure later than adapter enumeration would not
/// tell us anything useful about the driver, and a window would flash.
fn probe_one(backend: image_viewer::WgpuBackend) -> i32 {
    use eframe::egui_wgpu::wgpu::{
        Backends, DeviceDescriptor, Instance, InstanceDescriptor, RequestAdapterOptions,
    };

    let backends = match backend {
        image_viewer::WgpuBackend::Gl => Backends::GL,
        image_viewer::WgpuBackend::Vulkan => Backends::VULKAN,
        _ => Backends::DX12,
    };
    let mut desc = InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;

    // As far as the driver as possible, and no further: instance, adapter, then
    // **a device**. Enumerating adapters alone is not enough and this is not
    // hypothetical — the first version of this probe stopped at enumeration and
    // reported Vulkan as working, on a machine where starting the app on Vulkan
    // access-violates. Enumeration walks the loader's list; the crash is further
    // in, where the driver is actually initialised.
    //
    // A surface is deliberately not created: that needs a window, and a probe
    // that flashes a window is worse than one that is slightly less thorough.
    // The consequence is real and worth stating — a backend that fails only on
    // surface creation will still pass this probe.
    let verdict = pollster::block_on(async {
        let instance = Instance::new(desc);
        let adapter = match instance
            .request_adapter(&RequestAdapterOptions::default())
            .await
        {
            Ok(a) => a,
            Err(e) => return Err(format!("no adapter: {e}")),
        };
        let info = adapter.get_info();
        match adapter
            .request_device(&DeviceDescriptor {
                label: None,
                required_features: eframe::egui_wgpu::wgpu::Features::empty(),
                // Downlevel defaults so a weak or software adapter is not
                // rejected for limits this app never uses.
                required_limits: eframe::egui_wgpu::wgpu::Limits::downlevel_defaults(),
                experimental_features: eframe::egui_wgpu::wgpu::ExperimentalFeatures::disabled(),
                memory_hints: eframe::egui_wgpu::wgpu::MemoryHints::Performance,
                trace: eframe::egui_wgpu::wgpu::Trace::Off,
            })
            .await
        {
            Ok(_) => Ok(format!("{} / {}", info.name, info.driver)),
            Err(e) => Err(format!("no device: {e}")),
        }
    });

    match verdict {
        Ok(what) => {
            println!("ok {} {what}", backend.name());
            0
        }
        Err(why) => {
            println!("fail {} {why}", backend.name());
            1
        }
    }
}

/// Probe each candidate in its own process and return what worked.
///
/// A backend that *crashes* is contained by the child: the parent sees an exit
/// code that is not 0 and moves on. That is the only mechanism that works here,
/// because the failure is an access violation rather than an error return, and a
/// process that has access-violated cannot catch its own way out of it.
fn probe_backends() -> Vec<(image_viewer::WgpuBackend, bool)> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for &backend in &image_viewer::PROBE_ORDER {
        let out = std::process::Command::new(&exe)
            .arg("--probe-renderer")
            .arg(backend.name())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // A missing exe, or a non-zero exit for any reason including a
        // crash, both count as "this one does not work".
        let ok = matches!(out, Ok(s) if s.success());
        results.push((backend, ok));
        // **Stop at the first that works.** The other backends exist only to be
        // tried when this one fails, so probing past a success is work whose
        // result is thrown away — and it is not cheap. Probing all three cost
        // ~1456 ms of the first launch, because adapter + device creation is
        // 186 ms on GL, 455 ms on DX12 and 474 ms on Vulkan, run one after
        // another, and GL leads `PROBE_ORDER`, so almost every first launch was
        // paying for two backends it was never going to use.
        //
        // This changes no behaviour that matters, and the reason is a property
        // rather than an assertion: a shortened list contains at most one
        // success — everything before the first success is a failure by
        // construction — so the entries that were never probed cannot change what
        // `pick_probed` decides. Checked exhaustively by
        // `a_probe_that_stopped_at_the_first_success_decides_the_same_way`.
        //
        // The insurance is untouched: a machine where GL is unavailable still
        // falls through to DX12, then Vulkan.
        if ok {
            break;
        }
    }
    results
}

/// Resolve the backend to use at launch, probing only if the preference needs
/// it. Returns the backend and, if a probe ran and found something, the answer
/// to cache in the config.
fn resolve_renderer(pref: &image_viewer::RendererPref) -> (image_viewer::WgpuBackend, Option<image_viewer::WgpuBackend>) {
    if !pref.needs_probe() {
        return (pref.resolve(None), None);
    }
    eprintln!("Probing for a working graphics backend...");
    let results = probe_backends();
    for (b, ok) in &results {
        eprintln!("  {:>7}: {}", b.name(), if *ok { "works" } else { "unavailable" });
    }
    let picked = image_viewer::pick_probed(&results);
    (picked, Some(picked))
}

// ---------------------------------------------------------------------------
// Background loader: a tiny priority queue served by a few worker threads
// ---------------------------------------------------------------------------
//
// `Kind`, `Job` and `Loader` live in the library (see `lib.rs`, "Load
// scheduling") so the queue can be tested directly. The stranding bug that
// emptied this queue lived in the interaction between the plan and the queue,
// and neither half could be reached from a test while it lived in a binary.

use image_viewer::{Job, Kind, Loader, PRIO_CURRENT_EXIF, PRIO_THUMB_BASE};
const PRIO_THUMB: u32 = PRIO_THUMB_BASE;
const PRIO_EXIF: u32 = PRIO_CURRENT_EXIF;

/// Long side of grid thumbnails.
const THUMB_DIM: u32 = 256;
/// Max grid thumbnails kept on the GPU (256 x ~0.3 MB ≈ 80 MB).
const THUMB_CACHE_CAP: usize = 512;
/// Decode attempts per file before a grid cell gives up (permanent
/// placeholder). Enough for transient failures, bounded so a corrupt file
/// can't keep the worker pool busy.
const THUMB_TRIES: u8 = 3;

/// File name as a lossy string, the key `last_by_folder` resume stores.
fn file_name_of(p: &Path) -> String {
    image_viewer::file_name_of(p)
}

/// `--icc-scan <folder>`: classify every image in a folder and print the tally.
///
/// The question it answers is "how much of this library is wide gamut, and which
/// files?", which otherwise means opening every image and reading the metadata
/// panel. Nothing is configured and nothing is saved, so it prints and exits
/// rather than becoming a mode the viewer can be left in.
///
/// Note for whoever adds a flag with a value: `--icc-scan` is deliberately
/// **not** in `path_arg`'s `FLAGS_WITH_VALUE`. That list exists so the *viewer's*
/// folder argument is not stolen by a flag's own value; here the value **is** the
/// folder we want, and adding it would leave this command with no folder at all.
fn icc_scan(args: &[String]) -> eframe::Result<()> {
    use image_viewer::ColorSpace;

    let Some(folder) = image_viewer::path_arg(args.iter().skip(1)) else {
        eprintln!("--icc-scan needs a folder");
        eprintln!("  usage: image-viewer.exe --icc-scan <folder>");
        std::process::exit(2);
    };

    let found = image_viewer::scan_colour(&folder);
    if found.is_empty() {
        println!("No images in {}", folder.display());
        return Ok(());
    }

    // Fixed order so two runs of the same folder print the same thing, and so the
    // classes that need attention come first rather than in enum order.
    const ORDER: [ColorSpace; 6] = [
        ColorSpace::WideGamut,
        ColorSpace::Unknown,
        ColorSpace::NonRgb,
        ColorSpace::Srgb,
        ColorSpace::UntaggedSrgb,
        ColorSpace::NotInspected,
    ];

    println!(
        "Colour audit of {}  ({} image{})",
        folder.display(),
        found.len(),
        if found.len() == 1 { "" } else { "s" }
    );
    println!();
    for space in ORDER {
        let n = found.iter().filter(|(_, i)| i.space == space).count();
        let note = match space {
            ColorSpace::WideGamut => "displayed without conversion - reads flat",
            ColorSpace::Unknown => "profile could not be read",
            ColorSpace::NonRgb => "not an RGB space",
            ColorSpace::NotInspected => "colour lives where this build does not look",
            _ => "",
        };
        println!("  {:<18} {n:>6}   {note}", space.label());
    }

    // List what somebody could act on. `UntaggedSrgb` is the overwhelming
    // majority of a real library and listing it would bury everything else, so it
    // is counted only. `NotInspected` *is* listed, under its own heading: it is
    // this build's blind spot, and "here is what I cannot tell you" is the most
    // useful thing an audit can say.
    let of_space = |space: image_viewer::ColorSpace| -> Vec<&std::path::PathBuf> {
        found
            .iter()
            .filter(|(_, i)| i.space == space)
            .map(|(p, _)| p)
            .collect()
    };
    for (space, title) in [
        (
            image_viewer::ColorSpace::WideGamut,
            "Wide gamut - displayed without conversion, so they read flat",
        ),
        (
            image_viewer::ColorSpace::Unknown,
            "Unreadable profile - could not be classified either way",
        ),
        (
            image_viewer::ColorSpace::NonRgb,
            "Not an RGB space",
        ),
        (
            image_viewer::ColorSpace::NotInspected,
            "Not inspected - this build does not read this format's colour",
        ),
    ] {
        let list = of_space(space);
        if list.is_empty() {
            continue;
        }
        println!();
        println!("{} ({}):", title, list.len());
        for p in list.iter().take(MAX_LISTED) {
            println!("  {}", file_name_of(p));
        }
        if list.len() > MAX_LISTED {
            println!("  ... and {} more", list.len() - MAX_LISTED);
        }
    }
    Ok(())
}

/// How many filenames to print per class before summarising the rest. An audit
/// of 1000 files should not scroll 1000 lines, but it must also never hide that
/// there were more.
const MAX_LISTED: usize = 500;

/// Identity of a file's *contents* for change detection.
///
/// `mtime` alone is not enough: editors and copy tools can write a file with a
/// preserved timestamp, and a same-size overwrite within one filesystem tick
/// can keep the same one too. The pair catches the common cases at the cost of
/// one `metadata` call per file per poll, which the poll already does not
/// otherwise need.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    mtime: Option<std::time::SystemTime>,
    len: u64,
}

impl Stamp {
    fn of(path: &Path) -> Stamp {
        match std::fs::metadata(path) {
            Ok(m) => Stamp {
                mtime: m.modified().ok(),
                len: m.len(),
            },
            // A file we cannot stat is treated as distinct-but-stable rather
            // than as "changed every time", which would spin the reload path.
            Err(_) => Stamp { mtime: None, len: 0 },
        }
    }
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
    // The `target_dim` this decode was asked for. Kept so staleness can be
    // judged against what we *asked for* rather than the pixels we got back —
    // comparing pixels re-decodes forever on a source smaller than the display.
    target: u32,
    // The file's EXIF orientation tag, and the block grid a lossless JPEG crop
    // has to align to. Both are read on the worker from the bytes the decode
    // already had in hand, and both exist for the crop UI: a rectangle drawn on
    // the *displayed* picture has to be expressed in *stored* coordinates, which
    // is a composition involving the tag, and it has to land on the block grid
    // or TurboJPEG will not cut it where the user drew.
    exif_tag: u16,
    crop_grid: (u32, u32),
}

/// What one grid cell needs, and nothing else.
///
/// A cell is 150 points of a fixed picture, so every field of [`PreviewData`]
/// except the image is either meaningless for it (`orig`, `target`, `exif_tag` and
/// `crop_grid` all describe the *displayed* image) or actively harmful: `full` is
/// how a 256 px request ended up decoding at 4096 and shipping a 100 MB
/// full-resolution image back to be dropped, and `anim` is how a cell ended up
/// decoding 240 frames to show the first. Not having the fields is the point —
/// there is no way to fill them by accident.
struct ThumbData {
    image: ColorImage,
    load_ms: u64,
}

/// A byte count as something a person can read. Used by the settings panel to
/// state the thumbnail cache's cap rather than printing 268435456.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.0} {}", UNITS[u])
    }
}

enum Msg {
    Preview {
        key: ImgKey,
        result: Result<PreviewData, String>,
    },
    Thumb {
        path: PathBuf,
        result: Result<ThumbData, String>,
    },
    Full {
        key: ImgKey,
        result: Result<Arc<ImageData>, String>,
    },
    Exif {
        path: PathBuf,
        info: Option<String>,
    },
}

/// Start the worker pool for this app: real decoding, real panic capture, and
/// a repaint request when something arrives.
fn new_loader(ctx: &Context, tx: Sender<Msg>) -> image_viewer::Loader {
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 4);
    let wake_ctx = ctx.clone();
    image_viewer::Loader::spawn(
        workers,
        tx,
        run_job,
        fail_msg,
        move || wake_ctx.request_repaint(),
    )
}

/// A failure is still a *delivered* result: it gets reported and remembered,
/// so the loader must not treat it as work left undone.
fn fail_msg(job: &Job, err: String) -> (Msg, bool) {
    let msg = match job.kind {
        Kind::Preview => Msg::Preview {
            key: job.key.clone(),
            result: Err(err),
        },
        Kind::Full => Msg::Full {
            key: job.key.clone(),
            result: Err(err),
        },
        Kind::Thumb => Msg::Thumb {
            path: job.path().to_path_buf(),
            result: Err(err),
        },
        Kind::Exif => Msg::Exif {
            path: job.path().to_path_buf(),
            info: None,
        },
    };
    (msg, true)
}

/// Decode one job. Always `delivered = true` today: every job runs to
/// completion, so there is no "gave up without a result" case yet. That is the
/// hook reintroduced cancellation will use, and the reason the loader re-queues
/// a wanted job on abandonment rather than on completion.
fn run_job(job: &Job) -> (Msg, bool) {
    let path = job.path().to_path_buf();
    // The page is part of the request, so the worker decodes *that* page rather
    // than always the first. Pages of a multi-page TIFF are one file read and one
    // IFD walk apart, so this costs nothing beyond the IFD walk.
    let page = job.key.page;
    let msg = match job.kind {
        Kind::Preview => Msg::Preview {
            result: decode_preview(&path, job.target_dim, page),
            key: job.key.clone(),
        },
        Kind::Thumb => Msg::Thumb {
            // A grid cell shows the file's first page, always — a cell is one
            // picture, and making it a slideshow would be worse than useless.
            result: decode_thumb(&path, job.target_dim),
            path,
        },
        Kind::Full => Msg::Full {
            result: load_image_full(&path, page)
                .map(Arc::new)
                .map_err(|e| e.to_string()),
            key: job.key.clone(),
        },
        Kind::Exif => Msg::Exif {
            info: extract_exif(&path),
            path,
        },
    };
    (msg, true)
}

/// Decode a preview sized for the current display area (in physical pixels).
/// The result is the full-res image when it fits the target, otherwise a
/// texture capped at the target long side — always sharp at fit-to-window.
fn decode_preview(path: &Path, target_dim: u32, page: u32) -> Result<PreviewData, String> {
    // Single pass: reads the file once. Large JPEGs get a cheap DCT-scaled
    // preview; everything else is fully decoded once and the preview is
    // derived from that, so the full image is already available.
    let r = load_page_preview_full(path, target_dim, page).map_err(|e| e.to_string())?;
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
        target: target_dim,
        exif_tag: r.exif_tag,
        crop_grid: r.crop_grid,
    })
}

/// One grid cell, at the cell's own size.
///
/// Goes through [`load_grid_thumbnail`], so this is the only place a cell can be
/// served from the on-disk cache, and the reason a cell is a different request
/// from a preview rather than the same one with a smaller number.
fn decode_thumb(path: &Path, target_dim: u32) -> Result<ThumbData, String> {
    let d = load_grid_thumbnail(path, target_dim).map_err(|e| e.to_string())?;
    Ok(ThumbData {
        image: ColorImage::from_rgba_unmultiplied([d.width as usize, d.height as usize], &d.rgba),
        load_ms: d.load_time.as_millis() as u64,
    })
}

/// Metadata panel text for `path`, or `None` when there is nothing worth saying.
///
/// EXIF and the colour profile are gathered **independently**. They used to be
/// one function with `read_exif(path).ok()?` at the top, which meant a file
/// with an ICC profile but no EXIF block — which is exactly what a screenshot or
/// a downloaded/forwarded phone photo is — showed a completely empty panel and
/// hid the only interesting thing about it.
fn extract_exif(path: &Path) -> Option<String> {
    let mut info = Vec::new();

    // Colour first: it is independent of EXIF, and often the only thing there is.
    if let Some(line) = image_viewer::color_info(path).panel_line() {
        info.push(line);
    }

    // Everything below needs an EXIF block; absent one just means fewer lines.
    let Ok(exif) = read_exif(path) else {
        return if info.is_empty() {
            None
        } else {
            Some(info.join("\n"))
        };
    };

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
    // The `target_dim` this texture was decoded for. With `px` and `orig` this
    // is everything `needs_refine` needs, so staleness is a pure function of the
    // cache and the current display size — no bookkeeping to keep in sync.
    // (This replaces the old `below_target` flag, which could only say "the
    // source is bigger than what we asked for" and so could not tell a source
    // that is simply smaller than the display from one worth re-decoding.)
    target: u32,
    // Pixel dimensions of the texture, for the same check.
    px: (u32, u32),
    // The file's EXIF orientation tag and the block grid a lossless JPEG crop
    // aligns to. Cached with the preview so the crop UI never has to touch the
    // file: a rectangle drawn on the *displayed* picture must be expressed in
    // *stored* coordinates, and that composition needs the tag.
    exif_tag: u16,
    crop_grid: (u32, u32),
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
    /// The file's EXIF orientation, which the decode has **already applied** to
    /// `data`. Carried here so the clipboard can place a stored-space crop into
    /// an oriented buffer without reaching back into the preview cache — and
    /// because the value that describes a buffer is a property of the buffer.
    exif_tag: u16,
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

    /// The backend actually in use, and where to leave a "restart with this one"
    /// request for the launch loop. The app cannot return a value to
    /// `run_native`, so changing the renderer means closing the window and
    /// letting the loop build a new one.
    renderer: image_viewer::WgpuBackend,
    restart: RestartSlot,

    loader: Loader,
    rx: Receiver<Msg>,

    /// Decoded previews, keyed by **(file, page)** rather than by file. Page 1 and
    /// page 3 of one document are different pictures, and a path-keyed cache hands
    /// back the wrong one — silently, and only for multi-page files.
    previews: HashMap<ImgKey, Preview>,
    /// `(key, kind)` pairs that failed to decode, permanent for the session.
    /// Per *image*, so a document with one unreadable page keeps its other pages.
    failed: HashSet<(ImgKey, Kind)>,
    clock: u64,
    frame: u64,

    /// What the user asked for: a file and a page of it.
    cur: Option<ImgKey>,
    /// What is on screen: a file and a page of it. Lags `cur` until the wanted
    /// preview has been adopted by `sync_shown`.
    ///
    /// A key rather than a path, even though the two differ only by a page number
    /// that is 0 almost always. During the lag — which is every uncached
    /// navigation, and therefore most of them — `shown` names the *previous*
    /// image, and if it remembered only the path then a document's old page would
    /// be lost the moment you moved to another one. The page has to be part of
    /// what "what is on screen" means.
    shown: Option<ImgKey>,
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
    /// Resize coalescing: issues one sharper decode per settled size instead of
    /// one per frame, and never blanks the picture. See `ResizeTracker`.
    resize: image_viewer::ResizeTracker,
    waiting_load: bool, // block_while_loading: current image not decoded yet
    /// Index highlighted in the grid (G). Browsing the grid does *not* change
    /// the shown image — only Enter or a click does — so arrowing around a
    /// 2000-image folder costs no re-decodes.
    grid_sel: usize,
    /// Columns the grid last laid out with; Up/Down move by this much.
    grid_cols: usize,
    /// Quarter turns and mirrors applied per image, for this session. Keyed by path
    /// because a transform belongs to a *picture*: turning one photo must not turn
    /// the next one, and returning to a turned photo must still find it turned.
    /// Lives in `lib.rs` so the rule can be tested — see [`image_viewer::ImageEdits`].
    edits: image_viewer::ImageEdits,
    /// Crop mode: left-drag frames a crop instead of panning.
    ///
    /// A mode rather than a modifier, because a modifier would have to be held
    /// for the whole gesture and the frame is adjusted after the fact anyway —
    /// the crop is stored as soon as it exists and stays until cleared. Panning is
    /// unreachable while this is on, which is the one real cost; leaving crop mode
    /// is one click and the button says which mode you are in.
    crop_mode: bool,
    /// Selection index the grid's scroll offset was last set for. The offset is
    /// written only when this differs, so manual scrolling is never fought.
    grid_sel_scrolled: usize,
    /// "User intent -> useful pixels" tracking for the perf report.
    lat: Latency,
    /// First frame's timestamp, for startup latency.
    t_boot: Option<Instant>,
    /// Start of the previous frame, for the frame-interval metric.
    t_prev_frame: Option<Instant>,
    pending_advance: Option<bool>, // queued step, consumed after one painted frame
    zip_archive: Option<PathBuf>, // archive currently open (cbz/zip)
    zip_temp: Option<PathBuf>, // its extracted temp dir
    /// The user's own pairing decisions for the gallery on screen: which pages take
    /// a whole turn, and how many leading pages stand alone. Loaded when a gallery
    /// is opened, and one file's entries are dropped by `invalidate_path`, because
    /// a re-export can change a file's page count and an override naming page 3 of
    /// a one-page document is a decision about pixels that no longer exist.
    spreads: GallerySpreads,
    /// The gallery `spreads` belongs to, as the overrides file keys it. **For an
    /// archive this is the ORIGINAL archive path**, never the `%TEMP%` extraction
    /// directory, whose name changes on every re-extract and would orphan every
    /// override the first time the `.cbz` was edited. It is a field rather than
    /// something `save_spreads` re-derives so a save can never write to a different
    /// key than the load read.
    spreads_key: Option<String>,
    wheel_acc: f32, // accumulated wheel delta in wheel-navigation mode
    thumbs: HashMap<PathBuf, Thumb>, // grid thumbnails
    thumb_tries: HashMap<PathBuf, u8>, // per-path thumb attempts (bounded retry)
    gallery_view: bool, // grid browser open
    /// Every file in the folder, **unfiltered**. `gallery.files` is this list
    /// run through `filter`, so a filter is always reversible and the folder's
    /// real contents are never thrown away.
    all_files: Vec<PathBuf>,
    /// Case-insensitive substring match on the file name. Empty = show all.
    /// Deliberately not persisted: a filter that silently survived a restart
    /// would hide files with no obvious way back.
    filter: String,
    /// `Stamp` per gallery file as of the last poll. A stamp that moved means
    /// the file was edited on disk and anything cached for it is stale.
    stamps: HashMap<PathBuf, Stamp>,
    /// Grid rows currently on screen, so PageUp/PageDown can move a page.
    grid_page_rows: usize,
}

impl MyApp {
    fn new_with_renderer(cc: &eframe::CreationContext<'_>, restart: RestartSlot) -> Self {
        let ctx = &cc.egui_ctx;
        let config_path = Config::default_path();
        let config = Config::load(&config_path);
        // The launch loop already resolved this (probing if it needed to), and
        // the answer is in the config, so reading it back keeps the two in step.
        let renderer = config.renderer.resolve(None);

        // Optional CLI argument: a file or a folder ("Open with…"). Must be the
        // first *non-flag* argument: `nth(1)` would take `--perf` as the path
        // and silently open the persisted folder instead.
        let arg = image_viewer::path_arg(std::env::args().skip(1));
        // Which file, if any, the user named. Taken before the `match` below
        // consumes `arg`, and excluding archives because those are extracted to a
        // temp folder and opened as a directory instead.
        let named_file: Option<PathBuf> = arg.clone().filter(|a| a.is_file() && !is_archive_path(a));
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
            let files = scan_folder_sorted(&folder, config.sort);
            gallery.index = image_viewer::startup_index(
                &files,
                named_file.as_deref(),
                config.last_by_folder.get(&folder.to_string_lossy().to_string()).map(|s| s.as_str()),
            );
            gallery.files = files;
        }

        set_linear_resample(config.linear_resample);
        set_downscale_filter(config.downscale_filter);
        image_viewer::set_wide_gamut_conversion(config.convert_wide_gamut);
        set_thumbnail_cache(config.thumbnail_cache);

        if !config_path.exists() {
            config.save(&config_path);
        }

        let (tx, rx) = mpsc::channel();
        let loader = new_loader(ctx, tx);

        // Taken before `gallery` is moved into the struct below.
        let all_files = gallery.files.clone();
        gallery.loop_back = config.loop_back;
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
            resize: image_viewer::ResizeTracker::new(),
            renderer,
            restart,
            suppress_resize_redecode: false,
            waiting_load: false,
            grid_sel: 0,
            grid_cols: 1,
            edits: image_viewer::ImageEdits::default(),
            crop_mode: false,
            grid_sel_scrolled: usize::MAX,
            lat: Latency::default(),
            t_boot: perf::boot(),
            t_prev_frame: None,
            pending_advance: None,
            zip_archive,
            zip_temp,
            spreads: GallerySpreads::default(),
            spreads_key: None,
            wheel_acc: 0.0,
            thumbs: HashMap::new(),
            thumb_tries: HashMap::new(),
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
    /// **It reads `zip_archive` first, so it depends on `close_zip()` having run when
    /// leaving an archive.** Every caller that can follow one does: `open_folder` and
    /// `open_file` close before adopting, and `open_archive` closes before
    /// extracting. (The constructor has no archive to close.) A caller that opened a
    /// gallery without closing would key it by a temp directory that is about to be
    /// deleted, which is the exact mistake this function exists to prevent.
    fn refresh_spreads(&mut self) {
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
    fn save_spreads(&self) {
        let (Some(store), Some(key)) = (spreads_root(), self.spreads_key.as_deref()) else {
            return;
        };
        let _ = save_spread_overrides(&store, Path::new(key), &self.spreads);
    }

    // ---- navigation -------------------------------------------------------

    /// Point the app at `gallery.current()` and (re)plan all background work.
    fn goto(&mut self, ctx: &Context) {
        // A goto that follows a keypress is navigation; a fresh open/click is a
        // selection. They are reported separately because the preloaded case
        // (0 ms) is the common one for navigation.
        let is_nav = self.lat.t_request.is_some();
        self.lat.begin(Instant::now(), is_nav);
        // Learn how many pages the file we are arriving at has, *before* the page
        // is read, so `next` knows whether there is a page 2 to go to. This is
        // four bytes for anything that is not a TIFF, and the answer is cached, so
        // it is paid once per file rather than once per visit.
        if let Some(p) = self.gallery.current().map(|p| p.to_path_buf()) {
            self.gallery.learn_pages(&p);
        }
        // How large this page *displays* at, and the next one's, which is what the
        // manga pairing judges a page by: a turn is decided by asking about the page
        // after it, so learning only this one would let a spread re-flow under the
        // reader. Beside `learn_pages` because both are "measure what we are arriving
        // at" and both answer from a cache, and `learn_shapes` reads the page count
        // `learn_pages` just filled in, so it must come second. May already be
        // answered: `pump` records a page's dimensions as its preview lands, so a
        // page in the preload window has them before the user gets there.
        self.gallery.learn_shapes(self.gallery.page_pos());
        self.cur = self
            .gallery
            .current()
            .map(|p| ImgKey {
                path: p.to_path_buf(),
                page: self.gallery.current_page(),
            });
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
            .and_then(|p| p.path.file_name())
            .and_then(|n| n.to_str())
        {
            // "page 3 of 12" only when the file has more than one page, so a
            // normal photo's title is exactly what it always was.
            let title = match self.gallery.pages_in_current() {
                n if n > 1 => format!("Image Viewer - {name}  ({}/{})", self.gallery.current_page() + 1, n),
                _ => format!("Image Viewer - {name}"),
            };
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
        self.schedule();
    }

    fn step(&mut self, ctx: &Context, forward: bool) {
        // "Wait for load" mode: throttle stepping to decode completion.
        if self.config.block_while_loading && self.waiting_load {
            return;
        }
        // With looping off, stepping at either end goes nowhere. Returning
        // without `goto` matters: it avoids resetting the full-res dwell timer
        // and re-planning the queue for an image that did not change.
        let moved = if forward {
            self.gallery.next()
        } else {
            self.gallery.prev()
        };
        if moved {
            self.goto(ctx);
        }
    }

    // ---- file list and filtering ------------------------------------------

    /// Rebuild `gallery.files` from `all_files` and the current filter. The
    /// filtering itself lives in [`Gallery::set_filtered`] so it is testable.
    fn apply_filter(&mut self) {
        self.gallery.set_filtered(&self.all_files, &self.filter);
    }

    /// Install a new **unfiltered** file list and re-apply the filter.
    ///
    /// This is the single place `gallery.files` is rebuilt from a whole-folder
    /// scan, so the filter cannot be forgotten on any path.
    fn set_files(&mut self, files: Vec<PathBuf>, ctx: &Context) {
        let current = self.gallery.current().map(|p| p.to_path_buf());
        self.all_files = files;
        self.apply_filter();
        if self.gallery.current() != current.as_deref() {
            self.reset_display();
            self.goto(ctx);
        } else {
            self.schedule();
        }
    }

    /// Adopt a freshly built gallery (folder / archive / single file), honouring
    /// any active filter and the looping setting.
    fn adopt_gallery(&mut self, g: Gallery, ctx: &Context) {
        self.gallery = g;
        self.gallery.loop_back = self.config.loop_back;
        self.all_files = self.gallery.files.clone();
        self.apply_filter();
        self.reset_display();
        // Loaded before `goto`, so whatever the navigation plans sees this gallery's
        // own overrides rather than the last folder's. `adopt_gallery` is the
        // single site every open goes through, which is why it is here and not in
        // each caller.
        self.refresh_spreads();
        self.goto(ctx);
    }

    /// Forget what is on screen (used when switching folders).
    fn reset_display(&mut self) {
        self.shown = None;
        self.full = None;
        self.anim = None;
    }

    /// Whether the current image still needs a full-resolution decode.
    ///
    /// Takes the *key*, not the path, because "is the full decode done" is a
    /// question about a page: page 3 of a document has its own full decode and its
    /// own failure, and asking about the file would answer for page 1.
    fn needs_full(&self, key: &ImgKey) -> bool {
        if self.full.as_ref().is_some_and(|f| f.path == key.path) {
            return false;
        }
        if self.failed.contains(&(key.clone(), Kind::Full)) {
            return false;
        }
        let Some(pv) = self.previews.get(key) else {
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
        // Preview texture target: the display area in physical pixels, so the
        // texture is exactly as sharp as the screen can show.
        let target_dim = self
            .display_dim
            .map(image_viewer::preview_target)
            .unwrap_or(PREVIEW_TEXTURE_DIM);

        // The cache is keyed on (file, page), so this is the set of *images*
        // already decoded rather than of files. That is what lets the planner ask
        // "is page 3 cached" and get an honest answer instead of page 1's.
        let cached: HashSet<ImgKey> = self.previews.keys().cloned().collect();
        let i = self.gallery.index.min(self.gallery.files.len().saturating_sub(1));
        let cur = self.gallery.files.get(i).cloned();
        // The plan is computed by the library so it can be tested; all this does
        // is describe the current state to it.
        let jobs = match &cur {
            None => Vec::new(),
            Some(cur) => image_viewer::plan_jobs(&image_viewer::Plan {
                files: &self.gallery.files,
                index: i,
                current_page: self.gallery.current_page(),
                cached: &cached,
                failed: &self.failed,
                // Hidden panel: nobody can read the result, and the read is a
                // whole-file `fs::read` on a worker a decode is waiting for. The
                // panel asks for its own data when it is drawn, so this being
                // false costs nothing but the work it saves.
                wants_exif: image_viewer::wants_exif(self.show_panels, self.exif_loading),
                wants_full: self.full_due.is_none()
                    && self.needs_full(&ImgKey {
                        path: cur.clone(),
                        page: self.gallery.current_page(),
                    }),
                current_stale: self.resize.pending(),
                target_dim,
                radius: PRELOAD_RADIUS,
            }),
        };
        self.loader.replace(jobs);
    }

    /// True when the image on screen has a cached preview that is softer than
    /// the display can now show, and a re-decode would actually be sharper.
    fn current_needs_refine(&self, target: u32) -> bool {
        let Some(pv) = self.cur.as_ref().and_then(|p| self.previews.get(p)) else {
            return false;
        };
        image_viewer::needs_refine(
            pv.px.0.max(pv.px.1),
            pv.orig.0.max(pv.orig.1),
            pv.target,
            target,
            image_viewer::RESIZE_PX_FLOOR,
        )
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
                Msg::Preview { key, result } => match result {
                    Ok(p) => {
                        let name = key
                            .path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy();
                        let px = (p.image.size[0] as u32, p.image.size[1] as u32);
                        let tex = ctx.load_texture(name.as_ref(), p.image, TextureOptions::LINEAR);
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
                                px,
                            },
                        );
                        // A refine that lands clears the pending flag. If the
                        // display grew again while it was decoding, the next
                        // frame re-arms it, so a resize is never left served by
                        // a texture that is still soft.
                        if Some(&key) == self.cur.as_ref() {
                            self.resize.landed();
                        }
                        resched |= Some(&key) == self.cur.as_ref();
                        // "Wait for load" mode: arrived preview for the
                        // current image unblocks; if the key is still held,
                        // queue one more advance — consumed next frame so the
                        // image is guaranteed to have been painted on screen.
                        if Some(&key) == self.cur.as_ref() && self.waiting_load {
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
                        eprintln!("Failed to load {}: {}", key, e);
                        if Some(&key) == self.cur.as_ref() {
                            self.error = Some(format!("Failed to load {}: {}", key, e));
                            self.waiting_load = false;
                        }
                        // Failed is per *image*, not per file: a document whose
                        // page 3 is an unsupported colour type must not poison its
                        // other 199 pages for the session.
                        self.failed.insert((key, Kind::Preview));
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
                                path: key.path,
                                data,
                                tiles: HashMap::new(),
                                load_ms,
                                exif_tag,
                            });
                        }
                    }
                    Err(e) => {
                        eprintln!("Full-res load failed for {}: {}", key, e);
                        self.failed.insert((key, Kind::Full));
                    }
                },
                Msg::Exif { path, info } => {
                    if self.cur.as_ref().is_some_and(|c| c.path == path) {
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
        if self.anim.as_ref().is_some_and(|a| a.path != cur.path) {
            self.anim = None;
        }
        // Page 1 and page 3 of one document are the same *file* but a different
        // picture, so the re-fit has to happen when the page changes and not only
        // when the file does. Comparing the key is what catches that; a path
        // comparison cannot, because the two have the same path.
        if self.previews.contains_key(&cur) && self.shown.as_ref() != Some(&cur) {
            self.shown = Some(cur.clone());
            self.view.fit = true;
        }
        // Adopt the full image that came with the preview's decode pass
        // (single-pass files), whether it just arrived or was preloaded
        // while the user was on a neighbouring image.
        if let Some(pv) = self.previews.get(&cur) {
            if let Some(data) = &pv.full {
                if self.full.as_ref().map_or(true, |f| f.path != cur.path) {
                    let load_ms = data.load_time.as_millis() as u64;
                    self.full = Some(FullImage {
                        path: cur.path.clone(),
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
                    let tex = ctx.load_texture("anim", image, TextureOptions::LINEAR);
                    self.anim = Some(AnimView {
                        path: cur.path.clone(),
                        anim: Arc::clone(anim),
                        tex,
                        start: Instant::now(),
                        last_index: usize::MAX,
                    });
                }
            }
        }
        if let Some(s) = &self.shown {
            if self.full.as_ref().is_some_and(|f| f.path != s.path) {
                self.full = None;
            }
        }
    }

    // ---- file pickers -----------------------------------------------------

    fn open_folder(&mut self, ctx: &Context, folder: PathBuf) {
        self.folder = folder.clone();
        let mut g = Gallery::new(&folder);
        if self.config.load_all_on_startup {
            g.load_all_from_folder(&folder);
        }
        self.adopt_gallery(g, ctx);
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
                        if let Some(i) = self
                            .gallery
                            .files
                            .iter()
                            .position(|f| file_name_of(f) == *name)
                        {
                            self.gallery.index = i;
                        }
                    }
                }
                self.reset_display();
                // `zip_archive` was set above, so this keys the gallery by the
                // archive's own path and not by `self.folder`, which is the temp
                // directory `extract_zip` just wrote — a name that changes on every
                // re-extract, so keying by it would lose every override the first
                // time the archive was edited.
                self.refresh_spreads();
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
        let mut g = Gallery::from_file(file);
        if self.config.load_all_on_startup {
            g.load_all_from_folder(&self.folder);
        }
        self.adopt_gallery(g, ctx);
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
            // Change detection is *not* gated on `load_all_on_startup`: that
            // setting is about listing files at startup, but noticing that the
            // file you are looking at just got re-exported should not depend on
            // it.
            self.check_changed(ctx);
            self.rescan(ctx, false);
        }
        ctx.request_repaint_after(self.next_scan.saturating_duration_since(Instant::now()));
    }

    /// Re-stat every file and invalidate anything whose contents changed since
    /// the last poll.
    ///
    /// Only files we hold state for are worth reloading, so the common case
    /// (a big folder, one image on screen) costs a metadata call per file and
    /// no decoding at all.
    fn check_changed(&mut self, ctx: &Context) {
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
    fn has_cached_state_for(&self, p: &Path) -> bool {
        // Every page of the file, not just page 0: a re-exported document makes
        // all of its pages stale, and `any` over the keys is the only way to ask
        // that when the cache is keyed by (file, page).
        self.previews.keys().any(|k| k.path == p)
            || self.thumbs.contains_key(p)
            || self.full.as_ref().is_some_and(|f| f.path == p)
            || self.cur.as_ref().is_some_and(|c| c.path == p)
            || self.failed.iter().any(|(q, _)| q.path == p)
    }

    /// Throw away everything cached for `p` and, if it is on screen, ask for it
    /// again.
    ///
    /// Clearing `failed` here is what makes an edit **self-healing**: a file
    /// that is re-read while it is still being written fails to decode, and
    /// `failed` is permanent for the session — but the writer's finishing
    /// touches the stamp again, so the next poll re-detects the change, clears
    /// the entry and retries. Without this, "edit the file I am looking at"
    /// would reliably poison it.
    fn invalidate_path(&mut self, p: &Path, ctx: &Context) {
        // All pages, not one. A path-keyed `remove` would be a no-op now that the
        // cache is keyed by (file, page), and the symptom would be a
        // re-exported document still showing the old version of page 3.
        self.previews.retain(|k, _| k.path != p);
        self.thumbs.remove(p);
        self.thumb_tries.remove(p);
        self.failed.retain(|(q, _)| q.path != p);
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
    fn on_filter_changed(&mut self, ctx: &Context) {
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
        }
    }

    // ---- file triage -------------------------------------------------------

    /// Remove the current file from the gallery (file already gone from disk)
    /// and advance to the next image.
    fn remove_current_from_gallery(&mut self, ctx: &Context) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        // Every page of the file goes: the file is leaving the gallery, so its
        // other pages are unreachable and holding their pixels is pure waste.
        self.previews.retain(|k, _| k.path != p);
        self.failed.retain(|(k, _)| k.path != p);
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
    fn request_renderer_restart(&mut self, ctx: &Context, backend: image_viewer::WgpuBackend) {
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
    fn probe_renderer_now(&mut self, ctx: &Context) {
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

    fn save_transformed_copy(&mut self) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let p = cur.path.clone();
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
    fn copy_image_to_clipboard(&mut self) {
        let Some(cur) = self.cur.clone() else {
            return;
        };
        let p = cur.path.clone();
        let edit = self.edits.get(&p);
        let (xform, crop) = (edit.xform, edit.crop);
        let edited = !edit.is_identity();

        // Fast path: full-res pixels already in memory, EXIF orientation applied.
        if let Some(f) = self.full.as_ref().filter(|f| f.path == p) {
            match image_viewer::region_for_save(&f.data, f.exif_tag, xform, crop) {
                Some(out) => match set_clipboard_rgba(out.width, out.height, out.rgba) {
                    Ok(()) => self.notice = Some(copied_notice(out.width, out.height, &edit)),
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

    /// Set the current image as the desktop wallpaper.
    fn set_wallpaper(&mut self) {
        let Some(p) = self.gallery.current().map(|x| x.to_path_buf()) else {
            return;
        };
        let converted = image_viewer::wide_gamut_conversion()
            && image_viewer::color_info(&p).space == image_viewer::ColorSpace::WideGamut;
        // Two reasons the wallpaper cannot always be the original file: an archive
        // member only exists in a temp dir that dies with the app, and a converted
        // picture has to exist as a file at all, because the shell reads the file
        // and never goes near our decode path.
        let target: PathBuf = if self.zip_archive.is_some() || converted {
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
            .and_then(|k| k.path.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let location = match (&self.zip_archive, self.shown.as_ref()) {
            (Some(a), Some(k)) => format!(
                "{}!{}",
                a.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(),
                k.path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
            ),
            _ => format!("{}\\{}", self.folder.display(), filename),
        };
        let mut s = format!(
            "{} - {}/{}",
            location,
            idx,
            self.gallery.len()
        );
        // "page 3/12" in the readout as well as the title, because the title bar is
        // not always visible and this is where the position already is.
        let pages = self.gallery.pages_in_current();
        if pages > 1 {
            if let Some(k) = &self.shown {
                s += &format!(" - page {}/{}", k.page + 1, pages);
            }
        }
        if let Some((shown_key, pv)) = self
            .shown
            .as_ref()
            .and_then(|k| self.previews.get(k).map(|pv| (k, pv)))
        {
            s += &format!(" - {}x{} - preview {} ms", pv.orig.0, pv.orig.1, pv.load_ms);
            // Show the dimensions as displayed once turned, so the readout
            // matches what is on screen.
            let shown_xf = self.shown_xform();
            if shown_xf != Xform::Identity {
                let (w, h) = xformed_footprint(pv.orig.0 as f32, pv.orig.1 as f32, shown_xf);
                s += &format!(" ({} {}x{})", shown_xf.label(), w as u32, h as u32);
            }
            // A crop is not shown by cropping the picture — the frame is drawn over
            // the whole image instead, so that turning or mirroring still works on
            // the full frame. The readout says what the save will produce, or the
            // user has no way to find out what they framed.
            if let Some(crop) = self.edits.crop(&shown_key.path) {
                s += &format!(" - crop {}", crop.label());
            }
            if let Some(anim) = &pv.anim {
                s += &format!(" - anim {} frames", anim.frames.len());
            } else {
                match &self.full {
                    Some(f) if self.shown.as_ref().is_some_and(|k| f.path == k.path) => {
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
        // Resize. The texture is re-fitted to the new area by the drawing code
        // below, which happens on this very frame and costs nothing — so the
        // picture is never blanked and never soft *as a choice*. The only thing
        // a resize needs to decide is whether to decode something sharper
        // behind the existing texture.
        //
        // A window drag delivers a stream of sizes, and one decode costs the
        // same whatever the size, so the refine is issued only once the size
        // stops moving: "unchanged since the previous frame", not a wall-clock
        // settle. A large preview takes ~800ms to decode, which means frames
        // are ~800ms apart, and any timer shorter than that expires between
        // every pair of them and never fires.
        let target = image_viewer::preview_target(panel_px);
        if self.display_dim != Some(panel_px) {
            self.display_dim = Some(panel_px);
            // Hiding/showing panels resizes the display area but must not
            // reload the image: consume exactly one transition silently.
            if self.suppress_resize_redecode {
                self.suppress_resize_redecode = false;
                self.resize.suppress();
            }
        }
        match self.resize.frame(target, self.current_needs_refine(target)) {
            image_viewer::ResizeAction::Refine => {
                perf::count(perf::C_RESIZE_REDECODES, 1);
                self.schedule();
            }
            image_viewer::ResizeAction::Wait => {
                // Still moving. Ask for one more frame so the decision gets made
                // once it stops; without this the final size of a drag would
                // never be examined, because nothing else asks for a repaint.
                ctx.request_repaint_after(RESIZE_SETTLE_REPAINT);
            }
            image_viewer::ResizeAction::Idle => {}
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
        let (tex_id, tex_w) = if let Some(av) = self.anim.as_mut().filter(|a| a.path == shown.path) {
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
        // Quarter turns and mirrors, clockwise. Only the eight lattice symmetries,
        // so the image stays axis-aligned and 1:1 stays pixel-sharp.
        let xform = self.shown_xform();
        // What the image *occupies* on screen: four of the eight swap the axes.
        let (dw, dh) = xformed_footprint(img_px.x, img_px.y, xform);
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
        if resp.dragged() && !self.crop_mode {
            offset += resp.drag_delta();
            self.lat.t_pan = Some(Instant::now());
            self.lat.want_viewport_tiles = true;
        }

        // Keep the image from being lost off-screen; centre it on axes where it fits.
        // Uses the *transformed* footprint, so panning limits and fit are correct
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
                paint_tex(&painter, checker.id(), rect, uv, xform);
            }
        }
        paint_tex(&painter, tex_id, rect, uv, xform);
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
        if let Some(full) = self.full.as_mut().filter(|f| f.path == shown.path) {
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
                                paint_tex(&painter, tile.tex.id(), r, uv, xform);
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

        // --- crop ---
        //
        // egui already tracks where the current press started
        // (`InputState::pointer.press_origin`), so the frame needs no gesture
        // state of its own. Using egui's rather than accumulating
        // `resp.drag_delta()` matters: the rectangle is rebuilt from the start
        // point every frame, so it cannot drift, and egui's press origin already
        // applies the drag threshold.
        let pvx = self.previews.get(&shown);
        if self.crop_mode {
            let (down, origin, ptr) = ui.input(|i| {
                (
                    i.pointer.button_down(egui::PointerButton::Primary),
                    i.pointer.press_origin(),
                    resp.interact_pointer_pos(),
                )
            });
            if let (true, Some(start), Some(p), Some(pv), Some(cur)) =
                (down, origin, ptr, pvx, self.cur.clone())
            {
                // Shift is a temporary 1:1 whatever the setting says, because it
                // is the one shape that is always wanted and never worth a trip to
                // the settings panel. Read per frame rather than latched on press:
                // the shape a drag ends up with should be the one your hand is
                // doing now, and letting go of Shift mid-drag is how you get out
                // of a shape you did not want.
                let aspect = self.crop_aspect(&ctx, display_px);
                if let Some((crop, _)) =
                    self.crop_from_screen(start, p, aspect, footprint, display_px, pv)
                {
                    // Edits are per *file*, not per page: a turn or a crop frame
                    // you set while looking at page 2 belongs to the document, and
                    // that is the thing `Ctrl+S` writes.
                    self.edits.set_crop(&cur.path, Some(crop));
                }
            }
        }

        // Draw the frame from the *stored* crop mapped back out, so what is on
        // screen is the rectangle the save will cut — block-aligned, and correct
        // through any EXIF tag or turn or mirror.
        if let Some(pv) = pvx {
            if let Some(cr) = self.crop_screen_rect(footprint, display_px, pv) {
                let cr = cr.intersect(footprint);
                if cr.width() > 1.0 && cr.height() > 1.0 {
                    // Dim what is being cut away, as four bands rather than a
                    // full-panel wash with a hole: no blending, so the picture
                    // outside the frame is unchanged and only the frame is new ink.
                    // The geometry — which includes not dimming the middle of the
                    // top and bottom edges, which is what the first version did —
                    // is `dim_bands`, and it is tested there.
                    let band = Color32::from_black_alpha(140);
                    let outer = [
                        footprint.min.x,
                        footprint.min.y,
                        footprint.right_bottom().x,
                        footprint.right_bottom().y,
                    ];
                    let inner = [cr.min.x, cr.min.y, cr.max.x, cr.max.y];
                    for [x0, y0, x1, y1] in image_viewer::dim_bands(outer, inner) {
                        if x1 > x0 && y1 > y0 {
                            painter.rect_filled(
                                egui::Rect::from_min_max(
                                    egui::Pos2::new(x0, y0),
                                    egui::Pos2::new(x1, y1),
                                ),
                                0u8,
                                band,
                            );
                        }
                    }
                    painter.rect_stroke(
                        cr,
                        0u8,
                        egui::Stroke::new(1.0, Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                    // Rule of thirds. Two lines each way, faint: enough to line a
                    // portrait up on thirds without competing with the picture.
                    let thin = egui::Stroke::new(1.0, Color32::from_white_alpha(70));
                    for i in 1..3 {
                        let t = i as f32 / 3.0;
                        let x = cr.min.x + cr.width() * t;
                        let y = cr.min.y + cr.height() * t;
                        painter.line_segment(
                            [egui::Pos2::new(x, cr.min.y), egui::Pos2::new(x, cr.max.y)],
                            thin,
                        );
                        painter.line_segment(
                            [egui::Pos2::new(cr.min.x, y), egui::Pos2::new(cr.max.x, y)],
                            thin,
                        );
                    }
                    if let Some(crop) = self.edits.crop(&shown.path) {
                        painter.text(
                            cr.left_top() + Vec2::new(4.0, -6.0),
                            egui::Align2::LEFT_BOTTOM,
                            crop.label(),
                            egui::FontId::proportional(13.0),
                            Color32::WHITE,
                        );
                    }
                }
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
        // How many rows actually fit, so PageUp/PageDown move a page rather than
        // a fixed guess. `show_rows` adds vertical item spacing on top of
        // row_h, so the pitch is not row_h.
        let pitch = row_h as f32 + ui.spacing().item_spacing.y;
        self.grid_page_rows = ((panel.height() / pitch).floor() as usize).max(1);
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
        // auto_shrink([false, true]) is what makes the scrollbar appear: with
        // [false, false] the ScrollArea demands the full width and there is no
        // room left for the bar, so a long folder had no visible way to show
        // where you were.
        let mut sa = egui::ScrollArea::vertical().auto_shrink([false, true]);
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
    /// shape.
    ///
    /// **Per image, not per app.** This used to be a single `u8` on `MyApp`, so
    /// turning one photo turned every photo you then navigated to, and coming
    /// back showed the original at whatever the current rotation happened to be.
    /// That is not how any real viewer behaves — Windows Photos, FastStone,
    /// IrfanView and Preview all keep it per image and keep your edit when you
    /// return — and for an app whose whole job is showing a picture faithfully
    /// it is worse than wrong, because you cannot tell "I rotated this" from
    /// "this is broken".
    ///
    /// Still a view transform: nothing is written until `save_transformed_copy`.
    fn rotate(&mut self, steps: i32, ctx: &Context) {
        self.apply_to_current(|r, p| r.turn(p, steps), ctx);
    }

    /// Mirror the shown image left-to-right or top-to-bottom.
    ///
    /// Goes through the same store as `rotate`, so a mirror and a rotation
    /// compose instead of living in two counters. That matters: rotating and then
    /// mirroring is a *diagonal* mirror, and two independent counters would have
    /// the save work that out at the last moment, where a sign error is invisible
    /// because the viewer re-applies the same mistake to the file it just wrote.
    fn mirror(&mut self, horizontal: bool, ctx: &Context) {
        self.apply_to_current(
            |r, p| {
                if horizontal {
                    r.mirror_horizontal(p)
                } else {
                    r.mirror_vertical(p)
                }
            },
            ctx,
        );
    }

    /// Back to the file's own orientation, discarding the view transform.
    fn reset_transform(&mut self, ctx: &Context) {
        self.apply_to_current(|r, p| {
            r.reset(p);
            Xform::Identity
        }, ctx);
    }

    /// Shared tail for every view-transform button: re-fit and repaint.
    ///
    /// `view.fit` is reset because four of the eight transforms change the
    /// footprint's shape. A mirror does not, so re-fitting there is harmless but
    /// not required — and doing it anyway is the same code path for all of them,
    /// which is worth more than saving the reset on two buttons.
    fn apply_to_current(
        &mut self,
        f: impl FnOnce(&mut image_viewer::ImageEdits, &std::path::Path) -> Xform,
        ctx: &Context,
    ) {
        if let Some(p) = self.cur.clone() {
            // Per file, not per page: a turn is an edit to the document.
            f(&mut self.edits, &p.path);
        }
        self.view.fit = true;
        self.view.offset = Vec2::ZERO;
        ctx.request_repaint();
    }

    /// The stored-space crop for a rectangle the user dragged on the **displayed**
    /// picture, snapped to the block grid — that is, exactly the rectangle the
    /// save will cut.
    ///
    /// The path a dragged rectangle takes, and each step is a place this could be
    /// wrong in a way that looks fine:
    ///
    ///  1. screen points -> the displayed image's own pixels, via `footprint`,
    ///     with the frame first held to `aspect` if one is asked for. Rounded
    ///     *outwards* (floor the near edges, ceil the far ones) so a one-pixel
    ///     slip while dragging never shaves the frame.
    ///  2. displayed pixels -> stored pixels, through the inverse of the full
    ///     display transform. `Crop::from_display_rect` does this, and it is the
    ///     step that transposes if you get the direction wrong.
    ///  3. stored pixels -> the block grid TurboJPEG needs.
    ///  4. and back out again, so the frame that is *drawn* is the frame that will
    ///     be saved rather than the raw drag.
    ///
    /// Steps 3 and 4 are what stop the UI lying. Without them the user frames one
    /// rectangle and gets another, up to a block different, and never finds out.
    ///
    /// `anchor` and `ptr` rather than a finished rect, because holding a shape
    /// needs to know which corner the drag started from: the constrained frame
    /// grows out of the press point, not out of the middle. A min/max rect has
    /// already thrown that away.
    #[allow(clippy::too_many_arguments)]
    fn crop_from_screen(
        &self,
        anchor: egui::Pos2,
        ptr: egui::Pos2,
        aspect: Option<f32>,
        footprint: egui::Rect,
        display_px: Vec2,
        pv: &Preview,
    ) -> Option<(image_viewer::Crop, (u32, u32, u32, u32))> {
        let edit = self.edits.get(&self.cur.as_ref()?.path);
        let display = image_viewer::display_xform(pv.exif_tag, edit.xform);
        let stored = stored_dims(pv);

        let sx = footprint.width() / display_px.x.max(1.0);
        let sy = footprint.height() / display_px.y.max(1.0);
        if sx <= 0.0 || sy <= 0.0 {
            return None;
        }
        let to_px = |p: egui::Pos2| -> (f32, f32) {
            ((p.x - footprint.min.x) / sx, (p.y - footprint.min.y) / sy)
        };
        // The shape is held in the image's own pixels rather than in screen
        // points, because that is the space the rectangle is finally expressed
        // in — and because a square in image pixels is a square on screen, the
        // footprint scaling the same on both axes.
        let (ax, ay) = to_px(anchor);
        let (cx, cy) = to_px(ptr);
        let (x0, y0, x1, y1) = image_viewer::constrain_to_aspect((ax, ay), (cx, cy), aspect);
        // Outwards, so the frame never loses a row to rounding.
        let rect = (
            x0.floor().max(0.0) as u32,
            y0.floor().max(0.0) as u32,
            (x1.ceil() - x0.floor()).max(0.0) as u32,
            (y1.ceil() - y0.floor()).max(0.0) as u32,
        );

        let asked = image_viewer::Crop::from_display_rect(pv.exif_tag, edit.xform, rect, stored)?;
        // The same snap `save_transformed` will do, done here so the drawn frame
        // is the saved one. Idempotent: doing it again changes nothing.
        let snapped = asked.snapped_to_mcu(pv.crop_grid.0, pv.crop_grid.1, stored.0, stored.1)?;
        Some((snapped, snapped.to_display_rect(display, stored)))
    }

    /// The crop rectangle for the shown image, in **screen** points, or `None`.
    fn crop_screen_rect(
        &self,
        footprint: egui::Rect,
        display_px: Vec2,
        pv: &Preview,
    ) -> Option<egui::Rect> {
        let crop = self.edits.crop(&self.shown.as_ref()?.path)?;
        let edit = self.edits.get(&self.shown.as_ref()?.path);
        let display = image_viewer::display_xform(pv.exif_tag, edit.xform);
        let stored = stored_dims(pv);
        let (dx, dy, dw, dh) = crop.to_display_rect(display, stored);
        let sx = footprint.width() / display_px.x.max(1.0);
        let sy = footprint.height() / display_px.y.max(1.0);
        Some(egui::Rect::from_min_size(
            footprint.min + Vec2::new(dx as f32 * sx, dy as f32 * sy),
            Vec2::new(dw as f32 * sx, dh as f32 * sy),
        ))
    }

    /// The ratio a crop frame is being dragged to, from the setting and Shift.
    ///
    /// Shift wins, and it means square. The alternative — Shift meaning "ignore the
    /// setting" — is the other common convention, and it is worse here: with the
    /// default setting being `Free` there would be nothing for Shift to override,
    /// and the shortcut people reach for would do nothing at all.
    fn crop_aspect(&self, ctx: &Context, display_px: Vec2) -> Option<f32> {
        // The image's own size, in whole pixels: what "the same shape as the
        // image" means, and a degenerate zero cannot be divided by.
        let dims = (display_px.x.max(1.0) as u32, display_px.y.max(1.0) as u32);
        if ctx.input(|i| i.modifiers.shift) {
            return image_viewer::CropAspect::Square.ratio(dims);
        }
        self.config.crop_aspect.ratio(dims)
    }

    /// Enter or leave crop mode. Leaving it throws the frame away.
    ///
    /// The frame is cleared rather than kept so that crop mode is a single,
    /// reversible gesture: press `C`, drag, press `C` and you are back to where you
    /// were. Keeping a stale frame that reappears next time you crop is how you end
    /// up saving a crop you framed for a different picture and forgot about.
    fn toggle_crop_mode(&mut self, ctx: &Context) {
        self.crop_mode = !self.crop_mode;
        if !self.crop_mode {
            if let Some(p) = self.cur.clone() {
                self.edits.clear_crop(&p.path);
            }
        }
        ctx.request_repaint();
    }

    /// The transform of the image the user last asked for.
    fn current_xform(&self) -> Xform {
        self.cur
            .as_ref()
            .map_or(Xform::Identity, |k| self.edits.xform(&k.path))
    }

    /// The transform of the image actually *on screen*.
    ///
    /// Deliberately keyed on `shown`, not `cur`. Those are different images
    /// whenever the wanted one's preview has not been adopted yet, and the draw
    /// path paints `shown`'s texture — so reading the transform from `cur` drew one
    /// picture under another picture's transform. Whichever way the two get out of
    /// step, that is a visible flicker: with `cur` ahead you see the stale image
    /// briefly turned, and with it behind you see the new one arrive unrotated and
    /// then jump.
    ///
    /// `current_xform` is still what the *save* uses, because saving is about
    /// `cur` — the image the user last asked for — even if it is not up yet.
    fn shown_xform(&self) -> Xform {
        self.shown
            .as_ref()
            .map_or(Xform::Identity, |k| self.edits.xform(&k.path))
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
            cur.path.file_name().unwrap_or_default(),
            t.elapsed(),
            self.gallery.index,
            self.gallery.len(),
            self.shown.as_ref().and_then(|k| k.path.file_name()),
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
        perf::count(perf::C_FRAMES, 1);
        // Frames that arrived carrying winit events. Compared against
        // `C_FRAMES` this separates "the user is driving" from "something is
        // asking for frames by itself", which is the question you have to ask
        // before concluding the app is burning CPU.
        if ctx.input(|i| !i.events.is_empty()) {
            perf::count(perf::C_FRAMES_WITH_EVENTS, 1);
        }
        if let Some(prev) = self.t_prev_frame.replace(Instant::now()) {
            perf::record_since(perf::FRAME_INTERVAL_US, prev);
        }
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
            // A grid cell is one picture, so it is always the file's first page.
            key: ImgKey::first(path),
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
        ui.painter().rect(rect, 0.0, fill, stroke, egui::StrokeKind::Inside);
    }

    let inner = rect.shrink(4.0);
    // `Ui::child_ui(rect, layout)` (0.27) is gone. The migration target is
    // `Ui::new_child(UiBuilder)`, NOT `Ui::scope_builder(..)`: both create a Ui
    // bounded to `inner`, but `scope_builder` additionally calls
    // `advance_cursor_after_rect` on *this* `ui` afterwards. That is invisible
    // for a scope that owns the parent's space, and wrong here — the cell was
    // already allocated above, so the extra advance doubled up and ate 4pt of
    // `item_spacing` per cell (cells visibly tightened from 8pt to 4pt).
    // `new_child` returns an owned `Ui` and leaves the parent cursor alone,
    // which is exactly what 0.27's `child_ui` did.
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );

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
                let mut img_ui = child.new_child(
                    egui::UiBuilder::new()
                        .max_rect(draw)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
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
        .truncate(),
    );

    resp
}

/// A small 2x2-square checker texture used as the transparency backdrop.
/// Painted with repeat-wrapped UVs so it tiles at any image size.
/// The file's **stored** pixel dimensions, recovered from a preview's
/// already-oriented `orig`.
///
/// Three places need this — drawing a frame, reading one back out, and moving
/// one — and they all have to agree about which space a rectangle is in.
/// Repeating the derivation three times is three chances to crop a valid picture
/// of the wrong part of the photo, which is exactly the failure this codebase
/// has already paid for once. `Xform::from_exif` is the same source of truth the
/// decode used, rather than a second opinion about which tags swap axes.
fn stored_dims(pv: &Preview) -> (u32, u32) {
    if Xform::from_exif(pv.exif_tag)
        .unwrap_or(Xform::Identity)
        .swaps_axes()
    {
        (pv.orig.1, pv.orig.0)
    } else {
        pv.orig
    }
}

fn make_checker_texture(ctx: &Context) -> TextureHandle {    const N: usize = 64; // texture side in px
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
            source_size: egui::vec2(N as f32, N as f32),
            pixels,
        },
        TextureOptions::NEAREST_REPEAT,
    )
}

/// Put RGBA pixels on the Windows clipboard, from any thread.
///
/// Takes the buffer **by value** so arboard can take ownership of it. A 24 MP
/// photo is 96 MB, and cloning it to hand it to a function that was going to
/// clone it again is not a detail: this is the one call on this path that is
/// unavoidably a copy, and the point is that it happens once.
fn set_clipboard_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_image(arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: std::borrow::Cow::Owned(rgba),
    })
    .map_err(|e| e.to_string())
}

/// What went on the clipboard, said in the terms the user acted in.
///
/// A crop or a turn is worth naming in the confirmation. "Copied 6000x4000 image
/// to clipboard" when the user framed a quarter of the photo is true and useless;
/// the size alone does not say which quarter.
fn copied_notice(w: u32, h: u32, edit: &image_viewer::Edit) -> String {
    if edit.is_identity() {
        format!("Copied {w}x{h} image to clipboard")
    } else {
        format!("Copied {w}x{h} to clipboard ({})", edit.label())
    }
}

/// Paint a texture into `rect`, turned by `xform` about the rect's centre.
///
/// `painter.image` cannot rotate, so build a mesh and move its vertices — the same
/// trick egui's own `Image::paint_at` uses for `ImageOptions::rotation`. A
/// quarter turn keeps everything axis-aligned, so 1:1 stays pixel-sharp and
/// the caller can keep working in unrotated image space.
///
/// The matrix comes from [`Xform::paint_matrix`], i.e. it is derived from the same
/// `Xform::map` that decides what the *saved* file contains. That is the point:
/// a hand-written rotation matrix here is a second, independent statement of what
/// the transform is, and the day the two disagree the screen shows one picture and
/// the saved file contains another, with nothing to notice. Going through `map`
/// makes that impossible.
///
/// `Mesh::transform` is no use — `TSTransform` is a scale and a translation, with
/// no room for a rotation or a reflection. The vertices are moved directly, which
/// is what `Mesh::rotate` does internally anyway.
fn paint_tex(
    painter: &egui::Painter,
    tex_id: egui::TextureId,
    rect: egui::Rect,
    uv: egui::Rect,
    xform: Xform,
) {
    if xform == Xform::Identity {
        painter.image(tex_id, rect, uv, Color32::WHITE);
        return;
    }
    let m = xform.paint_matrix();
    let origin = rect.center();
    let mut mesh = egui::epaint::Mesh::with_texture(tex_id);
    mesh.add_rect_with_uv(rect, uv, Color32::WHITE);
    for v in &mut mesh.vertices {
        let d = v.pos - origin;
        v.pos = origin
            + Vec2::new(m[0][0] * d.x + m[0][1] * d.y, m[1][0] * d.x + m[1][1] * d.y);
    }
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
        source_size: egui::vec2(w as f32, h as f32),
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
    fn on_exit(&mut self) {
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

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // egui 0.31+ replaced `App::update(&Context)` with `App::ui(&mut Ui)`.
        // The panels below are now shown on this `ui` rather than on a
        // `Context`, which is the whole point of the change: Ui-first means
        // nested containers, not a flat sequence of Context-rooted panels.
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        self.frame_prelude(ctx);

        // Read every key up front, before anything acts on them: Esc needs to
        // know whether the search box has focus, and the panels are drawn last.
        //
        // `typing` is true when a text field owns the keyboard. All the global
        // single-key handling below has to stand down then, or typing "g" into
        // the filter would toggle the grid and "r" would reload the folder.
        let typing = ctx.egui_wants_keyboard_input();
        let f11 = ctx.input(|i| i.key_pressed(egui::Key::F11));
        let esc = ctx.input(|i| i.key_pressed(egui::Key::Escape));
        // Keyboard navigation + file triage
        let (next, prev, first, last, toggle, grid, page_up, page_down, focus_search) =
            ctx.input(|i| {
                (
                    i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::Space),
                    i.key_pressed(egui::Key::ArrowLeft),
                    i.key_pressed(egui::Key::Home),
                    i.key_pressed(egui::Key::End),
                    i.key_pressed(egui::Key::I),
                    i.key_pressed(egui::Key::G),
                    i.key_pressed(egui::Key::PageUp),
                    i.key_pressed(egui::Key::PageDown),
                    // `/` is the usual "search" chord and is not otherwise used.
                    i.key_pressed(egui::Key::Slash) && !i.modifiers.ctrl && !i.modifiers.alt,
                )
            });

        if f11 {
            let fs = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
            self.suppress_resize_redecode = true;
            self.show_panels = fs; // entering fullscreen hides panels; leaving restores
        }
        if esc {
            if typing {
                // Esc belongs to the search box while it has focus: clear the
                // filter, or if there is nothing to clear just defocus. It must
                // not fall through to "close the grid" or "quit the app" —
                // clicking into the search box and pressing Esc should never
                // exit the viewer.
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.on_filter_changed(ctx);
                } else {
                    ctx.memory_mut(|m| m.request_focus(egui::Id::NULL));
                }
            } else if self.gallery_view {
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

        // In grid mode the arrows drive the grid's own selection instead of
        // changing the shown image, and Enter opens the highlighted cell.
        let (up, down, enter) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Enter),
            )
        });
        // `M` (move) and `C` (copy) used to be bare single-key shortcuts. They are
        // not any more, and the reason is worth keeping: both **write to the
        // filesystem** on a keystroke with nothing standing between the key and
        // the file. `C` in particular is the wrong letter to have bound to a
        // mutation, because it is also the first thing anyone reaches for when
        // they think they are in a text box somewhere else. `C` is now Crop,
        // which touches nothing on disk until `Ctrl+S`.
        //
        // So the single-key budget goes to actions that cannot lose data. Triage
        // is still one keystroke for Delete, which is the one people use while
        // culling; Move and Copy are buttons. If they turn out to be wanted back
        // on the keyboard, a modifier is the shape to add rather than a bare
        // letter.
        // `Ctrl+C` is **not** a key event, and reading it as one is why it did
        // nothing. `egui-winit` intercepts the clipboard chords and converts
        // them, dropping the key:
        //
        //     } else if is_copy_command(self.modifiers, active_key) {
        //         self.egui_input.events.push(egui::Event::Copy);
        //         return;                      // <- no `Key { key: C, .. }` ever
        //     }
        //
        // and `is_copy_command` is `modifiers.command && key == C`, which on
        // Windows is `ctrl` — the same thing. So `key_pressed(Key::C) &&
        // modifiers.ctrl` is *unreachable*: the log of that frame shows
        // `Copy` followed by `Key { key: C, pressed: false }`, and nothing
        // between them. `Ctrl+S` survives because it is not on the list.
        //
        // So the chord is read from the converted event, which is also the
        // honest way round: this is what the platform means by copy, so it
        // now also answers to the Edit menu's Copy, `Ctrl+Insert`, and the
        // dedicated Copy key, none of which would ever match a `C` test.
        //
        // It sits inside the `!typing` block below, so `Ctrl+C` in the filter
        // box still copies the selected text rather than the picture — the
        // text field handles the same event first, and copying a whole
        // full-res image because you were renaming a filter is not a thing
        // anyone asked for.
        let clip_img = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        let (del, explore, reload, wall, save_rot, crop_key) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::E),
                i.key_pressed(egui::Key::R),
                i.key_pressed(egui::Key::W) && !i.modifiers.any(),
                i.key_pressed(egui::Key::S) && i.modifiers.ctrl,
                // Bare `C` is still a real key event, so crop mode is
                // unaffected: the chord interception is on the press *with*
                // a modifier, and this is the press without one.
                i.key_pressed(egui::Key::C) && !i.modifiers.any(),
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
        // `typing` is true when the search box owns the keyboard. All of this is
        // global single-key handling, so it must stand down or typing "g" into
        // the filter would toggle the grid and "r" would reload the folder.
        if !typing {
            if self.gallery_view {
                // Grid: arrows move the highlight, PageUp/PageDown move a page,
                // Home/End jump to the ends, Enter opens it. Clamped, so walking
                // off an edge stops instead of jumping to the far end.
                let cols = self.grid_cols.max(1);
                let page = self.grid_page_rows.max(1) * cols;
                let last_idx = self.gallery.len().saturating_sub(1);
                let sel = &mut self.grid_sel;
                if page_down {
                    *sel = (*sel + page).min(last_idx);
                } else if page_up {
                    *sel = sel.saturating_sub(page);
                } else if next || down {
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
            } else if explore {
                self.explore_current();
            } else if reload {
                self.rescan(ctx, true);
            } else if clip_img {
                self.copy_image_to_clipboard();
            } else if save_rot {
                self.save_transformed_copy();
            } else if crop_key {
                self.toggle_crop_mode(ctx);
            } else if wall {
                self.set_wallpaper();
            } else if mouse_back {
                self.step(ctx, false);
            } else if mouse_fwd {
                self.step(ctx, true);
            }
        }

        // Drag-and-drop: files or folders
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(path) = dropped.first().map(|f| f.path().to_path_buf()) {
            self.load_dropped(ctx, path);
        }

        if self.show_panels {
            egui::Panel::top("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(self.status_text());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Clear button first so it sits at the right edge.
                        if !self.filter.is_empty() && ui.small_button("x").clicked() {
                            self.filter.clear();
                            self.on_filter_changed(ctx);
                        }
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.filter)
                                .hint_text("filter (/)")
                                .desired_width(160.0),
                        );
                        if focus_search {
                            resp.request_focus();
                        }
                        if resp.changed() {
                            self.on_filter_changed(ctx);
                        }
                    });
                });
                if !self.filter.is_empty() {
                    ui.label(format!(
                        "{} of {} shown",
                        self.gallery.len(),
                        self.all_files.len()
                    ));
                }
            });
        }

        if self.show_panels {
            egui::Panel::bottom("menu").show(ui, |ui| {
                // Wrapped: a single row of buttons overflows a narrow
                // window and the tail (incl. the hints) gets clipped.
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Browse Folder…").clicked() {
                        self.pick_folder(ctx);
                    }
                    if ui.button("Browse File…").clicked() {
                        self.pick_file(ctx);
                    }
                    // "Left"/"Right" rather than the arrow glyphs: the default
                    // egui font has no U+2190/U+2192, so the arrows rendered as
                    // blank boxes, and the name of the key is clearer anyway.
                    if ui.button("Prev (Left)").clicked() {
                        self.step(ctx, false);
                    }
                    if ui.button("Next (Right)").clicked() {
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
                    // Mirrors are here for the same reason the rotate buttons are:
                    // a photo shot with a wrong orientation tag, or a scan that
                    // came out the wrong way round, is a one-press fix and there is
                    // no other way to get it. They compose with the turns rather
                    // than replacing them, so turn-then-mirror is a diagonal
                    // mirror and mirror-twice is back to where it started.
                    //
                    // "H" and "V" rather than ⇋ and ⇅: the default egui font has
                    // neither, and a missing glyph is a blank box, not a subtle
                    // omission. `no_ui_text_uses_a_glyph_the_font_lacks` is what
                    // stops that coming back.
                    let xform = self.current_xform();
                    let any = xform != Xform::Identity;
                    if ui
                        .add_enabled(
                            self.cur.is_some(),
                            egui::Button::new("Mirror H").shortcut_text(""),
                        )
                        .on_hover_text("Mirror left-to-right")
                        .clicked()
                    {
                        self.mirror(true, ctx);
                    }
                    if ui
                        .add_enabled(
                            self.cur.is_some(),
                            egui::Button::new("Mirror V").shortcut_text(""),
                        )
                        .on_hover_text("Mirror top-to-bottom")
                        .clicked()
                    {
                        self.mirror(false, ctx);
                    }
                    if ui
                        .add_enabled(
                            any,
                            egui::Button::new("Reset turn")
                                .shortcut_text("")
                                .min_size(Vec2::new(74.0, 0.0)),
                        )
                        .on_hover_text("Back to the file's own orientation, and drop any crop")
                        .clicked()
                    {
                        self.reset_transform(ctx);
                    }
                    // Crop is a mode, not an action: the frame stays after the drag
                    // so it can be adjusted, and the mode says so. The hover text
                    // spells out the one real cost, which is that panning is
                    // unreachable while it is on.
                    if ui
                        .selectable_label(self.crop_mode, "Crop (C)")
                        .on_hover_text("Drag on the image to frame. Hold Shift for a square.")
                        .clicked()
                    {
                        self.toggle_crop_mode(ctx);
                    }
                    let has_crop = self.cur.as_ref().is_some_and(|k| self.edits.crop(&k.path).is_some());
                    if ui
                        .add_enabled(
                            has_crop,
                            egui::Button::new("Clear crop")
                                .shortcut_text("")
                                .min_size(Vec2::new(74.0, 0.0)),
                        )
                        .on_hover_text("Remove the crop, keeping any rotation or mirroring")
                        .clicked()
                    {
                        if let Some(p) = self.cur.clone() {
                            self.edits.clear_crop(&p.path);
                        }
                        ctx.request_repaint();
                    }
                    // Sits with the transform buttons because that is the only
                    // thing it saves: without this the app's one writing action
                    // was reachable only by remembering a chord.
                    if ui
                        .add_enabled(
                            self.cur.is_some(),
                            egui::Button::new("Save transformed… (Ctrl+S)"),
                        )
                        .on_hover_text("New file beside this one, original untouched")
                        .clicked()
                    {
                        self.save_transformed_copy();
                    }
                    if ui.button("Panels (I)").clicked() {
                        self.suppress_resize_redecode = true;
                        self.show_panels = !self.show_panels;
                    }
                    if ui.button("Delete (Del)").clicked() {
                        self.delete_current(ctx);
                    }
                    // No key advertised, because there is none: Move and Copy write
                    // to the filesystem and are not on a bare keystroke. See the
                    // note where the bindings are read.
                    if ui.button("Move…").clicked() {
                        self.transfer_current(ctx, false);
                    }
                    if ui.button("Copy…").clicked() {
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
            egui::Panel::left("exif").show(ui, |ui| {
                ui.heading("EXIF");
                // The panel is the only thing that shows this, so it is also the
                // only thing that should ask for it. Asking here rather than
                // trusting the plan is what makes leaving fullscreen work: the
                // frame the panel comes back on is the frame the read starts, so
                // there is no "Loading EXIF…" with nothing behind it. `push`
                // dedups against queued and running work, and it is not in the
                // queue's `wanted` set — which is fine, because a rebuild that
                // drops it is followed by this same line on the next frame.
                if image_viewer::wants_exif(self.show_panels, self.exif_loading) {
                    if let Some(path) = self.cur.as_ref().map(|k| k.path().to_path_buf()) {
                        self.loader.push(Job {
                            // EXIF describes the file, so it is read once and not
                            // per page — the same key `plan_jobs` uses.
                            key: ImgKey::first(&path),
                            kind: Kind::Exif,
                            prio: PRIO_EXIF,
                            target_dim: 0,
                        });
                    }
                }
                if self.exif_loading {
                    ui.label("Loading EXIF…");
                } else if let Some(info) = &self.exif_info {
                    for line in info.lines() {
                        if let Some(url) = line.strip_prefix("Maps: ") {
                            // GPS link: clickable, opens the system browser.
                            if ui.link("Open in Google Maps").clicked() {
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
                    if let Ok(md) = std::fs::metadata(&p.path) {
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
            egui::Panel::right("settings").show(ui, |ui| {
                ui.heading("Settings");

                // ---- renderer ------------------------------------------------
                // Deliberately the first row: it is the only setting that can
                // make the app fail to start at all, so it should not be
                // something you have to go looking for.
                ui.horizontal(|ui| {
                    ui.label("Graphics:");
                    let mut choice = self.config.renderer.choice;
                    let mut changed = false;
                    egui::ComboBox::from_id_salt("renderer")
                        .selected_text(choice.label())
                        .show_ui(ui, |ui| {
                            for b in [
                                image_viewer::WgpuBackend::Auto,
                                image_viewer::WgpuBackend::Dx12,
                                image_viewer::WgpuBackend::Gl,
                                image_viewer::WgpuBackend::Vulkan,
                            ] {
                                changed |= ui
                                    .selectable_value(&mut choice, b, b.label())
                                    .clicked();
                            }
                        });
                    if changed {
                        self.request_renderer_restart(ui.ctx(), choice);
                    }
                });
                ui.small(format!("In use: {}", self.renderer.label()));
                if self.config.renderer.choice == image_viewer::WgpuBackend::Auto {
                    ui.small(
                        "Automatic probes each backend in its own process and keeps the \
                         first that starts, because a driver that fails does so by \
                         crashing rather than by returning an error.",
                    );
                }
                if ui.button("Probe backends now").clicked() {
                    self.probe_renderer_now(ui.ctx());
                }
                ui.add_space(6.0);

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
                // Off by default: this changes how photos look, so it is opt-in
                // rather than a correction. Like the toggle above it applies at
                // decode time and does not re-decode what is already cached — move
                // to another image, or restart, to see it everywhere.
                if ui
                    .checkbox(
                        &mut self.config.convert_wide_gamut,
                        "Convert Display P3 photos to sRGB",
                    )
                    .changed()
                {
                    image_viewer::set_wide_gamut_conversion(self.config.convert_wide_gamut);
                    self.config.save(&self.config_path);
                }
                if self.config.convert_wide_gamut {
                    ui.small("Colours outside sRGB are clipped to fit, which is lossy.");
                }
                if ui
                    .checkbox(
                        &mut self.config.thumbnail_cache,
                        "Cache grid thumbnails on disk",
                    )
                    .changed()
                {
                    // Nothing to invalidate. The cells in `thumbs` are decoded
                    // pictures and stay correct whichever way this goes; only
                    // whether *new* ones are written changes. Dropping them here
                    // would cost a re-decode of every visible cell — up to 50 of
                    // them at ~700 ms each on a folder of large scans — to buy
                    // nothing at all.
                    set_thumbnail_cache(self.config.thumbnail_cache);
                    self.config.save(&self.config_path);
                }
                ui.small(format!(
                    "Up to {} of cells, so a folder of large scans opens once at full \
                     speed. Stored in {}.",
                    human_bytes(image_viewer::THUMB_CACHE_MAX_BYTES),
                    thumb_cache_root().display()
                ));
                                let mut new_filter = self.config.downscale_filter;
                ui.horizontal(|ui| {
                    ui.label("Filter:");
                    egui::ComboBox::from_id_salt("ds_filter")
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
                if ui
                    .checkbox(&mut self.config.loop_back, "Loop back at folder ends")
                    .changed()
                {
                    // Apply straight away; the gallery owns the wrap behaviour,
                    // so it has to be told.
                    self.gallery.loop_back = self.config.loop_back;
                    self.config.save(&self.config_path);
                }
                // Sits with the crop button's own row rather than under Graphics
                // because it is a property of the *gesture*, not of the app: it
                // changes what a drag does, and nothing about it applies until
                // somebody drags.
                ui.horizontal(|ui| {
                    ui.label("Crop shape:");
                    let mut choice = self.config.crop_aspect;
                    egui::ComboBox::from_id_salt("crop_aspect")
                        .selected_text(choice.label())
                        .show_ui(ui, |ui| {
                            for a in image_viewer::CropAspect::ALL {
                                ui.selectable_value(&mut choice, a, a.label());
                            }
                        });
                    if choice != self.config.crop_aspect {
                        self.config.crop_aspect = choice;
                        self.config.save(&self.config_path);
                    }
                });
                if self.config.crop_aspect != image_viewer::CropAspect::Free {
                    ui.small(
                        "Frames a new crop to this shape; Shift forces a square for one drag. \
                         A frame already drawn keeps the shape it has.",
                    );
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
                    egui::ComboBox::from_id_salt("sort_mode")
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
                    // A missing icon is invisible here and only shows up as a
                    // generic application icon in a file list, so say so.
                    match fileassoc::default_icon() {
                        Some(icon) => {
                            ui.small(format!("Icon: {icon}"));
                        }
                        None => {
                            ui.colored_label(
                                ui.visuals().warn_fg_color,
                                "No icon registered — files will show a generic icon",
                            );
                        }
                    }
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
            .frame(egui::Frame::new().fill(Color32::BLACK))
            .show(ui, |ui| {
                self.central(ui);
            });

        // Who is asking for these repaints? egui tags every request with the
        // call site, so this names the culprit instead of guessing. No-op
        // unless sampling is on.
        for c in ctx.repaint_causes() {
            perf::count_repaint_cause(&c);
        }
    }
}
