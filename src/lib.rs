//! Core logic for the image viewer, kept separate from the GUI so it can be
//! unit-tested, benchmarked, and fuzz-tested.

pub mod perf;

/// The application icon, drawn in code rather than committed as a binary.
/// See the module docs for why it lives here and not in `examples/`.
pub mod icon;

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

    /// Wrap from the last image back to the first (and vice versa) when
    /// navigating. On by default, which is the historical behaviour, so an old
    /// config without this key keeps looping.
    #[serde(default = "default_true")]
    pub loop_back: bool,

    /// Which wgpu backend to render with, and the last probe result.
    #[serde(default)]
    pub renderer: RendererPref,

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
            loop_back: true,
            renderer: RendererPref::default(),
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
///
/// Purely a *listing* filter — decoding is chosen from the file's magic bytes
/// (`is_jpeg_bytes` and friends), so a misnamed file still opens whatever it
/// really is. Anything missed here simply never appears in a folder scan; it
/// can still be opened by passing the path on the command line.
pub const SUPPORTED_EXTENSIONS: [&str; 16] = [
    "jpg", "jpeg", "png", "webp", "gif", "jxl", "avif", "heic", "heif", // 9
    "tif", "tiff", // TIFF
    "bmp", "dib", // Windows bitmap
    "ico", "cur", // Windows icon / cursor (one container format)
    "qoi", // Quite OK Image: lossless, modern, and detectable
];

/// Decoders the `image` crate offers that this app does not use, each with the
/// reason.
///
/// Not a wish list, and not a claim about what the app *cannot* open — `avif` is
/// in that list and is very much supported, just not by this crate. The reasons
/// are not all the same kind:
///
///  * **Targa (`tga`)** — has *no magic bytes*. Decoding goes through
///    `image::load_from_memory`, which identifies by content, so a Targa cannot
///    be recognised at all. Making it work would mean passing the file extension
///    into the decode path, which is exactly the extension-based dispatch this
///    codebase is built to avoid — and the reason a PNG saved as `.jpg` keeps
///    working. `a_targa_cannot_be_identified_from_its_content` demonstrates this
///    with a real hand-built Targa rather than asserting it.
///  * **DDS** — decode-only in the crate, so a fixture cannot be generated with
///    the crate that decodes it, and it is a game-texture container rather than
///    something anyone keeps photographs in.
///  * **OpenEXR and Radiance HDR** — linear-light floating point. Rendered
///    through an 8-bit sRGB surface with no tone mapping they come out washed
///    out and blown out, which is worse than refusing them: the user cannot tell
///    the file is fine and the viewer is wrong. Belongs with the P3 → sRGB work
///    in `TODO.md` §1, as part of a real colour-management story.
///  * **PNM (pbm/pgm/ppm/pam)** and **farbfeld** — four more extensions of a
///    bitmap-mask family and a format that exists mostly inside Rust demos. They
///    would pad the file-association list for close to no real use.
///  * **AVIF** — already decoded, and better, by the `heic` crate, which also
///    reads the container's `colr` box. The `image` crate's AVIF support would be
///    a second, weaker decoder for a format already handled.
///
/// The line is not "can the crate decode it" but "can we show it *correctly*,
/// and can we still test it". QOI clears both; the rest do not.
pub const IMAGE_CRATE_DECODERS_WE_SKIP: &[(&str, &str)] = &[
    ("tga", "no magic bytes; content-based detection cannot see it"),
    ("dds", "decode-only in the crate, so untestable with it"),
    ("exr", "linear float; would display wrong without tone mapping"),
    ("hdr", "high dynamic range; same, and would look plausible but be wrong"),
    ("pnm", "bitmap-mask family; no real use in a photo viewer"),
    ("ff", "farbfeld; a demo format"),
    ("avif", "already handled, and better, by the heic crate"),
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

/// Which wgpu backends to enable at instance-creation time.
///
/// wgpu has no "try this, else that": `WgpuSetup::native_adapter_selector`
/// still filters enumeration by `InstanceDescriptor::backends` first, so every
/// enabled backend is *probed* before one is chosen, and the probe is a hard
/// crash (`0xC0000005`) when wgpu's Vulkan backend cannot cope with a driver.
///
/// That crash is why this enum exists at all, and it is also why `Auto` cannot
/// be smarter. Measured on this machine: `vulkaninfo` reports a healthy
/// Intel UHD 620 device at API 1.2.187, and wgpu + Vulkan still access-violates
/// every single time. So "is Vulkan installed" is the wrong question — Vulkan
/// working does not imply wgpu-on-Vulkan working, and the only way to find out
/// is to crash.
/// `rename_all = "lowercase"` is load-bearing, not tidiness. Without it serde
/// writes and expects `"Dx12"`, while [`WgpuBackend::name`] and
/// [`WgpuBackend::parse`] — the two functions everything else goes through —
/// use `"dx12"`. The result is a config file that this program cannot read back
/// if a person edits it to the name the documentation says, and two spellings of
/// the same setting in the same file. This was caught by reading a config the
/// app had just written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WgpuBackend {
    /// Probe, and use the first backend that survives. See [`PROBE_ORDER`].
    Auto,
    /// DirectX 12. The Windows native backend, and the only combination verified
    /// to work here.
    Dx12,
    /// OpenGL, through wgpu's GLES backend.
    Gl,
    /// Vulkan.
    Vulkan,
}

impl WgpuBackend {
    /// Every backend, in the order a human would pick them off the list. Excludes
    /// `Auto`, which is not a backend.
    pub const CHOICES: [WgpuBackend; 3] = [Self::Dx12, Self::Gl, Self::Vulkan];

    /// Parse a backend choice, case-insensitively. `None` for anything
    /// unrecognised, so a typo falls back to the default rather than picking
    /// something surprising.
    ///
    /// **There is no DX11 option and there cannot be one.** wgpu removed its
    /// Direct3D 11 backend upstream (it is absent from `Backends` entirely by
    /// wgpu 22), so `dx11` here is a typo that correctly resolves to nothing.
    /// Windows exposes DX12 and OpenGL to wgpu, and those are the choices.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "vulkan" | "vk" => Some(Self::Vulkan),
            "dx12" | "d3d12" => Some(Self::Dx12),
            "gl" | "opengl" | "gles" => Some(Self::Gl),
            _ => None,
        }
    }

    /// The stable name used in the config file and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dx12 => "dx12",
            Self::Gl => "gl",
            Self::Vulkan => "vulkan",
        }
    }

    /// The label shown in the settings dropdown.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (probe for a working one)",
            Self::Dx12 => "DirectX 12",
            Self::Gl => "OpenGL",
            Self::Vulkan => "Vulkan",
        }
    }
}

/// The order `Auto` probes in, most-likely-to-work first.
///
/// DX12 leads because it is the native Windows backend and the only one verified
/// to work on the machine this was developed on. GL is second: it goes through a
/// different driver stack, so it is the one most likely to work where DX12 does
/// not. Vulkan is last *because it is the one that crashes*: on the development
/// machine wgpu access-violates during adapter enumeration 3 times out of 3 even
/// though `vulkaninfo` reports a healthy device. That is survivable here only
/// because each probe runs in its own process — see [`pick_probed`].
pub const PROBE_ORDER: [WgpuBackend; 3] =
    [WgpuBackend::Dx12, WgpuBackend::Gl, WgpuBackend::Vulkan];

/// The backend to use, given which probes survived.
///
/// `results` is `(backend, survived)` in probe order. The first survivor wins.
/// If nothing survived, DX12: it is the only backend with no way to fail a
/// probe *and* no way to fail to start, so it is the least-bad answer when the
/// probes were themselves untrustworthy.
///
/// This is a pure function of the probe results, which is the only reason it can
/// be tested — the probing itself cannot, because a failure is a crash.
pub fn pick_probed(results: &[(WgpuBackend, bool)]) -> WgpuBackend {
    for &want in &PROBE_ORDER {
        if results.iter().any(|(b, ok)| *b == want && *ok) {
            return want;
        }
    }
    WgpuBackend::Dx12
}

/// How the user chose the renderer, and what the last probe concluded.
///
/// Kept as two fields so `Auto` does not re-probe on every launch: probing
/// spawns a child process per candidate, which is a poor thing to do on every
/// start. `probed` is only a *cache of a probe* — an explicit choice ignores it
/// entirely, and if the machine's drivers change the user can clear it by
/// choosing a backend and switching back to Automatic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererPref {
    /// What the user picked.
    pub choice: WgpuBackend,
    /// The last backend a probe found working, reused to skip probing.
    pub probed: Option<WgpuBackend>,
}

impl Default for RendererPref {
    fn default() -> Self {
        Self {
            choice: WgpuBackend::Auto,
            probed: None,
        }
    }
}

impl RendererPref {
    /// The backend to actually use, given what a fresh probe found (or `None`
    /// if no probe was run).
    pub fn resolve(&self, fresh: Option<WgpuBackend>) -> WgpuBackend {
        match self.choice {
            WgpuBackend::Auto => fresh
                .or(self.probed)
                .unwrap_or(WgpuBackend::Dx12),
            other => other,
        }
    }

    /// Whether launching with this preference needs to probe, i.e. whether
    /// `Auto` is set and no usable answer is cached.
    pub fn needs_probe(&self) -> bool {
        self.choice == WgpuBackend::Auto && self.probed.is_none()
    }
}

/// The wgpu backend bits to enable for a resolved backend.
///
/// **Exactly one, always.** This is the whole reason the probe design exists:
/// wgpu enumerates every backend in the set *before* choosing an adapter, and on
/// some drivers that enumeration is a hard access violation before any window
/// exists. So a "try the others if this fails" set is not a fallback, it is just
/// a way to crash. One backend per process, and let the probe decide which.
///
/// `Auto` never reaches here — it is resolved first — but it maps to DX12 rather
/// than to an empty set, because an empty `Backends` would fail in a much more
/// confusing way.
pub fn wgpu_backend_set(requested: WgpuBackend) -> u32 {
    match requested {
        // Bit values are wgpu's `Backends` constants; `main.rs` converts to the
        // real type so this module does not have to depend on wgpu.
        WgpuBackend::Auto | WgpuBackend::Dx12 => 1 << 3, // DX12
        WgpuBackend::Gl => 1 << 1,                        // GL
        WgpuBackend::Vulkan => 1 << 0,                   // VULKAN
    }
}

