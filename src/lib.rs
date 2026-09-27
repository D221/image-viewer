//! Core logic for the image viewer, kept separate from the GUI so it can be
//! unit-tested, benchmarked, and fuzz-tested.

use nom_exif::{ExifTag, URational};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

// Import GenericImageView to access dimensions() method
use image::{DynamicImage, GenericImageView, RgbaImage};

/// Quick low-res preview. With DCT-domain scaling this is a fraction of a full decode.
pub fn load_image_preview(path: &Path, preview_max_dim: u32) -> Result<ImageData, LoadError> {
    load_image(path, preview_max_dim.min(512))
}

/// Configuration settings for the image viewer.
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Whether to load all images from the current folder on startup.
    pub load_all_on_startup: bool,

    /// Whether to show images before applying auto-fit (fit_to_window).
    pub show_image_before_auto_fit: bool,

    /// Optional folder to load images from on startup.
    pub folder: Option<String>,

    /// Gallery sort order.
    #[serde(default)]
    pub sort: SortMode,

    /// Resize previews in linear light with a CatmullRom filter (better
    /// edges/moiré on lineart) instead of fast gamma-space Triangle.
    #[serde(default = "default_true")]
    pub linear_resample: bool,

    /// Downsampling filter for the linear-light path.
    #[serde(default)]
    pub downscale_filter: DownscaleFilter,

    /// Hold-to-browse: arrow keys advance only as fast as images decode.
    #[serde(default)]
    pub block_while_loading: bool,

    /// Mouse wheel flips images (instead of zooming). Ctrl+wheel still zooms.
    #[serde(default)]
    pub wheel_navigation: bool,

    /// Last viewed image per folder, so reopening a folder resumes there.
    #[serde(default)]
    pub last_by_folder: std::collections::HashMap<String, String>,
}

fn default_true() -> bool {
    true
}

impl Config {
    /// Load configuration from a JSON file.
    pub fn load(path: &Path) -> Self {
        let default = Self {
            load_all_on_startup: true,
            show_image_before_auto_fit: true,
            folder: None,
            sort: SortMode::Name,
            linear_resample: true,
            downscale_filter: DownscaleFilter::Hermite,
            block_while_loading: false,
            wheel_navigation: false,
            last_by_folder: std::collections::HashMap::new(),
        };

        let Ok(data) = std::fs::read_to_string(path) else {
            return default;
        };

        match serde_json::from_str::<Self>(&data) {
            Ok(config) => config,
            Err(err) => {
                eprintln!("Failed to load config {}: {}", path.display(), err);
                default
            }
        }
    }

    /// Save configuration to a JSON file.
    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "Failed to create config directory {}: {}",
                    parent.display(),
                    err
                );
                return;
            }
        }

        let Ok(data) = serde_json::to_string_pretty(self) else {
            eprintln!("Failed to serialize config");
            return;
        };

        if let Err(err) = std::fs::write(path, data) {
            eprintln!("Failed to save config {}: {}", path.display(), err);
        }
    }

    /// Get the default configuration path.
    pub fn default_path() -> PathBuf {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            PathBuf::from(appdata)
                .join("image-viewer")
                .join("config.json")
        } else if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home)
                .join(".config")
                .join("image-viewer")
                .join("config.json")
        } else {
            PathBuf::from("config.json")
        }
    }
}

/// Supported image extensions for gallery scanning.
pub const SUPPORTED_EXTENSIONS: [&str; 9] = [
    "jpg", "jpeg", "png", "webp", "gif", "jxl", "avif", "heic", "heif",
];

/// Scan a directory for supported image files, sorted alphabetically.
pub fn scan_folder(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                let ext = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
                    files.push(p);
                }
            }
        }
    }
    files.sort_by(|a, b| {
        let an = a.file_name().unwrap_or_default().to_string_lossy();
        let bn = b.file_name().unwrap_or_default().to_string_lossy();
        natural_cmp(&an, &bn)
    });
    files
}

/// How to order the gallery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortMode {
    /// Explorer-style natural sort by file name (IMG_2 before IMG_10).
    #[default]
    Name,
    /// Newest file first.
    Date,
    /// Largest file first.
    Size,
    /// Shuffled once per folder load.
    Random,
}

/// Side of one gallery-grid (G) thumbnail.
pub const GRID_CELL: f32 = 150.0;
/// Label strip reserved under each grid thumbnail for the file name.
pub const GRID_LABEL_H: f32 = 20.0;
/// Vertical pitch of one grid row (cell + label + inter-row padding).
pub const GRID_ROW_H: f32 = GRID_CELL + GRID_LABEL_H + 8.0;

/// How many grid cells fit across a display area `panel_w` points wide.
/// Cells are laid out with egui's default 8pt item spacing, hence the +8.
pub fn grid_cols(panel_w: f32) -> usize {
    ((panel_w / (GRID_CELL + 8.0)).floor() as usize).clamp(1, 24)
}

/// Scale factor that fits an image of `natural` size into an `area`, keeping
/// the aspect ratio and never upscaling (1.0 means "draw at native size").
///
/// The gallery grid centres thumbnails with this: upscaling a small image to
/// fill a cell only makes it blurry, so a 40x30 picture stays 40x30 and is
/// simply centred in the middle of the cell. All sizes are in the same unit
/// (the caller works in points: texture texels / `pixels_per_point`).
pub fn fit_scale_no_upscale(natural: (f32, f32), area: (f32, f32)) -> f32 {
    let (w, h) = (natural.0.max(1.0), natural.1.max(1.0));
    if w <= 0.0 || h <= 0.0 || area.0 <= 0.0 || area.1 <= 0.0 {
        return 1.0;
    }
    (area.0 / w).min(area.1 / h).min(1.0)
}

