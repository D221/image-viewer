//! Module `app_types`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// File name as a lossy string, the key `last_by_folder` resume stores.
pub(crate) fn file_name_of(p: &Path) -> String {
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
pub(crate) fn icc_scan(args: &[String]) -> eframe::Result<()> {
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
        found.iter().filter(|(_, i)| i.space == space).map(|(p, _)| p).collect()
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
        (image_viewer::ColorSpace::NonRgb, "Not an RGB space"),
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
pub(crate) const MAX_LISTED: usize = 500;

/// Identity of a file's *contents* for change detection.
///
/// `mtime` alone is not enough: editors and copy tools can write a file with a
/// preserved timestamp, and a same-size overwrite within one filesystem tick
/// can keep the same one too. The pair catches the common cases at the cost of
/// one `metadata` call per file per poll, which the poll already does not
/// otherwise need.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub(crate) mtime: Option<std::time::SystemTime>,
    pub(crate) len: u64,
}

impl Stamp {
    pub(crate) fn of(path: &Path) -> Stamp {
        match std::fs::metadata(path) {
            Ok(m) => Stamp { mtime: m.modified().ok(), len: m.len() },
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
pub(crate) struct Thumb {
    pub(crate) tex: TextureHandle,
    pub(crate) texels: [usize; 2],
    /// LRU stamp (frame clock).
    pub(crate) used: u64,
}

/// Tracks one "user intent -> useful pixels" transition so the perf module can
/// report real latencies instead of decode times.
///
/// The tricky part is *first visible pixels*: a preview arriving on the worker
/// is not on screen until the frame that paints it has been presented. We only
/// know a frame was presented when the next one starts, so a paint is recorded
/// as pending and committed in [`MyApp::frame_prelude`].
#[derive(Default)]
pub(crate) struct Latency {
    /// When the user asked for the current image.
    pub(crate) t_request: Option<Instant>,
    /// False for a fresh open, true for next/prev (reported separately).
    pub(crate) is_nav: bool,
    /// Set when the requested image was painted during the last frame.
    pub(crate) painted: Option<Instant>,
    pub(crate) recorded_first: bool,
    /// A preview for the requested image exists (fit-to-window quality).
    pub(crate) t_preview: Option<Instant>,
    /// A full-resolution decode of it has landed.
    pub(crate) t_full: Option<Instant>,
    /// A stall was already reported for this request (report once, not per
    /// frame).
    pub(crate) stall_logged: bool,
    /// Zoom / pan / gallery / scan in flight.
    pub(crate) t_zoom: Option<Instant>,
    pub(crate) t_pan: Option<Instant>,
    pub(crate) t_gallery: Option<Instant>,
    pub(crate) t_scan: Option<Instant>,
    /// Tiles wanted by a zoom/pan, satisfied when the visible set is complete.
    pub(crate) want_viewport_tiles: bool,
}

impl Latency {
    pub(crate) fn begin(&mut self, now: Instant, is_nav: bool) {
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
    pub(crate) fn preview(&mut self, now: Instant) {
        if self.t_preview.is_none() {
            self.t_preview = Some(now);
        }
    }

    pub(crate) fn full(&mut self, now: Instant) {
        if self.t_full.is_none() {
            self.t_full = Some(now);
            if let (Some(req), Some(first)) = (self.t_request, self.painted) {
                let _ = first;
                perf::record(
                    if self.is_nav { perf::NAV_TO_FULL } else { perf::SELECT_TO_FULL },
                    now.duration_since(req).as_micros() as u64,
                );
            }
        }
    }

    /// The requested image was painted this frame; commit next frame.
    pub(crate) fn painted(&mut self, now: Instant) {
        self.painted = Some(now);
    }

    /// Called at the start of every frame: the previous frame has been shown.
    pub(crate) fn commit_frame(&mut self) {
        if let (Some(painted), Some(req), false) =
            (self.painted.take(), self.t_request, self.recorded_first)
        {
            self.recorded_first = true;
            perf::record(
                if self.is_nav { perf::NAV_TO_FIRST_PIXELS } else { perf::SELECT_TO_FIRST_PIXELS },
                painted.duration_since(req).as_micros() as u64,
            );
        }
        if let (Some(prev), Some(req)) = (self.t_preview, self.t_request) {
            perf::record(perf::SELECT_TO_PREVIEW, prev.duration_since(req).as_micros() as u64);
            self.t_preview = None;
        }
    }

    /// The viewport's tiles are all present: zoom/pan is satisfied.
    pub(crate) fn viewport_tiles_ready(&mut self, now: Instant) {
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

pub(crate) struct PreviewData {
    pub(crate) image: ColorImage, // already converted on the worker thread
    pub(crate) orig: (u32, u32),  // source dimensions
    pub(crate) load_ms: u64,
    // Full-res image when it was already decoded in the same pass
    // (small files, and formats like PNG with no fast preview path).
    pub(crate) full: Option<Arc<ImageData>>,
    // Decoded frames when the file is an animated GIF.
    pub(crate) anim: Option<Arc<Animation>>,
    // Any pixel is translucent (draw checkerboard behind it).
    pub(crate) has_alpha: bool,
    // The `target_dim` this decode was asked for. Kept so staleness can be
    // judged against what we *asked for* rather than the pixels we got back —
    // comparing pixels re-decodes forever on a source smaller than the display.
    pub(crate) target: u32,
    // The file's EXIF orientation tag, and the block grid a lossless JPEG crop
    // has to align to. Both are read on the worker from the bytes the decode
    // already had in hand, and both exist for the crop UI: a rectangle drawn on
    // the *displayed* picture has to be expressed in *stored* coordinates, which
    // is a composition involving the tag, and it has to land on the block grid
    // or TurboJPEG will not cut it where the user drew.
    pub(crate) exif_tag: u16,
    pub(crate) crop_grid: (u32, u32),
    // Levels and clipped-pixel counts, counted on the worker from the same
    // preview buffer the texture was uploaded from: a few hundred kilobytes
    // walked once per decode, never per frame. Carried rather than recomputed
    // because the CPU pixels are dropped after upload.
    pub(crate) hist: image_viewer::Histogram,
    // The file's colour classification, classified on the worker from the bytes
    // the decode already had in hand. It fills the metadata panel's colour line —
    // which used to be the metadata read's own second whole-file read of the
    // file, on the worker, per image — and it rides on the preview, so it is
    // cached alongside the texture and costs nothing to re-show.
    pub(crate) colour: image_viewer::ColorInfo,
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
pub(crate) struct ThumbData {
    pub(crate) image: ColorImage,
    pub(crate) load_ms: u64,
}

/// A byte count as something a person can read. Used by the settings panel to
/// state the thumbnail cache's cap rather than printing 268435456.
pub(crate) fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.0} {}", UNITS[u]) }
}

pub(crate) enum Msg {
    Preview {
        key: ImgKey,
        result: Box<Result<PreviewData, String>>,
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
    /// A sidecar rating read, made on a worker.
    ///
    /// `path` is the image it was read for, so a stale answer — the reader
    /// moved on while the read was in flight — is recognised in `pump` and
    /// thrown away rather than shown for the wrong image.
    Rating {
        path: PathBuf,
        result: Result<image_viewer::Ratings, String>,
    },
    /// One grid cell's sidecar rating read, made on a worker.
    ///
    /// Keyed by path like the grid's thumbnail map, so a stale answer is
    /// still the right file's answer — unlike the cursor read above, which
    /// must be thrown away when the reader moved on.
    GridRating {
        path: PathBuf,
        result: Result<image_viewer::Ratings, String>,
    },
    /// A dropped file's copy, made on a worker.
    ///
    /// `src` is the path that was dropped, and it is here so a **stale** answer
    /// can be recognised: a drop is a user action with a lifetime now, and a copy
    /// that lands after the reader has dropped something else must not open over
    /// the top of the newer one.
    DropCopy {
        src: PathBuf,
        result: Result<image_viewer::DropCopy, String>,
    },
    /// A wallpaper set-complete, from its worker.
    ///
    /// A wallpaper request reads, decodes, converts and re-encodes **off the
    /// paint thread**, and this is its answer — a notice or an error, nothing
    /// else is needed because the desktop itself is the output. `order` is the
    /// issuance token the press stamped, so a **stale** answer — a second
    /// press's worker landed after this one's — is recognised in `pump` and
    /// silently dropped: the desktop must end on the user's *last* action,
    /// which may have landed first.
    Wallpaper {
        order: u64,
        result: Result<(), String>,
    },
}

/// Start the worker pool for this app: real decoding, real panic capture, and
/// a repaint request when something arrives.
pub(crate) fn new_loader(
    ctx: &Context,
    tx: Sender<Msg>,
    preferred_workers: usize,
) -> image_viewer::Loader {
    let available = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
    let workers = image_viewer::resolve_worker_count(available, preferred_workers);
    let wake_ctx = ctx.clone();
    image_viewer::Loader::spawn(workers, tx, run_job, fail_msg, move || wake_ctx.request_repaint())
}

/// A failure is still a *delivered* result: it gets reported and remembered,
/// so the loader must not treat it as work left undone.
pub(crate) fn fail_msg(job: &Job, err: String) -> (Msg, bool) {
    let msg = match job.kind {
        Kind::Preview => Msg::Preview { key: job.key.clone(), result: Box::new(Err(err)) },
        Kind::Full => Msg::Full { key: job.key.clone(), result: Err(err) },
        Kind::Thumb => Msg::Thumb { path: job.path().to_path_buf(), result: Err(err) },
        Kind::Exif => Msg::Exif { path: job.path().to_path_buf(), info: None },
    };
    (msg, true)
}

/// Decode one job. Always `delivered = true` today: every job runs to
/// completion, so there is no "gave up without a result" case yet. That is the
/// hook reintroduced cancellation will use, and the reason the loader re-queues
/// a wanted job on abandonment rather than on completion.
pub(crate) fn run_job(job: &Job) -> (Msg, bool) {
    let path = job.path().to_path_buf();
    // The page is part of the request, so the worker decodes *that* page rather
    // than always the first. Pages of a multi-page TIFF are one file read and one
    // IFD walk apart, so this costs nothing beyond the IFD walk.
    let page = job.key.page;
    let msg = match job.kind {
        Kind::Preview => Msg::Preview {
            result: Box::new(decode_preview(&path, job.target_dim, page)),
            key: job.key.clone(),
        },
        Kind::Thumb => Msg::Thumb {
            // A grid cell shows the file's first page, always — a cell is one
            // picture, and making it a slideshow would be worse than useless.
            result: decode_thumb(&path, job.target_dim),
            path,
        },
        Kind::Full => Msg::Full {
            result: load_image_full(&path, page).map(Arc::new).map_err(|e| e.to_string()),
            key: job.key.clone(),
        },
        Kind::Exif => Msg::Exif { info: extract_exif(&path), path },
    };
    (msg, true)
}

/// Decode a preview sized for the current display area (in physical pixels).
/// The result is the full-res image when it fits the target, otherwise a
/// texture capped at the target long side — always sharp at fit-to-window.
pub(crate) fn decode_preview(
    path: &Path,
    target_dim: u32,
    page: u32,
) -> Result<PreviewData, String> {
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
        hist: image_viewer::histogram_rgba(&r.preview.rgba, r.preview.width, r.preview.height),
        colour: r.colour,
    })
}

/// One grid cell, at the cell's own size.
///
/// Goes through [`load_grid_thumbnail`], so this is the only place a cell can be
/// served from the on-disk cache, and the reason a cell is a different request
/// from a preview rather than the same one with a smaller number.
pub(crate) fn decode_thumb(path: &Path, target_dim: u32) -> Result<ThumbData, String> {
    let d = load_grid_thumbnail(path, target_dim).map_err(|e| e.to_string())?;
    Ok(ThumbData {
        image: ColorImage::from_rgba_unmultiplied([d.width as usize, d.height as usize], &d.rgba),
        load_ms: d.load_time.as_millis() as u64,
    })
}

/// EXIF panel text for `path`, or `None` when there is nothing worth saying.
///
/// The colour line is **not** one of these any more: it is classified on the
/// worker from the bytes the decode already read and travels with the preview,
/// so asking here would be a second whole-file read of the file for a fact the
/// decode already established. What is left is the cheap half — `read_exif`
/// seeks the APP1 block out rather than reading the file, 0.1–0.2 ms on a real
/// photograph against the 8–12 ms `color_info`'s `fs::read` cost.
pub(crate) fn extract_exif(path: &Path) -> Option<String> {
    let mut info = Vec::new();

    let Ok(exif) = read_exif(path) else {
        return None;
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

    // Free-text comments: EXIF description/UserComment first, then a JPEG COM
    // segment when it says something the EXIF text does not already say.
    // The COM read is bounded to the first 256 KiB — a COM lives in the header
    // when one exists — so this is not a second whole-file read on the worker.
    let exif_text = image_viewer::exif_comment(&exif);
    if let Some(c) = &exif_text {
        info.push(format!("Comment: {c}"));
    }
    if let Ok(mut f) = std::fs::File::open(path) {
        let mut head = Vec::new();
        if f.by_ref().take(262144).read_to_end(&mut head).is_ok()
            && let Some(c) = image_viewer::jpeg_comment(&head)
            && Some(&c) != exif_text.as_ref()
        {
            info.push(format!("Comment: {c}"));
        }
    }

    // Photo settings, joined on one line like a camera EXIF summary.
    let mut shot = Vec::new();
    if let Some(entry) = exif.get(ExifTag::FNumber)
        && let Some(v) = entry.as_urational().and_then(|r| r.to_f64())
    {
        shot.push(format!("f/{v:.1}"));
    }
    if let Some(entry) = exif.get(ExifTag::ExposureTime)
        && let Some(r) = entry.as_urational()
    {
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
    if let Some(entry) = exif.get(ExifTag::ISOSpeedRatings)
        && let Some(iso) =
            entry.as_u16().map(|iso| iso as u32).or_else(|| entry.as_u32()).map(|iso| iso as u16)
    {
        shot.push(format!("ISO {iso}"));
    }
    if let Some(entry) = exif.get(ExifTag::FocalLength)
        && let Some(v) = entry.as_urational().and_then(|r| r.to_f64())
    {
        shot.push(format!("{v:.0} mm"));
    }
    if let Some(entry) = exif.get(ExifTag::ExposureBiasValue)
        && let Some(v) = entry.as_irational().and_then(|r| r.to_f64())
        && v != 0.0
    {
        shot.push(format!("{v:+.1} EV"));
    }
    if let Some(entry) = exif.get(ExifTag::DigitalZoomRatio)
        && let Some(v) = entry.as_urational().and_then(|r| r.to_f64())
        && v > 1.0
    {
        shot.push(format!("zoom x{v:.1}"));
    }
    if !shot.is_empty() {
        info.push(format!("Shot: {}", shot.join(" · ")));
    }

    if let Some(entry) = exif.get(ExifTag::MeteringMode)
        && let Some(v) = entry.as_u16()
        && v > 0
    {
        info.push(format!("Metering: {}", metering_label(v)));
    }
    if let Some(entry) = exif.get(ExifTag::Flash)
        && let Some(v) = entry.as_u16()
    {
        info.push(format!("Flash: {}", flash_label(v)));
    }
    if let Some(entry) = exif.get(ExifTag::ColorSpace)
        && let Some(v) = entry.as_u16()
    {
        info.push(format!("Color: {}", color_space_label(v)));
    }
    if let Some(entry) = exif.get(ExifTag::Orientation)
        && let Some(v) = entry.as_u16()
        && v > 1
    {
        info.push(format!("Orientation: {}", orientation_label(v)));
    }
    if let Some(entry) = exif.get(ExifTag::ImageWidth)
        && let Some(h_entry) = exif.get(ExifTag::ImageHeight)
    {
        info.push(format!("Dimensions: {}x{} px", entry, h_entry));
    }

    // GPS: decimal degrees + altitude, and a Google Maps link.
    if let Some((lat, lon)) = image_viewer::gps_coords(&exif) {
        info.push(format!("Location: {:.5}, {:.5}", lat, lon));
        if let Some(alt) = image_viewer::gps_altitude(&exif) {
            info.push(format!("Altitude: {alt:.0} m"));
        }
        info.push(format!("Maps: https://maps.google.com/?q={:.6},{:.6}", lat, lon));
    }

    if info.is_empty() { None } else { Some(info.join("\n")) }
}

pub(crate) fn orientation_label(v: u16) -> &'static str {
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

pub(crate) fn metering_label(v: u16) -> &'static str {
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

pub(crate) fn color_space_label(v: u16) -> &'static str {
    match v {
        1 => "sRGB",
        0xFFFF => "Uncalibrated",
        _ => "Other",
    }
}

pub(crate) fn flash_label(v: u16) -> &'static str {
    if v & 0x1 == 1 { "Fired" } else { "No flash" }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------