/// The file or folder to open, taken from the command line: the first argument
/// that is not a flag.
///
/// This exists because taking `args().nth(1)` is wrong the moment a flag can
/// come first, and it fails *silently and expensively*: `--perf` is not a path,
/// so it fell through to the persisted/default folder and the app quietly
/// measured or displayed the wrong thing. That cost a wrong performance
/// conclusion before it was caught, because `--perf <folder>` is the exact
/// invocation AGENTS.md documents.
///
/// An argument is treated as a flag when it starts with `-`. On Windows that is
/// safe for real paths; if it ever is not, `.\-name.jpg` still works.
pub fn path_arg<I, S>(args: I) -> Option<std::path::PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    // Flags that consume the *next* argument. Their value does not start with a
    // dash, so a plain "not a flag" test hands the value back as the folder —
    // which is precisely how `--perf <folder>` once silently opened the wrong
    // directory. Any new flag that takes a value has to be listed here.
    const FLAGS_WITH_VALUE: [&str; 1] = ["--probe-renderer"];

    let mut skip_next = false;
    args.into_iter()
        .map(|s| s.as_ref().to_string())
        .filter(|s| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if FLAGS_WITH_VALUE.contains(&s.as_str()) {
                skip_next = true;
                return false;
            }
            true
        })
        .find(|s| !s.starts_with('-') && !s.is_empty())
        .map(std::path::PathBuf::from)
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

/// Footprint `(width, height)` an image of `w x h` occupies once `xform` has been
/// applied to it.
///
/// Only the four axis-swapping transforms change the shape; a mirror does not,
/// which is why this is asked of the transform rather than derived from a turn
/// count. All eight keep the result axis-aligned, so 1:1 stays pixel-sharp.
pub fn xformed_footprint(w: f32, h: f32, xform: Xform) -> (f32, f32) {
    if xform.swaps_axes() {
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

// ---------------------------------------------------------------------------
// Load scheduling: what to decode, and the worker pool that decodes it
// ---------------------------------------------------------------------------
//
// This is the machinery that caused "random images never load", so it lives
// here rather than in `main.rs` where it cannot be tested. `main.rs` keeps the
// egui shell (textures, panels) and hands this module a decode function; a test
// hands it a trivial one and can drive the queue through any interleaving.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

/// What a decode job is for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Preview,
    Full,
    Exif,
    Thumb,
}

/// One unit of decoding work.
#[derive(Clone, Debug)]
pub struct Job {
    pub path: PathBuf,
    pub kind: Kind,
    /// Lower runs first. The current image is 0 and every prefetch is >= 10, so
    /// prefetch can never starve it.
    pub prio: u32,
    /// Display target long side in physical pixels (preview jobs only).
    pub target_dim: u32,
}

/// Shared queue state. Public only so tests can assert on it.
#[derive(Default)]
pub struct QueueState {
    pub jobs: Vec<Job>,
    pub busy: HashSet<(PathBuf, Kind)>,
    /// The viewer's current desired set, as of the last `replace`, held *whole*
    /// so a re-queue can preserve priority and target size.
    ///
    /// This is the fix for the stranding bug, and getting it right took two
    /// attempts. The naive version remembered only those wanted jobs that
    /// happened to be *running* at the instant `replace` was called, which is
    /// not enough: a job that was merely queued then, and abandoned later, was
    /// still lost. The only correct question on a job finishing is "is this
    /// still wanted?", not "was this wanted and running a moment ago?".
    ///
    /// The earlier bug was `replace` filtering wanted-but-busy jobs out of the
    /// queue and forgetting them. That is harmless while every job runs to
    /// completion — the running one delivers. It becomes fatal as soon as a job
    /// can give up: the re-request is already discarded, and `schedule` only
    /// re-runs on a step, the current image's preview arriving, a dwell expiry
    /// or a resize, so a stranded *neighbour* is never asked for again.
    wanted: HashMap<(PathBuf, Kind), Job>,
}

/// Priority queue served by a small pool of worker threads.
pub struct Loader {
    shared: Arc<(Mutex<QueueState>, Condvar)>,
}

impl Loader {
    /// Start the pool.
    ///
    /// `run` does the work for one job and returns `(message, delivered)`.
    /// `delivered` is false only when the job gave up *without producing a
    /// result* — today that never happens, and it is the hook that makes
    /// abandoning work safe: a job that gave up is re-queued if it was still
    /// wanted, and a job that delivered is not re-run.
    ///
    /// `panic_msg` turns a caught panic back into a message so a corrupt file
    /// cannot kill a worker, and keeps the payload — a bare "decoder panicked"
    /// once hid OOM, the most likely cause, behind an unreproducible symptom.
    /// `wake` is called on the thread that received a result, to ask for a
    /// repaint.
    pub fn spawn<M, R, P, W>(
        workers: usize,
        tx: Sender<M>,
        run: R,
        panic_msg: P,
        wake: W,
    ) -> Self
    where
        M: Send + 'static,
        R: Fn(&Job) -> (M, bool) + Send + Sync + 'static,
        P: Fn(&Job, String) -> (M, bool) + Send + Sync + 'static,
        W: Fn() + Send + Sync + 'static,
    {
        let shared: Arc<(Mutex<QueueState>, Condvar)> =
            Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        let run = Arc::new(run);
        let panic_msg = Arc::new(panic_msg);
        let wake = Arc::new(wake);
        for _ in 0..workers.max(1) {
            let shared = shared.clone();
            let tx = tx.clone();
            let run = run.clone();
            let panic_msg = panic_msg.clone();
            let wake = wake.clone();
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

                let (msg, delivered) =
                    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&job))) {
                        Ok(r) => r,
                        Err(payload) => {
                            perf::count(perf::C_DECODE_PANICS, 1);
                            let detail = payload
                                .downcast_ref::<&str>()
                                .map(|s| (*s).to_string())
                                .or_else(|| payload.downcast_ref::<String>().cloned())
                                .unwrap_or_else(|| "unknown".to_string());
                            // A panicking decode is still a *delivered* failure:
                            // it is reported and remembered, not retried here.
                            panic_msg(&job, format!("decoder panicked: {detail}"))
                        }
                    };

                let alive = tx.send(msg).is_ok();
                // Free the slot, then honour anything still waiting on it.
                let (lock, cv) = &*shared;
                let mut q = lock.lock().unwrap();
                let key = (job.path.clone(), job.kind);
                q.busy.remove(&key);
                if !delivered {
                    // Gave up without producing anything. If it is still wanted,
                    // ask again — this is the whole safety property, and it is
                    // why the wanted set is kept rather than inferred.
                    if let Some(wanted) = q.wanted.get(&key).cloned() {
                        if !q.jobs.iter().any(|j| (j.path.clone(), j.kind) == key) {
                            q.jobs.push(wanted);
                            cv.notify_all();
                        }
                    }
                }
                drop(q);
                if !alive {
                    return; // UI is gone
                }
                wake();
            });
        }
        Self { shared }
    }

    /// Queue a job without disturbing existing queued work (the grid's
    /// on-demand thumbnail requests). Already queued or already running is
    /// left alone.
    pub fn push(&self, job: Job) {
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

    /// Make the queued work match `jobs` exactly. In-flight work is not
    /// disturbed; a wanted job that is already running is remembered in
    /// `pending` and re-queued when it finishes, so nothing wanted is lost.
    pub fn replace(&self, jobs: Vec<Job>) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        let before = q.jobs.len();
        q.wanted.clear();
        let mut keep = Vec::with_capacity(jobs.len());
        for j in jobs {
            let key = (j.path.clone(), j.kind);
            // `wanted` is recorded either way, and is what makes losing the job
            // safe: a job that is already running is not queued twice, and a job
            // that is queued can still be re-queued if it gives up later.
            let running = q.busy.contains(&key);
            q.wanted.insert(key, j.clone());
            if !running {
                keep.push(j);
            }
        }
        q.jobs = keep;
        // Coalescing: how much queued work a navigation threw away. High values
        // mean the user outran the decoder, which is the thing to tune.
        if before > q.jobs.len() {
            perf::count(perf::C_JOBS_DROPPED, (before - q.jobs.len()) as u64);
        }
        cv.notify_all();
    }

    /// Queued + running, for tests and diagnostics.
    pub fn outstanding(&self) -> usize {
        let q = self.shared.0.lock().unwrap();
        q.jobs.len() + q.busy.len()
    }

    /// Jobs waiting to be picked up.
    pub fn queued(&self) -> usize {
        self.shared.0.lock().unwrap().jobs.len()
    }

    /// Jobs a worker is currently running.
    pub fn running(&self) -> usize {
        self.shared.0.lock().unwrap().busy.len()
    }

    /// Wanted jobs that are neither queued nor running. Once the loader is idle
    /// this must be zero: non-zero means the viewer wants a picture that nobody
    /// is going to fetch, which is the "random images never load" failure.
    pub fn wanted_but_idle(&self) -> usize {
        let q = self.shared.0.lock().unwrap();
        q.wanted
            .keys()
            .filter(|k| {
                !q.busy.contains(*k)
                    && !q.jobs.iter().any(|j| (j.path.clone(), j.kind) == **k)
            })
            .count()
    }
}

/// Inputs to [`plan_jobs`]. Everything the planner needs, and nothing it does
/// not, so the plan is a pure function of observable state.
pub struct Plan<'a> {
    pub files: &'a [PathBuf],
    pub index: usize,
    /// Paths that already have a cached preview.
    pub cached: &'a HashSet<PathBuf>,
    /// `(path, kind)` pairs known to be undecodable this session.
    pub failed: &'a HashSet<(PathBuf, Kind)>,
    /// Exif for the current image has not been fetched yet.
    pub wants_exif: bool,
    /// The current image still needs a full-resolution decode.
    pub wants_full: bool,
    /// The current image *has* a cached preview, but it is softer than the
    /// display can show, so it must be re-decoded anyway. See [`needs_refine`].
    pub current_stale: bool,
    /// Preview texture long side, physical pixels.
    pub target_dim: u32,
    pub radius: usize,
}

/// Priority of the current image's preview. Every prefetch is >= 10, so
/// prefetch can never starve it.
pub const PRIO_CURRENT_PREVIEW: u32 = 0;
/// Priority of the current image's full-resolution decode.
pub const PRIO_FULL: u32 = 15;
/// Priority of the current image's exif read.
pub const PRIO_CURRENT_EXIF: u32 = 5;
/// Grid thumbnail priorities start here.
pub const PRIO_THUMB_BASE: u32 = 45;