/// Footprint `(width, height)` an image of `w x h` occupies after `quarter`
/// quarter turns clockwise. Odd quarter turns swap the axes; only whole
/// quarter turns are supported, so the result stays axis-aligned (and 1:1
/// stays pixel-sharp).
pub fn rotated_footprint(w: f32, h: f32, quarter: u8) -> (f32, f32) {
    if quarter & 1 == 1 {
        (h, w)
    } else {
        (w, h)
    }
}

/// Case-insensitive "natural" string comparison: runs of digits compare
/// numerically, so `IMG_10` sorts after `IMG_2` like Explorer does.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (ab, bb) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0usize, 0usize);
    while i < ab.len() && j < bb.len() {
        let (ca, cb) = (ab[i].to_ascii_lowercase(), bb[j].to_ascii_lowercase());
        if ca.is_ascii_digit() && cb.is_ascii_digit() {
            let sa: Vec<u8> = ab[i..]
                .iter()
                .copied()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            i += sa.len();
            let sb: Vec<u8> = bb[j..]
                .iter()
                .copied()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            j += sb.len();
            // Numeric compare: fewer significant digits first, then by value,
            // then shorter digit runs (a1 before a01).
            let na = trim_leading_zeros(&sa);
            let nb = trim_leading_zeros(&sb);
            let ord = na
                .len()
                .cmp(&nb.len())
                .then_with(|| na.cmp(nb))
                .then_with(|| sa.len().cmp(&sb.len()));
            if ord != Ordering::Equal {
                return ord;
            }
        } else if ca != cb {
            return ca.cmp(&cb);
        } else {
            i += 1;
            j += 1;
        }
    }
    (ab.len() - i).cmp(&(bb.len() - j))
}

fn trim_leading_zeros(digits: &[u8]) -> &[u8] {
    let first = digits.iter().position(|&c| c != b'0').unwrap_or(digits.len());
    &digits[first..]
}

/// Scan and sort with the given mode. Date sorts newest-first, Size
/// largest-first, Random shuffles once per call.
pub fn scan_folder_sorted(folder: &Path, mode: SortMode) -> Vec<PathBuf> {
    let mut files = scan_folder(folder);
    match mode {
        SortMode::Name => {}
        SortMode::Date => {
            files.sort_by_cached_key(|p| {
                std::fs::metadata(p)
                    .and_then(|m| m.modified())
                    .map(std::cmp::Reverse)
                    .ok()
            });
        }
        SortMode::Size => {
            files.sort_by_cached_key(|p| {
                std::fs::metadata(p).map(|m| std::cmp::Reverse(m.len())).ok()
            });
        }
        SortMode::Random => shuffle(&mut files),
    }
    files
}

/// Tiny xorshift shuffle (avoids a `rand` dependency).
fn shuffle(files: &mut [PathBuf]) {
    if files.len() < 2 {
        return;
    }
    let mut s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64 ^ (d.as_secs() << 8))
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;
    for i in (1..files.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        let j = (s % (i as u64 + 1)) as usize;
        files.swap(i, j);
    }
}

/// Decoded image data plus load timing metadata.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub load_time: std::time::Duration,
}

/// Errors that can occur while loading an image.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] image::ImageError),
    #[error("decoder error: {0}")]
    Other(String),
    #[error("jpeg error: {0}")]
    Jpeg(String),
}

/// EXIF orientation tag (1-8) of a JPEG file, or 0 when absent/unreadable.
/// Only JPEG carries the tag in practice; callers gate on content sniffing.
fn exif_orientation(path: &Path) -> u16 {
    nom_exif::read_exif(path)
        .ok()
        .and_then(|exif| exif.get(nom_exif::ExifTag::Orientation).and_then(|v| v.as_u16()))
        .unwrap_or(0)
}

/// True if the orientation tag rotates by 90/270, swapping width and height.
fn orientation_swaps_dims(tag: u16) -> bool {
    matches!(tag, 5..=8)
}

/// Apply an EXIF orientation tag (1-8) to decoded RGBA pixels in place.
/// Tags 0 and 1 are no-ops; 2/4 flip, 3 rotates 180°, 6/8 rotate 90/270° CW,
/// 5/7 are the mirrored diagonals (transpose / transverse).
pub fn apply_exif_orientation(data: &mut ImageData, tag: u16) {
    let buf = std::mem::take(&mut data.rgba);
    let Some(mut img) = RgbaImage::from_raw(data.width, data.height, buf) else {
        // Buffer length doesn't match the declared dims — put back whatever
        // we got instead of leaving an empty buffer (which would trip the
        // texture-size assert later).
        data.rgba = Vec::new();
        return;
    };
    let rotated: RgbaImage = match tag {
        2 => {
            image::imageops::flip_horizontal_in_place(&mut img);
            img
        }
        3 => image::imageops::rotate180(&img),
        4 => {
            image::imageops::flip_vertical_in_place(&mut img);
            img
        }
        5 => {
            let mut img = image::imageops::rotate90(&img);
            image::imageops::flip_horizontal_in_place(&mut img);
            img
        }
        6 => image::imageops::rotate90(&img),
        7 => {
            let mut img = image::imageops::rotate270(&img);
            image::imageops::flip_horizontal_in_place(&mut img);
            img
        }
        8 => image::imageops::rotate270(&img),
        _ => {
            // Unknown or normal: put the untouched buffer back.
            data.rgba = img.into_raw();
            return;
        }
    };
    data.width = rotated.width();
    data.height = rotated.height();
    data.rgba = rotated.into_raw();
}

