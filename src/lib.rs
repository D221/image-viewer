//! Core logic for the image viewer, kept separate from the GUI so it can be
//! unit-tested, benchmarked, and fuzz-tested.

pub mod perf;

use nom_exif::{ExifTag, URational};
use serde::{Deserialize, Serialize};
use rayon::prelude::*;
use std::borrow::Cow;
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

/// File name as a lossy string, the key `last_by_folder` resume stores and the
/// thing a gallery filter matches against.
pub fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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

// ---------------------------------------------------------------------------
// EXIF block: a minimal, allocation-free TIFF walker
// ---------------------------------------------------------------------------
//
// `nom_exif` can report the *values* of the thumbnail tags but not the bytes
// they point at, and it wants a path (re-reading the whole file). We already
// hold the file in memory, so this walks the APP1/Exif block directly: it costs
// one pass over a few hundred bytes, needs no allocation, and hands back the
// raw embedded JPEG.
//
// Every read is bounds-checked against the APP1 segment, because this parses
// untrusted input and a malformed offset must degrade to "no thumbnail", never
// to a panic on a worker.

/// Byte offset of a TIFF block's header ("II"/"MM"), i.e. just past `Exif\0\0`.
/// Returns `(start, end)` of the TIFF data, which is what all internal offsets
/// are relative to and what they must be bounds-checked against.
fn exif_tiff_span(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 2usize; // skip SOI
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        // Standalone markers carry no length.
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        // Length lives at i+2 and counts itself, so the payload starts at i+4
        // and the whole segment ends at i+2+len.
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if len < 2 || i + 2 + len > bytes.len() {
            return None;
        }
        let payload = i + 4;
        if marker == 0xE1 && &bytes[payload..payload + 6] == b"Exif\0\0" {
            return Some((payload + 6, i + 2 + len));
        }
        if marker == 0xDA {
            return None; // start of scan: no more metadata
        }
        i += 2 + len;
    }
    None
}

struct Tiff<'a> {
    b: &'a [u8],
    /// Index of the first byte of the TIFF header.
    base: usize,
    /// One past its last byte; every offset must resolve inside this.
    end: usize,
    le: bool,
}

impl<'a> Tiff<'a> {
    fn u16_at(&self, off: usize) -> Option<u16> {
        let a = self.base.checked_add(off)?;
        let b = a.checked_add(2)?;
        if b > self.end {
            return None;
        }
        let v = [self.b[a], self.b[b - 1]];
        Some(if self.le {
            u16::from_le_bytes(v)
        } else {
            u16::from_be_bytes(v)
        })
    }

    fn u32_at(&self, off: usize) -> Option<u32> {
        let a = self.base.checked_add(off)?;
        let b = a.checked_add(4)?;
        if b > self.end {
            return None;
        }
        let v = [self.b[a], self.b[a + 1], self.b[a + 2], self.b[a + 3]];
        Some(if self.le {
            u32::from_le_bytes(v)
        } else {
            u32::from_be_bytes(v)
        })
    }

    /// Offset of the IFD after the one at `off`, i.e. the "next IFD" pointer
    /// that follows an IFD's entry table. IFD0's next pointer is how you find
    /// IFD1, where the thumbnail lives.
    fn ifd_next(&self, off: usize) -> Option<u32> {
        let n = usize::from(self.u16_at(off)?);
        self.u32_at(off.checked_add(2 + n.checked_mul(12)?)?)
    }

    /// First `count` entries of the IFD at `off`, as `(tag, entry_offset)`.
    fn ifd(&self, off: usize) -> Vec<(u16, usize)> {
        let mut out = Vec::new();
        let Some(n) = self.u16_at(off) else {
            return out;
        };
        // A sane IFD has tens of entries, not thousands; cap it so a corrupt
        // count cannot make us walk a megabyte of attacker-controlled offsets.
        for i in 0..n.min(64) {
            let Some(e) = off.checked_add(2 + usize::from(i) * 12) else {
                break;
            };
            let Some(tag) = self.u16_at(e) else {
                break;
            };
            out.push((tag, e + 8)); // +8 = the value field
        }
        out
    }
}

/// Location of the compressed thumbnail inside IFD1.
#[derive(Debug, Clone, Copy)]
struct ThumbRef {
    /// Range into the *whole file*.
    range: (usize, usize),
    /// Dimensions read from the thumbnail's own SOF marker, when we could find it.
    dims: Option<(u32, u32)>,
}