/// The work the viewer wants for the current position, as a job list.
///
/// Pure on purpose: the stranding bug lived in the interaction between this
/// function and a queue that discarded wanted-but-busy jobs, and neither half
/// was testable. Now the plan can be asserted directly.
///
/// "Next" is queued slightly ahead of "previous" at each distance, so a step
/// forward is favoured — that is the fixed asymmetric window, not
/// direction-aware, which is a later change and must be measured separately.
pub fn plan_jobs(p: &Plan) -> Vec<Job> {
    let mut jobs = Vec::new();
    let n = p.files.len();
    if n == 0 {
        return jobs;
    }
    let i = p.index.min(n - 1);
    let cur = &p.files[i];

    let need_preview = |path: &PathBuf| {
        !p.cached.contains(path) && !p.failed.contains(&(path.clone(), Kind::Preview))
    };

    // A stale preview is cached *and* still wanted: the texture stays on screen
    // (so the display never blanks) while a sharper one is decoded behind it.
    // Deliberately current-image-only — refining the whole preload window costs
    // one decode per neighbour, which is the difference between 1 and 7 decodes
    // after a drag.
    if need_preview(cur) || (p.current_stale && !p.failed.contains(&(cur.clone(), Kind::Preview))) {
        jobs.push(Job {
            path: cur.clone(),
            kind: Kind::Preview,
            prio: PRIO_CURRENT_PREVIEW,
            target_dim: p.target_dim,
        });
    }
    if p.wants_exif {
        jobs.push(Job {
            path: cur.clone(),
            kind: Kind::Exif,
            prio: PRIO_CURRENT_EXIF,
            target_dim: 0,
        });
    }
    if p.wants_full {
        jobs.push(Job {
            path: cur.clone(),
            kind: Kind::Full,
            prio: PRIO_FULL,
            target_dim: 0,
        });
    }

    let mut seen: HashSet<usize> = HashSet::from([i]);
    for d in 1..=p.radius {
        for (idx, prio) in [
            ((i + d) % n, 10 + 2 * d as u32),
            ((i + n - d % n) % n, 11 + 2 * d as u32),
        ] {
            if seen.insert(idx) && need_preview(&p.files[idx]) {
                jobs.push(Job {
                    path: p.files[idx].clone(),
                    kind: Kind::Preview,
                    prio,
                    target_dim: p.target_dim,
                });
            }
        }
    }
    jobs
}

// ---------------------------------------------------------------------------
// Resize refinement
// ---------------------------------------------------------------------------

/// Long side of the preview texture for a display area, in physical pixels.
pub fn preview_target(panel_px: (u32, u32)) -> u32 {
    panel_px
        .0
        .max(panel_px.1)
        .clamp(256, PREVIEW_TEXTURE_DIM)
}

/// How much the display must grow before a sharper decode is worth issuing, in
/// physical pixels. Below this the extra sharpness is invisible but the decode
/// costs as much as any other, and a window drag produces a stream of these.
pub const RESIZE_PX_FLOOR: u32 = 8;

/// True when the cached preview is softer than the display can now show *and* a
/// re-decode at `now_target` would actually be sharper.
///
/// Pure, and in `lib.rs` so the rule can be tested without a window — this
/// replaces "drop the preview on resize", which blanked the picture and
/// re-decoded once per frame of a drag.
///
/// * `preview_long` — long side of the texture on screen.
/// * `source_long` — long side of the source, so a source that is *already*
///   smaller than the target is never re-decoded (that is the loop guard: the
///   target keeps growing while no decode can ever be sharper).
/// * `decoded_for` — the target the cached preview was decoded for, not its
///   pixel size. Comparing pixels would re-decode forever on a source smaller
///   than the display; comparing targets converges after one decode.
/// * `floor` — [`RESIZE_PX_FLOOR`].
pub fn needs_refine(
    preview_long: u32,
    source_long: u32,
    decoded_for: u32,
    now_target: u32,
    floor: u32,
) -> bool {
    // Not enough growth to be worth a decode.
    if now_target <= decoded_for.saturating_add(floor) {
        return false;
    }
    // Only if the texture is genuinely soft for the display now, and the source
    // has more to give.
    preview_long < now_target && source_long > preview_long
}

/// What the viewer should do about one frame of a resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeAction {
    /// Nothing to do: either the texture is sharp enough, or a refine is
    /// already in flight.
    Idle,
    /// The size is still moving. Ask for one more frame and decide then —
    /// without that, the *final* size of a drag is never examined, because
    /// nothing else would ask for a repaint.
    Wait,
    /// A sharper decode is wanted. Issue it once; do not ask again until it
    /// lands.
    Refine,
}

/// The resize state machine, as a pure value.
///
/// This is the coalescing. A window drag delivers a stream of sizes and one
/// decode costs the same whatever the size, so the rule is "issue once the size
/// stops moving" — tested by *comparing with the previous frame*, not with a
/// wall-clock timer, because a large preview takes ~800 ms to decode and frames
/// are then ~800 ms apart, so any timer shorter than that expires between every
/// pair of them and never fires.
///
/// In `lib.rs` next to `plan_jobs` for the same reason the planner is: the
/// decisions that once lived in a binary were the ones that could not be tested,
/// and the bug they caused ("random images never load") was invisible for
/// exactly that reason.
#[derive(Debug, Default, Clone, Copy)]
pub struct ResizeTracker {
    /// A refine has been issued and has not landed. Sticky, so a drag costs one
    /// decode rather than one per frame.
    pending: bool,
    prev_target: Option<u32>,
}

impl ResizeTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Is a refine in flight?
    pub fn pending(&self) -> bool {
        self.pending
    }

    /// One frame of the resize path. `target` is the display's long side in
    /// physical pixels; `needs_refine` is [`needs_refine`] for the texture
    /// currently on screen.
    pub fn frame(&mut self, target: u32, needs_refine: bool) -> ResizeAction {
        let settled = self.prev_target == Some(target);
        self.prev_target = Some(target);
        if self.pending || !needs_refine {
            return ResizeAction::Idle;
        }
        if settled {
            self.pending = true;
            ResizeAction::Refine
        } else {
            ResizeAction::Wait
        }
    }

    /// The refined preview arrived. Clears the pending flag; the next frame
    /// re-evaluates, so a resize that grew again while this was decoding asks
    /// for one more rather than being served a texture that is still soft.
    pub fn landed(&mut self) {
        self.pending = false;
    }

    /// The display changed for a reason that must not re-decode — a panel
    /// toggle or F11. Consume the transition silently.
    pub fn suppress(&mut self) {
        self.pending = false;
    }
}

// ---------------------------------------------------------------------------
// Lossless transform: the algebra of the eight lattice symmetries
// ---------------------------------------------------------------------------

/// One of the eight square-lattice symmetries — the complete set that both an
/// EXIF orientation tag and a lossless JPEG transform can express.
///
/// This exists because "rotate the saved file to match the screen" is a
/// *composition* of two of them: the EXIF tag the camera wrote, and the quarter
/// turns the user has since pressed. Composing them by hand is how you get a
/// save that is mirrored, or transposed, or applied twice.
///
/// A value maps output pixels back to input pixels, so composing is
/// substitution: where does the final output pixel come from, through the
/// second transform and then the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Xform {
    #[default]
    Identity,
    /// 90 degrees clockwise.
    Rot90,
    Rot180,
    /// 90 degrees counter-clockwise.
    Rot270,
    FlipH,
    FlipV,
    /// Mirror along the top-left to bottom-right axis.
    Transpose,
    /// Mirror along the top-right to bottom-left axis.
    Transverse,
}

impl Xform {
    pub const ALL: [Xform; 8] = [
        Xform::Identity,
        Xform::Rot90,
        Xform::Rot180,
        Xform::Rot270,
        Xform::FlipH,
        Xform::FlipV,
        Xform::Transpose,
        Xform::Transverse,
    ];

    /// Do the axes swap? True for everything that turns the picture on its side.
    pub fn swaps_axes(self) -> bool {
        matches!(
            self,
            Xform::Rot90 | Xform::Rot270 | Xform::Transpose | Xform::Transverse
        )
    }