/// Hard cap on the long side of any decoded image (GPU texture limit).
pub const MAX_TEXTURE_DIM: u32 = 16384;

/// Don't keep a full-res copy of anything above this (RGBA = 4 bytes/pixel).
pub const MAX_FULL_PIXELS: u64 = 300_000_000;

/// Decode a JPEG straight to RGBA at the smallest DCT reduction (1/8, 1/4, 1/2, 1)
/// whose long side is still >= `max_dim`. The IDCT runs at reduced size, so a
/// 95 MP file never has to be materialised at full resolution.
fn decode_jpeg_scaled(bytes: &[u8], max_dim: u32) -> Result<(u32, u32, Vec<u8>), LoadError> {
    use turbojpeg::{Decompressor, Image, PixelFormat, ScalingFactor};
    let err = |e: turbojpeg::Error| LoadError::Jpeg(e.to_string());

    let mut dec = Decompressor::new().map_err(err)?;
    let hdr = dec.read_header(bytes).map_err(err)?;
    let long = hdr.width.max(hdr.height);

    // Smallest reduction that still gives >= max_dim, but never exceed the GPU texture limit.
    let limit = MAX_TEXTURE_DIM as usize;
    let sf = [8usize, 4, 2, 1]
        .into_iter()
        .map(|d| ScalingFactor::new(1, d))
        .find(|sf| sf.scale(long) >= max_dim as usize && sf.scale(long) <= limit)
        .or_else(|| {
            [1usize, 2, 4, 8]
                .into_iter()
                .map(|d| ScalingFactor::new(1, d))
                .find(|sf| sf.scale(long) <= limit)
        })
        .unwrap_or(ScalingFactor::new(1, 8));
    dec.set_scaling_factor(sf).map_err(err)?;
    let scaled = hdr.scaled(sf);

    let mut img = Image {
        pixels: vec![0u8; scaled.width * scaled.height * 4],
        width: scaled.width,
        pitch: scaled.width * 4,
        height: scaled.height,
        format: PixelFormat::RGBA,
    };
    dec.decompress(bytes, img.as_deref_mut()).map_err(err)?;
    Ok((scaled.width as u32, scaled.height as u32, img.pixels))
}

pub fn load_image_full(path: &Path) -> Result<ImageData, LoadError> {
    load_image(path, MAX_TEXTURE_DIM)
}

/// True if the bytes look like a JPEG (magic bytes), regardless of file name.
pub fn is_jpeg_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// Image dimensions sniffed from the file content, not the extension.
/// Cheap: only the header is parsed.
pub fn image_dims_bytes(bytes: &[u8]) -> Option<(u32, u32)> {
    // Formats the `image` crate can't open get their own header probes.
    if is_jxl_bytes(bytes) {
        return jxl_oxide::integration::JxlDecoder::new(std::io::Cursor::new(bytes))
            .ok()
            .map(|dec| image::ImageDecoder::dimensions(&dec));
    }
    if is_heif_bytes(bytes) {
        return heic::ImageInfo::from_bytes(bytes)
            .ok()
            .map(|info| (info.width, info.height));
    }
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Files at or under this many pixels decode fast enough to skip the cheap
/// preview stage entirely and go straight to a full decode. ~12 MP covers
/// 4K screens and typical wallpapers; turbojpeg decodes those in well under
/// 150 ms, so two stages would only add latency.
pub const FAST_FULL_MAX_PIXELS: u64 = 12_000_000;

/// Long side of the display texture when the preview pass decoded the full
/// image. 4096 is crisp fit-to-window on a 4K screen; zooming further refines
/// with tiles from the full-res data.
pub const PREVIEW_TEXTURE_DIM: u32 = 4096;

/// Result of a single-pass preview load: the preview to show immediately, plus
/// the full-res image when it was already decoded in the same pass.
pub struct PreviewAndFull {
    pub preview: ImageData,
    /// Set when the full-res decode already happened (small files and formats
    /// without a fast preview path, e.g. PNG).
    pub full: Option<ImageData>,
    /// Fully decoded animation when the file is an animated GIF.
    pub anim: Option<Animation>,
    /// True source dimensions from the header, when they could be sniffed.
    pub orig: Option<(u32, u32)>,
    /// Source is larger than the requested target: the preview texture is
    /// below source density and should be re-decoded if the display grows.
    pub below_target: bool,
}


/// True if the bytes look like a GIF (magic bytes).
fn is_gif_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF8")
}

/// Caps for animation decoding: stop when either limit is hit.
pub const MAX_ANIM_FRAMES: usize = 240;
pub const MAX_ANIM_BYTES: usize = 256 * 1024 * 1024;

/// A decoded animation: fully composited RGBA frames and their delays.
#[derive(Debug, Clone)]
pub struct Animation {
    pub frames: Vec<ImageData>,
    pub delays: Vec<Duration>,
}