/// Scan the APP1/Exif block for a compressed (old-style JPEG) thumbnail in IFD1.
///
/// Returns `None` for any JPEG without one, which is the common case for
/// screenshots, exports and anything not written by a camera.
fn find_embedded_thumbnail(bytes: &[u8]) -> Option<ThumbRef> {
    let (base, end) = exif_tiff_span(bytes)?;
    if end < base + 8 || bytes.len() < base + 8 {
        return None;
    }
    let le = match &bytes[base..base + 2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = Tiff {
        b: bytes,
        base,
        end,
        le,
    };
    // IFD0 sits at the offset in the header; IFD1 (the thumbnail) is reachable
    // only through IFD0's *next-IFD* pointer, which is after its entry table.
    let ifd0 = t.u32_at(4)? as usize;
    let ifd1 = t.ifd_next(ifd0)? as usize;
    let entries = t.ifd(ifd1);
    if entries.is_empty() {
        return None;
    }
    let mut compression = None;
    let mut off = None;
    let mut len = None;
    for (tag, val) in entries {
        match tag {
            0x0103 => compression = t.u16_at(val),          // Compression
            0x0201 => off = t.u32_at(val),                  // ThumbnailOffset
            0x0202 => len = t.u32_at(val),                  // ThumbnailLength
            _ => {}
        }
    }
    // 6 = old-style JPEG. Anything else (uncompressed TIFF, or newer
    // formats like HEIF thumbnails) is not something we can slice out.
    if compression != Some(6) {
        return None;
    }
    let (off, len) = (off? as usize, len? as usize);
    if off == 0 || len < 4 {
        return None;
    }
    let start = base.checked_add(off)?;
    let stop = start.checked_add(len)?;
    if stop > bytes.len() {
        return None;
    }
    let slice = &bytes[start..stop];
    // It has to actually be a JPEG: SOI at the front, EOI at the back. A
    // truncated block is common in the wild and must not reach a decoder.
    if slice.len() < 4 || slice[0] != 0xFF || slice[1] != 0xD8 || slice[stop - start - 2] != 0xFF
        || slice[stop - start - 1] != 0xD9
    {
        return None;
    }
    // SOF0..SOF15 (minus the non-frame markers DHT/JPG/DAC) carry the size.
    let dims = find_sof_dims(slice);
    Some(ThumbRef {
        range: (start, stop),
        dims,
    })
}

/// Read `(width, height)` out of a JPEG's first SOF marker.
fn find_sof_dims(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        let m = jpeg[i + 1];
        // SOF0/1/2/3/5/6/7/9/10/11/13/14/15 all have the same layout.
        if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]);
            let w = u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]);
            return Some((u32::from(w), u32::from(h)));
        }
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            i += 2;
            continue;
        }
        if m == 0xDA {
            return None; // entropy-coded data, no more headers
        }
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Grid cells are decoded at 256 px. Only below this target is it worth using a
/// file's embedded EXIF thumbnail: it is ~160 px and *already encoded*, so it
/// beats decoding a 24 MP file to throw 99% of it away. Above this (the main
/// view) the loss of sharpness would be visible, so we decode for real.
pub const EMBEDDED_THUMB_MAX_TARGET: u32 = 512;

/// Relative difference between two ratios, tolerant of rounding.
fn ratio_diff(a: f32, b: f32) -> f32 {
    let m = a.max(b).max(1e-6);
    (a - b).abs() / m
}