    /// Dimensions of the result, given the input.
    pub fn out_dims(self, w: u32, h: u32) -> (u32, u32) {
        if self.swaps_axes() {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Which input pixel ends up at output pixel `(ox, oy)`.
    pub fn map(self, ox: u32, oy: u32, w: u32, h: u32) -> (u32, u32) {
        match self {
            Xform::Identity => (ox, oy),
            // Output is h x w. A CW turn takes the input's left edge to the top.
            Xform::Rot90 => (oy, h - 1 - ox),
            Xform::Rot180 => (w - 1 - ox, h - 1 - oy),
            Xform::Rot270 => (w - 1 - oy, ox),
            Xform::FlipH => (w - 1 - ox, oy),
            Xform::FlipV => (ox, h - 1 - oy),
            Xform::Transpose => (oy, ox),
            // Anti-diagonal mirror. The output is h x w, so `ox` ranges over
            // [0, h) and indexes the *height*: getting this pair the wrong way
            // round underflows as soon as the image is wider than it is tall.
            Xform::Transverse => (w - 1 - oy, h - 1 - ox),
        }
    }

    /// The linear part of the **input -> output** map, row-major, for painting.
    ///
    /// [`Self::map`] goes the other way (output pixel -> input pixel), which is
    /// what a sampler wants; drawing a texture needs the opposite, and taking the
    /// transpose is the whole of the difference. Every one of the eight is a
    /// *signed permutation* — entries in `{0, +-1}` with one per row and column —
    /// so the inverse is the transpose, and no division or inversion is involved.
    ///
    /// Derived by probing `map` and transposing, for the same reason `then` is:
    /// transcribing this by hand is eight matrices of sign errors, and each sign
    /// error produces a picture that looks plausible. Because it comes from `map`,
    /// the screen and the saved file cannot disagree about what the transform is —
    /// `the_paint_matrix_reproduces_the_old_quarter_turn_behaviour` pins the four
    /// rotations against the `Rot2` this replaced.
    pub fn paint_matrix(self) -> [[f32; 2]; 2] {
        // Any dimensions work: the differences are taken before the axes matter.
        // Two pixels of slack in each axis is all `map` needs.
        let (w, h) = (9u32, 5u32);
        let p0 = self.map(0, 0, w, h);
        let px = self.map(1, 0, w, h);
        let py = self.map(0, 1, w, h);
        // `map` is output -> input, so these two differences are the *columns* of
        // that matrix.
        let cx = (px.0 as i64 - p0.0 as i64, px.1 as i64 - p0.1 as i64);
        let cy = (py.0 as i64 - p0.0 as i64, py.1 as i64 - p0.1 as i64);
        let a = [
            [cx.0 as f32, cy.0 as f32], // row 0
            [cx.1 as f32, cy.1 as f32], // row 1
        ];
        // Transposed: input -> output.
        [[a[0][0], a[1][0]], [a[0][1], a[1][1]]]
    }

    /// A human-readable name, for the save confirmation and the status readout.
    pub fn label(self) -> &'static str {
        match self {
            Xform::Identity => "no transform",
            Xform::Rot90 => "rotated 90° CW",
            Xform::Rot180 => "rotated 180°",
            Xform::Rot270 => "rotated 270° CW",
            Xform::FlipH => "mirrored left-right",
            Xform::FlipV => "mirrored top-bottom",
            Xform::Transpose => "transposed",
            Xform::Transverse => "transversed",
        }
    }

    /// The transform an EXIF orientation tag describes, or `None` for 0 and for
    /// tags outside 1-=8.
    ///
    /// The mapping is not invented here: it is checked against
    /// [`apply_exif_orientation`] by a test, so the two cannot drift.
    pub fn from_exif(tag: u16) -> Option<Xform> {
        Some(match tag {
            1 => Xform::Identity,
            2 => Xform::FlipH,
            3 => Xform::Rot180,
            4 => Xform::FlipV,
            5 => Xform::Transpose,
            6 => Xform::Rot90,
            7 => Xform::Transverse,
            8 => Xform::Rot270,
            _ => return None,
        })
    }

    /// `turns` quarter turns clockwise — what the rotate buttons produce.
    pub fn quarters_cw(turns: u8) -> Xform {
        match turns & 3 {
            0 => Xform::Identity,
            1 => Xform::Rot90,
            2 => Xform::Rot180,
            _ => Xform::Rot270,
        }
    }

    /// `self` first, then `next`.
    ///
    /// Found by *probing* rather than by transcribing a multiplication table: the
    /// eight ops form a group, so the composition is whichever single op maps the
    /// output to the same input pixels. Working this out structurally is four
    /// pages of sign analysis and gets it wrong in a way that only shows up as a
    /// mirrored saved photo; this is twenty lines and a test proves it.
    pub fn then(self, next: Xform) -> Xform {
        // Deliberately not square, so a transpose cannot masquerade as a
        // rotation: both preserve an area but swap the axes, and only the
        // corners tell them apart on a square.
        let (iw, ih) = (7u32, 5u32);
        let (mw, mh) = self.out_dims(iw, ih); // `self`'s output = `next`'s input
        let (ow, oh) = next.out_dims(mw, mh); // the composition's output
        // `map` takes the *input* dimensions, which is the easy thing to get
        // wrong here and produces underflow rather than a wrong answer.
        let probe = |ox: u32, oy: u32| {
            let (bx, by) = next.map(ox, oy, mw, mh);
            self.map(bx, by, iw, ih)
        };
        let mut found = None;
        for cand in Xform::ALL {
            if cand.out_dims(iw, ih) != (ow, oh) {
                continue;
            }
            if cand.map(0, 0, iw, ih) == probe(0, 0)
                && cand.map(ow - 1, oh - 1, iw, ih) == probe(ow - 1, oh - 1)
                && cand.map(0, oh - 1, iw, ih) == probe(0, oh - 1)
                && cand.map(ow - 1, 0, iw, ih) == probe(ow - 1, 0)
            {
                debug_assert!(found.is_none(), "two transforms match one composition");
                found = Some(cand);
            }
        }
        found.expect("the eight transforms are closed under composition")
    }

    /// The transform that undoes `self`.
    ///
    /// Needed because a crop is chosen on the **displayed** picture and applied to
    /// the **stored** one, so getting from one to the other is an inverse. Each of
    /// the eight is a bijection, so an inverse exists and is one of the eight; it
    /// is found by probing, exactly as [`Self::then`] composes, rather than
    /// transcribed. A sign error here does not corrupt the image — it crops the
    /// wrong region, which is a perfectly good picture of the wrong part of the
    /// photo.
    pub fn inverse(self) -> Xform {
        Xform::ALL
            .into_iter()
            .find(|cand| cand.then(self) == Xform::Identity)
            .expect("the eight transforms are closed under inversion")
    }

    /// The same operation as a TurboJPEG lossless transform.
    pub fn as_turbojpeg(self) -> turbojpeg::TransformOp {
        match self {
            Xform::Identity => turbojpeg::TransformOp::None,
            Xform::Rot90 => turbojpeg::TransformOp::Rot90,
            Xform::Rot180 => turbojpeg::TransformOp::Rot180,
            Xform::Rot270 => turbojpeg::TransformOp::Rot270,
            Xform::FlipH => turbojpeg::TransformOp::Hflip,
            Xform::FlipV => turbojpeg::TransformOp::Vflip,
            Xform::Transpose => turbojpeg::TransformOp::Transpose,
            Xform::Transverse => turbojpeg::TransformOp::Transverse,
        }
    }

    /// Apply to an RGBA buffer. Only for the formats that have to be re-encoded.
    pub fn apply_rgba(self, w: u32, h: u32, rgba: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let (ow, oh) = self.out_dims(w, h);
        let want_in = (w as usize) * (h as usize) * 4;
        let want_out = (ow as usize) * (oh as usize) * 4;
        if rgba.len() != want_in {
            return None;
        }
        let mut out = vec![0u8; want_out];
        for oy in 0..oh {
            for ox in 0..ow {
                let (ix, iy) = self.map(ox, oy, w, h);
                let src = ((iy as usize) * (w as usize) + ix as usize) * 4;
                let dst = ((oy as usize) * (ow as usize) + ox as usize) * 4;
                out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
            }
        }
        Some((ow, oh, out))
    }
}

// ---------------------------------------------------------------------------
// Cropping
// ---------------------------------------------------------------------------

/// A crop rectangle in **stored** pixel coordinates — that is, before the EXIF
/// orientation is applied.
///
/// Stored, not display, and that is the whole subtlety. TurboJPEG's crop region
/// is expressed against the compressed source, so the rectangle has to be
/// converted from the picture the user dragged on. It is safe to do it in that
/// order — crop the stored pixels, *then* turn them — because a lattice symmetry
/// maps a rectangle to a rectangle: `T(crop(src, R'))` is exactly
/// `crop(T(src), R)` when `R' = T⁻¹(R)`. So the result is the region the user
/// actually framed, not the same region somewhere else in the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Crop {
    /// A validated crop, or `None` for an empty or inverted rectangle.
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Option<Crop> {
        (w > 0 && h > 0).then_some(Crop { x, y, w, h })
    }

    pub fn right(&self) -> u32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> u32 {
        self.y + self.h
    }

    /// Does this crop keep every pixel? Then there is nothing to do, and the save
    /// should behave exactly as it did before cropping existed.
    pub fn is_full(&self, w: u32, h: u32) -> bool {
        self.x == 0 && self.y == 0 && self.w >= w && self.h >= h
    }

    /// Pull the rectangle inside a `w x h` image, shrinking rather than moving,
    /// and refusing to become empty.
    pub fn clamped_to(&self, w: u32, h: u32) -> Option<Crop> {
        if self.x >= w || self.y >= h {
            return None;
        }
        Some(Crop {
            x: self.x,
            y: self.y,
            w: self.w.min(w - self.x),
            h: self.h.min(h - self.y),
        })
    }

    /// Grow the rectangle onto the MCU grid, and clamp it to the image.
    ///
    /// TurboJPEG requires a crop's *origin* to sit on an MCU boundary — 8 px for
    /// 4:4:4, 16 px for the common 4:2:0 — and a size that stops part-way through
    /// a block leaves a partial block that `trim` then discards. Either way the
    /// boundary lands somewhere other than where the user asked.
    ///
    /// Grown, not shrunk, so nothing inside the selection is ever lost: you get
    /// up to one block of *extra* picture rather than up to one block cut off the
    /// edge you framed. The UI draws this rectangle, not the raw drag, so what is
    /// on screen is what gets saved — the same bargain the rotate trim already
    /// makes, and the only honest one available at this granularity.
    pub fn snapped_to_mcu(&self, mcu_w: u32, mcu_h: u32, w: u32, h: u32) -> Option<Crop> {
        let mcu_w = mcu_w.max(1);
        let mcu_h = mcu_h.max(1);
        // The origin moves *down* to the grid, which pushes the far edge out, so
        // the rectangle only ever grows before the clamp.
        let snapped = Crop {
            x: self.x - self.x % mcu_w,
            y: self.y - self.y % mcu_h,
            w: self.w + self.x % mcu_w,
            h: self.h + self.y % mcu_h,
        };
        snapped.clamped_to(w, h)
    }

    /// The rectangle the user framed on the **displayed** picture, expressed in
    /// stored coordinates.
    ///
    /// `rect` is `(x, y, w, h)` in the displayed image's own pixels — not screen
    /// points; the caller does the scaling. `stored` is the file's `(w, h)`.
    ///
    /// The four corners go through [`Xform::inverse`], and the answer is their
    /// bounding box. A lattice symmetry sends a rectangle to a rectangle, so the
    /// box is exact rather than an approximation, and going through `map` means
    /// this cannot disagree with what is drawn on screen.
    pub fn from_display_rect(
        exif_tag: u16,
        user: Xform,
        rect: (u32, u32, u32, u32),
        stored: (u32, u32),
    ) -> Option<Crop> {
        let (dx, dy, dw, dh) = rect;
        let (dw, dh) = (dw.checked_sub(1)?, dh.checked_sub(1)?); // to inclusive corners
        let (sw, sh) = stored;
        let display = display_xform(exif_tag, user);

        // `Xform::map` takes *output* coordinates plus the *input* dimensions: it
        // answers "this output pixel came from which input pixel". The displayed
        // picture is the output here and the stored pixels are the input, so this
        // is `display.map(..)` with the stored dimensions — no inverse needed.
        // Passing the inverse here instead transposes the rectangle, which for a
        // quarter turn is the difference between cropping the top-left of the
        // photo and the bottom-left.
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (cx, cy) in [(dx, dy), (dx + dw, dy), (dx, dy + dh), (dx + dw, dy + dh)] {
            let (sx, sy) = display.map(cx, cy, sw, sh);
            min_x = min_x.min(sx);
            min_y = min_y.min(sy);
            max_x = max_x.max(sx);
            max_y = max_y.max(sy);
        }
        Crop::new(min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)?.clamped_to(sw, sh)
    }