impl Animation {
    /// Total loop duration. Zero-delay frames are floored to 20 ms (50 fps),
    /// matching how browsers clamp pathological GIF timing.
    pub fn total_duration(&self) -> Duration {
        let ms: u64 = self
            .delays
            .iter()
            .map(|d| d.as_millis().max(20) as u64)
            .sum();
        Duration::from_millis(ms)
    }
}

/// Decode an animated GIF into composited frames, capped at the limits above.
/// The `image` crate composites partial frames and applies disposal methods.
fn load_animation(bytes: &[u8], max_dim: u32, start: std::time::Instant) -> Result<Animation, LoadError> {
    use image::AnimationDecoder;
    let dec = image::codecs::gif::GifDecoder::new(std::io::Cursor::new(bytes))?;
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    let mut total = 0usize;
    for frame in dec.into_frames() {
        if frames.len() >= MAX_ANIM_FRAMES || total >= MAX_ANIM_BYTES {
            break;
        }
        // Tolerate a truncated trailing frame: keep what we have.
        let frame = match frame {
            Ok(f) => f,
            Err(_) => break,
        };
        let mut data = ImageData {
            width: frame.buffer().width(),
            height: frame.buffer().height(),
            rgba: frame.buffer().clone().into_raw(),
            load_time: start.elapsed(),
        };
        if data.width.max(data.height) > max_dim {
            data = downscale_image(&data, max_dim);
        }
        let data = normalized(data);
        total += data.rgba.len();
        delays.push(Duration::from(frame.delay()));
        frames.push(data);
    }
    if frames.is_empty() {
        return Err(LoadError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "no decodable animation frames",
        )));
    }
    Ok(Animation { frames, delays })
}

/// Load a preview in one pass, decoding the file only once.
///
/// * Large JPEGs get the cheap DCT-scaled preview; the full decode happens
///   later on dwell, as before.
/// * Everything else (PNG etc. has no reduced-decode shortcut, so preview and
///   full cost the same) is decoded once at full resolution: the result is
///   returned both as the full image and downscaled as the preview.
pub fn load_image_preview_full(
    path: &Path,
    preview_max_dim: u32,
) -> Result<PreviewAndFull, LoadError> {
    let start = std::time::Instant::now();
    let bytes = std::fs::read(path)?;
    let max_dim = preview_max_dim.min(MAX_TEXTURE_DIM);
    // EXIF orientation: camera pixels are stored unrotated; the tag says how
    // to display them. Rotate after decode so preview and full agree, and fix
    // the reported source dimensions accordingly.
    let tag = if is_jpeg_bytes(&bytes) { exif_orientation(path) } else { 0 };
    let orig = image_dims_bytes(&bytes)
        .map(|(w, h)| if orientation_swaps_dims(tag) { (h, w) } else { (w, h) });

        // Animated GIF: decode the whole loop once; no second full decode, no tiles.
    if is_gif_bytes(&bytes) {
        if let Ok(anim) = load_animation(&bytes, PREVIEW_TEXTURE_DIM, start) {
            if anim.frames.len() > 1 {
                let dims = (anim.frames[0].width, anim.frames[0].height);
                return Ok(PreviewAndFull {
                    preview: anim.frames[0].clone(),
                    full: None,
                    anim: Some(anim),
                    orig: Some(dims),
                    below_target: false,
                });
            }
        }
        // Static or unreadable GIF: fall through to the normal decode path.
    }

    let full_is_cheap = orig.map_or(true, |(w, h)| w as u64 * h as u64 <= FAST_FULL_MAX_PIXELS);

    if is_jpeg_bytes(&bytes) && !full_is_cheap {
        if let Ok((w, h, rgba)) = decode_jpeg_scaled(&bytes, max_dim) {
            let mut preview = normalized(ImageData {
                width: w,
                height: h,
                rgba,
                load_time: start.elapsed(),
            });
            apply_exif_orientation(&mut preview, tag);
            return Ok(PreviewAndFull {
                preview,
                full: None,
                anim: None,
                orig,
                // DCT preview never reaches source density for these files.
                below_target: true,
            });
        }
        eprintln!("turbojpeg failed, falling back to image crate");
    }

    // One full decode serves as both the full-res image and the preview.
    let (preview, full, below_target) = match orig {
        // Too big to keep full-res: decode only a preview-sized copy, like the
        // old preview path did.
        Some((w, h)) if w as u64 * h as u64 > MAX_FULL_PIXELS => {
            let mut preview = normalized(decode_bytes(&bytes, max_dim, start)?);
            apply_exif_orientation(&mut preview, tag);
            (preview, None, false)
        }
        _ => {
            let mut full = normalized(decode_bytes(&bytes, MAX_TEXTURE_DIM, start)?);
            apply_exif_orientation(&mut full, tag);
            // Show the full-res image right away (capped at the display
            // target) — a tiny preview texture would look blurry at
            // fit-to-window even though the full data is already in memory.
            let preview = if full.width.max(full.height) <= max_dim {
                full.clone()
            } else {
                downscale_image(&full, max_dim)
            };
            // Source bigger than the target: texture is below source density.
            let below = full.width.max(full.height) > max_dim;
            (preview, Some(full), below)
        }
    };
    Ok(PreviewAndFull {
        preview,
        full,
        anim: None,
        orig,
        below_target,
    })
}