/// Which rotation, if any, to apply to an embedded thumbnail so that it is
/// displayed the same way the main image is.
///
/// The EXIF spec does not settle whether a camera stores the thumbnail already
/// rotated to the display orientation or in raw sensor orientation, and the
/// answer varies by maker. Guessing would show sideways photos in the grid, so
/// instead we **decide per file from the aspect ratio**: for a quarter-turn
/// orientation the two hypotheses produce mirrored aspect ratios, and the one
/// that matches the thumbnail is the right one.
///
/// Returns `None` when the evidence cannot separate them (mirrored/180
/// orientations, which preserve ratio, and near-square sources). The caller
/// then decodes the file properly — correct and merely slower.
fn embedded_thumb_rotation(
    main_tag: u16,
    main: (u32, u32),
    thumb: (u32, u32),
) -> Option<u16> {
    /// Aspect ratios within this of each other count as the same.
    const TOL: f32 = 0.15;
    let (tw, th) = thumb;
    if tw == 0 || th == 0 {
        return None;
    }
    let thumb_r = tw as f32 / th as f32;
    match main_tag {
        0 | 1 => Some(1), // already the display orientation
        // 2/3/4 (mirror / 180) keep the aspect ratio, so a ratio tells us
        // nothing. Rare in real photos; refuse rather than guess.
        2..=4 => None,
        5..=8 => {
            let (mw, mh) = main;
            if mw == 0 || mh == 0 {
                return None;
            }
            // A square source is ambiguous: both hypotheses give ~1.0.
            if ratio_diff(mw as f32 / mh as f32, 1.0) < TOL {
                return None;
            }
            let main_r = mw as f32 / mh as f32;
            if ratio_diff(thumb_r, main_r) < TOL {
                // Thumbnail has the *stored* aspect ratio: apply the tag.
                Some(main_tag)
            } else if ratio_diff(thumb_r, 1.0 / main_r) < TOL {
                // Already rotated: the thumbnail is the finished picture.
                Some(1)
            } else {
                None
            }
        }
        _ => None,
    }
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

/// Decode the file's embedded EXIF thumbnail, if it is worth using.
///
/// Returns `None` — meaning "decode the file properly" — for every JPEG without
/// a usable one, which is most of them. `main` is the *stored* source size and
/// `main_tag` its EXIF orientation.
fn decode_embedded_thumbnail(
    bytes: &[u8],
    target: u32,
    main: Option<(u32, u32)>,
    main_tag: u16,
    start: std::time::Instant,
) -> Option<ImageData> {
    // Only for grid-sized requests. See EMBEDDED_THUMB_MAX_TARGET.
    if target == 0 || target > EMBEDDED_THUMB_MAX_TARGET {
        return None;
    }
    let Some(t) = find_embedded_thumbnail(bytes) else {
        return None;
    };
    let (w, h) = t.dims?;
    let (mw, mh) = main?;
    // An upscale means the embedded copy is too small to be worth anything, and
    // a "thumbnail" as large as the image is a mislabelled full-res copy.
    if w.max(h) * 2 > mw.max(h) {
        return None;
    }
    // Decide the rotation from the evidence; bail out if it is ambiguous.
    embedded_thumb_rotation(main_tag, (mw, mh), (w, h))?;

    let raw = bytes.get(t.range.0..t.range.1)?;
    let img = image::load_from_memory_with_format(raw, image::ImageFormat::Jpeg).ok()?;
    let rgba = img.to_rgba8();
    let mut data = normalized(ImageData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        load_time: start.elapsed(),
    });
    // Apply whichever transform the aspect ratio said was needed. Computed
    // again from the *decoded* size so the dims always end up self-consistent.
    let want = match embedded_thumb_rotation(main_tag, (mw, mh), (data.width, data.height)) {
        Some(1) => 1,
        Some(t) => t,
        None => return None,
    };
    if want != 1 {
        apply_exif_orientation(&mut data, want);
    }
    if data.width.max(data.height) > target {
        data = downscale_image(&data, target);
    }
    Some(data)
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
    let jpeg = is_jpeg_bytes(&bytes);
    // EXIF orientation: camera pixels are stored unrotated; the tag says how
    // to display them. Rotate after decode so preview and full agree, and fix
    // the reported source dimensions accordingly.
    let tag = if jpeg { exif_orientation(path) } else { 0 };
    let stored = image_dims_bytes(&bytes);
    let orig = stored.map(|(w, h)| if orientation_swaps_dims(tag) { (h, w) } else { (w, h) });

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

    // A camera's embedded EXIF thumbnail, for grid-sized requests. Ahead of the
    // DCT path on purpose: that one still decompresses every coefficient, so it
    // saves RAM but not much time, whereas this reads a few KB that were already
    // encoded. No full decode happens, so `full` stays `None` and the caller
    // re-decodes if the user actually opens the image.
    if jpeg {
        if let Some(preview) = decode_embedded_thumbnail(&bytes, max_dim, stored, tag, start) {
            return Ok(PreviewAndFull {
                preview,
                full: None,
                anim: None,
                orig,
                below_target: true,
            });
        }
    }

    let full_is_cheap = orig.map_or(true, |(w, h)| w as u64 * h as u64 <= FAST_FULL_MAX_PIXELS);

    if jpeg && !full_is_cheap {
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

/// Is linear-light downsampling on? Public so tests can save/restore it
/// around assertions that only hold on one of the two filter paths.
pub fn linear_resample_enabled() -> bool {
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
        (data.width as f32 * scale).max(1.0) as u32,
        (data.height as f32 * scale).max(1.0) as u32,
    );
    let out = if linear_resample_enabled() {
        ImageData {
            width: new_w,
            height: new_h,
            rgba: linear_resize(data, new_w, new_h),
            load_time: data.load_time, // keep original decode time
        }
    } else {
        let img = DynamicImage::ImageRgba8(
            RgbaImage::from_raw(data.width, data.height, data.rgba.clone()).unwrap(),
        );
        // **Report the size the library actually produced, not the size we asked
        // for.** `DynamicImage::resize` runs `resize_dimensions(fill = false)`,
        // which *preserves the aspect ratio* (`ratio = min(nw/w, nh/h)`), so it
        // routinely hands back an image a pixel or two smaller than requested:
        // 980x552 asked for 300x168 comes back 298x168. Pairing our own
        // `new_w`/`new_h` with its shorter buffer violated the
        // width*height*4 == rgba.len() invariant, and
        // `ColorImage::from_rgba_unmultiplied` asserts on exactly that — so the
        // worker panicked, the app reported "decoder panicked", and the image was
        // then poisoned in `failed` for the rest of the session.
        //
        // It only showed up once previews were sized to the display area
        // (11d778c) instead of a fixed 4096: a small target rounds much harder,
        // so the aspect-ratio clamp started shaving pixels off. That is also why
        // it looked machine- and window-dependent, and why only the gamma-space
        // path failed — `linear_resize` writes its own exact buffer.
        let resized = img
            .resize(new_w, new_h, image::imageops::FilterType::Triangle)
            .to_rgba8();
        ImageData {
            width: resized.width(),
            height: resized.height(),
            rgba: resized.into_raw(),
            load_time: data.load_time, // keep original decode time
        }
    };
    // Belt and braces: the downstream texture-size assert is unforgiving and a
    // worker panic there is unrecoverable, so no downscale may escape without
    // re-establishing the buffer/dimension invariant.
    normalized(out)
}

/// `PERF_PHASE=1` prints a per-phase breakdown of `linear_resize`. Cached: this
/// is read from inside the resampler.
static PHASE_TIMING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Source span `(start, len)` covered by each of `n` output samples when
/// reducing `src` samples to `n`. Precomputed once, so the hot loops never
/// divide.
fn box_spans(n: usize, src: usize) -> Vec<(usize, usize)> {
    (0..n)
        .map(|i| {
            let a = (i * src) / n;
            let b = (((i + 1) * src) / n).max(a + 1).min(src);
            (a, b - a)
        })
        .collect()
}

/// `1 << 32 / len`, so the inner loops average with a multiply instead of a
/// divide. Integer division in the innermost loop cost more than the whole
/// rest of the resample. Add [`ROUND`] before shifting: truncating instead
/// biases every average down by up to 1 (a 255/255 average came out 254).
fn box_reciprocal(len: usize) -> u64 {
    (1u64 << 32) / len.max(1) as u64
}

/// Rounding term for the reciprocal-multiply average.
const ROUND: u64 = 1u64 << 31;

/// Separable box reduction of an RGBA8 buffer to `pre_w x pre_h`.
///
/// Stays in sRGB8 with integer sums, the way a GPU mip chain does: the
/// expensive step downstream is the 12-bit linear conversion, and doing *that*
/// on the reduced buffer rather than the full-resolution source is the point.
///
/// The vertical pass keeps a running column accumulator so it reads the
/// intermediate buffer strictly in order; summing each output row from scratch
/// instead strides by `pre_w * 4` bytes and misses cache on every pixel.
fn box_reduce_srgb(data: &ImageData, pre_w: u32, pre_h: u32) -> Vec<u8> {
    let (sw, sh) = (data.width as usize, data.height as usize);
    let (pw, ph) = (pre_w as usize, pre_h as usize);
    let src_stride = sw * 4;
    let xs = box_spans(pw, sw);
    // Reciprocals next to the spans: `box_reciprocal` is a 64-bit division, and
    // calling it per output pixel cost more than everything else combined.
    let xrecip: Vec<u64> = xs.iter().map(|&(_, l)| box_reciprocal(l)).collect();

    // Horizontal: sw -> pw, still at full height. Row-parallel: each output row
    // is independent, and this is the single hottest loop in the viewer.
    let mut tmp = vec![0u8; pw * sh * 4];
    let src = &data.rgba;
    tmp.par_chunks_mut(pw * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let row = &src[y * src_stride..(y + 1) * src_stride];
            for (px, (&(x0, len), &inv)) in xs.iter().zip(xrecip.iter()).enumerate() {
                let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
                for x in x0..x0 + len {
                    let p = &row[x * 4..x * 4 + 4];
                    r += p[0] as u32;
                    g += p[1] as u32;
                    b += p[2] as u32;
                    a += p[3] as u32;
                }
                let o = px * 4;
                out_row[o] = (((r as u64 * inv) + ROUND) >> 32) as u8;
                out_row[o + 1] = (((g as u64 * inv) + ROUND) >> 32) as u8;
                out_row[o + 2] = (((b as u64 * inv) + ROUND) >> 32) as u8;
                out_row[o + 3] = (((a as u64 * inv) + ROUND) >> 32) as u8;
            }
        });
    // Vertical: sh -> ph, with a running column accumulator.
    if ph == sh {
        return tmp;
    }
    let ys = box_spans(ph, sh);
    let tmp_stride = pw * 4;
    let mut out = vec![0u8; pw * ph * 4];
    let mut acc = vec![0u32; pw * 4];
    for (oy, &(y0, len)) in ys.iter().enumerate() {
        if oy == 0 {
            for y in y0..y0 + len {
                let row = &tmp[y * tmp_stride..(y + 1) * tmp_stride];
                for (i, v) in row.iter().enumerate() {
                    acc[i] += *v as u32;
                }
            }
        } else {
            // Slide the window. Spans are contiguous and increasing, so the
            // previous window is [py0, py0+plen) and the new one is a sub-range
            // of it: drop the rows that left at the top, add the ones that
            // arrived at the bottom.
            let (py0, plen) = ys[oy - 1];
            for y in py0..y0 {
                let row = &tmp[y * tmp_stride..(y + 1) * tmp_stride];
                for (i, v) in row.iter().enumerate() {
                    acc[i] -= *v as u32;
                }
            }
            for y in py0 + plen..y0 + len {
                let row = &tmp[y * tmp_stride..(y + 1) * tmp_stride];
                for (i, v) in row.iter().enumerate() {
                    acc[i] += *v as u32;
                }
            }
        }
        let inv = box_reciprocal(len);
        let base = oy * pw * 4;
        for i in 0..pw * 4 {
            out[base + i] = (((acc[i] as u64 * inv) + ROUND) >> 32) as u8;
        }
    }
    out
}