    /// This stored rectangle expressed in the **displayed** picture's pixels.
    ///
    /// The forward direction, and the exact inverse of [`Self::from_display_rect`].
    /// The UI draws the result of this, so what is framed on screen is what the
    /// save will cut — and
    /// `a_display_rect_survives_the_round_trip_through_stored_space` checks the
    /// two against each other over every transform and a grid of rectangles.
    ///
    /// `display` is the full stored -> display transform, i.e. the one from
    /// [`display_xform`], not the user's part alone. `stored` is the file's
    /// `(w, h)`, which is what the display's dimensions are derived from.
    pub fn to_display_rect(&self, display: Xform, stored: (u32, u32)) -> (u32, u32, u32, u32) {
        let (dw, dh) = display.out_dims(stored.0, stored.1);
        let inv = display.inverse();
        let x1 = self.right().saturating_sub(1);
        let y1 = self.bottom().saturating_sub(1);
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (cx, cy) in [
            (self.x, self.y),
            (x1, self.y),
            (self.x, y1),
            (x1, y1),
        ] {
            let (dx, dy) = inv.map(cx, cy, dw, dh);
            min_x = min_x.min(dx);
            min_y = min_y.min(dy);
            max_x = max_x.max(dx);
            max_y = max_y.max(dy);
        }
        (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
    }

    /// Cut this rectangle out of an RGBA buffer in stored coordinates.
    pub fn apply_rgba(&self, w: u32, h: u32, rgba: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let c = self.clamped_to(w, h)?;
        let stride = w as usize * 4;
        let row = c.w as usize * 4;
        let mut out = Vec::with_capacity(c.w as usize * c.h as usize * 4);
        for y in c.y..c.bottom() {
            let start = y as usize * stride + c.x as usize * 4;
            out.extend_from_slice(rgba.get(start..start + row)?);
        }
        Some((c.w, c.h, out))
    }

    /// A short description for the status line and the save confirmation.
    pub fn label(&self) -> String {
        format!("{}×{}", self.w, self.h)
    }
}

/// The four rectangles that dim everything outside `crop` but inside `outer`.
///
/// Axis-aligned rectangles as `[min_x, min_y, max_x, max_y]`, ordered top, bottom,
/// left, right. The top and bottom bands span the **full** width of `outer`, and
/// the left and right bands span only between them. That split is what makes them
/// tile the difference *exactly* — every pixel outside the frame is covered once
/// and no pixel inside it is covered at all.
///
/// Which is the whole content of this function, and the first version got it
/// wrong: the top band ran from the picture's corner to the frame's top-*left*
/// corner and the bottom band from the frame's bottom-*right* to the picture's
/// corner, so only the corners of the top and bottom edges were dimmed while the
/// left and right bands were right. It looked like a rendering bug and was
/// actually an arithmetic one.
/// `the_dimming_bands_cover_exactly_what_is_being_cut_away` is what pins it.
///
/// Plain floats rather than `egui::Rect` so it can be tested: this module has no
/// egui dependency on purpose.
pub fn dim_bands(outer: [f32; 4], crop: [f32; 4]) -> [[f32; 4]; 4] {
    let [ox0, oy0, ox1, oy1] = outer;
    // Clip the frame to the picture. A drag that ends outside the image must not
    // dim anything beyond the edge.
    let cx0 = crop[0].max(ox0);
    let cy0 = crop[1].max(oy0);
    let cx1 = crop[2].min(ox1);
    let cy1 = crop[3].min(oy1);
    [
        [ox0, oy0, ox1, cy0], // top:    full width
        [ox0, cy1, ox1, oy1], // bottom: full width
        [ox0, cy0, cx0, cy1], // left:   only between the other two
        [cx1, cy0, ox1, cy1], // right:  only between the other two
    ]
}

/// The transform that takes a file's *stored* pixels to what the viewer shows:
/// the EXIF orientation applied at decode, then whatever the user has done since.
///
/// The user's part is a whole [`Xform`], not a turn count, because a mirror and a
/// rotation are the same kind of edit — a quarter turn then a mirror is a
/// *diagonal* mirror, and keeping two independent counters would mean the save had
/// to work out that composition at the last moment. Composing it as it is pressed
/// means there is only ever one thing to store, one thing to display, and one
/// thing to hand TurboJPEG.
pub fn display_xform(exif_tag: u16, user: Xform) -> Xform {
    Xform::from_exif(exif_tag)
        .unwrap_or(Xform::Identity)
        .then(user)
}

/// Why a transform could not be saved, when it could not.
///
/// Deliberately has *no* `#[from] std::io::Error`: reading and writing are
/// different failures to the user, and collapsing them into one variant loses
/// which half of the operation went wrong.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("could not read the file: {0}")]
    Read(std::io::Error),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("{0} would have to be re-encoded, which is lossy — this only saves losslessly")]
    Lossy(String),
    #[error("writing failed: {0}")]
    Write(std::io::Error),
}

/// Human name for a format, for the "cannot save this" message.
fn format_name(bytes: &[u8]) -> &'static str {
    if is_jpeg_bytes(bytes) {
        "JPEG"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "PNG"
    } else if is_gif_bytes(bytes) {
        "GIF"
    } else if is_tiff_bytes(bytes) {
        "TIFF"
    } else if is_bmp_bytes(bytes) {
        "BMP"
    } else if is_ico_bytes(bytes) {
        "ICO"
    } else if is_qoi_bytes(bytes) {
        "QOI"
    } else if is_jxl_bytes(bytes) {
        "JPEG XL"
    } else if is_heif_bytes(bytes) {
        "HEIF/AVIF"
    } else {
        "this format"
    }
}

/// A lossless re-encode target for `bytes`, if there is one.
///
/// Only formats that can hold the transformed pixels *without* recompression. A
/// WebP, GIF, HEIC or JXL would have to be decoded and re-encoded, which is a
/// second generation of compression — and this is a button that says "save", so
/// it must never quietly do that.
fn lossless_raster(bytes: &[u8]) -> Option<image::ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(image::ImageFormat::Png)
    } else if is_tiff_bytes(bytes) {
        Some(image::ImageFormat::Tiff)
    } else if is_bmp_bytes(bytes) {
        Some(image::ImageFormat::Bmp)
    } else {
        None
    }
}

/// Write `src` to a new file beside it, with the transform baked in, and return
/// the path written.
///
/// **The original is never touched.** This writes `photo_rotated.jpg` next to
/// `photo.jpg`, or `photo_rotated2.jpg` and onwards if that exists, so the edit
/// is reversible and re-rotating accumulates instead of compounding. There is no
/// undo for an in-place write, and a wrong transform is exactly the kind of
/// mistake you notice after closing the app.
///
/// JPEG goes through `tjTransform`, which operates on the DCT coefficients, so
/// the result is the same compressed data rearranged — no second generation,
/// and typically faster than decoding the file. PNG, BMP and TIFF are decoded
/// and re-encoded into their own lossless containers, which is lossless because
/// those formats have no lossy step.
///
/// The output's EXIF orientation is set to 1, because the pixels now *are* the
/// display orientation. See [`neutralize_exif_orientation`].
pub fn save_transformed(
    src: &Path,
    user: Xform,
    crop: Option<Crop>,
) -> Result<PathBuf, SaveError> {
    let bytes = std::fs::read(src).map_err(SaveError::Read)?;
    let is_jpeg = is_jpeg_bytes(&bytes);
    let exif_tag = if is_jpeg { exif_orientation(src) } else { 0 };
    let xform = display_xform(exif_tag, user);

    // Resolve the crop **once**, up front, rather than inside the JPEG branch.
    // It has to be settled before the file is named: a crop that keeps every
    // pixel is not a crop, and if the name were chosen from the caller's value
    // instead of the resolved one, a no-op crop would produce `photo_cropped.jpg`
    // for a file that was not cropped. That is the sort of thing that only shows
    // up when somebody tries it.
    let crop = match crop {
        None => None,
        Some(c) => {
            if is_jpeg {
                let mut dec = turbojpeg::Decompressor::new().map_err(|e| {
                    SaveError::Decode(format!("could not open a decoder: {e}"))
                })?;
                let header = dec.read_header(&bytes).map_err(|e| {
                    SaveError::Decode(format!("could not read the JPEG header: {e}"))
                })?;
                let (iw, ih) = (header.width as u32, header.height as u32);
                if c.is_full(iw, ih) {
                    None
                } else {
                    c.snapped_to_mcu(
                        header.subsamp.mcu_width() as u32,
                        header.subsamp.mcu_height() as u32,
                        iw,
                        ih,
                    )
                }
            } else {
                // No block structure outside JPEG, so nothing to snap to; just
                // refuse one that falls off the image.
                c.clamped_to(u32::MAX, u32::MAX)
            }
        }
    };

    let mut out = bytes.clone();
    if is_jpeg {
        // A crop and a turn are done as **two** `tjTransform` calls, not one with
        // both fields set. That is a measured decision, not a stylistic one, and
        // the measurement is in `crop_and_turn_take_two_calls_because_one_will_not_do`
        // in `tests/rotate_save.rs`:
        //
        //  * TurboJPEG reads the crop region in the transform's **destination**
        //    space, not the source. With `rot180` or a flip, a rectangle meant for
        //    the source lands mirrored in the output — a valid crop of the wrong
        //    part of the photo.
        //  * For the four axis-swapping operations it refuses outright:
        //    "Invalid crop request", because the destination has different
        //    dimensions from the source.
        //
        // So cropping first (with the identity op, where source and destination
        // are the same space and the coordinates are unambiguous) and turning the
        // cropped result second is the only formulation that is right for all
        // eight. Both calls rearrange DCT coefficients and neither decompresses
        // to pixels, so this is still lossless — the size ratio in
        // `bench_rotate_save` is the evidence, and it holds.
        if let Some(c) = crop {
            let mut t1 = turbojpeg::Transform::op(turbojpeg::TransformOp::None);
            // Lossless transforms move whole MCU blocks, so a dimension that is
            // not a multiple of the block size leaves partial blocks that cannot
            // reach the new edge. Left in place they show as strips; `trim`
            // discards them instead, costing at most one block (16 px) off an
            // edge. It is a no-op when the image is already aligned, so there is
            // nothing to give up in the common case.
            t1.trim = true;
            t1.crop = Some(turbojpeg::TransformCrop {
                x: c.x as usize,
                y: c.y as usize,
                width: Some(c.w as usize),
                height: Some(c.h as usize),
            });
            let cropped = turbojpeg::transform(&t1, &bytes)
                .map_err(|e| SaveError::Decode(format!("lossless crop failed: {e}")))?;

            if xform == Xform::Identity {
                out = cropped.to_vec();
            } else {
                let mut t2 = turbojpeg::Transform::op(xform.as_turbojpeg());
                t2.trim = true;
                out = turbojpeg::transform(&t2, &cropped)
                    .map_err(|e| SaveError::Decode(format!("lossless transform failed: {e}")))?
                    .to_vec();
            }
        } else if xform != Xform::Identity {
            let mut t = turbojpeg::Transform::op(xform.as_turbojpeg());
            t.trim = true;
            out = turbojpeg::transform(&t, &bytes)
                .map_err(|e| SaveError::Decode(format!("lossless transform failed: {e}")))?
                .to_vec();
        }
        // The pixels are the display orientation now, so the tag must not fight them.
        neutralize_exif_orientation(&mut out);
    } else if let Some(fmt) = lossless_raster(&bytes) {
        let img = image::load_from_memory(&bytes).map_err(|e| SaveError::Decode(format!("{e}")))?;
        let (w, h) = (img.width(), img.height());
        let rgba = img.to_rgba8();
        // Crop the *stored* pixels first, then turn them — the same order
        // TurboJPEG uses, and the one that makes the result the region the user
        // framed rather than the same region of the wrong picture.
        let (cw, ch, cropped) = match crop {
            Some(c) => c
                .apply_rgba(w, h, &rgba)
                .ok_or_else(|| SaveError::Decode("crop fell outside the image".into()))?,
            None => (w, h, rgba.into_raw()),
        };
        let (ow, oh, px) = xform
            .apply_rgba(cw, ch, &cropped)
            .ok_or_else(|| SaveError::Decode("buffer did not match its dimensions".into()))?;
        let out_img = image::RgbaImage::from_raw(ow, oh, px)
            .ok_or_else(|| SaveError::Decode("transformed buffer was malformed".into()))?;
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(out_img)
            .write_to(&mut std::io::Cursor::new(&mut buf), fmt)
            .map_err(|e| SaveError::Decode(format!("re-encoding failed: {e}")))?;
        out = buf;
    } else {
        return Err(SaveError::Lossy(format_name(&bytes).to_owned()));
    }

    let dest = unique_dest_named(src, if crop.is_some() { "cropped" } else { "rotated" });
    std::fs::write(&dest, &out).map_err(SaveError::Write)?;
    Ok(dest)
}