/// True if the bytes look like a JPEG XL image (naked codestream or container).
pub fn is_jxl_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0x0A])
        || bytes.starts_with(&[
            0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
        ])
}

/// The 4CC brand of an ISOBMFF file (HEIC/HEIF/AVIF family), if any.
fn isobmff_brand(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        Some(&bytes[8..12])
    } else {
        None
    }
}

/// True if the bytes look like a HEIF-family file (HEIC/HEIF/AVIF).
pub fn is_heif_bytes(bytes: &[u8]) -> bool {
    isobmff_brand(bytes).is_some_and(|brand| {
        matches!(
            brand,
            b"avif" | b"avis" | b"heic" | b"heix" | b"mif1" | b"msf1" | b"heim" | b"heis"
                | b"hevc" | b"hevx" | b"av01"
        )
    })
}

/// Decode JPEG XL bytes via jxl-oxide (pure Rust), routed through the image crate.
fn decode_jxl(bytes: &[u8]) -> Result<DynamicImage, LoadError> {
    use jxl_oxide::integration::JxlDecoder;
    let dec = JxlDecoder::new(std::io::Cursor::new(bytes))?;
    Ok(DynamicImage::from_decoder(dec)?)
}

/// Decode HEIC/HEIF/AVIF bytes via the pure-Rust `heic` crate (AV1 via rav1d
/// for AVIF files). Container transforms (clap/irot/imir) are applied.
fn decode_heif(bytes: &[u8]) -> Result<DynamicImage, LoadError> {
    let mut limits = heic::Limits::default();
    limits.max_width = Some(MAX_TEXTURE_DIM as u64);
    limits.max_height = Some(MAX_TEXTURE_DIM as u64);
    limits.max_pixels = Some(MAX_FULL_PIXELS);
    let out = heic::DecoderConfig::new()
        .decode_request(bytes)
        .with_output_layout(heic::PixelLayout::Rgba8)
        .with_limits(&limits)
        .decode()
        .map_err(|e| LoadError::Other(e.to_string()))?;
    let img = RgbaImage::from_raw(out.width, out.height, out.data).ok_or_else(|| {
        LoadError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HEIF decode returned a mismatched pixel buffer",
        ))
    })?;
    Ok(DynamicImage::ImageRgba8(img))
}

/// Resize a decoded DynamicImage to `max_dim` and convert to ImageData.
fn dynamic_to_image_data(
    img: DynamicImage,
    max_dim: u32,
    start: std::time::Instant,
) -> ImageData {
    let (w, h) = img.dimensions();
    let mut data = ImageData {
        width: w,
        height: h,
        rgba: img.to_rgba8().into_raw(),
        load_time: start.elapsed(),
    };
    if w.max(h) > max_dim {
        data = downscale_image(&data, max_dim);
    }
    data
}

/// Decode bytes to RGBA, capped at `max_dim` on the long side. JPEGs use the
/// turbojpeg DCT-scaling fast path; everything else goes through the `image` crate
/// or the dedicated pure-Rust decoders for formats it can't handle.
fn decode_bytes(bytes: &[u8], max_dim: u32, start: std::time::Instant) -> Result<ImageData, LoadError> {
    if is_jpeg_bytes(bytes) {
        if let Ok((w, h, rgba)) = decode_jpeg_scaled(bytes, max_dim) {
            return Ok(ImageData {
                width: w,
                height: h,
                rgba,
                load_time: start.elapsed(),
            });
        }
        eprintln!("turbojpeg failed, falling back to image crate");
    }

    if is_jxl_bytes(bytes) {
        match decode_jxl(bytes) {
            Ok(img) => return Ok(dynamic_to_image_data(img, max_dim, start)),
            Err(e) => eprintln!("JXL decode failed: {e}"),
        }
    }

    if is_heif_bytes(bytes) {
        match decode_heif(bytes) {
            Ok(img) => return Ok(dynamic_to_image_data(img, max_dim, start)),
            Err(e) => eprintln!("HEIF/AVIF decode failed: {e}"),
        }
    }

    let img = image::load_from_memory(bytes)?;
    Ok(dynamic_to_image_data(img, max_dim, start))
}

/// Load and decode an image file (format detected from content, not extension).
/// JPEGs are rotated according to their EXIF orientation tag.
pub fn load_image(path: &Path, max_dim: u32) -> Result<ImageData, LoadError> {
    let start = std::time::Instant::now();
    let bytes = std::fs::read(path)?;
    let mut data = normalized(decode_bytes(&bytes, max_dim.min(MAX_TEXTURE_DIM), start)?);
    if is_jpeg_bytes(&bytes) {
        apply_exif_orientation(&mut data, exif_orientation(path));
    }
    Ok(data)
}

