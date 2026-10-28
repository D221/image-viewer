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

use image_viewer::fileassoc as fileassoc;
mod app;

pub(crate) use egui::{Color32, ColorImage, Context, Pos2, Rect, TextureHandle, TextureOptions, Vec2};
pub(crate) use nom_exif::{read_exif, ExifTag};
pub(crate) use std::collections::{HashMap, HashSet};
pub(crate) use std::io::Read as _;
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::mpsc::{self, Receiver, Sender};
pub(crate) use std::sync::Arc;
pub(crate) use std::time::{Duration, Instant};

pub(crate) use image_viewer::perf;
pub(crate) use image_viewer::{Job, Kind, Loader, PRIO_CURRENT_EXIF, PRIO_THUMB_BASE};
pub(crate) use image_viewer::{
    archive_image_count, buffer_has_alpha, extract_archive, fill_scale, fit_scale_no_upscale,
    forget_thumbnail, grid_cols,
    is_archive_path, last_anchor, load_grid_thumbnail, load_image_full, load_page_preview_full,
    load_spread_overrides, nav_is_forward, protected_preview_keys, save_spread_overrides,
    scan_folder_sorted_dir, set_downscale_filter, set_downscale_smoothing, set_linear_resample,
    set_thumbnail_cache,
    spread_root_key, spreads_root, thumb_cache_root, xformed_footprint, Animation, Config,
    DownscaleFilter, Gallery, GallerySpreads, ImgKey, ImageData, NavInput, PageCtx, PagePos, Shape,
    SavedView, SortMode, Xform, GRID_LABEL_H, grid_row_h, MAX_FULL_PIXELS,
};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Wheel delta accumulated before a flip triggers in wheel-navigation mode.

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
    // Single-instance gate: a second launch exits rather than fighting
    // with the first over the shared archive temp dir (and making the
    // watcher's already-wrong "file vanished" notifications fatal).
    if config.single_instance && !hold_single_instance() {
        eprintln!("Another image-viewer instance is already running — exiting.");
        std::process::exit(0);
    }
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
        // Window chrome from config at creation; the checkboxes apply the
        // same flags live, so this only matters for what a fresh launch
        // looks like.
        let mut viewport = egui::ViewportBuilder::default();
        // Remembered size: fall back to the shipped 1280x800 on first launch.
        if let Some((w, h)) = config.window_size {
            viewport = viewport.with_inner_size([w as f32, h as f32]);
        } else {
            viewport = viewport.with_inner_size([1280.0, 800.0]);
        }
        // Remembered position, maximized state first so the window opens where
        // the user left it. Position is applied even when maximized is set —
        // winit ignores the position of a maximized window, so nothing breaks.
        if let Some((x, y)) = config.window_pos {
            viewport = viewport.with_position([x as f32, y as f32]);
        }
        if config.window_maximized {
            viewport = viewport.with_maximized(true);
        }
        if config.always_on_top {
            viewport = viewport.with_always_on_top();
        }
        if config.hide_title_bar {
            viewport = viewport.with_decorations(false);
        }
        let options = eframe::NativeOptions {
            viewport: viewport
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

// ---------------------------------------------------------------------------
// The modules
// ---------------------------------------------------------------------------
//
// This file was 10,713 lines. The constants, the types and the methods now live
// in modules of their own; main.rs keeps the imports, n main and this list.
pub(crate) use app::consts::*;
pub(crate) use app::main::*;
pub(crate) use app::types::*;
pub(crate) use app::model::*;
pub(crate) use app::paint::*;