/// `photo.jpg` -> `photo_rotated.jpg`, or `photo_rotated2.jpg`, `...3` and so on
/// if that already exists. Never overwrites.
pub fn unique_dest(src: &Path) -> PathBuf {
    unique_dest_named(src, "rotated")
}

/// As [`unique_dest`], with the infix chosen by the caller.
///
/// `_rotated` or `_cropped`, depending on whether the save actually cropped. A
/// crop turns the picture too, so `_rotated` would be true and useless; `_cropped`
/// is the thing the user actually did, and the confirmation line names the
/// transform as well.
pub fn unique_dest_named(src: &Path, infix: &str) -> PathBuf {
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned());
    let ext = src.extension().map(|s| s.to_string_lossy().into_owned());
    let (Some(stem), Some(ext)) = (stem, ext) else {
        return numbered_sibling(src, parent);
    };
    for n in 1..1000u32 {
        let name = if n == 1 {
            format!("{stem}_{infix}.{ext}")
        } else {
            format!("{stem}_{infix}{n}.{ext}")
        };
        let cand = parent.join(name);
        if !cand.exists() {
            return cand;
        }
    }
    numbered_sibling(src, parent)
}

fn numbered_sibling(src: &Path, parent: &Path) -> PathBuf {
    let base = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
    for n in 1..1000u32 {
        let cand = parent.join(format!("{base}_rotated{n}.bin"));
        if !cand.exists() {
            return cand;
        }
    }
    parent.join(format!("{base}_rotated.bin"))
}

/// Rewrite a JPEG's EXIF after a lossless rotation: set the orientation tag to 1
/// and unlink the embedded thumbnail.
///
/// The pixels are now already in display orientation, so any orientation tag
/// left in place gets applied *on top* of them — the classic double-rotate,
/// and the sort of thing that is invisible in this viewer because it applies the
/// tag itself and so cancels out, and obvious everywhere else.
///
/// Two edits, both of which only overwrite bytes that already exist:
///  * the orientation tag's value becomes 1, and
///  * IFD0's next-IFD pointer is zeroed, so the thumbnail in IFD1 stops being
///    reachable.
///
/// Leaving IFD1 in place but unreferenced is deliberate. Removing it properly
/// means reserialising the whole APP1 segment and every offset inside it, and a
/// malformed EXIF block is worse than a thumbnail nothing points at. Conformant
/// readers follow the pointer, and both Explorer and the browsers do.
///
/// Returns what was changed, for the caller to report. `false` with no EXIF at
/// all is not a failure: a file with no orientation tag is already correct once
/// the pixels are rotated. The buffer is never grown or shrunk, so no offset
/// inside the segment can go stale — and every offset is bounds-checked against
/// the segment's end, exactly as the reader is, because these bytes come from
/// the file and are about to be handed to other programs.
pub fn neutralize_exif_orientation(jpeg: &mut [u8]) -> bool {
    /// Read a `u16` at `at`, inside `base..end`, in the given byte order.
    fn rd16(b: &[u8], at: usize, end: usize, le: bool) -> Option<u16> {
        if at.checked_add(2)? > end || end > b.len() {
            return None;
        }
        let v = [b[at], b[at + 1]];
        Some(if le { u16::from_le_bytes(v) } else { u16::from_be_bytes(v) })
    }
    fn wr16(b: &mut [u8], at: usize, v: u16, le: bool) {
        let v = if le { v.to_le_bytes() } else { v.to_be_bytes() };
        b[at] = v[0];
        b[at + 1] = v[1];
    }
    fn rd32(b: &[u8], at: usize, end: usize, le: bool) -> Option<u32> {
        if at.checked_add(4)? > end || end > b.len() {
            return None;
        }
        let v = [b[at], b[at + 1], b[at + 2], b[at + 3]];
        Some(if le { u32::from_le_bytes(v) } else { u32::from_be_bytes(v) })
    }
    fn wr32(b: &mut [u8], at: usize, v: u32, le: bool) {
        let v = if le { v.to_le_bytes() } else { v.to_be_bytes() };
        b[at..at + 4].copy_from_slice(&v);
    }

    let Some((base, end)) = exif_tiff_span(jpeg) else {
        return false;
    };
    if end > jpeg.len() || base + 4 > end {
        return false;
    }
    let le = match &jpeg[base..base + 2] {
        [0x49, 0x49] => true,
        [0x4D, 0x4D] => false,
        _ => return false,
    };

    let Some(off) = rd32(jpeg, base + 4, end, le) else {
        return false;
    };
    let ifd0 = match base.checked_add(off as usize) {
        Some(v) if v + 2 <= end => v,
        _ => return false,
    };
    let Some(count) = rd16(jpeg, ifd0, end, le) else {
        return false;
    };
    // Capped exactly as the reader caps it: a corrupt count must not make us
    // walk attacker-controlled offsets across the whole buffer.
    let count = usize::from(count.min(64));

    // Locate the orientation entry's value field, and the next-IFD pointer.
    let mut orientation_at = None;
    for i in 0..count {
        let Some(e) = ifd0.checked_add(2 + i * 12) else {
            break;
        };
        if e + 12 > end {
            break;
        }
        match rd16(jpeg, e, end, le) {
            Some(EXIF_TAG_ORIENTATION) => {
                orientation_at = Some(e + 8);
                break;
            }
            _ => {}
        }
    }
    // The next-IFD pointer sits after the entry table.
    let next_at = ifd0 + 2 + count * 12;

    let mut changed = false;
    if let Some(at) = orientation_at {
        if at + 2 <= end {
            wr16(jpeg, at, 1, le);
            changed = true;
        }
    }
    if next_at + 4 <= end {
        // Only bother if it actually pointed somewhere.
        if rd32(jpeg, next_at, end, le).is_some_and(|v| v != 0) {
            wr32(jpeg, next_at, 0, le);
            changed = true;
        }
    }
    changed
}

/// Tag 0x0112, the orientation.
const EXIF_TAG_ORIENTATION: u16 = 0x0112;

// ---------------------------------------------------------------------------
// Per-image transform
// ---------------------------------------------------------------------------

/// What the user has done to one image: a crop, a turn, mirrors. All of it.
///
/// Crop and transform live in **one** entry rather than two maps for a practical
/// reason: they are saved by the same action, and "Reset" should mean all of it.
/// Two maps means a state where the crop is cleared and the turn is not, or where
/// one of them is bounded and the other is not, and neither state is one anybody
/// asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edit {
    /// Quarter turns and mirrors, composed as they were pressed.
    pub xform: Xform,
    /// Rectangle in **stored** coordinates, already snapped to the block grid, or
    /// `None` for no crop.
    pub crop: Option<Crop>,
}

impl Edit {
    /// Is this edit a no-op? If so it should not be stored, and the save should
    /// behave exactly as it did before the edit existed.
    pub fn is_identity(&self) -> bool {
        self.xform == Xform::Identity && self.crop.is_none()
    }

    /// A short description for the status line and the save confirmation.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = &self.crop {
            parts.push(format!("cropped to {}", c.label()));
        }
        if self.xform != Xform::Identity {
            parts.push(self.xform.label().to_owned());
        }
        if parts.is_empty() {
            "no transform".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

/// The view transform and crop the user has applied to each image, this session.
///
/// **Per image, not per app**, which is the whole point. This used to be a single
/// `u8` on the app struct, so turning one photo turned every photo you then
/// navigated to, and navigating back showed the original at whatever rotation
/// happened to be current. No real viewer does that — Windows Photos, FastStone,
/// IrfanView and Preview all keep the edit per image and keep it when you return —
/// and for an app whose job is showing a picture faithfully it is worse than
/// merely wrong, because there is no way to tell "I rotated this" from "this is
/// broken".
///
/// The value is a whole [`Xform`] rather than a turn count, because a mirror is
/// the same *kind* of edit as a rotation and the two do not commute: turn a photo
/// then mirror it and you have asked for a diagonal mirror, which is neither
/// operation. Composing on the way in means this map only ever holds finished
/// transforms, so display, the lossless save and the tests all read one value and
/// there is no second place for the two halves to disagree.
///
/// Not persisted: this is a view transform until it is saved, and it is lost on
/// restart like every other view state.
#[derive(Debug, Clone)]
pub struct ImageEdits {
    map: std::collections::HashMap<std::path::PathBuf, Edit>,
    cap: usize,
}

impl Default for ImageEdits {
    fn default() -> Self {
        Self {
            map: std::collections::HashMap::new(),
            cap: 4096,
        }
    }
}

impl ImageEdits {
    /// The whole edit for `path`. [`Edit::default`] when untouched.
    pub fn get(&self, path: &Path) -> Edit {
        self.map.get(path).copied().unwrap_or_default()
    }

    /// Just the transform, which is what the draw path and most callers want.
    pub fn xform(&self, path: &Path) -> Xform {
        self.get(path).xform
    }

    /// Just the crop.
    pub fn crop(&self, path: &Path) -> Option<Crop> {
        self.get(path).crop
    }

    /// Store `path`'s edit. A no-op removes the entry rather than storing it, so
    /// an untouched image costs nothing.
    pub fn set(&mut self, path: &Path, edit: Edit) {
        if edit.is_identity() {
            self.map.remove(path);
        } else {
            if self.map.len() >= self.cap {
                // Wholesale rather than evicting one entry: the map is a record
                // of a session's view state, and a viewer left open on a very
                // large folder must not grow it without bound. Losing edits is a
                // far smaller problem than unbounded growth.
                self.map.clear();
            }
            self.map.insert(path.to_path_buf(), edit);
        }
    }

    /// Replace just the crop, leaving any transform alone.
    pub fn set_crop(&mut self, path: &Path, crop: Option<Crop>) {
        let mut e = self.get(path);
        e.crop = crop;
        self.set(path, e);
    }

    /// Drop the crop, leaving any transform alone.
    pub fn clear_crop(&mut self, path: &Path) {
        self.set_crop(path, None);
    }

    /// Compose `extra` onto whatever transform `path` already has, and store the
    /// result. Any crop is left untouched — turning a picture does not move the
    /// frame around it.
    ///
    /// `extra` goes *first*: the button you just pressed is the innermost edit, so
    /// pressing rotate-then-mirror really does mean "rotate, then mirror". That
    /// ordering is invisible in the final picture — both orders are valid pictures
    /// — but it is what makes the buttons feel like they do what they say, and it
    /// is why this is written as one function rather than three setters.
    fn compose(&mut self, path: &Path, extra: Xform) -> Xform {
        let mut e = self.get(path);
        e.xform = extra.then(e.xform);
        self.set(path, e);
        e.xform
    }

    /// `path` turned by `steps` quarter turns clockwise (negative = the other way).
    pub fn turn(&mut self, path: &Path, steps: i32) -> Xform {
        self.compose(path, Xform::quarters_cw(steps.rem_euclid(4) as u8))
    }

    /// `path` mirrored left-to-right.
    pub fn mirror_horizontal(&mut self, path: &Path) -> Xform {
        self.compose(path, Xform::FlipH)
    }

    /// `path` mirrored top-to-bottom.
    pub fn mirror_vertical(&mut self, path: &Path) -> Xform {
        self.compose(path, Xform::FlipV)
    }

    /// Forget everything about `path` — crop and transform both.
    pub fn reset(&mut self, path: &Path) {
        self.map.remove(path);
    }

    /// How many images currently carry an edit. For tests and diagnostics.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Colour profile detection (read-only, no transformation)
// ---------------------------------------------------------------------------
//
// This stage only *reports*. Nothing here touches a decode path, so it cannot
// change a pixel: the renderer already uploads textures as
// `Rgba8UnormSrgb` (egui-wgpu), so the bytes are expected to be sRGB-encoded
// and a future transform's only job is to make that true at decode time.

/// What can be said about a file's declared colour space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// No profile. For JPEG and PNG that means sRGB by convention, which is
    /// what the viewer already assumes.
    UntaggedSrgb,
    /// Carries an sRGB profile. Converting it to sRGB is a no-op.
    Srgb,
    /// A recognised non-sRGB RGB space we are **not** converting yet. These are
    /// the files that actually look wrong today.
    WideGamut,
    /// A profile we could not classify. Treated as "leave it alone".
    Unknown,
    /// Not an RGB space (CMYK, Lab, ...).
    NonRgb,
    /// The colour info lives somewhere this stage does not parse: an ISO-BMFF
    /// `colr` box (HEIC/AVIF) or inside a JXL codestream. Deliberately not
    /// guessed at — see [`IccInfo::not_inspected`].
    NotInspected,
}

impl ColorSpace {
    /// Short label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            ColorSpace::UntaggedSrgb => "sRGB (untagged)",
            ColorSpace::Srgb => "sRGB",
            ColorSpace::WideGamut => "wide gamut",
            ColorSpace::Unknown => "unknown profile",
            ColorSpace::NonRgb => "non-RGB",
            ColorSpace::NotInspected => "not inspected",
        }
    }
}