/// Global toggle for linear-light downsampling (set from the config toggle in
/// main.rs). Workers read it; the UI thread writes it.
static LINEAR_RESAMPLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Enable/disable linear-light downsampling at runtime.
pub fn set_linear_resample(on: bool) {
    LINEAR_RESAMPLE.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn linear_resample_enabled() -> bool {
    LINEAR_RESAMPLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Downsampling filter for the linear-light path, selectable at runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DownscaleFilter {
    /// B=0 C=0 cubic: smallest support (2 px), no negative lobes — softest,
    /// zero ringing. JPEGView's default for downscaling.
    #[default]
    Hermite,
    /// B=0 C=0.5 cubic: sharp like CatmullRom with milder ringing.
    Catrom,
    /// 3-lobe sinc: sharpest, most visible ringing on hard edges.
    Lanczos3,
}

static DOWNSCALE_FILTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set the downsampling filter at runtime (workers read it per-resize).
pub fn set_downscale_filter(f: DownscaleFilter) {
    let v = match f {
        DownscaleFilter::Hermite => 0,
        DownscaleFilter::Catrom => 1,
        DownscaleFilter::Lanczos3 => 2,
    };
    DOWNSCALE_FILTER.store(v, std::sync::atomic::Ordering::Relaxed);
}

fn downscale_filter() -> DownscaleFilter {
    match DOWNSCALE_FILTER.load(std::sync::atomic::Ordering::Relaxed) {
        1 => DownscaleFilter::Catrom,
        2 => DownscaleFilter::Lanczos3,
        _ => DownscaleFilter::Hermite,
    }
}

/// Kernel weight and support radius for each filter.
fn kernel(f: DownscaleFilter) -> (Box<dyn Fn(f32) -> f32>, f32) {
    match f {
        DownscaleFilter::Hermite => (
            Box::new(|x: f32| {
                // Hermite = cubic B-spline with B=0, C=0 (a.k.a. "smoothstep").
                let x = x.abs();
                if x < 1.0 {
                    1.0 - 2.5 * x * x + 1.5 * x * x * x
                } else {
                    0.0
                }
            }),
            1.0,
        ),
        DownscaleFilter::Catrom => (
            Box::new(|x: f32| {
                // Catmull-Rom = cubic B=0, C=0.5.
                let x = x.abs();
                if x < 1.0 {
                    1.5 * x * x * x - 2.5 * x * x + 1.0
                } else if x < 2.0 {
                    -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
                } else {
                    0.0
                }
            }),
            2.0,
        ),
        DownscaleFilter::Lanczos3 => (
            Box::new(|x: f32| {
                let x = x.abs();
                if x <= f32::EPSILON {
                    1.0
                } else if x < 3.0 {
                    let pix = std::f32::consts::PI * x;
                    (pix.sin() / pix) * ((pix / 3.0).sin() / (pix / 3.0))
                } else {
                    0.0
                }
            }),
            3.0,
        ),
    }
}

/// sRGB <-> 12-bit-linear LUTs (JPEGView-style): resizing in linear light
/// avoids darkened edges and moiré on lineart/screentones.
fn lin_tables() -> (&'static [u16; 256], &'static [u8; 4096]) {
    static TABLES: std::sync::OnceLock<(Box<[u16; 256]>, Box<[u8; 4096]>)> =
        std::sync::OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        let mut to_lin = Box::new([0u16; 256]);
        for (i, v) in to_lin.iter_mut().enumerate() {
            let s = i as f64 / 255.0;
            let lin = if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
            *v = (lin * 4095.0).round() as u16;
        }
        let mut to_srgb = Box::new([0u8; 4096]);
        for (i, v) in to_srgb.iter_mut().enumerate() {
            let lin = i as f64 / 4095.0;
            let s = if lin <= 0.003_130_8 {
                lin * 12.92
            } else {
                1.055 * lin.powf(1.0 / 2.4) - 0.055
            };
            *v = ((s * 255.0).round()).clamp(0.0, 255.0) as u8;
        }
        (to_lin, to_srgb)
    });
    (&tables.0, &tables.1)
}

/// Downscale a full ImageData (RGBA) to a smaller preview size. When the
/// linear-light toggle is on, this converts to 12-bit linear light and uses a
/// CatmullRom filter (sharper, correct edges on lineart); otherwise it uses the
/// fast gamma-space Triangle filter.
pub fn downscale_image(data: &ImageData, max_dim: u32) -> ImageData {
    let scale = (max_dim as f32 / data.width as f32)
        .min(max_dim as f32 / data.height as f32)
        .min(1.0);
    if scale >= 1.0 {
        return ImageData {
            width: data.width,
            height: data.height,
            rgba: data.rgba.clone(),
            load_time: data.load_time, // keep original decode time
        };
    }
    let (new_w, new_h) = (
        (data.width as f32 * scale) as u32,
        (data.height as f32 * scale) as u32,
    );
    let rgba = if linear_resample_enabled() {
        linear_resize(data, new_w, new_h)
    } else {
        let img = DynamicImage::ImageRgba8(
            RgbaImage::from_raw(data.width, data.height, data.rgba.clone()).unwrap(),
        );
        let resized = img.resize(new_w, new_h, image::imageops::FilterType::Triangle);
        resized.to_rgba8().into_raw()
    };
    ImageData {
        width: new_w,
        height: new_h,
        rgba,
        load_time: data.load_time, // keep original decode time
    }
}