/// Resample in 12-bit linear light with a separable kernel.
///
/// Large reductions get a cheap **box pre-reduction** first (down to at most
/// 2x the target). Measured on a 40 MP source this took the resample from
/// ~790ms to well under 200ms, and it is a quality *win* too: a big reduction
/// straight into Lanczos undersamples and aliases, because its ~6 taps end up
/// spread over thousands of source pixels.
fn linear_resize(data: &ImageData, new_w: u32, new_h: u32) -> Vec<u8> {
    let (to_lin, to_srgb) = lin_tables();

    // Pass 1: reduce to at most 2x the target (sRGB box, integer maths), then
    // convert *that* to 12-bit linear. The conversion is the expensive part, so
    // it must not run over the full-resolution source.
    let pre_w = data.width.min(new_w.saturating_mul(2)).max(1);
    let pre_h = data.height.min(new_h.saturating_mul(2)).max(1);
    let t0 = std::time::Instant::now();
    let (small, lw, lh): (Cow<'_, [u8]>, u32, u32) = if pre_w < data.width || pre_h < data.height {
        (Cow::Owned(box_reduce_srgb(data, pre_w, pre_h)), pre_w, pre_h)
    } else {
        (Cow::Borrowed(&data.rgba), data.width, data.height)
    };
    let t1 = std::time::Instant::now();
    let mut lin = vec![0u16; (lw as usize) * (lh as usize) * 4];
    small
        .par_chunks_exact(lw as usize * 4)
        .zip(lin.par_chunks_mut(lw as usize * 4))
        .for_each(|(srow, lrow)| {
            for (c, chunk) in srow.chunks_exact(4).enumerate() {
                lrow[c * 4] = to_lin[chunk[0] as usize];
                lrow[c * 4 + 1] = to_lin[chunk[1] as usize];
                lrow[c * 4 + 2] = to_lin[chunk[2] as usize];
                // alpha is linear by definition
                lrow[c * 4 + 3] = (chunk[3] as u16) << 4;
            }
        });

    let t2 = std::time::Instant::now();
    // Separable kernel resample in 12-bit linear light with precomputed weights.
    let (kern, support) = kernel(downscale_filter());
    let horizontal = contributions(lw, new_w, &kern, support);
    let vertical = contributions(lh, new_h, &kern, support);

    // Horizontal pass: width -> new_w.
    let mut tmp = vec![0u16; (new_w as usize) * (lh as usize) * 4];
    let pre_stride = lw as usize * 4;
    let new_wu = new_w as usize;
    tmp.par_chunks_mut(new_wu * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
        let row = &lin[y * pre_stride..(y + 1) * pre_stride];
        for (x, con) in horizontal.iter().enumerate() {
            let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
            for &(si, w) in con {
                let p = &row[si * 4..si * 4 + 4];
                r += p[0] as f32 * w;
                g += p[1] as f32 * w;
                b += p[2] as f32 * w;
                a += p[3] as f32 * w;
            }
            let o = x * 4;
            out_row[o] = r as u16;
            out_row[o + 1] = g as u16;
            out_row[o + 2] = b as u16;
            out_row[o + 3] = a as u16;
        }
    });

    let t3 = std::time::Instant::now();
    // Vertical pass: height -> new_h, back to sRGB8.
    let mut out = vec![0u8; (new_w as usize) * (new_h as usize) * 4];
    let stride = new_w as usize * 4;
    out.par_chunks_mut(stride).enumerate().for_each(|(y, out_row)| {
        {
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
            let o = x * 4;
            out_row[o] = to_srgb[(r.max(0.0).min(4095.0)) as usize];
            out_row[o + 1] = to_srgb[(g.max(0.0).min(4095.0)) as usize];
            out_row[o + 2] = to_srgb[(b.max(0.0).min(4095.0)) as usize];
            out_row[o + 3] = ((a.max(0.0).min(4080.0) / 16.0).round()) as u8;
        }
        }
    });
    if *PHASE_TIMING.get_or_init(|| std::env::var("PERF_PHASE").is_ok()) {
        eprintln!(
            "  linear_resize {}x{}->{}x{}: box={:?} to_linear={:?} weights={:?} hpass={:?} vpass={:?}",
            data.width, data.height, new_w, new_h,
            t1 - t0, t2 - t1, t3 - t2, t3.elapsed(), t3.elapsed()
        );
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

    /// Rebuild [`Self::files`] from `all`, keeping only names matching `needle`.
    ///
    /// The match is a case-insensitive substring of the file name; an empty or
    /// whitespace-only needle means "no filter". The folder is scanned flat, so
    /// there is no directory component to match beyond the name.
    ///
    /// The current image is kept if it survived, otherwise the index falls back
    /// to the nearest surviving position (clamped, never out of range). Returns
    /// true if the current image changed, which is the caller's cue to re-goto.
    pub fn set_filtered(&mut self, all: &[PathBuf], needle: &str) -> bool {
        let current = self.current().map(|p| p.to_path_buf());
        let needle = needle.trim().to_lowercase();
        self.files = if needle.is_empty() {
            all.to_vec()
        } else {
            all.iter()
                .filter(|p| file_name_of(p).to_lowercase().contains(&needle))
                .cloned()
                .collect()
        };
        self.index = current
            .as_ref()
            .and_then(|p| self.files.iter().position(|f| f == p))
            .unwrap_or_else(|| self.index.min(self.files.len().saturating_sub(1)));
        self.current() != current.as_deref()
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