/// Everything the detector found out about one file's colour.
#[derive(Debug, Clone)]
pub struct ColorInfo {
    pub space: ColorSpace,
    /// Profile description when the file carried one ("sRGB IEC61966-2.1").
    pub description: String,
    /// Size of the embedded profile in bytes, 0 when there is none.
    pub profile_bytes: usize,
    /// Data colour space tag, e.g. "RGB " or "CMYK".
    pub data_space: String,
    /// The profile's red primary as D50-adapted XYZ, 15-bit fixed point.
    ///
    /// Reported because the *numbers* are the ground truth: a surprising number
    /// of real profiles carry no description at all, and colour space then has
    /// to be decided from the primaries. `None` when the profile is absent or
    /// carries no `rXYZ` tag.
    pub red_primaries: Option<[i32; 3]>,
}

impl ColorInfo {
    fn plain(space: ColorSpace) -> ColorInfo {
        ColorInfo {
            space,
            description: String::new(),
            profile_bytes: 0,
            data_space: String::new(),
            red_primaries: None,
        }
    }

    /// For containers whose colour info this stage does not read.
    pub fn not_inspected() -> ColorInfo {
        ColorInfo::plain(ColorSpace::NotInspected)
    }

    /// One-line summary for the UI.
    pub fn summary(&self) -> String {
        if self.description.is_empty() {
            self.space.label().to_string()
        } else {
            format!("{} — {}", self.space.label(), self.description)
        }
    }

    /// The line to show in the metadata panel, or `None` when it is not worth
    /// saying.
    ///
    /// An untagged or sRGB-profiled file is the overwhelmingly common case, and
    /// "Colour profile: sRGB" on every screenshot is noise. What earns a line is
    /// something the user might act on: a wide-gamut or non-RGB space, a profile
    /// we could not classify, a format whose colour we do not read at all, or
    /// any real embedded profile.
    pub fn panel_line(&self) -> Option<String> {
        let routine = matches!(self.space, ColorSpace::UntaggedSrgb | ColorSpace::Srgb)
            && self.profile_bytes == 0;
        if routine {
            return None;
        }
        Some(format!("Colour profile: {}", self.summary()))
    }
}

/// The red primary of the standard sRGB ICC profile, D50-adapted, as it appears
/// in the `rXYZ` tag. Measured from a real camera file; used only as a fallback
/// cross-check when a profile has no description.
const SRGB_D50_RED: (i32, i32, i32) = (28578, 14581, 912);

/// Display P3's red primary, D50-adapted. **Measured, not looked up**: 192 of
/// 1001 images in a real phone backup carry a profile whose `rXYZ` is exactly
/// this, against 111 at the sRGB value above. The two never mix, which is what
/// makes this usable as a classifier without trusting the description string.
const P3_D50_RED: (i32, i32, i32) = (33759, 15807, -69);

/// Concatenate a JPEG's APP2 `ICC_PROFILE` chunks. A profile over 64 KB is split
/// across chunks, so they have to be joined in sequence order.
fn jpeg_icc_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut i = 2usize; // SOI
    let mut out: Vec<u8> = Vec::new();
    let mut found = false;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if len < 2 || i + 2 + len > bytes.len() {
            return None;
        }
        // "ICC_PROFILE\0" is 12 bytes, then 1 byte sequence number and 1 byte
        // chunk count, so the profile data starts at i+18.
        if marker == 0xE2 && bytes.len() >= i + 18 && &bytes[i + 4..i + 16] == b"ICC_PROFILE\0" {
            out.extend_from_slice(&bytes[i + 18..i + 2 + len]);
            found = true;
        }
        if marker == 0xDA {
            break; // start of scan
        }
        i += 2 + len;
    }
    found.then_some(out)
}

/// PNG colour chunks: `sRGB` (intentional, sRGB), `iCCP` (a real profile).
/// Returns `(has_srgb_chunk, icc_profile)`.
fn png_colour_chunks(bytes: &[u8]) -> (bool, Option<Vec<u8>>) {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return (false, None);
    }
    let mut i = 8usize;
    let mut srgb = false;
    let mut iccp = None;
    while i + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        let ty = &bytes[i + 4..i + 8];
        let data = i + 8;
        let end = match data.checked_add(len) {
            Some(e) if e <= bytes.len() => e,
            _ => break, // truncated: stop trusting the chunk stream
        };
        match ty {
            b"sRGB" => srgb = true,
            b"iCCP" => {
                // keyword \0 compression-method \0 *zlib-compressed* profile.
                // The compression is the easy thing to forget: handing the raw
                // chunk to the ICC parser just yields "unknown", because there
                // is no 'acsp' signature at offset 36 in a deflate stream.
                if let Some(z) = bytes[data..end].iter().position(|b| *b == 0) {
                    if let Some(z2) = bytes[data + z + 1..end].iter().position(|b| *b == 0) {
                        let packed = &bytes[data + z + 1 + z2 + 1..end];
                        use std::io::Read;
                        let mut out = Vec::new();
                        let inflated = flate2::read::ZlibDecoder::new(packed)
                            // A profile is a few KB; refuse to inflate a bomb.
                            .take(1 << 20)
                            .read_to_end(&mut out)
                            .is_ok();
                        if inflated && out.len() > 132 {
                            iccp = Some(out);
                        }
                    }
                }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        i = end + 4; // skip CRC
    }
    (srgb, iccp)
}

fn icc_tag(b: &[u8], at: usize) -> String {
    String::from_utf8_lossy(&b[at..at + 4])
        .trim_end()
        .to_string()
}

/// The `desc` tag, which is ASCII in an ICC v2 profile and UTF-16BE `mluc` in
/// v4. Returns "" when absent or unparseable — the caller must cope.
fn icc_description(icc: &[u8]) -> String {
    icc_description_opt(icc).unwrap_or_default()
}

fn icc_description_opt(icc: &[u8]) -> Option<String> {
    let ntags = u32::from_be_bytes(icc.get(128..132)?.try_into().ok()?) as usize;
    for t in 0..ntags.min(256) {
        let e = 132 + t * 12;
        if e + 12 > icc.len() || &icc[e..e + 4] != b"desc" {
            continue;
        }
        let off = u32::from_be_bytes(icc[e + 4..e + 8].try_into().ok()?) as usize;
        if off + 12 > icc.len() {
            return None;
        }
        return match &icc[off..off + 4] {
            // v2: 4-byte length then ASCII.
            b"desc" => {
                let n = u32::from_be_bytes(icc[off + 8..off + 12].try_into().ok()?) as usize;
                let end = (off + 12 + n).min(icc.len());
                Some(
                    icc[off + 12..end]
                        .iter()
                        .take_while(|b| **b != 0)
                        .map(|b| *b as char)
                        .collect::<String>()
                        .trim()
                        .to_string(),
                )
            }
            // v4: 12-byte header, then 12-byte records of [lang, country, len, off].
            b"mluc" => {
                let recsize = u32::from_be_bytes(icc[off + 12..off + 16].try_into().ok()?) as usize;
                if recsize < 12 {
                    return None;
                }
                let r = off + 16;
                // Record count; only the first record is the name, so it is read
                // for validation and otherwise unused.
                let _n = u32::from_be_bytes(icc.get(r + 4..r + 8)?.try_into().ok()?) as usize;
                let str_off = r + 8;
                let len = u32::from_be_bytes(icc.get(str_off..str_off + 4)?.try_into().ok()?) as usize;
                let at = u32::from_be_bytes(icc.get(str_off + 4..str_off + 8)?.try_into().ok()?) as usize;
                let start = off.checked_add(at)?;
                let end = start.checked_add(len)?.min(icc.len());
                if start >= icc.len() || start % 2 != 0 {
                    return None;
                }
                let units: Vec<u16> = icc[start..end]
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16(&units)
                    .ok()
                    .map(|s| s.trim().to_string())
            }
            _ => None,
        };
    }
    None
}

/// The `rXYZ` tag, the giveaway for gamut.
fn icc_red_primaries(icc: &[u8]) -> Option<(i32, i32, i32)> {
    let ntags = u32::from_be_bytes(icc.get(128..132)?.try_into().ok()?) as usize;
    for t in 0..ntags.min(256) {
        let e = 132 + t * 12;
        if e + 12 > icc.len() || &icc[e..e + 4] != b"rXYZ" {
            continue;
        }
        let off = u32::from_be_bytes(icc[e + 4..e + 8].try_into().ok()?) as usize;
        if off + 20 > icc.len() {
            return None;
        }
        return Some((
            i32::from_be_bytes(icc[off + 8..off + 12].try_into().ok()?),
            i32::from_be_bytes(icc[off + 12..off + 16].try_into().ok()?),
            i32::from_be_bytes(icc[off + 16..off + 20].try_into().ok()?),
        ));
    }
    None
}