fn linear_resize(data: &ImageData, new_w: u32, new_h: u32) -> Vec<u8> {
    let (to_lin, to_srgb) = lin_tables();
    let px = (data.width as usize) * (data.height as usize);
    let mut lin = vec![0u16; px * 4];
    for (c, chunk) in data.rgba.chunks_exact(4).enumerate() {
        lin[c * 4] = to_lin[chunk[0] as usize];
        lin[c * 4 + 1] = to_lin[chunk[1] as usize];
        lin[c * 4 + 2] = to_lin[chunk[2] as usize];
        lin[c * 4 + 3] = (chunk[3] as u16) << 4; // alpha is linear by definition
    }

    // Separable resample in 12-bit linear light with precomputed weights.
    let (kern, support) = kernel(downscale_filter());
    let horizontal = contributions(data.width, new_w, &kern, support);
    let vertical = contributions(data.height, new_h, &kern, support);

    // Horizontal pass: width -> new_w.
    let mut tmp = vec![0u16; (new_w as usize) * (data.height as usize) * 4];
    for y in 0..data.height as usize {
        let row = &lin[y * data.width as usize * 4..(y + 1) * data.width as usize * 4];
        for (x, con) in horizontal.iter().enumerate() {
            let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
            for &(si, w) in con {
                let p = &row[si * 4..si * 4 + 4];
                r += p[0] as f32 * w;
                g += p[1] as f32 * w;
                b += p[2] as f32 * w;
                a += p[3] as f32 * w;
            }
            let o = (y * new_w as usize + x) * 4;
            tmp[o] = r as u16;
            tmp[o + 1] = g as u16;
            tmp[o + 2] = b as u16;
            tmp[o + 3] = a as u16;
        }
    }

    // Vertical pass: height -> new_h, back to sRGB8.
    let mut out = Vec::with_capacity((new_w as usize) * (new_h as usize) * 4);
    let stride = new_w as usize * 4;
    for y in 0..new_h as usize {
        for x in 0..new_w as usize {
            let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
            for &(si, w) in &vertical[y] {
                let p = &tmp[si * stride + x * 4..si * stride + x * 4 + 4];
                r += p[0] as f32 * w;
                g += p[1] as f32 * w;
                b += p[2] as f32 * w;
                a += p[3] as f32 * w;
            }
            // Clamp to the 12-bit linear range (filters overshoot on edges).
            out.push(to_srgb[(r.max(0.0).min(4095.0)) as usize]);
            out.push(to_srgb[(g.max(0.0).min(4095.0)) as usize]);
            out.push(to_srgb[(b.max(0.0).min(4095.0)) as usize]);
            out.push(((a.max(0.0).min(4080.0) / 16.0).round()) as u8);
        }
    }
    out
}

/// Precomputed (source index, weight) pairs per destination pixel.
fn contributions(
    src_len: u32,
    dst_len: u32,
    kern: &impl Fn(f32) -> f32,
    support: f32,
) -> Vec<Vec<(usize, f32)>> {
    let mut out = Vec::with_capacity(dst_len as usize);
    let scale = dst_len as f32 / src_len as f32;
    // When downscaling, widen the filter by the inverse ratio (anti-aliasing).
    let filter_scale = if scale < 1.0 { 1.0 / scale } else { 1.0 };
    let radius = support * filter_scale;
    for dst in 0..dst_len as usize {
        let center = (dst as f32 + 0.5) / scale - 0.5;
        let lo = ((center - radius).floor() as i32).max(0) as usize;
        let hi = ((center + radius).ceil() as i32).min(src_len as i32 - 1) as usize;
        let mut con: Vec<(usize, f32)> = Vec::with_capacity((hi - lo + 1) as usize);
        let mut sum = 0f32;
        for si in lo..=hi {
            let w = kern((si as f32 - center) / filter_scale);
            if w != 0.0 {
                con.push((si, w));
                sum += w;
            }
        }
        // Normalize so weights sum to 1 (keeps brightness exact).
        for c in &mut con {
            c.1 /= sum;
        }
        out.push(con);
    }
    out
}

/// Normalize a decoded ImageData so dims and buffer agree. If the decoder's
/// buffer length doesn't match the declared dimensions, trust the buffer and
/// recompute dims when possible, otherwise pad the buffer. This guards
/// against decoders that mis-report sizes on damaged files.
pub fn normalized(mut data: ImageData) -> ImageData {
    let want = data.width as usize * data.height as usize * 4;
    if data.rgba.len() == want {
        return data;
    }
    let got = data.rgba.len();
    // If the buffer is self-consistent at some other height (or width), trust
    // it: that's what a decoder that mis-reported its size looks like.
    if data.width > 0 && got % (data.width as usize * 4) == 0 && got >= data.width as usize * 4 {
        data.height = (got / (data.width as usize * 4)) as u32;
        return data;
    }
    if data.height > 0 && got % (data.height as usize * 4) == 0 && got >= data.height as usize * 4 {
        data.width = (got / (data.height as usize * 4)) as u32;
        return data;
    }
    eprintln!(
        "image buffer {} bytes does not match {}x{}; padding to size",
        got, data.width, data.height
    );
    data.rgba.resize(want, 0);
    data
}

/// True if any pixel has a non-opaque alpha channel (used to decide whether
/// to draw the transparency checkerboard).
pub fn buffer_has_alpha(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|p| p[3] != 255)
}

/// True if the path looks like a zip-based image archive (cbz/zip).
pub fn is_archive_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_lowercase().as_str(), "cbz" | "zip"))
        .unwrap_or(false)
}