/// Is the tag table structurally trustworthy?
///
/// Every entry must lie entirely inside the profile, and the declared count
/// must actually fit. This is what stops a corrupt profile from producing a
/// *confident wrong answer*: mis-reading it as sRGB would skip a conversion the
/// image needs, and mis-reading it as wide gamut would push a correct image
/// through a P3 matrix and wreck it. Both failure modes are worse than saying
/// "unknown", so a profile that does not validate is refused outright.
fn icc_tags_valid(icc: &[u8]) -> bool {
    let Some(count) = icc.get(128..132) else {
        return false;
    };
    let ntags = u32::from_be_bytes([count[0], count[1], count[2], count[3]]) as usize;
    let Some(ntags_bytes) = ntags.checked_mul(12) else {
        return false;
    };
    let Some(table_end) = 132usize.checked_add(ntags_bytes) else {
        return false;
    };
    if table_end > icc.len() {
        return false;
    }
    for t in 0..ntags {
        let e = 132 + t * 12;
        let off = u32::from_be_bytes(icc[e + 4..e + 8].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(icc[e + 8..e + 12].try_into().unwrap()) as usize;
        let end = match off.checked_add(size) {
            Some(e) => e,
            None => return false,
        };
        if off < table_end || end > icc.len() {
            return false; // overlaps the table, or runs off the end
        }
    }
    true
}

/// Classify an ICC profile. Name-based where the description names a space we
/// know, with a primaries cross-check as a fallback; anything else is
/// [`ColorSpace::Unknown`] rather than a guess.
fn classify_icc(icc: &[u8]) -> ColorInfo {
    if icc.len() < 132 || &icc[36..40] != b"acsp" || !icc_tags_valid(icc) {
        return ColorInfo::plain(ColorSpace::Unknown);
    }
    let data_space = icc_tag(icc, 16);
    let red = icc_red_primaries(icc);
    let mut info = ColorInfo {
        space: ColorSpace::Unknown,
        description: icc_description(icc),
        profile_bytes: icc.len(),
        data_space: data_space.clone(),
        red_primaries: red.map(|r| [r.0, r.1, r.2]),
    };
    if !data_space.starts_with("RGB") {
        info.space = ColorSpace::NonRgb;
        return info;
    }
    // Tokenise on non-alphanumerics so "DCI-P3-D65-2_2-Gamma" yields `p3` and
    // "sRGB IEC61966-2.1" yields `srgb`, without matching a `p3` buried inside a
    // longer word.
    let d = info.description.to_ascii_lowercase();
    let tokens: Vec<&str> = d
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    let has = |w: &str| tokens.iter().any(|t| *t == w);
    info.space = if has("prophoto") {
        ColorSpace::WideGamut
    } else if has("p3") || has("adobergb") || has("2020") {
        // Display P3 / DCI-P3, AdobeRGB, Rec.2020 and BT.2020: all matrix or
        // wide-transfer spaces, all of which need a transform to sRGB.
        ColorSpace::WideGamut
    } else if has("srgb") {
        ColorSpace::Srgb
    } else {
        // No usable name. Fall back to comparing the red primary with sRGB's.
        match red {
            Some(r) => {
                if near3(r, SRGB_D50_RED) {
                    ColorSpace::Srgb
                } else if near3(r, P3_D50_RED) {
                    ColorSpace::WideGamut
                } else {
                    // A matrix/TRC RGB profile that matches neither is, by
                    // elimination, some other wide space.
                    ColorSpace::WideGamut
                }
            }
            None => ColorSpace::Unknown,
        }
    };
    info
}

/// True when two 15-bit XYZ triples agree to within 1%.
fn near3(a: (i32, i32, i32), b: (i32, i32, i32)) -> bool {
    let near = |x: i32, y: i32| (x - y).abs() * 100 / y.abs().max(1) < 1;
    near(a.0, b.0) && near(a.1, b.1) && near(a.2, b.2)
}

/// What a file declares about its colour space. Never fails: an unreadable or
/// unrecognised file comes back as [`ColorSpace::Unknown`].
///
/// Formats whose colour info is not in a JPEG APP2 / PNG chunk — HEIC, AVIF and
/// JXL — are reported as [`ColorSpace::NotInspected`] rather than assumed to be
/// sRGB, because assuming is how wide-gamut HEIC photos end up looking flat.
pub fn color_info_from_bytes(bytes: &[u8], is_jpeg: bool, is_png: bool) -> ColorInfo {
    if is_jpeg {
        return match jpeg_icc_profile(bytes) {
            Some(p) => classify_icc(&p),
            None => ColorInfo::plain(ColorSpace::UntaggedSrgb),
        };
    }
    if is_png {
        let (has_srgb, iccp) = png_colour_chunks(bytes);
        if let Some(p) = iccp {
            return classify_icc(&p);
        }
        // No profile: an `sRGB` chunk states sRGB outright; without one, PNG
        // with no colour info is conventionally sRGB too.
        let _ = has_srgb;
        return ColorInfo::plain(ColorSpace::UntaggedSrgb);
    }
    ColorInfo::not_inspected()
}

/// [`color_info_from_bytes`] for a file on disk.
pub fn color_info(path: &Path) -> ColorInfo {
    let Ok(bytes) = std::fs::read(path) else {
        return ColorInfo::plain(ColorSpace::Unknown);
    };
    let (is_jpeg, is_png) = (is_jpeg_bytes(&bytes), bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    if !is_jpeg && !is_png {
        return ColorInfo::not_inspected();
    }
    color_info_from_bytes(&bytes, is_jpeg, is_png)
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
pub fn exif_orientation(path: &Path) -> u16 {
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
    /// The file's EXIF orientation tag, 0 when there is none.
    ///
    /// The decode has *already* applied it to the pixels, so nothing needs this
    /// to draw. It travels with the result for the crop UI, which has to express
    /// a rectangle the user dragged on the **displayed** picture back in
    /// **stored** coordinates, and that is a composition involving the tag.
    pub exif_tag: u16,
    /// The block grid `(w, h)` a lossless JPEG crop origin must align to, and
    /// `(1, 1)` for every format with no block structure.
    ///
    /// 8 px for 4:4:4, 16 px for the usual 4:2:0. The crop UI snaps the frame it
    /// draws to this, so that what is on screen is what the save cuts — see
    /// [`Crop::snapped_to_mcu`].
    pub crop_grid: (u32, u32),
}


/// True if the bytes look like a GIF (magic bytes).
pub fn is_gif_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF8")
}

/// True if the bytes look like a QOI image (magic bytes).
///
/// `"qoif"` at offset 0, followed by a big-endian width and height.
pub fn is_qoi_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 14 && bytes.starts_with(b"qoif")
}

/// True if the bytes look like a Windows bitmap (magic bytes).
///
/// `BM` plus a plausible 14-byte file header. There is no other format that
/// starts `BM`, so the length check is only there to keep a two-byte file from
/// being accepted.
pub fn is_bmp_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 26 && bytes.starts_with(b"BM")
}

/// True if the bytes look like a Windows icon or cursor (magic bytes).
///
/// The container is shared: reserved word 0, type 1 (icon) or 2 (cursor), then
/// a count. Both `.ico` and `.cur` are the same format, and both hold a mixture
/// of PNG-compressed and BMP-compressed entries — which the decoder handles, so
/// nothing here looks inside.
pub fn is_ico_bytes(bytes: &[u8]) -> bool {
    // Layout: reserved u16 (0), type u16 (1 = icon, 2 = cursor), count u16.
    bytes.len() >= 6
        && bytes[0..2] == [0, 0]
        && matches!(bytes[2], 1 | 2)
        && u16::from_le_bytes([bytes[4], bytes[5]]) != 0
}

/// True if the bytes look like a TIFF (magic bytes).
///
/// Both byte orders, and both the classic and BigTIFF headers: `II*\0` /
/// `MM\0*` (42) and `II+\0` / `MM\0+` (43). Only the 42 forms are common, but
/// a BigTIFF header is just as unambiguous and rejecting it would mean a large
/// file failing for no reason.
pub fn is_tiff_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    let order = &bytes[0..2];
    let le = order == [0x49, 0x49];
    let be = order == [0x4D, 0x4D];
    if !le && !be {
        return false;
    }
    let tag = if le {
        u16::from_le_bytes([bytes[2], bytes[3]])
    } else {
        u16::from_be_bytes([bytes[2], bytes[3]])
    };
    tag == 42 || tag == 43
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
    // The block grid a lossless JPEG crop must align to. Read here, from the
    // bytes already in hand, rather than re-reading the file later: a crop is
    // snapped to this, and the UI needs it on the worker thread where the read
    // is free. `(1, 1)` for anything that is not a JPEG, so callers never have to
    // special-case a non-JPEG.
    let crop_grid: (u32, u32) = if jpeg {
        turbojpeg::Decompressor::new()
            .ok()
            .and_then(|mut d| d.read_header(&bytes).ok())
            .map(|h| (h.subsamp.mcu_width() as u32, h.subsamp.mcu_height() as u32))
            .unwrap_or((1, 1))
    } else {
        (1, 1)
    };
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
                    exif_tag: tag,
                    crop_grid,
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
                exif_tag: tag,
                crop_grid,
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
                exif_tag: tag,
                crop_grid,
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
        exif_tag: tag,
        crop_grid,
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
    /// Whether advancing past either end wraps around to the other end.
    ///
    /// Defaults to on (the historical behaviour, and what old configs get). It
    /// lives here rather than in the caller so `next`/`prev` cannot be used in a
    /// way that quietly ignores the setting.
    pub loop_back: bool,
}

impl Gallery {
    pub fn new(folder: &Path) -> Self {
        Self {
            files: scan_folder(folder),
            index: 0,
            loop_back: true,
        }
    }

    /// Create a gallery containing a single file.
    pub fn from_file(path: PathBuf) -> Self {
        Self {
            files: vec![path],
            index: 0,
            loop_back: true,
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

    /// Advance one image. Returns whether the view actually moved — `false` at
    /// the end with `loop_back` off, so the caller can skip the work that a
    /// real change would justify.
    pub fn next(&mut self) -> bool {
        if self.files.is_empty() {
            return false;
        }
        match self.index.checked_add(1) {
            Some(i) if i < self.files.len() => {
                self.index = i;
                true
            }
            _ if self.loop_back => {
                self.index = 0;
                true
            }
            _ => false,
        }
    }

    /// Go back one image. Returns whether the view actually moved.
    pub fn prev(&mut self) -> bool {
        if self.files.is_empty() {
            return false;
        }
        match self.index.checked_sub(1) {
            Some(i) => {
                self.index = i;
                true
            }
            None if self.loop_back => {
                self.index = self.files.len() - 1;
                true
            }
            None => false,
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