/// Extract supported image entries from a zip/cbz archive into `dest`,
/// flattening nested paths into the destination directory. Encrypted entries
/// are skipped and counted. Returns (extracted, encrypted_skipped).
pub fn extract_zip(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut extracted = 0;
    let mut encrypted = 0;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        if f.is_dir() {
            continue;
        }
        if f.encrypted() {
            encrypted += 1;
            continue;
        }
        let Some(name) = std::path::Path::new(f.name())
            .file_name()
            .and_then(|n| n.to_str())
        else {
            continue;
        };
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
            continue;
        }
        // Windows-safe file name (entries may contain characters that are
        // illegal in NTFS file names), deduplicated case-insensitively.
        let safe: String = name
            .chars()
            .map(|c| {
                if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        if !seen.insert(safe.to_lowercase()) {
            continue;
        }
        let mut out = std::fs::File::create(dest.join(&safe)).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
        extracted += 1;
    }
    Ok((extracted, encrypted))
}

/// A simple gallery state: a list of files and a current index.
#[derive(Debug, Clone)]
pub struct Gallery {
    pub files: Vec<PathBuf>,
    pub index: usize,
}

impl Gallery {
    pub fn new(folder: &Path) -> Self {
        Self {
            files: scan_folder(folder),
            index: 0,
        }
    }

    /// Create a gallery containing a single file.
    pub fn from_file(path: PathBuf) -> Self {
        Self {
            files: vec![path],
            index: 0,
        }
    }

    pub fn current(&self) -> Option<&Path> {
        self.files.get(self.index).map(|p| p.as_path())
    }

    pub fn next(&mut self) {
        if !self.files.is_empty() {
            self.index = (self.index + 1) % self.files.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.files.is_empty() {
            self.index = if self.index == 0 {
                self.files.len() - 1
            } else {
                self.index - 1
            };
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Load all images from the folder, preserving the current image's position.
/// Sorts with the natural (Explorer-like) name order.
pub fn load_all_from_folder(&mut self, folder: &Path) {
        // Remember current image path
        let current_path = match self.current() {
            Some(p) => p.to_path_buf(),
            None => PathBuf::new(),
        };

        // Scan and sort naturally (scan_folder already does)
        let new_files = scan_folder(folder);

        // Find position of current image in sorted list
        let mut new_index = 0;
        if !current_path.as_os_str().is_empty() && !new_files.is_empty() {
            for (i, file) in new_files.iter().enumerate() {
                if file == &current_path {
                    new_index = i;
                    break;
                }
            }
        }

        self.files = new_files;
        self.index = new_index;
    }
}

/// Zoom and pan state for the image viewport.
#[derive(Debug, Clone)]
pub struct ZoomPan {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ZoomPan {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoomPan {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }

    /// Apply a scroll delta to zoom, clamped to [0.1, 8.0].
    pub fn zoom_by(&mut self, delta: f32) {
        self.zoom = (self.zoom + delta * 0.01).clamp(0.1, 8.0);
    }

    /// Reset to fit-to-screen defaults.
    pub fn reset(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Fit the image to the available window area, maintaining aspect ratio.
    /// Always applies the fit scale (downscales large images, upscales small ones).
    pub fn fit_to_window(&mut self, tex_w: f32, tex_h: f32, avail_w: f32, avail_h: f32) {
        let scale_x = avail_w / tex_w;
        let scale_y = avail_h / tex_h;
        self.zoom = scale_x.min(scale_y).clamp(0.1, 8.0);
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }
}

/// GPS latitude/longitude in signed decimal degrees (N/E positive).
/// DMS rationals [deg, min, sec] combined with the N/S, E/W ref tags.
pub fn gps_coords(exif: &nom_exif::Exif) -> Option<(f64, f64)> {
    let lat = dms_to_deg(exif.get(ExifTag::GPSLatitude).and_then(|e| e.as_urational_slice()))?;
    let lon = dms_to_deg(exif.get(ExifTag::GPSLongitude).and_then(|e| e.as_urational_slice()))?;
    let lat_ref = exif
        .get(ExifTag::GPSLatitudeRef)
        .and_then(|v| v.as_str().map(|s| s.trim().to_ascii_uppercase()))
        .unwrap_or_else(|| "N".into());
    let lon_ref = exif
        .get(ExifTag::GPSLongitudeRef)
        .and_then(|v| v.as_str().map(|s| s.trim().to_ascii_uppercase()))
        .unwrap_or_else(|| "E".into());
    let lat = if lat_ref.starts_with('S') { -lat } else { lat };
    let lon = if lon_ref.starts_with('W') { -lon } else { lon };
    Some((lat, lon))
}

/// [deg, min, sec] rationals -> decimal degrees.
pub fn dms_to_deg(parts: Option<&[URational]>) -> Option<f64> {
    let parts = parts?;
    if parts.len() != 3 {
        return None;
    }
    let d = parts[0].to_f64()?;
    let m = parts[1].to_f64()?;
    let s = parts[2].to_f64()?;
    Some(d + m / 60.0 + s / 3600.0)
}

/// GPS altitude in meters (below-sea-level encoded as negative by the ref tag).
pub fn gps_altitude(exif: &nom_exif::Exif) -> Option<f64> {
    let alt = exif
        .get(ExifTag::GPSAltitude)
        .and_then(|e| e.as_urational())
        .and_then(|r| r.to_f64())?;
    let below = exif
        .get(ExifTag::GPSAltitudeRef)
        .and_then(|v| v.as_u8())
        .unwrap_or(0);
    Some(if below == 1 { -alt } else { alt })
}


