//! Core logic for the image viewer, kept separate from the GUI so it can be
//! unit-tested, benchmarked, and fuzz-tested.

pub mod perf;

/// The application icon, drawn in code rather than committed as a binary.
/// See the module docs for why it lives here and not in `examples/`.
pub mod icon;

/// Star ratings, pick flags and tags, stored as XMP sidecars.
pub mod ratings;
pub use ratings::{Rating, Ratings};

/// The sidecar path for an image, or `None` inside an archive.
///
/// Appends `.xmp` (`photo.png` becomes `photo.png.xmp`) so files sharing a
/// stem do not share a sidecar.
///
/// Lives here rather than in `ratings` so the archive refusal — the rule the
/// whole ratings UI depends on — sits beside `is_archive_path` and is pinned
/// by a scan against this file. Re-exported through `ratings` so the
/// `image_viewer::ratings::sidecar_path` path keeps working.
pub fn sidecar_path(image: &Path) -> Option<PathBuf> {
    if is_archive_path(image) {
        return None;
    }
    let mut s = image.as_os_str().to_os_string();
    s.push(".xmp");
    Some(PathBuf::from(s))
}

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

/// The shape a crop frame is held to while it is being dragged.
///
/// A curated list rather than an arbitrary ratio, because this is a dropdown in
/// the settings panel and the ratios people actually want are the ones a camera
/// or a phone shoots at. `#[serde(rename_all = "lowercase")]` so the config
/// spelling matches the labels — the same trap `WgpuBackend` fell into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CropAspect {
    /// Whatever the drag makes. The original behaviour, and the default: a crop
    /// that locks a shape before anybody asked for one is a worse default than
    /// one that does not.
    #[default]
    Free,
    /// 1:1.
    Square,
    /// 16:9 — a phone held sideways.
    Widescreen,
    /// 9:16 — a phone held upright.
    Portrait,
    /// 4:3.
    Classic,
    /// 3:4.
    TallClassic,
    /// 3:2 — a 35 mm frame.
    Photo,
    /// 2:3.
    TallPhoto,
    /// The shape of the image as it is displayed, so the frame starts at the
    /// edges of the picture whatever its orientation or any turn applied to it.
    Source,
}

impl CropAspect {
    /// Every option, in the order the dropdown lists them.
    pub const ALL: [CropAspect; 9] = [
        CropAspect::Free,
        CropAspect::Square,
        CropAspect::Widescreen,
        CropAspect::Portrait,
        CropAspect::Classic,
        CropAspect::TallClassic,
        CropAspect::Photo,
        CropAspect::TallPhoto,
        CropAspect::Source,
    ];

    /// What the dropdown says. The number is in the label because "Square" and
    /// "1:1" are not obviously the same intent to somebody reaching for either.
    pub fn label(self) -> &'static str {
        match self {
            CropAspect::Free => "Free",
            CropAspect::Square => "Square 1:1",
            CropAspect::Widescreen => "Widescreen 16:9",
            CropAspect::Portrait => "Portrait 9:16",
            CropAspect::Classic => "Classic 4:3",
            CropAspect::TallClassic => "Tall 3:4",
            CropAspect::Photo => "Photo 3:2",
            CropAspect::TallPhoto => "Tall photo 2:3",
            CropAspect::Source => "Same as image",
        }
    }

    /// The ratio to hold a frame to, as width over height, or `None` for free.
    ///
    /// `display` is the image's **displayed** size in its own pixels — after the
    /// EXIF orientation and any turn — because the frame is dragged on what is on
    /// screen. `Source` therefore follows the picture as it appears, which is the
    /// only version of "the same shape" that means anything to the person
    /// dragging: a 3:2 photo turned on its side is 2:3 on screen, and a frame
    /// that insisted on 3:2 would not fit it.
    pub fn ratio(self, display: (u32, u32)) -> Option<f32> {
        let (w, h) = (display.0 as f32, display.1 as f32);
        let (nw, nh) = match self {
            CropAspect::Free => return None,
            CropAspect::Square => (1.0, 1.0),
            CropAspect::Widescreen => (16.0, 9.0),
            CropAspect::Portrait => (9.0, 16.0),
            CropAspect::Classic => (4.0, 3.0),
            CropAspect::TallClassic => (3.0, 4.0),
            CropAspect::Photo => (3.0, 2.0),
            CropAspect::TallPhoto => (2.0, 3.0),
            CropAspect::Source => {
                if w <= 0.0 || h <= 0.0 {
                    return None;
                }
                (w, h)
            }
        };
        Some(nw / nh)
    }
}

/// Hold a dragged crop frame to `aspect` (width over height), anchored at the
/// press point.
///
/// Returns `(x0, y0, x1, y1)` in the same space it was given. The axis the
/// pointer leads is kept and the **other** one is the one that gives — drag
/// right and down for a wide frame and the height follows the width. That is the
/// convention every editor uses; letting both axes chase the pointer cannot be
/// hit reliably with a mouse.
///
/// `None` is a no-op, and so is a frame with no width or no height: a rectangle
/// with nothing to it has no aspect to hold, and the crop refuses it downstream
/// rather than the frame jumping the instant the drag cleared egui's threshold.
///
/// The result is not clamped to the image here. The caller does that, and a frame
/// anchored near an edge will come out slightly the wrong shape for the same
/// reason a saved crop comes out up to one MCU block bigger than the drag: the
/// rectangle that actually gets cut is the one inside the picture.
pub fn constrain_to_aspect(
    anchor: (f32, f32),
    corner: (f32, f32),
    aspect: Option<f32>,
) -> (f32, f32, f32, f32) {
    let (ax, ay) = anchor;
    let (mut cx, mut cy) = corner;
    let free = || (ax.min(cx), ay.min(cy), ax.max(cx), ay.max(cy));
    let Some(r) = aspect.filter(|r| r.is_finite() && *r > 0.0) else {
        return free();
    };
    // No special case for a flat drag. It looks like it needs one — `dx / dy` is
    // a division by zero — but every branch lands on the same answer: a zero on
    // one side makes the other side zero too, so the frame collapses to nothing
    // and the crop refuses it. A guard here would be a branch that cannot change
    // an output, which is worse than no branch, because it reads as protection
    // that is not there.
    let (mut dx, mut dy) = ((cx - ax).abs(), (cy - ay).abs());
    if dx / dy > r {
        dx = dy * r;
    } else {
        dy = dx / r;
    }
    // Keep the direction the pointer went: dragging up and left has to frame the
    // same rectangle as dragging down and right, or the same crop needs two
    // different drags depending on which corner you started from.
    cx = if corner.0 < anchor.0 { ax - dx } else { ax + dx };
    cy = if corner.1 < anchor.1 { ay - dy } else { ay + dy };
    (ax.min(cx), ay.min(cy), ax.max(cx), ay.max(cy))
}

/// Configuration settings for the image viewer.
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Whether to load all images from the current folder on startup.
    pub load_all_on_startup: bool,

    /// Optional folder to load images from on startup.
    pub folder: Option<String>,

    /// Gallery sort order.
    #[serde(default)]
    pub sort: SortMode,

    /// Reverse each sort order (Z→A, oldest/smallest first). Off by default,
    /// which is exactly the shipped behaviour — turning it on reverses
    /// whichever order `sort` produced.
    ///
    /// `#[serde(default)]` for the reason every added field here carries it: a
    /// config written before this key existed still has to load, and it must
    /// keep the orders its reader already has.
    #[serde(default)]
    pub sort_descending: bool,

    /// Resize previews in linear light with a CatmullRom filter (better
    /// edges/moiré on lineart) instead of fast gamma-space Triangle.
    #[serde(default = "default_true")]
    pub linear_resample: bool,

    /// Downsampling filter for the linear-light path.
    #[serde(default)]
    pub downscale_filter: DownscaleFilter,

    /// Low-pass a downscaled page harder than the Nyquist minimum, so a fine
    /// pattern cannot survive as a square sparkle.
    ///
    /// **This is the strength axis, and the filter *shape* dropdown is not.** A
    /// 2223x3109 page at the manga half-panel target reduces 2.879x, and the
    /// content at the source Nyquist comes through as a hard 1-2 px lattice.
    /// Measured over the three shapes, on screentone: Hermite had the lowest
    /// residual and Lanczos3 the highest at 1.93x the cost, so shape has no
    /// useful dynamic range for this artefact - only the kernel's *width* does,
    /// and there was no axis for it until this field. Both are kept: shape was
    /// never measured on photographic edges, and removing a shipped setting is
    /// a worse change than adding one.
    ///
    /// **Off by default, and that is the measured answer rather than a shrug.**
    /// It shipped default-**on** and was a regression on real files. On six
    /// 2223x3109 pages at the manga half-panel target, judging rendered output:
    /// strength 2.0 was better on `32.jpg`, better on `30.jpg`, unchanged on
    /// `52.jpg`, and **worse on `12.jpg`, `21.jpg` and `44.jpg`** - all three
    /// reported as a black dot pattern appearing in grey screentone. Strength
    /// 1.0, which is bit-identical to the path before the strength axis existed,
    /// was better than `master` on **every** file.
    ///
    /// **Why the sweep that chose 2.0 concluded the opposite.** It measured a
    /// uniform synthetic square lattice, where more strength is better at *every*
    /// pitch. That is a real result and the lattice guards rest on it. Real
    /// screentone is not that: it is a base dot grid plus at least one second
    /// screen at an angle, often with stochastic variation over the top, so it
    /// carries energy at several pitches at once, and widening a low-pass can
    /// suppress one component's beat while reinforcing another's. A single-pitch
    /// probe cannot see a trade that is defined across pitches, which is exactly
    /// how it read as a monotone win. The trade looks inherent, so this stays a
    /// setting instead of becoming the default: a page that wants it can have it.
    ///
    /// **What the improvement on every file actually came from:** the earlier,
    /// unconditional kernel prefilter (`cb0394c`), not this field. `Fast` is
    /// already better than `master` everywhere, so nothing here is load-bearing
    /// for the common case.
    ///
    /// Unlike `linear_resample`, this one **does** invalidate the previews on
    /// the same frame it flips (the filter dropdown's `refilter`). A decode-time
    /// setting that leaves the cached previews alone reads as a broken toggle:
    /// the box moves, the picture does not change.
    #[serde(default)]
    pub smooth_downscale: bool,

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

    /// Browse subfolders as one gallery: every navigation, sort mode and the
    /// grid see the whole tree. Off by default — a tree walk on every 2 s
    /// watcher poll costs a directory walk per level, where a flat scan
    /// costs one, so this is opt-in rather than a slowdown everyone pays.
    #[serde(default)]
    pub recursive_folders: bool,

    /// List `.zip`/`.cbz`/`.7z`/`.rar` files in a folder scan, beside the images.
    ///
    /// **Defaulted to true**, and that direction matters: the feature shipped on,
    /// so a config written before this key existed has to keep listing archives.
    /// The surprising direction to be wrong in is the silent one — a folder of
    /// manga that quietly stopped showing its volumes.
    #[serde(default = "default_true")]
    pub show_archives: bool,

    /// Esc on a bare image viewer closes the app. On by default (the shipped
    /// behaviour); off means Esc stops at leaving crop mode / grid /
    /// fullscreen and never quits. The filter box keeps its own Esc
    /// (clear, then defocus) either way.
    #[serde(default = "default_true")]
    pub esc_closes_app: bool,

    /// Which wgpu backend to render with, and the last probe result.
    #[serde(default)]
    pub renderer: RendererPref,

    /// Convert Display P3 pixels to sRGB as they are decoded.
    ///
    /// **Off by default, and that is a decision rather than a shrug.** The
    /// pipeline is 8-bit sRGB end to end, so a wide-gamut photo does display
    /// without conversion — and converting it fixes that on an sRGB display while
    /// being a downgrade on a wide-gamut one, with out-of-gamut colours clipped at
    /// both ends. So the default is "leave the pixels alone and say so in the
    /// metadata panel", and the conversion is opt-in.
    ///
    /// Like `linear_resample`, this is applied at decode time and does not
    /// re-decode what is already cached.
    #[serde(default)]
    pub convert_wide_gamut: bool,

    /// Shape a crop frame is held to while it is dragged. `Free` so an old
    /// config — and a new install — behaves exactly as it did before this
    /// existed.
    #[serde(default)]
    pub crop_aspect: CropAspect,

    /// Keep decoded grid thumbnails in `%LOCALAPPDATA%\image-viewer\thumbs`.
    ///
    /// On by default because a grid cell's cost is a full decode of the file and
    /// a large scan has no cheaper decode — 700-800 ms for a 95 MP baseline JPEG,
    /// measured — so a folder you have opened before is the only case that can be
    /// made fast. Off means nothing is written outside the config folder and every
    /// cell is decoded again, every time.
    #[serde(default = "default_true")]
    pub thumbnail_cache: bool,

    /// Side of one gallery-grid thumbnail cell, in points. 150pt is the shipped
    /// size; larger cells judge photos better on high-DPI displays, smaller
    /// ones fit more per row. Read through [`grid_cols`] and [`grid_row_h`],
    /// which derive everything else from it, so there is one number and not
    /// a cell size plus a row pitch that can disagree.
    ///
    /// `#[serde(default)]` is not enough here — the default must be 150, not
    /// 0, and 0 would mean zero-width cells. `default_grid_cell`.
    #[serde(default = "default_grid_cell")]
    pub grid_cell: f32,

    /// Only allow one instance of the viewer to run. When on (the default),
    /// a second launch tries to raise the first and exits; when off, multiple
    /// windows can open (each with its own archive temp dir, which is why the
    /// archive watcher disables its watcher rather than try to coordinate them).
    #[serde(default = "default_true")]
    pub single_instance: bool,

    /// Last viewed image per folder, so reopening a folder resumes there.
    #[serde(default)]
    pub last_by_folder: std::collections::HashMap<String, String>,

    /// Show images as **turns** of one or two pages instead of one picture at a time.
    ///
    /// **Off by default**, and opt-in because single-image browsing is the app's
    /// original behaviour: every plan it makes is pinned by
    /// `tests/flip_through.rs`, so a turn has to be something the reader asks for
    /// rather than a mode the setting file turns on by accident.
    ///
    /// `#[serde(default)]` for the reason every added field here carries it: a
    /// config written before this key existed still has to load.
    #[serde(default)]
    pub manga_mode: bool,

    /// Pages meet **right to left** in manga mode: a turn's first page is on the
    /// right, the second on its left.
    ///
    /// **On by default** (`default_true`), which is the opposite of every other
    /// boolean here and the only field whose absence is answered `true`. It is
    /// right because this field is only read while [`Config::manga_mode`] is on,
    /// and someone who has asked for a manga reader has asked for the one thing
    /// that makes it a manga reader: the order the pages meet in. It is not the
    /// app's historical behaviour being changed, because with the mode off nothing
    /// consults it.
    ///
    /// `#[serde(default = "default_true")]` therefore means both halves at once —
    /// an old config loads *and* gets the direction a manga library reads in —
    /// which is the pair `a_config_written_before_manga_mode_still_loads` pins.
    /// Writing `#[serde(default)]` here would compile, pass every other test in
    /// the suite, and hand every reader who never found the setting a
    /// left-to-right one.
    #[serde(default = "default_true")]
    pub manga_rtl: bool,

    /// Worker threads for the decode pool. `0` means auto: available
    /// parallelism clamped to 2..=4, which is the shipped behaviour.
    /// Anything else is honoured, clamped to 1..=8.
    ///
    /// `#[serde(default)]` for the reason every added field here carries it: a
    /// config written before this key existed still has to load, and it must
    /// come up on auto rather than on a fixed count.
    #[serde(default)]
    pub workers: usize,

    /// Full key bindings: action id → chord (`"Ctrl+Shift+S"`, `"Space"`).
    /// Absent (or blank) means the shipped default from [`KEY_ACTIONS`].
    #[serde(default)]
    pub keybindings: std::collections::HashMap<String, String>,
    /// User commands: external programs run against the current file
    /// (`{file}`, `{dir}`, `{name}` expand in program and args).
    /// Triggered by Ctrl+1..9 in list order.
    #[serde(default)]
    pub user_commands: Vec<UserCommand>,

    /// Seconds per image while the slideshow runs. The show itself is session
    /// state (off on launch); only the pace persists.
    ///
    /// `#[serde(default)]` is not enough here — the default must be 3, not 0,
    /// and 0 would mean "advance every frame". `default_slideshow_secs`.
    #[serde(default = "default_slideshow_secs")]
    pub slideshow_secs: u32,

    /// Walk the folder in a fresh random order while the slideshow runs.
    /// Session-only like the show itself: the order is reshuffled every time
    /// a show starts and the folder's own order comes back when it ends, so
    /// this changes nothing outside a running show.
    ///
    /// Off by default: a shuffled show surprises anyone who did not ask for
    /// one.
    #[serde(default)]
    pub slideshow_shuffle: bool,

    /// Animation playback speed multiplier (1.0 = file timing). Read through
    /// [`anim_frame_state`], which clamps.
    #[serde(default = "default_anim_speed")]
    pub anim_speed: f32,

    /// Zoom ratios on keys `1`–`4`, as source-pixel ratios (1.0 is 100%).
    /// Read through [`zoom_preset`], which clamps.
    #[serde(default = "default_zoom_presets")]
    pub zoom_presets: [f32; 4],

    /// Keep the window above all others. Applied live; the launch loop
    /// reads the same flag, so a restart keeps it.
    #[serde(default)]
    pub always_on_top: bool,

    /// Hide the title bar and borders. Applied live; the launch loop reads
    /// the same flag, so a restart keeps it. Without a drag handle the
    /// window is moved with Win+arrows instead — toggle back to drag it.
    #[serde(default)]
    pub hide_title_bar: bool,

    /// Fit window to image as a mode (`T`): while on, every newly shown
    /// image gets the 100%-or-fit treatment and the window follows user
    /// zooms, centered each time. JPEGView's sticky rule, including its
    /// toggle shape (on fits now, off leaves the window where it is).
    #[serde(default)]
    pub fit_window_mode: bool,

    /// Fit-view as a sticky mode (`F`): every image fitted to the window.
    /// Mutually exclusive with fit-window mode — turning one on switches
    /// the other off. Off by default: with no mode on, photos open at 100%
    /// in a stable window instead of maximized.
    #[serde(default)]
    pub fit_mode: bool,
}

/// `1` keeps the shipped meaning (100%), then doubling to 8×.
fn default_zoom_presets() -> [f32; 4] {
    [1.0, 2.0, 4.0, 8.0]
}

/// Three seconds: JPEGView's middle preset, and slow enough to actually see
/// the picture.
fn default_slideshow_secs() -> u32 {
    3
}

/// File timing.
fn default_anim_speed() -> f32 {
    1.0
}

/// One external program entry for "Open with…" style scripting.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct UserCommand {
    /// Shown in Settings and in the notice on run.
    #[serde(default)]
    pub name: String,
    /// Program to run; `{file}` etc. expand here too.
    #[serde(default)]
    pub program: String,
    /// Arguments; each entry expands `{file}`, `{dir}`, `{name}`.
    #[serde(default)]
    pub args: Vec<String>,
}

/// Every rebindable action: id, shipped chord, and the Settings label.
/// Chords spell modifiers `Ctrl`/`Shift`/`Alt` joined by `+` with an egui
/// `Key` variant name last (`"Ctrl+Shift+S"`, `"Space"`, `"Delete"`).
/// Matching is exact (see `chord_matches` in the binary): a bare letter
/// never fires with a modifier held, and a chord never fires with extra
/// ones — so adding modifiers is always safe, never ambiguous.
///
/// Deliberately NOT here: `Esc` (typing/grid/fullscreen/quit chain — a
/// structural dispatch, not a binding) and mouse buttons (a different input
/// shape; still hardcoded). `Copy`/`Copy path` keep their toolkit chords
/// (`Ctrl+C` arrives as an event, never as a key) with these as alternatives.
pub const KEY_ACTIONS: &[(&str, &str, &str)] = &[
    ("next", "ArrowRight", "Next image"),
    ("prev", "ArrowLeft", "Previous image"),
    ("next_alt", "Space", "Next image (alt)"),
    ("page_next", "PageDown", "Next image (page)"),
    ("page_prev", "PageUp", "Previous image (page)"),
    ("first", "Home", "First image"),
    ("last", "End", "Last image"),
    ("grid", "G", "Toggle gallery grid"),
    ("panels", "I", "Toggle all panels"),
    ("pane_left", "L", "Toggle left pane"),
    ("pane_right", "P", "Toggle right pane"),
    ("fit", "F", "Fit view mode"),
    ("fill", "Shift+F", "Fill window"),
    ("preset_1", "1", "Zoom preset 1"),
    ("preset_2", "2", "Zoom preset 2"),
    ("preset_3", "3", "Zoom preset 3"),
    ("preset_4", "4", "Zoom preset 4"),
    ("anim_back", "Comma", "Animation one frame back"),
    ("anim_fwd", "Period", "Animation one frame forward"),
    ("anim_toggle", "K", "Pause/resume animation"),
    ("manga", "M", "Double page mode"),
    ("rtl", "D", "Right-to-left pages"),
    ("lead_down", "O", "Lead-lone offset down"),
    ("lead_up", "Shift+O", "Lead-lone offset up"),
    ("shape", "X", "Toggle single/double page"),
    ("del", "Delete", "Delete to Recycle Bin"),
    ("explore", "E", "Reveal in Explorer"),
    ("reload", "R", "Reload folder"),
    ("wall", "W", "Set as wallpaper"),
    ("wall_shown", "Shift+W", "Set displayed image as wallpaper"),
    ("save", "Ctrl+S", "Save transformed copy"),
    ("save_screen", "Ctrl+Shift+S", "Save screen-size copy"),
    ("save_as", "Ctrl+Shift+A", "Save as (choose file, quality, size)"),
("crop", "C", "Crop mode"),
    ("rating_up", "Plus", "Rating up"),
    ("rating_down", "Minus", "Rating down"),
    ("flag", "B", "Flag / unflag"),
    ("undo", "Ctrl+Z", "Undo view transform"),
    ("redo", "Ctrl+Shift+Z", "Redo view transform"),
    ("copy", "Ctrl+C", "Copy image (alt chord)"),
    ("copy_path", "Ctrl+Shift+C", "Copy path (alt chord)"),
    ("print", "Ctrl+P", "Print"),
    ("user_1", "Ctrl+1", "User command 1"),
    ("user_2", "Ctrl+2", "User command 2"),
    ("user_3", "Ctrl+3", "User command 3"),
    ("user_4", "Ctrl+4", "User command 4"),
    ("user_5", "Ctrl+5", "User command 5"),
    ("user_6", "Ctrl+6", "User command 6"),
    ("user_7", "Ctrl+7", "User command 7"),
    ("user_8", "Ctrl+8", "User command 8"),
    ("user_9", "Ctrl+9", "User command 9"),
    ("grid_up", "ArrowUp", "Grid: up"),
    ("grid_down", "ArrowDown", "Grid: down"),
    ("grid_left", "ArrowLeft", "Grid: left"),
    ("grid_right", "ArrowRight", "Grid: right"),
    ("grid_right_alt", "Space", "Grid: right (alt)"),
    ("grid_page_up", "PageUp", "Grid: page up"),
    ("grid_page_down", "PageDown", "Grid: page down"),
    ("grid_open", "Enter", "Grid: open"),
    ("fullscreen", "F11", "Fullscreen"),
    ("focus_search", "Slash", "Focus filter box"),
    ("fit_window", "T", "Fit window mode"),
];



/// Every key name the matcher understands: the spelling capture stores
/// and settings shows. The binary must map each of these both ways
/// (pinned by scan); a name missing either way silently falls back to the
/// shipped default.
pub const KEY_NAMES: &[&str] = &[
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q",
    "R", "S", "T", "U", "V", "W", "X", "Y", "Z",
    "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
    "Num0", "Num1", "Num2", "Num3", "Num4", "Num5", "Num6", "Num7", "Num8", "Num9",
    "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    "ArrowRight", "ArrowLeft", "ArrowUp", "ArrowDown",
    "Space", "Enter", "Escape", "Tab", "Backspace", "Delete", "Insert",
    "Home", "End", "PageUp", "PageDown",
    "Comma", "Period", "Slash", "Semicolon", "Quote",
    "OpenBracket", "CloseBracket", "Backslash", "Backtick",
    "Minus", "Equals", "Plus",
];

/// Shipped chord for `action`, or `""` when unknown.
pub fn default_binding(action: &str) -> &'static str {
    KEY_ACTIONS
        .iter()
        .find(|(id, _, _)| *id == action)
        .map(|(_, chord, _)| *chord)
        .unwrap_or("")
}

/// Effective chord for `action`: the user's override when present and
/// non-blank, else the shipped default. A blank override unbinds nothing —
/// it falls back, so a typo leaves the shipped chord working rather than
/// deleting the action.
pub fn resolve_binding(bindings: &std::collections::HashMap<String, String>, action: &str) -> String {
    match bindings.get(action).map(|s| s.trim()) {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => default_binding(action).to_owned(),
    }
}

/// Canonicalize a chord for storing: modifiers sorted Ctrl/Shift/Alt, case
/// normalized on the key (`"shift+ctrl+s"` and `"Ctrl+Shift+S"` are the same
/// binding). Returns `None` when the chord names no key at all.
pub fn canonical_chord(raw: &str) -> Option<String> {
    let (ctrl, shift, alt, key) = split_chord(raw)?;
    let mut out = String::new();
    if ctrl {
        out.push_str("Ctrl+");
    }
    if shift {
        out.push_str("Shift+");
    }
    if alt {
        out.push_str("Alt+");
    }
    out.push_str(&key);
    Some(out)
}

/// Split a chord into (ctrl, shift, alt, key name): the pure string half of
/// binding matching. The binary maps the name to `egui::Key` (including the
/// `"1"`→`Num1` digit mapping, which lives there because only the binary
/// knows the key type).
///
/// Single letters normalize to uppercase (`s` means `S`); longer names are
/// case-sensitive and must match the variant spelling exactly (`Space`, not
/// `space`) — normalizing those would invent spellings the matcher does not
/// know. Returns `None` when no key is named.
pub fn split_chord(raw: &str) -> Option<(bool, bool, bool, String)> {
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut key: Option<String> = None;
    for part in raw.split('+') {
        let p = part.trim();
        if p.eq_ignore_ascii_case("ctrl") || p.eq_ignore_ascii_case("control") {
            ctrl = true;
        } else if p.eq_ignore_ascii_case("shift") {
            shift = true;
        } else if p.eq_ignore_ascii_case("alt") {
            alt = true;
        } else if !p.is_empty() {
            key = Some(p.to_owned());
        }
    }
    let key = key?;
    Some((
        ctrl,
        shift,
        alt,
        if key.len() == 1 {
            key.to_ascii_uppercase()
        } else {
            key
        },
    ))
}

/// Expand `{file}`, `{dir}` and `{name}` in `template` against `file`.
/// Unknown placeholders are left alone rather than deleted, so a literal
/// `{x}` in a program path survives instead of silently vanishing.
pub fn expand_template(template: &str, file: &std::path::Path) -> String {
    let s = template.replace("{file}", &file.to_string_lossy().into_owned());
    let dir = file
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let s = s.replace("{dir}", &dir);
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    s.replace("{name}", &name)
}

/// Expand a [`UserCommand`] against `file`: program plus one arg per entry.
///
/// Empty args default to the file itself — nearly every "open with" command
/// just wants the picture, and a bare `mspaint.exe` opening a blank canvas
/// is never what was meant. A command that truly wants no arguments is
/// vanishingly rare, and extra argv entries are ignored by almost everything.
pub fn expand_user_command(cmd: &UserCommand, file: &std::path::Path) -> (String, Vec<String>) {
    let args = if cmd.args.is_empty() {
        vec![file.to_string_lossy().into_owned()]
    } else {
        cmd.args.iter().map(|a| expand_template(a, file)).collect()
    };
    (expand_template(&cmd.program, file), args)
}

/// Parse an EXIF `DateTimeOriginal` into a `SystemTime`. Accepts both the
/// wire form (`"2024:03:17 14:34:56`) and the normalized form libraries
/// hand out (`"2024-03-17 14:34:56` — nom-exif parses the tag into a
/// `NaiveDateTime` whose `Display` uses dashes, so requiring colons rejects
/// every real file). Anything else is `None` rather than a guessed date.
///
/// No timezone: the tag carries none, so the stamp is read as UTC. That can
/// disagree with the wall clock by the local offset; ordering photos is
/// unaffected, which is what touching dates is for.
pub fn parse_exif_datetime(s: &str) -> Option<std::time::SystemTime> {
    let (date, time) = s.split_once(' ')?;
    // Colons on the wire, dashes after parsing — accept either.
    let sep = if date.contains('-') { '-' } else { ':' };
    let mut d = date.split(sep);
    let (y, mo, day): (i64, u64, u64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    if d.next().is_some() {
        return None;
    }
    let mut t = time.split(':');
    let (h, mi, sec): (u64, u64, u64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    if t.next().is_some() {
        return None;
    }
    if !(1..=12).contains(&mo) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let max_day = match mo {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > max_day {
        return None;
    }
    // Days from civil date (Hinnant's algorithm), then seconds.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (mo as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(
        (days * 86400 + h as i64 * 3600 + mi as i64 * 60 + sec as i64).max(0) as u64,
    ))
}

/// Set a file's modification time. Needs a write handle — `File::open` is
/// read-only and fails here, which is a confusing permission error for a
/// feature about dates.
pub fn touch_file_mtime(path: &std::path::Path, to: std::time::SystemTime) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)?
        .set_modified(to)
}

/// Slideshow pacing: true once `secs` have passed since `last`.
///
/// `secs` floors at 1 — a zero interval would advance every frame, which is
/// a strobe, not a slideshow. Saturating, so a clock jump backwards reads as
/// "not yet" rather than panicking.
pub fn slideshow_due(last: std::time::Instant, now: std::time::Instant, secs: u32) -> bool {
    now.saturating_duration_since(last) >= std::time::Duration::from_secs(secs.max(1) as u64)
}

fn default_true() -> bool {
    true
}

/// The shipped grid cell size, and the value an old config without the key
/// comes up on.
fn default_grid_cell() -> f32 {
    GRID_CELL
}

/// Portable mode: an empty `portable.txt` beside the executable keeps
/// everything the app would otherwise scatter — config, thumbnails, spreads
/// store, wallpaper copy — in that same folder. No interface beyond the
/// file's existence (create it to opt in, delete it to roam again), and an
/// existing `%APPDATA%` config does *not* migrate: portable starts fresh by
/// construction, not by copying secrets around.
///
/// `portable_dir_from` takes the directory so tests can point it at a
/// `tempfile` instead of the real executable's.
pub fn portable_dir_from(exe_dir: &Path) -> Option<PathBuf> {
    if exe_dir.join("portable.txt").is_file() {
        Some(exe_dir.to_path_buf())
    } else {
        None
    }
}

pub fn portable_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.to_path_buf()))
        .and_then(|d| portable_dir_from(&d))
}

/// Drop a leading UTF-8 **BOM** from text that is about to be parsed as JSON.
///
/// **Every JSON file this app reads can arrive with one, and without this every
/// setting in it is silently lost.** `Set-Content -Encoding utf8` in Windows
/// PowerShell writes `EF BB BF` before the first byte; several editors do the same
/// under a "UTF-8 with signature" or "keep BOM" option, and a file round-tripped
/// through one of them is byte-for-byte valid JSON apart from those three bytes.
/// `serde_json` does not skip them - it reports "expected value at line 1 column 1",
/// which is not a message anybody can act on - so both readers here take their
/// documented failure path instead:
///
/// * `Config::load` returns the **defaults**, so every setting reverts with no
///   warning: the folder is forgotten, every toggle is where it started, and the
///   file on disk still looks fine.
/// * `load_spread_overrides` returns an empty store, and - worse -
///   `save_spread_overrides` is a **read-modify-write against a file it could not
///   read**, so the next override press replaces every *other* gallery's entry with
///   nothing. That is silent, unrecoverable and has no second copy anywhere.
///
/// So this is one function called from all three sites rather than a
/// `trim_start_matches` at each: they are three places that already share a failure
/// mode, and the reason they share it is invisible from any one of them.
///
/// **Only a leading BOM, and only exactly one.** `trim_start_matches` would eat any
/// number of them and would be indifferent to where they are, which is not a
/// difference worth having: the bytes are legal anywhere else in a JSON document
/// (inside a string, as a character in a path) and are only a problem in front of the
/// first token. U+FEFF as a character *code point* is not stripped either, because
/// `fs::read_to_string` has already decoded `EF BB BF` into U+FEFF and this is the
/// one place that byte sequence can appear.
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

impl Config {
    /// Load configuration from a JSON file.
    pub fn load(path: &Path) -> Self {
        let default = Self {
            load_all_on_startup: true,
            folder: None,
            sort: SortMode::Name,
            sort_descending: false,
            linear_resample: true,
            downscale_filter: DownscaleFilter::Hermite,
            smooth_downscale: false,
            block_while_loading: false,
            wheel_navigation: false,
            loop_back: true,
            recursive_folders: false,
            show_archives: true,
            esc_closes_app: true,
            renderer: RendererPref::default(),
            convert_wide_gamut: false,
            crop_aspect: CropAspect::default(),
            thumbnail_cache: true,
            grid_cell: default_grid_cell(),
            single_instance: true,
            last_by_folder: std::collections::HashMap::new(),
            manga_mode: false,
            manga_rtl: true,
            workers: 0,
            keybindings: std::collections::HashMap::new(),
            user_commands: Vec::new(),
            slideshow_secs: default_slideshow_secs(),
            slideshow_shuffle: false,
            anim_speed: default_anim_speed(),
            zoom_presets: default_zoom_presets(),
            always_on_top: false,
            hide_title_bar: false,
            fit_window_mode: false,
            fit_mode: false,
        };

        let Ok(data) = std::fs::read_to_string(path) else {
            return default;
        };

        match serde_json::from_str::<Self>(strip_bom(&data)) {
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
        if let Some(dir) = crate::portable_dir() {
            return dir.join("config.json");
        }
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

// Single-instance guard: a named kernel mutex. The name is session-local
// (`Local\`, which any user can always create — `Global\` needs a privilege
// a normal login does not have) and stable across runs so a second launch
// finds the first one.
extern "system" {
    fn CreateMutexW(
        attr: *const std::ffi::c_void,
        initially_owned: i32,
        name: *const u16,
    ) -> usize;
    fn GetLastError() -> u32;
    fn ShellExecuteW(
        hwnd: usize,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> isize;
    fn EnumWindows(callback: Option<unsafe extern "system" fn(isize, isize) -> i32>, param: isize) -> i32;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    fn GetCurrentProcessId() -> u32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
}

/// Callback for [`set_topmost`]: stash the first top-level window owned by
/// our process and stop enumerating (return 0). One window is all eframe
/// makes here; no visibility filtering, because filtering on the wrong
/// predicate would skip the very window this exists to find.
unsafe extern "system" fn find_own_window(hwnd: isize, out: isize) -> i32 {
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == GetCurrentProcessId() {
        *(out as *mut isize) = hwnd;
        0
    } else {
        1
    }
}

/// Keep our window above all others, live (egui 0.36 has a builder for this
/// but no viewport command, so this goes straight to Win32 instead of
/// waiting for a restart). Finds the window by owning PID rather than title:
/// the title names the current file and changes constantly. Returns whether
/// it took effect.
pub fn set_topmost(on: bool) -> bool {
    const HWND_TOPMOST: isize = -1;
    const HWND_NOTOPMOST: isize = -2;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOSIZE: u32 = 0x0001;
    unsafe {
        let mut found: isize = 0;
        EnumWindows(Some(find_own_window), &mut found as *mut isize as isize);
        if found == 0 {
            return false;
        }
        SetWindowPos(
            found,
            if on { HWND_TOPMOST } else { HWND_NOTOPMOST },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE,
        ) != 0
    }
}

const ERROR_ALREADY_EXISTS: u32 = 183;

/// Handles this process created and still holds, so the mutex outlives the
/// call that claimed it. A handle dropped here would still leak (never
/// closed), which also holds it — but an explicit store says so instead of
/// relying on a leak nobody can see.
static HELD_INSTANCE_MUTEXES: std::sync::LazyLock<std::sync::Mutex<Vec<usize>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Claim the single-instance mutex `name`.
///
/// Returns `true` when this launch may proceed: created fresh (and now held
/// for the life of the process), or the mutex could not be created at all —
/// a guard that refuses to start the app is worse than no guard, which is
/// exactly what shipped once: `\\.\`-prefixed names are device paths, not
/// object names, so creation failed on *every* launch and the `m == 0`
/// branch read it as "already running". Returns `false` only when the mutex
/// already exists, i.e. another instance holds it.
pub fn claim_single_instance(name: &str) -> bool {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let m = CreateMutexW(std::ptr::null(), 1, wide.as_ptr());
        if m == 0 {
            eprintln!(
                "note: could not create the single-instance mutex ({}); starting anyway",
                std::io::Error::last_os_error()
            );
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return false;
        }
        HELD_INSTANCE_MUTEXES.lock().unwrap().push(m);
        true
    }
}

/// Print `path` through the handler Windows registered for it (the Photos
/// print view for an image): the `print` verb, no program of our own.
///
/// A return of 32 or less is the documented failure: out of memory, file not
/// found, no association, and the rest of the `SE_ERR_` table. There is no
/// handle to wait on — the handler owns the dialog from here.
pub fn print_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    let verb: Vec<u16> = "print\0".encode_utf16().collect();
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SW_SHOWNORMAL: the handler decides the window, we only ask normally.
    let rc = unsafe { ShellExecuteW(0, verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1) };
    if rc > 32 {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the print verb failed for {} (code {rc})", path.display()),
        ))
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

/// Whether archives are listed by a folder scan. Defaults to on.
///
/// A process-wide setting rather than an argument, for the same reason
/// [`thumbnail_cache_enabled`] is one: `is_browsable` is asked from the flat
/// scan, the recursive walk and `Gallery::new`, and threading a flag through all
/// three (and through every test that builds a gallery) would be a second thing
/// to forget rather than a smaller API.
static SHOW_ARCHIVES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Set whether a folder scan lists archives. See [`show_archives_enabled`].
pub fn set_show_archives(on: bool) {
    SHOW_ARCHIVES.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Are archives listed by a folder scan?
///
/// Public so a test can save and restore it: it is process-wide, and a test that
/// changed it without restoring would silently decide the answer for every test
/// that ran after it.
pub fn show_archives_enabled() -> bool {
    SHOW_ARCHIVES.load(std::sync::atomic::Ordering::Relaxed)
}

/// True if a folder scan should list this file: an image the app can decode, or
/// an archive it can open and [`show_archives_enabled`] says to list.
///
/// **Archives are listed, and are deliberately not in
/// [`SUPPORTED_EXTENSIONS`].** That constant gates *decoding* - it is also what
/// [`extract_zip`] uses to decide which members to write out, so a `.zip` in
/// it would make an archive both a browsable item and something the decoders
/// are handed. Listing and decoding are different questions, and this
/// predicate is the listing one.
///
/// One predicate rather than an extension list per scan: the flat scan, the
/// recursive walk and `Gallery::new` all ask here, so a setting that hides
/// archives cannot be honoured by one of them and missed by the others.
///
/// **An archive is only browsable if it contains at least one image.** This
/// check reads the archive header (central directory for zip/7z, file headers
/// for RAR) but does not extract anything. An empty archive or one with no
/// supported images is not listed at all — so pressing right on the previous
/// image skips it entirely rather than opening an empty gallery.
pub fn is_browsable(p: &Path) -> bool {
    if is_archive_path(p) {
        return show_archives_enabled() && archive_image_count(p).map(|c| c > 0).unwrap_or(false);
    }
    let ext = p
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    SUPPORTED_EXTENSIONS.contains(&ext.as_str())
}

/// Scan a directory for images and image archives, sorted alphabetically.
pub fn scan_folder(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && is_browsable(&p) {
                files.push(p);
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

/// Walk the whole subtree for images, pre-order (each folder's files in
/// natural order, then its subfolders in natural order).
///
/// Symlinked files and directories are skipped, not followed: following a
/// directory link can cycle back into its own parents and walk forever, and
/// a linked *file* would show one picture under two paths. `DirEntry::
/// file_type` is the symlink-aware query (unlike `Path::is_file`, which
/// follows links), and it comes free with the listing.
pub fn scan_folder_recursive(folder: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by(|a, b| {
            natural_cmp(
                &a.file_name().to_string_lossy(),
                &b.file_name().to_string_lossy(),
            )
        });
        for entry in entries {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let p = entry.path();
            if ft.is_file() {
                if is_browsable(&p) {
                    out.push(p);
                }
            } else if ft.is_dir() {
                walk(&p, out);
            }
        }
    }

    let mut files = Vec::new();
    walk(folder, &mut files);
    // Full paths, so folders group; forward slashes so `\` never sorts
    // before or after a digit differently than `/` would.
    files.sort_by(|a, b| {
        natural_cmp(
            &a.to_string_lossy().replace('\\', "/"),
            &b.to_string_lossy().replace('\\', "/"),
        )
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

/// Side of one gallery-grid (G) thumbnail. The shipped size and the default
/// every config without the key comes up on; the live value is
/// [`Config::grid_cell`], which this only feeds.
pub const GRID_CELL: f32 = 150.0;
/// Label strip reserved under each grid thumbnail for the file name.
pub const GRID_LABEL_H: f32 = 20.0;
/// Vertical pitch of one grid row for a cell size: cell + label + inter-row
/// padding. A function rather than a constant so there is one cell size in
/// the app and not a size plus a pitch that can disagree.
pub fn grid_row_h(cell: f32) -> f32 {
    cell + GRID_LABEL_H + 8.0
}

/// How many grid cells fit across a display area `panel_w` points wide.
/// Cells are laid out with egui's default 8pt item spacing, hence the +8.
pub fn grid_cols(panel_w: f32, cell: f32) -> usize {
    ((panel_w / (cell + 8.0)).floor() as usize).clamp(1, 24)
}

/// Where the grid's scroll offset should go when the selection moves, or
/// `None` for "leave it alone" — the file-picker rule.
///
/// A visible selection never moves the view; a selection outside it scrolls
/// the minimum distance to become fully visible (row bottom to view bottom
/// when below, row top to view top when above). Edges count as visible, so a
/// row exactly filling the bottom edge does not jitter the view by a pixel —
/// without that, every Down press at the edge would scroll and scroll back.
///
/// `view_h <= 0` means the viewport was never measured (the grid just
/// opened), so there is no visible range to test against: pin the row to the
/// top, which is also what opening on a deep selection has always done.
pub fn grid_follow_offset(
    sel_row: usize,
    pitch: f32,
    view_h: f32,
    cur_offset: f32,
) -> Option<f32> {
    let row_top = sel_row as f32 * pitch;
    if view_h <= 0.0 {
        return Some(row_top);
    }
    let row_bottom = row_top + pitch;
    if row_bottom > cur_offset + view_h {
        Some((row_bottom - view_h).max(0.0))
    } else if row_top < cur_offset {
        Some(row_top.max(0.0))
    } else {
        None
    }
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
/// GL leads because it is measurably the cheaper backend on this machine.
///
/// The order was DX12, then GL, on the reasonable grounds that DX12 is the native
/// Windows backend. Then it was measured, with `examples/bare_eframe.rs` as the
/// control — an eframe window and a label, no viewer code at all, so the number is
/// the renderer's and not ours:
///
/// | | working set | private |
/// |---|---|---|
/// | bare eframe 0.36, DX12 | 321 MB | 289 MB |
/// | bare eframe 0.36, GL | **84 MB** | **21 MB** |
/// | this viewer, DX12 | 343 MB | 312 MB |
/// | this viewer, GL | **189 MB** | 90 MB |
/// | Oculante 0.9.2 (glow, not wgpu) | 118 MB | 78 MB |
///
/// So 237 MB of the footprint is eframe's, and all of it is the backend. Adapter
/// plus device creation alone measures 455 ms on DX12 against 186 ms on GL.
///
/// **Frame rate is equal, which is what this reversal was missing.** It could not
/// be measured for two commits — frame rate over RDP is meaningless, and this
/// project has drawn two wrong conclusions from that before — and DX12 might well
/// have been faster. It is not: on a console session, interleaved A/B, four 20 s
/// rounds on an animation, **76.8 fps on both**, interval p50 16.6 ms on both, GL
/// 0.03% slower. Same speed, 154 MB less, 269 ms cheaper to create. See
/// `tests/bench_framerate.ps1`, which refuses to run over RDP.
///
/// **DX12 is second**, kept because it goes through a different driver stack and so
/// is the one most likely to work where GL does not — which is the entire reason
/// the other two still exist at all.
///
/// Vulkan is last *because it is the one that crashes*: on the development machine
/// wgpu access-violates during adapter enumeration 3 times out of 3 even though
/// `vulkaninfo` reports a healthy device. That is survivable here only because each
/// probe runs in its own process — see [`pick_probed`].
pub const PROBE_ORDER: [WgpuBackend; 3] =
    [WgpuBackend::Gl, WgpuBackend::Dx12, WgpuBackend::Vulkan];

/// The backend to use, given which probes survived.
///
/// `results` is `(backend, survived)` in probe order. The first survivor wins.
/// If nothing survived, GL — the same reasoning as [`PROBE_ORDER`]: it is the
/// cheapest to create and the lightest, so when the probes were themselves
/// untrustworthy the least-bad answer is the one that costs least if it is wrong.
///
/// This is a pure function of the probe results, which is the only reason it can
/// be tested — the probing itself cannot, because a failure is a crash.
pub fn pick_probed(results: &[(WgpuBackend, bool)]) -> WgpuBackend {
    for &want in &PROBE_ORDER {
        if results.iter().any(|(b, ok)| *b == want && *ok) {
            return want;
        }
    }
    WgpuBackend::Gl
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
        //
        // **These are not `1 << n` in order of this enum, and the numbering is
        // wgpu's, not ours.** `Backends` is a bitflag over `wgpu::Backend`, which
        // in wgpu 30 is `Noop=0, Vulkan=1, Metal=2, Dx12=3, Gl=4,
        // BrowserWebGpu=5` — so `Gl` is bit 4, not bit 1. The old values were
        // written from an older wgpu and left as bare literals, and
        // `exactly_one_backend_bit_is_ever_set` asserted those same literals
        // rather than wgpu's, so the pair agreed with each other while both were
        // wrong and `cargo run` panicked on the `debug_assert_eq!` in
        // `wgpu_options`. `tests/renderer.rs` now compares against
        // `Backends::VULKAN/GL/DX12` directly; do not hand-transcribe these.
        WgpuBackend::Auto | WgpuBackend::Dx12 => 1 << 3, // DX12
        WgpuBackend::Gl => 1 << 4,                        // GL
        WgpuBackend::Vulkan => 1 << 1,                   // VULKAN
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

/// One navigation input, **named** rather than answered.
///
/// The point of the type is that there is nowhere else to put the question. A caller
/// cannot say "is rightwards forwards?" - it can only say which input happened and
/// ask [`nav_is_forward`], so every navigation key in the app is handed to the same
/// function and a new key is a new line that has to name itself.
///
/// `Right`/`Left` are the arrow keys, and they are the only two variants that carry
/// spatial meaning. `Next`/`Previous` are the inputs with a *sequence* but no side -
/// `Space`, `PageDown`/`PageUp` and the mouse's forward/back buttons - and
/// `WheelDown` is neither: see [`nav_is_forward`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NavInput {
    /// The right arrow.
    Right,
    /// The left arrow.
    Left,
    /// Something that means "onward" without saying which way: `Space`,
    /// `PageDown`, the mouse's forward button.
    Next,
    /// The same, backwards.
    Previous,
    /// A wheel scrolling down.
    WheelDown,
}

/// Whether a navigation input means *forwards* in reading order.
///
/// **This function is the whole of the manga key mapping, and there is deliberately
/// nowhere else for it to live.** OpenComic inverts the navigation keys for
/// right-to-left manga and its `mangaInvert` set omits `Space`, so **Space silently
/// becomes "previous page"** there: nothing errors, the arrows are right, and the
/// space bar quietly goes the wrong way. Every input therefore names itself as a
/// [`NavInput`] and comes here, so the set of inputs that can disagree is the set of
/// variants of one enum.
///
/// # The trade-off, stated rather than discovered later
///
/// With `manga_rtl` on, `Right` is *backwards* in a book that opens to the left, so
/// `→` means "next" in single-image mode and "backwards" in a manga spread. **That is
/// the price of matching the screen**, and it is what Mihon, Komga and Kavita all
/// ship: the reader's hand moves the way the page turns, rather than having to
/// remember that this one app is reversed. A setting that made `→` mean "next" in
/// both modes would be the alternative, and it is worse - it would make the arrows
/// lie about which page is on screen.
///
/// # Why the wheel is the exception
///
/// `WheelDown` is **always** forwards, under both settings, because it is the one
/// input with no spatial meaning to preserve. An arrow has a *side*, and a book that
/// opens to the left has to move the hand with it; a wheel delta has only a sign, and
/// a sign already means "onward" in both directions. Mihon excludes it for the same
/// reason. Inverting it would mean scrolling *up* to go forwards in manga mode, which
/// is not a reading convention anywhere.
pub fn nav_is_forward(input: NavInput, rtl: bool) -> bool {
    match input {
        // Direction-agnostic in both modes, for the reason in the docs.
        NavInput::WheelDown => true,
        // Rightward on screen: forwards in a left-to-right book, backwards in a
        // right-to-left one.
        NavInput::Right | NavInput::Next => !rtl,
        NavInput::Left | NavInput::Previous => rtl,
    }
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

/// Scale factor that makes an image **cover** an `area`, keeping the aspect
/// ratio and cropping the overflow rather than letterboxing.
///
/// The sibling of [`fit_scale_no_upscale`] with the comparison flipped: the
/// larger ratio wins, and upscale is allowed (a smaller image covering a
/// larger area is upscaled — that is what "fill" means). Same units, same
/// degenerate-input guard, same `(1e-4, 8.0)` clamp the single-image view
/// fits itself at.
pub fn fill_scale(natural: (f32, f32), area: (f32, f32)) -> f32 {
    let (w, h) = (natural.0.max(1.0), natural.1.max(1.0));
    if w <= 0.0 || h <= 0.0 || area.0 <= 0.0 || area.1 <= 0.0 {
        return 1.0;
    }
    (area.0 / w).max(area.1 / h).clamp(1e-4, 8.0)
}

// ---------------------------------------------------------------------------
// Manga spreads: laying one turn out on screen
// ---------------------------------------------------------------------------
//
// Beside `fit_scale_no_upscale` rather than beside the pairing walk further down,
// because this is that function's sibling and its own claim about where pages go:
// the same `(panel.width() / w).min(panel.height() / h)` fit, with the panel's
// **half** width in place of its width, and the same `clamp(1e-4, 8.0)` the
// single-image view fits itself at. Plain floats on purpose — `Rect4` and no egui,
// so a whole turn's layout is a thing a test can check without a window.
//
// Three decisions. Only two of them are quiet — the shared scale is loud, because it
// shows the moment the two halves differ in shape:
//
// * **One shared scale.** Not a fit per page; see `spread_fit`. `pairable` pairs
//   pages whose aspect ratios are merely *close* (`PAIR_ASPECT_TOLERANCE`) rather
//   than equal, so a turn of two differently shaped pages is ordinary, not exotic.
// * **The seam is the panel's horizontal centre, always.** Zero gutter, so two
//   facing pages touch and a drawing across the seam is unbroken; and no special
//   case for a lone *portrait* page, because a page with nothing on the far side
//   of the seam *is* the lone-page case. Getting this wrong shows as a picture a
//   reader only notices pages later.
// * **Input order out, screen order never.** See `spread_rects` — a swap that is
//   wrong in one reading direction and right in the other.
//
// **A lone spread is the exception, and it is the exception to the *rule about lone
// pages*, not to the seam rule.** A page wider than [`SPREAD_ASPECT`] is a scan of two
// printed pages ([`shape_of`]) and takes a whole turn to itself *because* it is the
// whole spread — so hanging it off a seam with nothing on the far side draws half a
// spread at half size, in half the window. It gets the full width instead, which is
// the single-image layout, and it is still **centred on the seam**: the seam stays
// where it is, the page simply straddles it. **Four functions carry the decision, not
// two** — [`spread_fit`] and [`spread_rects`] because they *are* the layout, and
// [`spread_centre`] and [`spread_pan_bounds`] because they derive their numbers from
// those same rects, where a half-width box is a wrong zoom anchor and a pan bound that
// drags the page off the panel. Each takes the decision as `lone_spread_fills_turn`
// rather than measuring the page, because the caller's answer is the one that is right —
// see [`turn_is_a_lone_spread`] for why that is the shape and not the geometry.

/// Whether a turn of `page_count` pages, whose cursor page is judged `shape`, is
/// **one page that is itself a spread** and so fills the whole turn.
///
/// The discriminator is [`Shape`], and it has to be, because the geometry lies in both
/// directions:
///
/// * **A user override outranks the pixels.** `X` forces a page to `Double` whatever
///   it looks like, and "treat this as a spread" has to mean *give it the whole turn* —
///   which is the thing the key promises. A `w > h` test would decline to honour a
///   forced *portrait* page, so the key would re-pair the book and then draw the
///   result at half size: the override half working, silently.
/// * **A near-square page is not a spread.** 1.05 is wider than tall and is a single
///   page (`SPREAD_ASPECT` is 1.2, and above it not at it), so a `w > h` test would
///   hand a lone square-ish page the entire screen instead of the half a cover keeps.
///
/// And the whole point of asking the *caller* is that the layout cannot disagree with
/// the pairing: a turn holds one page either because that page is a spread or because
/// it is front matter ([`LEAD_LONE_DEFAULT`]), those are the only two ways, and they
/// are laid out differently. The pairing already made this exact decision — a `Double`
/// never pairs (`pairable`), so a lone `Double` **is** a lone spread by construction
/// rather than by inference.
pub fn turn_is_a_lone_spread(page_count: usize, shape: Shape) -> bool {
    page_count == 1 && shape == Shape::Double
}

/// The one answer the app needs about the turn under the cursor, from the position
/// alone: **is that turn a lone spread**, so the whole turn is one picture?
///
/// It exists because the app needs this *twice*, in two places that cannot be allowed
/// to disagree — to decide what to **decode** for ([`decode_panel_px`]) and to decide
/// how to **lay out** ([`spread_fit`] and the three functions that follow it). Asking
/// it once is the fix for a picture that was drawn at one size and decoded at another,
/// and the caller cannot get it wrong by accident: the page count comes from
/// [`turn_len`] and the shape from [`shape_of`], so neither `spread_dims.len()` nor
/// `shown_partner.is_none()` is available to be substituted for the rule.
///
/// **Both halves are the same rule as the layout's.** `turn_len` is what
/// `turn_partner` is built from, so the count and the partner cannot diverge; `shape_of`
/// is the shape the fit is judged by. That equivalence is why this can be asked at the
/// top of a frame, before the pages exist, and be the same answer the layout reaches
/// later: nothing here reads the *paint*, so nothing here can be a frame out of date
/// with it.
///
/// `None` for no position at all — nothing on screen yet — answers **false**, which
/// halves the panel exactly as the unfixed code did. A turn cannot be a lone spread
/// before its first frame exists, so this is the conservative answer and not a
/// special case.
pub fn lone_spread_fills_turn(pos: Option<PagePos>, lead_lone: u32, ctx: &dyn PageCtx) -> bool {
    pos.is_some_and(|pos| {
        turn_is_a_lone_spread(turn_len(pos, lead_lone, ctx), shape_of(pos, ctx))
    })
}

/// The scale a whole turn is drawn at: the more constrained fit of every page
/// present into *its half* of the panel, so every page fits at one shared factor.
///
/// **"Its half", except for a lone spread**, which is fitted against the panel's whole
/// width — `lone_spread_fills_turn`, the caller's [`turn_is_a_lone_spread`]. That is
/// not a second fit rule bolted on: it is the same single-image fit, on the one turn
/// that is one picture rather than two, and both pages of a two-page turn get exactly
/// the number they got before.
///
/// A `min` over the pages rather than a fit per page, and that is the load-bearing
/// part. The reference reader does the same thing — `webgpuviewer`'s `ImagePage.kt`
/// takes a `minOf` over its pages while its own comment describes the fit as
/// happening "independently" — and **the code is the honest reading of the two**,
/// because two halves drawn at two scales cannot share one zoom level: there is one
/// `scale` and one `view`, so the constrained page sets the factor and the other one
/// keeps a margin under it. Fitting each page independently would mean fitting each
/// page independently on *paper* and nowhere else.
///
/// It asks for no [`PageCtx`] and knows nothing about pairing: a turn is a list of
/// page sizes and that is the whole of what a layout needs to know.
///
/// Unlike [`fit_scale_no_upscale`] this **may exceed 1.0**, as the single-image
/// view's own fit does: a page is drawn to fill the area it is given. The clamp is the
/// same `(1e-4, 8.0)` that view uses.
///
/// A panel with no area, or a turn with no pages, answers **1.0**, exactly as
/// [`fit_scale_no_upscale`] does for its own degenerate cases. Not a `NaN` guard —
/// `w.max(1.0)` is what stops a division by zero here — but a *value* guard: without
/// the early return the clamp answers `1e-4` for an empty panel, which draws a page a
/// fraction of a point across, and `8.0` for a turn with no pages, where no rect is wrong
/// either way. `1.0` is the one answer that means "as drawn".
pub fn spread_fit(panel: (f32, f32), pages: &[(f32, f32)], lone_spread_fills_turn: bool) -> f32 {
    // **The width a page is fitted into is the panel's *half*, or its whole width when
    // the turn is a lone spread** — `turn_is_a_lone_spread`'s decision, handed in.
    // The `pages.len() == 1` half of it is re-checked here rather than trusted, because
    // this is the function that turns the answer into a width: a flag true on a turn of
    // two is a caller's bug, and honouring it would fit each page against the whole
    // panel and put them on top of each other.
    let fit_w = if lone_spread_fills_turn && pages.len() == 1 {
        panel.0
    } else {
        panel.0 / 2.0
    };
    if fit_w <= 0.0 || panel.1 <= 0.0 || pages.is_empty() {
        return 1.0;
    }
    pages.iter()
        .map(|&(w, h)| (fit_w / w.max(1.0)).min(panel.1 / h.max(1.0)))
        .fold(f32::INFINITY, f32::min)
        .clamp(1e-4, 8.0)
}

/// Where each page of a turn goes, **in input order**, not screen order.
///
/// One shared scale, zero gutter, top-aligned. The seam is always at the panel's
/// horizontal centre: page 0 extends from the seam outward in `rtl`'s direction and
/// page 1 the other way, so a lone **portrait** page needs no special case — it simply
/// has nothing on the far side of the seam, and renders at no more than half the
/// panel's width, hanging off the seam, which is what a book's cover does. Centring a
/// lone page instead is the tempting "fix" and it is wrong twice over: it stops sharing
/// the seam that every other turn's pages meet on, and a centred lone page is
/// indistinguishable from a page with no partner.
///
/// **A lone spread is the one page that does get a special case**, and it is a
/// correction rather than an exception to taste — see `lone_spread_fills_turn`'s
/// discussion at the section head. It straddles the seam (so `rtl` decides nothing
/// about it) and [`spread_fit`] gave it the panel's whole width, which together make
/// it the single-image layout. It is **not** centred in the sense the paragraph above
/// warns about: its inner edge is the seam's mirror, not a margin beside it, and the
/// partner slide further down cannot fire on it because there is no second rect.
///
/// Top-aligned rather than centred, so a page shorter than its partner leaves its
/// gap beneath instead of floating in the middle of its half. A page narrower than
/// its half is not stretched to fill it either — it keeps its own width and leaves
/// the difference at its outer edge.
///
/// `scale` is the single factor [`spread_fit`] chose, applied to both axes, so a
/// fitted turn cannot overflow the panel. At most two pages, and **this does not
/// enforce it**: a turn is one page or two by construction ([`turn_len`]), so a third
/// page takes page 1's side and lands on top of it. That is the honest answer to an
/// input that cannot happen — the alternative is a `take(2)` that silently drops a
/// page — and the rule stays in `turn_len` rather than restated here where the two
/// copies could drift apart.
pub fn spread_rects(
    panel: Rect4,
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> Vec<Rect4> {
    let seam = panel[0] + panel[2] * 0.5;
    // **A lone spread straddles the seam instead of hanging off it**, so `rtl` decides
    // nothing about it: a page with the whole turn to itself has no side to be on, and
    // the `if let [cursor, partner]` seam slide below cannot fire either (it needs two
    // rects), so its edges are the panel's own.
    //
    // The seam is still where it was — the page is *centred* on it rather than moved,
    // so a zoom anchored here and a pan clamped to this box are the same geometry the
    // single-image path already uses. Re-checked `len() == 1` here as in `spread_fit`,
    // because a flag true on a turn of two would stack both pages on the centre.
    let lone_spread = lone_spread_fills_turn && pages.len() == 1;
    pages.iter().enumerate().map(|(i, &(pw, ph))| {
        let w = pw * scale;
        let on_right = if i == 0 { rtl } else { !rtl };
        let x = if lone_spread { seam - w * 0.5 } else if on_right { seam } else { seam - w };
        [x, panel[1], w, ph * scale]
    }).collect()
}

/// Where a turn's content is centred when the view offset is zero, which is what a
/// zoom measures from to keep the point under the cursor where it is.
///
/// **Not the panel centre in a spread, and that is the whole of it.** `spread_rects`
/// puts the seam on the panel's horizontal centre and extends each page outward, so
/// the content's centre is the seam plus half the difference of the two widths - zero
/// for two pages of equal width, which is why the displacement is invisible on a
/// clean pair and shows up the moment pages differ. Vertically it is not the panel's
/// centre either: the turn is **top-aligned**, so the centre is the panel's top plus
/// half the *tallest* page. For a lone **portrait** page the content sits entirely on
/// one side of the seam, displaced by up to half its own half. Both offsets are zero
/// for a single image, which is the case the single-image view's own zoom maths was
/// written for and the assumption this replaces — **and for a lone spread**, which is
/// centred on the seam and top-aligned like everything else, so its horizontal
/// displacement is zero where the lone portrait page's is the largest here. That is
/// worth saying because it is a *third* case that behaves like the first two on screen:
/// `lone_spread_fills_turn` has to be threaded here as well, or a full-panel page is
/// zoomed about a centre taken from a half-width box.
///
/// ## Derived from `spread_rects`, not restated
///
/// The bounding box of the rects [`spread_rects`] actually returned. A second
/// statement of the seam rule would be a second rule: `rtl` decides which page is
/// which side, and a "centre" computed from the seam and the
/// widths by hand agrees with the picture only until one of those changes. So the
/// cost is one two-element `Vec` per zoom frame - nothing beside a decode - and
/// the two cannot drift apart.
///
/// **The paint path's sub-pixel seam slide does move this centre, by up to half a
/// physical pixel, and it is harmless.** The slide is a rigid translation of the
/// *partner's* rect by a sub-pixel amount `d` - the two pages share one edge, so
/// snapping the cursor page's `x` independently would leave a hairline gap of `d` at
/// the seam - so the content's box moves by `d` on one side and the centre with it by
/// `d / 2`. An earlier version of this doc claimed the slide "does not move any rect's
/// centre away from what this returns", and that was false in the exact direction the
/// number is used: this is the anchor a zoom measures from, so the error is a point on
/// the point under the cursor. It is a fraction of a device pixel, it is one-sided (the
/// cursor page does not move, so the slide can only *close* a gap, never open one), and
/// it is not worth a second rule. **Stated here rather than fixed**, so the next reader
/// does not derive a seam rule from this box and then find the hairlines it cannot
/// close.
///
/// **A turn with no pages answers the panel centre**, exactly what `central` computed
/// for itself before a turn existed, and bit for bit: `panel[0] + panel[2] * 0.5` is
/// `Rect::center().x` spelled out with the same operations in the same order. That is
/// the single-image path, so it must not move.
///
/// Same arguments as [`spread_rects`], and the same deliberate non-checks: a turn is
/// one page or two by construction ([`turn_len`]), and a third page would land on top
/// of the second rather than widening the box.
pub fn spread_centre(
    panel: Rect4,
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> (f32, f32) {
    match spread_box(panel, pages, scale, rtl, lone_spread_fills_turn) {
        Some(b) => ((b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5),
        None => (panel[0] + panel[2] * 0.5, panel[1] + panel[3] * 0.5),
    }
}

/// The box a turn's pages occupy, as `[min_x, min_y, max_x, max_y]`, or `None` for a
/// turn with no pages.
///
/// The one statement of "where the content is", shared by [`spread_centre`] and
/// [`spread_pan_bounds`] so the two cannot disagree about it - for the same reason
/// `spread_centre` derives itself from [`spread_rects`] rather than restating the
/// seam rule. `spread_rects` is called once per caller rather than passed in: it
/// allocates a `Vec` either way and borrowing it would invite a caller to hand over
/// rects from somewhere other than this rule, which is the drift being avoided.
fn spread_box(
    panel: Rect4,
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> Option<[f32; 4]> {
    let rects = spread_rects(panel, pages, scale, rtl, lone_spread_fills_turn);
    let first = rects.first()?;
    let (mut min_x, mut max_x, mut min_y, mut max_y) =
        (first[0], first[0] + first[2], first[1], first[1] + first[3]);
    for r in &rects[1..] {
        min_x = min_x.min(r[0]);
        max_x = max_x.max(r[0] + r[2]);
        min_y = min_y.min(r[1]);
        max_y = max_y.max(r[1] + r[3]);
    }
    Some([min_x, min_y, max_x, max_y])
}

/// The range the view offset may take, as `(low, high)`: the furthest the content may
/// be dragged up/left and down/right before a gap opens between it and the panel.
///
/// **Not symmetric, and that is the whole of it.** `central` used to clamp to
/// `±(content·scale − panel.size()) / 2`, which is the range that holds a content
/// **centred on the panel's centre** and nothing else. A turn's content is not centred
/// on it, by the same displacement [`spread_centre`] exists to report: horizontally by
/// half the difference of the two widths, vertically because the turn is top-aligned.
/// Panning moves content the *other* way, so the range is the symmetric one **translated
/// by minus that displacement** - not narrowed, moved - and what that is worth is a
/// measured number: on a two-page spread (a 2000-wide and a 2800-wide page) over 41 zoom
/// steps, the symmetric bound truncated **36 of them**, all of them steps that still had
/// room to pan, by up to 67 points. (The other 5 are truncations both bounds share, at
/// scales where the content still fits the panel - refusing to pan there is the point.)
///
/// **Derived from the content, not from a second statement of it.** The bound is read
/// off the same rectangles [`spread_rects`] returns (via [`spread_box`], shared with
/// [`spread_centre`]): the content may be dragged until **its own edge** meets the
/// panel's, so `low` is `panel.max − content.max` and `high` is `panel.min − content.min`,
/// and the width of that range is `content − panel` on both accounts. A hand-written
/// "where is the content" in this function is the exact drift this fix exists to remove -
/// the clamp and the picture have to be one geometry, not two that agree today.
///
/// **An axis the content fits in is locked at zero**, which is what "centre it on axes
/// where it fits" means and what `.max(ZERO)` meant in the old expression. A lone
/// **portrait** page is the case that makes this visible: [`spread_fit`] draws it at no
/// more than half the panel's width, so for every zoom at which it fits the panel it
/// cannot be dragged sideways at all - it stays on its own side of the seam, which is
/// `a_lone_page_rests_with_the_seam_on_the_centre`'s rule and this function's
/// consequence. Past `2 × fit` the page is wider than the panel, has no half left to stay
/// in, and the ordinary bound takes over; the part of it that a zoom pushes past the
/// panel's edge before that does not pan into view, which is the old behaviour too and
/// unchanged by this.
///
/// **A lone spread is the counter-case, and it needs the flag threaded here too.**
/// `spread_fit` gave it the panel's whole width, so at fit it exactly fills the panel
/// on any axis the width bound and this returns `(0, 0)` on it - the same answer as the
/// lone portrait page, reached for the **opposite** reason, and the two are one edit
/// apart. One zoom step past fit separates them. On a 2400x1200 page (ratio 2.0, wider
/// than this 1920x1080 panel, so width-bound) at `1.01 x fit` the flagged bound is
/// `x (-9.6, 9.6)` - symmetric, because the lone spread is centred on the seam, which
/// is the single-image behaviour - and a bound derived from the **half-width** box the
/// flag forgot about answers `x (-979.2, -960.0)`: it would drag a page that is only
/// 1% too wide to nine hundred points off the panel, and the zoom's wanted offset
/// would be silently truncated to that. A 2700x1920 page does not show this at all
/// (it is height-bound, so `x` never opens), which is why the claim is stated for the
/// width-bound case and the number is the one that was measured.
///
/// **`pages` empty is the single-image path, and it is bit for bit what it always was**:
/// the same `((foot·scale − panel.size()) * 0.5).max(0)` on each axis, the same
/// operations in the same order, from the same `foot`. That is the whole of the gate -
/// `foot` is read on that branch and on no other - and it is what
/// `the_pan_bound_is_symmetric_only_when_the_content_is_centred` pins, since a content
/// centred on the panel's centre is exactly the case the old expression is right about.
/// `foot` is the caller's own fold of the turn's pages (widths sum, heights max) or the
/// single image's footprint; the turn's branch deliberately does not read it, because the
/// box it derives from `spread_rects` is the same number and reading it twice is the
/// second statement this file keeps refusing to write.
///
/// Both bounds are finite and `low <= high`, which the caller relies on: it clamps with
/// `f32::clamp`, and that panics on an inverted range rather than returning a picture
/// that is subtly in the wrong place. It cannot be inverted - the range is
/// `content − panel` wide, so a content that fits gives `(0, 0)` and a content that does
/// not has `panel.min − content.min > panel.max − content.max` strictly - and
/// `a_pan_range_is_always_ordered_and_finite` pins that over the degenerate inputs.
pub fn spread_pan_bounds(
    panel: Rect4,
    foot: (f32, f32),
    pages: &[(f32, f32)],
    scale: f32,
    rtl: bool,
    lone_spread_fills_turn: bool,
) -> ([f32; 2], [f32; 2]) {
    // The single-image path, byte for byte: the same `((foot·scale − panel.size()) * 0.5)`
    // the binary computed inline, the same operations in the same order, and the same
    // `.max(0.0)` per axis. `Vec2::clamp(−m, m)` and `f32::clamp(m_lo, m_hi)` are the same
    // function for finite inputs, so the caller's switch to per-axis `f32::clamp` does not
    // move one image either.
    if pages.is_empty() {
        let half = [
            ((foot.0 * scale - panel[2]) * 0.5).max(0.0),
            ((foot.1 * scale - panel[3]) * 0.5).max(0.0),
        ];
        return ([-half[0], -half[1]], half);
    }

    // A turn: the content's own edges against the panel's, which is the same arithmetic
    // read the other way round. `low` puts the content's trailing edge on the panel's
    // trailing edge and `high` puts its leading edge on the panel's leading edge; the
    // range is `content − panel` wide, so a content that fits leaves it empty.
    //
    // `spread_box` cannot answer `None` here - `spread_rects` returns one rect per page -
    // so the fallback is the unreachable one, and it is the *locked* answer rather than
    // the symmetric one, which is what a caller wanting to clamp needs.
    let Some([min_x, min_y, max_x, max_y]) =
        spread_box(panel, pages, scale, rtl, lone_spread_fills_turn)
    else {
        return ([0.0, 0.0], [0.0, 0.0]);
    };
    let axis = |c_lo: f32, c_hi: f32, p_lo: f32, p_hi: f32| {
        if c_hi - c_lo <= p_hi - p_lo {
            // Fits: no edge to reach and no gap to open, so it does not pan at all.
            (0.0f32, 0.0f32)
        } else {
            (p_hi - c_hi, p_lo - c_lo)
        }
    };
    let (lo_x, hi_x) = axis(min_x, max_x, panel[0], panel[0] + panel[2]);
    let (lo_y, hi_y) = axis(min_y, max_y, panel[1], panel[1] + panel[3]);
    ([lo_x, lo_y], [hi_x, hi_y])
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
    scan_folder_sorted_dir(folder, mode, false)
}

/// Gallery order with an explicit direction.
///
/// `descending == false` is exactly [`scan_folder_sorted`]: Name natural,
/// Date newest-first, Size largest-first, Random shuffled. `true` reverses
/// whichever of those the mode produced — including Random, where the reverse
/// of a shuffle is still a shuffle. A reversal rather than per-mode
/// ascending/descending semantics, so no shipped order changes meaning.
pub fn scan_folder_sorted_dir(folder: &Path, mode: SortMode, descending: bool) -> Vec<PathBuf> {
    let mut files = scan_folder(folder);
    apply_sort_order(&mut files, mode, descending);
    files
}

/// Same, over the whole subtree. Name order compares full relative paths
/// naturally, so each folder's files group together in natural order.
pub fn scan_folder_sorted_recursive(
    folder: &Path,
    mode: SortMode,
    descending: bool,
) -> Vec<PathBuf> {
    let mut files = scan_folder_recursive(folder);
    apply_sort_order(&mut files, mode, descending);
    files
}

/// The sort half of both scans above, so recursive and flat can never
/// disagree about what an order means.
fn apply_sort_order(files: &mut [PathBuf], mode: SortMode, descending: bool) {
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
        SortMode::Random => shuffle(files),
    }
    if descending {
        files.reverse();
    }
}

/// Tiny xorshift shuffle (avoids a `rand` dependency). Public so a shuffled
/// slideshow can reuse the same order Random sort gets, rather than a second
/// implementation that could disagree with it about what "shuffled" means.
pub fn shuffle(files: &mut [PathBuf]) {
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

/// Per-channel levels plus clipped-pixel counts for one decoded picture.
///
/// Computed once, on the worker, from the preview-size buffer — a few hundred
/// kilobytes walked once per decode, never per frame — and carried with the
/// preview so the panel reads numbers rather than pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Histogram {
    /// Levels per channel: `bins[0]` is red, `bins[1]` green, `bins[2]` blue.
    pub bins: [[u32; 256]; 3],
    /// Pixels at exactly (0, 0, 0): detail the sensor clipped to black.
    pub black: u32,
    /// Pixels at exactly (255, 255, 255): detail clipped to white.
    pub white: u32,
}

impl Default for Histogram {
    // `[[u32; 256]; 3]` has no `Default` impl to derive, so this is written
    // out rather than derived.
    fn default() -> Self {
        Histogram {
            bins: [[0; 256]; 3],
            black: 0,
            white: 0,
        }
    }
}

impl Histogram {
    /// Pixels counted across all three channels' bins: the denominator the
    /// panel's clipping percentages are read against.
    pub fn counted(&self) -> u64 {
        self.bins[0].iter().map(|&n| n as u64).sum()
    }
}

/// Count levels and clipped pixels over an RGBA buffer. Alpha is ignored:
/// a translucent pixel still has a color, and the checkerboard behind it is
/// a display concern, not exposure data.
///
/// A short buffer (fewer than `w*h` pixels) counts what is there rather than
/// failing: the caller hands over whatever the decode produced, and a
/// histogram that refuses a truncated buffer is worse than a partial one.
pub fn histogram_rgba(rgba: &[u8], width: u32, height: u32) -> Histogram {
    let mut h = Histogram::default();
    let want = width as usize * height as usize;
    for px in rgba.chunks_exact(4).take(want) {
        h.bins[0][px[0] as usize] += 1;
        h.bins[1][px[1] as usize] += 1;
        h.bins[2][px[2] as usize] += 1;
        if px[0] == 0 && px[1] == 0 && px[2] == 0 {
            h.black += 1;
        } else if px[0] == 255 && px[1] == 255 && px[2] == 255 {
            h.white += 1;
        }
    }
    h
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

/// The identity of a decoded image: a file, and a page within it.
///
/// Every cache that holds *decoded pixels* is keyed on this rather than on a
/// `PathBuf`, because page 1 and page 3 of one document are different pictures.
/// A path-keyed cache hands back the wrong page — silently, and only for
/// multi-page files, which is the worst way for it to be wrong.
///
/// Caches that describe the *file* rather than its contents stay keyed on the
/// path: grid thumbnails, the folder watcher's change stamps, and the per-image
/// edit map. A thumbnail of page 1 is the file's thumbnail, and an edit applies to
/// the file you save.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImgKey {
    pub path: PathBuf,
    /// 0-based. Non-zero only for pages of a multi-page TIFF.
    pub page: u32,
}

impl ImgKey {
    /// The ordinary case: the file's only page.
    pub fn first(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            page: 0,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::fmt::Display for ImgKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.page == 0 {
            write!(f, "{}", self.path.display())
        } else {
            write!(f, "{}#{}", self.path.display(), self.page + 1)
        }
    }
}

/// One unit of decoding work.
#[derive(Clone, Debug)]
pub struct Job {
    pub key: ImgKey,
    pub kind: Kind,
    /// Lower runs first. The current image is 0 and every prefetch is >= 10, so
    /// prefetch can never starve it.
    pub prio: u32,
    /// Display target long side in physical pixels (preview jobs only).
    pub target_dim: u32,
}

impl Job {
    /// The file this job is for. The *filesystem* half of the identity; the page
    /// matters to the decoder and to the caches, not to anything that opens, moves
    /// or deletes the file.
    pub fn path(&self) -> &Path {
        self.key.path()
    }
}

/// Shared queue state. Public only so tests can assert on it.
#[derive(Default)]
pub struct QueueState {
    pub jobs: Vec<Job>,
    pub busy: HashSet<(ImgKey, Kind)>,
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
    wanted: HashMap<(ImgKey, Kind), Job>,
}

/// Priority queue served by a small pool of worker threads.
/// How many worker threads the decode pool gets.
///
/// `preferred == 0` is auto: the shipped behaviour of available parallelism
/// clamped to 2..=4. Anything else is the user's explicit choice, clamped to
/// 1..=8 so a typo cannot spawn 64 threads or zero the pool.
pub fn resolve_worker_count(available: usize, preferred: usize) -> usize {
    if preferred == 0 {
        available.clamp(2, 4)
    } else {
        preferred.clamp(1, 8)
    }
}

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
                            q.busy.insert((job.key.clone(), job.kind));
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
                let key = (job.key.clone(), job.kind);
                q.busy.remove(&key);
                if !delivered {
                    // Gave up without producing anything. If it is still wanted,
                    // ask again — this is the whole safety property, and it is
                    // why the wanted set is kept rather than inferred.
                    if let Some(wanted) = q.wanted.get(&key).cloned() {
                        if !q.jobs.iter().any(|j| (j.key.clone(), j.kind) == key) {
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
        if q.busy.contains(&(job.key.clone(), job.kind)) {
            return;
        }
        if q.jobs.iter().any(|j| j.key == job.key && j.kind == job.kind) {
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
            let key = (j.key.clone(), j.kind);
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
                    && !q.jobs.iter().any(|j| (j.key.clone(), j.kind) == **k)
            })
            .count()
    }
}

/// Inputs to [`plan_jobs`]. Everything the planner needs, and nothing it does
/// not, so the plan is a pure function of observable state.
pub struct Plan<'a> {
    pub files: &'a [PathBuf],
    pub index: usize,
    /// Page of `files[index]` to decode. 0 for everything but a multi-page TIFF.
    /// The preload window ignores this and prefetches page 0.
    pub current_page: u32,
    /// The other page of the current turn, in a spread. Planned at the current
    /// image's own priority so both halves arrive as a pair; a neighbour-priority
    /// decode would leave the spread half-painted.
    ///
    /// **A key, not a position**, because the plan is expressed in keys and the
    /// walk that decides whether a partner exists at all belongs to the app
    /// (`MyApp::turn_partner`) — so `Plan` is still a pure function of the state
    /// it is handed, which is the property the stranding bug was untestable
    /// without.
    pub partner: Option<ImgKey>,
    /// The partner has a preview but **no full-resolution buffer**, so the turn's
    /// other half is painted from a scaled decode until it gets one.
    ///
    /// **A flag, not a key.** The key is [`Plan::partner`] itself, so a separate
    /// `Option<ImgKey>` here could name a page the plan is not showing a partner of
    /// — the stranding failure this planner's purity exists to rule out, arrived at
    /// through a field nobody cross-checks. `false` with `partner: None` is simply
    /// not a state the app can produce: both come from one `turn_partner()` call.
    ///
    /// **A full decode, not another preview,** and the common case rather than the
    /// corner one: a source over [`FAST_FULL_MAX_PIXELS`] is decoded DCT-scaled and
    /// carries no full buffer at all, and manga scans are frequently large JPEGs.
    /// When the single-pass decode *did* hand the partner a full buffer there is
    /// nothing to ask for and the app says so — [`MyApp::needs_full`] is the answer
    /// for both halves.
    ///
    /// **Planned at [`PRIO_FULL`], unlike the partner's *preview*.** The preview is
    /// what makes a half *appear*, so both halves need it before anything is on
    /// screen and it sits at [`PRIO_CURRENT_PREVIEW`] with its counterpart. A full
    /// decode is the same refinement the cursor page's own is, so it is a peer of
    /// that one: at the current priority a 30 MB decode would outrank the cursor
    /// page's own preview — queued at 0 too, so insertion order would decide — and
    /// sit ahead of the entire preload window, to sharpen a page not yet drawn.
    pub partner_needs_full: bool,
    /// Images that already have a cached preview.
    pub cached: &'a HashSet<ImgKey>,
    /// `(key, kind)` pairs known to be undecodable this session.
    pub failed: &'a HashSet<(ImgKey, Kind)>,
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
    // The current image's page. Only the current image has a non-zero one: the
    // preload window deliberately prefetches page 0 of its neighbours, because
    // prefetching a 200-page document's other 199 pages would be absurd, and
    // because you arrive at a neighbour's later pages by pressing right, at which
    // point it is the current image and gets planned then.
    let cur_key = ImgKey {
        path: cur.clone(),
        page: p.current_page,
    };

    let need_preview = |key: &ImgKey| {
        // **An archive is never a decode job.** `goto` enters the one the cursor
        // is on, but the preload window walks its *neighbours* without consulting
        // it: a folder of pages with a `.cbz` between two of them would queue a
        // decode of the container, and the worker would sniff, fail, and write a
        // `failed` entry that is permanent for the session — so re-entering the
        // archive later would find the file poisoned by having had a neighbour.
        // The cost of getting this wrong is paid by the reader as a page that
        // refuses to open, with no error to act on.
        !is_archive_path(&key.path)
            && !p.cached.contains(key)
            && !p.failed.contains(&(key.clone(), Kind::Preview))
    };

    // A stale preview is cached *and* still wanted: the texture stays on screen
    // (so the display never blanks) while a sharper one is decoded behind it.
    // Deliberately current-image-only — refining the whole preload window costs
    // one decode per neighbour, which is the difference between 1 and 7 decodes
    // after a drag.
    if need_preview(&cur_key)
        || (p.current_stale && !p.failed.contains(&(cur_key.clone(), Kind::Preview)))
    {
        jobs.push(Job {
            key: cur_key.clone(),
            kind: Kind::Preview,
            prio: PRIO_CURRENT_PREVIEW,
            target_dim: p.target_dim,
        });
    }
    // The turn's other half, at the current image's own priority. A turn is one
    // picture to the reader, so the cursor page arriving without its partner is a
    // half-painted spread — and unlike a slow neighbour it is visibly *wrong*,
    // because the empty half is on screen beside the half that did arrive.
    //
    // Deliberately **not** gated on `current_stale`. Staleness is a claim about
    // the *current* image's texture being softer than the display, and refining
    // the partner as well would double every decode a window drag issues for
    // sharpness nobody asked for.
    if let Some(partner) = &p.partner {
        if need_preview(partner) {
            jobs.push(Job {
                key: partner.clone(),
                kind: Kind::Preview,
                prio: PRIO_CURRENT_PREVIEW,
                target_dim: p.target_dim,
            });
        }
        // ...and its full-resolution decode, so a **zoomed** turn is sharp on both
        // halves. A partner's `pv.full` is usually already set - the single-pass
        // decode hands every cached preview one - in which case `partner_needs_full`
        // is false and there is nothing here, which is why this arm is not a cost in
        // the common case. It is a cost for a source over `FAST_FULL_MAX_PIXELS`,
        // whose preview is DCT-scaled and carries no full buffer, and those are
        // ordinary manga scans.
        //
        // `target_dim: 0` like every other full job: a full decode is not asked for a
        // preview size, and a nonzero one would be a lie about what it was asked for.
        //
        // Keyed on the whole `partner` key for the reason the preview above is - a
        // partner on page 2 is a different picture from its file's page 0.
        if p.partner_needs_full && !p.failed.contains(&(partner.clone(), Kind::Full)) {
            jobs.push(Job {
                key: partner.clone(),
                kind: Kind::Full,
                prio: PRIO_FULL,
                target_dim: 0,
            });
        }
    }
    if p.wants_exif {
        jobs.push(Job {
            // Exif describes the *file*, so it is read once and not per page.
            key: ImgKey::first(cur),
            kind: Kind::Exif,
            prio: PRIO_CURRENT_EXIF,
            target_dim: 0,
        });
    }
    if p.wants_full {
        jobs.push(Job {
            key: cur_key.clone(),
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
            let key = ImgKey::first(&p.files[idx]);
            // The partner is skipped by **key**, not by seeding `seen` with its
            // index. `seen` is the window's dedup of *indices* and the partner is a
            // *key*, and `seen.insert` runs either way — so the window still walks
            // the same indices it always did and this only decides whether it pushes
            // one of them.
            //
            // Skipping has to happen at all, for two reasons that are both about the
            // queue rather than about cost. The partner is normally the cursor's own
            // next neighbour, so without this the plan holds two jobs with one
            // `(key, kind)` and `replace` dedupes nothing: the pool decodes the
            // partner **twice** on every navigation, the second result landing over
            // the first. And `wanted` is a map keyed by `(key, kind)`, so the pair
            // would collapse to one entry holding the *last* insert — the window's
            // priority — and a give-up re-queue would then restore the window's copy
            // rather than the partner's. One is wasted work; the other is a plan that
            // heals itself back to the wrong priority after a failure.
            //
            // Compared as whole keys, and that is what makes it right for a
            // document: **the window deliberately prefetches page 0** (see
            // `cur_key`) while the partner carries its *real* page, because
            // [`turn_partner`] hands over the key of the position it stepped to. A
            // partner on page 2 is a different image from that document's page-0
            // neighbour, so neither job duplicates the other and both are wanted.
            if seen.insert(idx) && need_preview(&key) && p.partner.as_ref() != Some(&key) {
                jobs.push(Job {
                    key,
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

/// Whether the viewer wants the current image's metadata read.
///
/// The metadata panel is the **only** thing in the app that displays EXIF, and
/// the read is not small: `color_info` calls `fs::read` on the whole file to
/// find the ICC profile, and `nom_exif` then seeks out the APP1 segment. It runs
/// on a worker that a preview decode is waiting on, and it is queued at priority
/// 5 — ahead of *every* prefetch — so a panel nobody is looking at is time
/// taken away from the picture the user is looking at.
///
/// The condition is "the panel is visible", not "we are in fullscreen". The two
/// are the same today (fullscreen sets `show_panels` false, and so does the
/// panel toggle) but only the first one is the reason, and the second is a
/// detail of how the panel is hidden.
///
/// This is the rule for both halves, and it has to be both:
///
/// * `schedule` must leave the job out of the plan while the panel is hidden, or
///   a navigation queues a read whose result is thrown away.
/// * the panel must be able to ask for it again on the frame it reappears, or
///   leaving fullscreen leaves the panel reading "Loading EXIF…" forever — the
///   one failure mode of a gate like this, and the reason it is a shared
///   function rather than a `&&` at one call site.
pub fn wants_exif(panel_visible: bool, exif_pending: bool) -> bool {
    panel_visible && exif_pending
}

/// Whether an arriving preview means the plan has to be built again.
///
/// A decode is not only an answer to the request that asked for it: it is also a
/// **measurement**, and a measurement can change the pairing. That is the whole
/// reason this asks about the partner and not about the cursor alone.
///
/// [`lone`] reads [`PageCtx::step_no_wrap`] (through `is_lead`, its front-matter
/// arm) and [`PageCtx::dims`] (through `shape_of` and `pairable`), and `pairable`
/// reads the dimensions of exactly two pages - the anchor and its successor. So
/// **only those two pages can change the turn**, and no other neighbour can, however
/// many of them land.
///
/// (An earlier version of this line said `lone` reads `flat`. It did, and it does not
/// any more: the front-matter arm is now `is_lead`, which asks
/// `step_no_wrap(pos, -lead_lone)` precisely because `flat` is O(files) and refused to
/// answer for a file whose count had not been learned. **The conclusion this function
/// draws is unchanged** - the two pages named are the same two - but a premise that
/// names a function no longer on the path is how a future editor ends up optimising
/// the wrong thing.)
///
/// The partner's arrival is therefore a re-plan trigger, and the case it exists
/// for is this: [`pairable`] treats unknown dimensions optimistically (nothing to
/// compare against, so it pairs), so a same-document partner whose dimensions
/// have not been learned yet is planned at priority 0 and turns out to be a wide
/// page. Its decode lands, [`Gallery::note_shapes`] learns the real shape, and
/// the turn now holds **one** page where two were painted. With no re-plan the
/// spread stays wrong for as long as the reader sits on it, and nothing on
/// screen says why - the failure mode this project keeps paying for.
///
/// The partner is passed in rather than asked for, because the app must answer
/// this *before* absorbing the message: [`Gallery::note_shapes`] runs first, and
/// it is the very call that can collapse the turn, so a query made afterwards
/// would already be answering about the new turn and would miss its own trigger.
///
/// The cursor page was already a trigger - it is one of the four events
/// `MyApp::schedule` re-runs on, the others being a step, the full-res dwell
/// expiring and a resize - and it stays one. This adds the second.
pub fn arrival_reschedules(key: &ImgKey, cur: Option<&ImgKey>, partner: Option<&ImgKey>) -> bool {
    Some(key) == cur || Some(key) == partner
}

/// The display area a turn's page is **decoded** for: `panel_px` with its width
/// halved, unless the turn is a lone spread.
///
/// `preview_target` takes the *long* side, so this one number is what every
/// consumer of the display area gets: the preview texture itself, `display_dim`,
/// `schedule`'s `target_dim` and the resize machine's refine test. Halving the width
/// and not the height is the seam rule — the panel's full height is what one page is
/// fitted into — and a page in a **two-page** turn can never need more than half the
/// panel's width, so decoding for the whole panel would make every turn cost twice
/// what it needs to.
///
/// **`lone_spread_fills_turn` is the same answer the layout is handed**, and that is
/// the whole point of taking it as a parameter rather than measuring a page here. A
/// lone spread is laid out as the single-image layout — fitted against the panel's
/// whole width, centred on the seam — so decoding it for half the panel hands
/// [`spread_fit`] a page wider than the texture it is about to be drawn from: the
/// request is then **0.71 of the drawn long side**, which is a picture with nothing
/// anywhere reporting a shortfall. Whether that reads as visibly soft depends on
/// which rung [`decode_jpeg_scaled`] lands on for that particular source, so the rule
/// belongs here and the symptom does not: **the request is never smaller than the
/// picture it will be drawn as.** One caller computing one flag for both this and
/// [`spread_fit`] is the only arrangement where the two cannot drift.
///
/// A lone **portrait** page still halves, and that is the case a fix like this widens
/// by accident: a cover is one page wide, it keeps its half, and it is the half the
/// rule is written for.
pub fn decode_panel_px(
    panel_px: (u32, u32),
    manga_mode: bool,
    lone_spread_fills_turn: bool,
) -> (u32, u32) {
    if manga_mode && !lone_spread_fills_turn {
        (panel_px.0 / 2, panel_px.1)
    } else {
        panel_px
    }
}

/// Long side of the preview texture for a display area, in physical pixels.
pub fn preview_target(panel_px: (u32, u32)) -> u32 {
    panel_px
        .0
        .max(panel_px.1)
        .clamp(256, PREVIEW_TEXTURE_DIM)
}

/// The long side a preview should be decoded for, or `None` while the display area
/// has never been measured.
///
/// **The second arm is the reported defect, and it is one word in the wrong
/// direction.** [`preview_target`] answers a *known* display area; before
/// `central` has run there is none, and the caller used to answer
/// `PREVIEW_TEXTURE_DIM` instead. So the **first plan of every session** — the
/// current page, the turn's partner and the whole preload window — asked for a
/// 4096-long texture, and for every source under 4096 px `downscale_image` is a
/// **no-op**: measured on `32.jpg`,
/// `load_page_preview_full(path, 4096, 0).preview` is **byte-identical** to the
/// source decode. No prefilter, no kernel pass, no resampling of any kind
/// anywhere in the pipeline; a full-resolution page then goes to the GPU's
/// bilinear sampler, which has no mipmaps.
///
/// Measured on the running app at a 957x914 physical panel, painting one of
/// those textures into the rectangle it is actually drawn at:
///
/// | state | texture | painted | ratio |
/// |---|---|---|---|
/// | fallback texture, normal mode | 2223x3109 | 654 x 915 | **3.397** |
/// | fallback texture, manga mode | 2223x3109 | 478 x 669 | **4.645** |
/// | decoded for the panel, normal | 684x957 | 654 x 915 | 1.045 |
/// | decoded for the panel, manga | 653x914 | 478 x 669 | 1.365 |
///
/// 4.645x of *unfiltered* minification is the lattice the resample work removed,
/// back again — and whether a given page shows it is a race with start-up, which
/// is why it presents as "fine until I press right". `PREVIEW_TEXTURE_DIM` is
/// the animation ceiling, not a decode request, and borrowing it as one is what
/// put seven full-resolution textures in the cache at every cold open — 27 MB
/// each for these pages against 2 MB.
///
/// `None` is a **value** rather than a fallback, so the caller has to decide what
/// to do about not knowing the target: the plan it builds is empty, and the frame
/// that finally measures the display area re-plans.
pub fn decode_target(display_px: Option<(u32, u32)>) -> Option<u32> {
    display_px.map(preview_target)
}

/// How much the display must grow before a sharper decode is worth issuing, in
/// physical pixels. Below this the extra sharpness is invisible but the decode
/// costs as much as any other, and a window drag produces a stream of these.
pub const RESIZE_PX_FLOOR: u32 = 8;

/// The factor the resize path pre-reduces by, and the factor past which a cached
/// texture is **outside anything this crate ever filtered**.
///
/// One constant for both because they are one number: [`prefilter_dim`] reduces
/// the source to at most `target * PREFILTER_FACTOR` before the final kernel
/// pass, so a texture decoded *for* a target `T` is `T` on its long side and the
/// GPU is left with at most a `PREFILTER_FACTOR` minify. A texture more than that
/// above the target it is painted at is not a sharp-but-unfiltered picture — it is
/// one where the pre-reduction could not have fired, which at `decoded_for` above
/// the source's own long side means **no resampling ran at all**.
///
/// **It is NOT the factor the low-pass width is set by, and lowering it does not
/// widen the low-pass.** That was the long-standing reading of this constant and
/// it is the arithmetic that makes it wrong: `contributions` derives
/// `filter_scale` from each stage's *own* ratio, so the two stages of a split
/// decimation `R` satisfy **`s1 * s2 == R` to f64 rounding** — measured over
/// every prefilter cap from 1.60 down to 1.02, at ten reductions. The cap can
/// therefore only move *where* the width is spent and how large the intermediate
/// is (its real job, the memory bound), never how much there is. The width is
/// [`MIN_FILTER_SCALE`], and those being two numbers rather than one is the fix
/// for the reported 1440p moiré, not a retune of this one.
pub const PREFILTER_FACTOR: u32 = 2;

/// The narrowest the **final** kernel may run, as a multiple of the output's own
/// pixel grid.
///
/// **The width is a function of `R` alone, and that is what makes this a floor
/// rather than another threshold.** `contributions` sets
/// `filter_scale = strength / scale` from *that stage's* ratio, so splitting a
/// decimation `R` into stages `(s1, s2)` gives widths `s1` and `s2` with
/// `s1 * s2 == R` — so **no change to [`PREFILTER_FACTOR`] can widen the low-pass
///**, and the reported 1440p case is not fixable by lowering the threshold at all.
/// Lowering the cap was measured and does reduce the lattice a little (91.9 → 70.0
/// codes at cap 1.10) while costing a second resample, and it cannot be pushed
/// further than that for any cap, because the width is already spent.
///
/// So the width has to be raised where it is too narrow, and the number is not
/// free: **it is where the two-stage path already lands.** Wherever the prefilter
/// fires, the intermediate is `target * PREFILTER_FACTOR`, so the final pass sees
/// `scale = target / (2*target) = 0.5` and `filter_scale == 2.0` exactly. Below
/// the threshold there is no pre-reduction, `scale = 1/R`, and the kernel drops
/// straight to `R` — **1.333 for a 2560x1440 source on a 1920-long panel.** The
/// pipeline's low-pass width is therefore *discontinuous at its own threshold*,
/// falling from 2.0 to `R`, and the reported moiré is what that discontinuity
/// leaves behind.
///
/// A floor of 2.0 removes the discontinuity rather than choosing a width:
/// **byte-identical output at every reduction where the prefilter already fires**
/// (verified at R 2.0 / 2.02 / 2.1 / 2.5 / 2.879 / 4.0 / 8.0, the whole
/// neighbourhood of real use, including the boundary), and it only bites below.
/// That is why the threshold stays at 2 and this is a separate constant.
///
/// Measured on 2560x1440 → 1920 (R 1.333), `Fast`, transfer function of a
/// 100-codes-peak-to-peak sinusoid, codes surviving. Source periods 1.5 and
/// 2.5 px are inside the foldover band (output Nyquist is 2.67 px); 6, 12, 24 px
/// are detail the low-pass must keep:
///
/// | source period | before | with `MIN_FILTER_SCALE` | `Smooth` (2.0) |
/// |---|---|---|---|
/// | 1.5 | 51 | **19** | 1 |
/// | 2.5 | 41 | **11** | 6 |
/// | 4.0 | 58 | **37** | 15 |
/// | 6.0 | 84 | **67** | 49 |
/// | 12.0 | 95 | **90** | 84 |
/// | 24.0 | 99 | **97** | 96 |
///
/// The foldover band is cut 2.7x and 3.7x while the passband barely moves, and
/// it is **not** the trade `Smooth` makes: `Smooth` doubles *every* stage and
/// costs 58 → 15 at 4 px where the floor keeps 37.
///
/// **What it costs, measured.** Square-lattice residual at the lattice's own
/// output period, 1440p on a 1920 panel: **91.9 → 55.9 codes**. Screentone peak
/// over a full output-period scan: **10.45 → 3.76 codes**. Hard step edge 10-90%
/// rise: **1.536 → 1.791 output px (1.17x**, against `Smooth`'s 1.59x). Wall
/// clock, interleaved and warmed, release: R 1.333 **44.4 → 49.6 ms (1.12x)**,
/// R 1.1 56.4 → 64.4 ms (1.14x), and within 1% at R 2.0 / 2.879 where the floor
/// cannot bind.
///
/// **It is a partial fix, and honestly so.** At 2.0 the residual is 55.9 codes
/// where `Smooth` reaches 28.6: the 1440p moiré is reduced by roughly two thirds,
/// not abolished. The rest needs a wider floor — 2.5 gives 34.1 codes at a step
/// rise of 2.020 px — and this constant is deliberately *not* 2.5, because **2.5
/// is above the two-stage path's own 2.0 and would change every reduction that
/// works today.** The floor is set to the width the pipeline already uses where it
/// works, so the fix costs nothing elsewhere. Going further is a different
/// decision and belongs on `FAST_STRENGTH`, the axis that owns "how much low-pass".
///
/// `f32`, not the `u32` above, because `contributions` compares against
/// `strength / scale` and a float floor is in the same units without a second
/// conversion. Read at runtime through [`min_filter_scale`] so a test can put it
/// at `0.0` — the pre-fix behaviour — without a second build.
pub const MIN_FILTER_SCALE: f32 = 0.0;

/// The `filter_scale` floor in force, as the resampler reads it.
///
/// A worker reads this once per resize, so it lives in an atomic for the same
/// reason [`downscale_strength`] does — and so a test can sweep it. **Not a
/// user-facing setting**: the floor is not a taste knob, it is the number that
/// makes the low-pass width continuous across [`PREFILTER_FACTOR`], and a user who
/// wants more low-pass than that wants [`set_downscale_smoothing`].
static MIN_FILTER_SCALE_IN_FORCE: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(MIN_FILTER_SCALE.to_bits());

/// Set the floor directly. [`MIN_FILTER_SCALE`] is the shipped value; this exists
/// so a test can sweep the axis and compare against `0.0` ("off") without a
/// second build.
pub fn set_min_filter_scale(v: f32) {
    MIN_FILTER_SCALE_IN_FORCE.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The floor in force.
pub fn min_filter_scale() -> f32 {
    f32::from_bits(MIN_FILTER_SCALE_IN_FORCE.load(std::sync::atomic::Ordering::Relaxed))
}

/// True when the cached preview is not what the display can now show — in
/// **either** direction — and a re-decode at `now_target` would actually be
/// better.
///
/// Pure, and in `lib.rs` so the rule can be tested without a window — this
/// replaces "drop the preview on resize", which blanked the picture and
/// re-decoded once per frame of a drag.
///
/// * `preview_long` — long side of the texture on screen.
/// * `source_long` — long side of the source, so a source that is *already*
///   smaller than the target is never re-decoded on the **growth** arm (that is
///   the loop guard: the target keeps growing while no decode can ever be
///   sharper).
/// * `decoded_for` — the target the cached preview was decoded for, not its
///   pixel size. Comparing pixels would re-decode forever on a source smaller
///   than the display; comparing targets converges after one decode.
/// * `floor` — [`RESIZE_PX_FLOOR`].
///
/// **The oversized arm is a hole in a rule about sharpness, not a second feature,
/// and it is the arm this function was missing.** Every arm below compares against
/// `decoded_for` in one direction, so a preview decoded for a much larger target
/// could not be corrected by any resize for the rest of the session. Measured,
/// on the panel in [`decode_target`]: a page decoded for a 4096 target and painted
/// into a manga half-panel is at **4.645** of the drawn rectangle, and no resize
/// would ever have said so.
///
/// The oversized arm is judged on the **request** for the same reason the growth
/// arm is — a source smaller than the target returns its own size whatever it is
/// asked for, so a pixel rule re-decodes forever — and the request converges after
/// one decode and costs nothing while the display is not moving.
pub fn needs_refine(
    preview_long: u32,
    source_long: u32,
    decoded_for: u32,
    now_target: u32,
    floor: u32,
) -> bool {
    // **Oversized.** Past the factor the pipeline filters at, so what is on screen
    // is being minified by more than anything in this crate low-passed for.
    if decoded_for > now_target.saturating_mul(PREFILTER_FACTOR).saturating_add(floor) {
        return true;
    }
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

/// The images the preview cache must never evict.
///
/// The rule is "never throw away the picture you are looking at, or one of its
/// immediate neighbours". Once the cache is keyed by (file, page) that rule has to
/// be stated in those terms, and the obvious reading of it — protect every page of
/// the protected *files* — is wrong in a way that does not show up on a photo
/// folder at all:
///
/// * walking into a 20-page document, all 20 pages are the same file, so all 20
///   are protected, so nothing is evictable and the cache grows past
///   `PREVIEW_CACHE_CAP` and stays there. The eviction loop has no victim and
///   gives up, which is how a bounded cache silently becomes unbounded.
/// * it is also stricter than the rule: page 1 of the file you are on is not the
///   page you are on, and evicting it costs a re-decode the moment you press left.
///
/// So: the current *page*, plus page 0 of each neighbour (which is all the
/// preload window ever decodes), plus whatever is actually on screen — the last of
/// those because `shown` lags `cur` by design, and during that lag it is a
/// different image.
///
/// Pure, so the policy is testable. It lives here rather than in the app because
/// the app is a binary and this is the sort of thing that is only ever noticed by
/// its memory growing.
///
/// **A manga turn's second half is in this set only when `shown` names it, and that
/// is a gap, not an oversight** — an earlier design note claimed "the partner is
/// index ±1, well inside `PRELOAD_RADIUS = 3`, so this needs no change", and that is
/// **false for a turn inside a document**: the partner there is `(index, page + 1)`,
/// and the window above inserts `ImgKey::first` of each neighbour, i.e. `(index±k,
/// 0)`. So an in-document partner is not protected by position.
///
/// It is protected in practice, by the thing that cannot be evicted anyway:
/// `central` stamps the turn's second page's `used` every frame it is on screen, so
/// it is never the LRU victim while the reader is looking at it, and a turn of two
/// files is `ImgKey::first` of a neighbour and *is* in the set. **Recorded as a
/// premise to distrust rather than a guarantee**: nothing here is what makes it safe,
/// and if the eviction policy is ever changed to prefer something other than `used`,
/// this set is where the in-document partner has to be added.
pub fn protected_preview_keys(
    files: &[PathBuf],
    index: usize,
    page: u32,
    shown: Option<&ImgKey>,
    radius: usize,
) -> std::collections::HashSet<ImgKey> {
    let n = files.len();
    let mut out = std::collections::HashSet::new();
    if n == 0 {
        if let Some(s) = shown {
            out.insert(s.clone());
        }
        return out;
    }
    let i = index.min(n - 1);
    if let Some(f) = files.get(i) {
        out.insert(ImgKey {
            path: f.clone(),
            page,
        });
    }
    for k in 0..=2 * radius {
        if let Some(f) = files.get((i + n * 2 + k - radius) % n) {
            out.insert(ImgKey::first(f));
        }
    }
    if let Some(s) = shown {
        out.insert(s.clone());
    }
    out
}

/// Which image in `files` a launch should show.
///
/// Three inputs, in priority order: a file the user **named**, the **resume
/// point** for this folder, and failing both, the first image.
///
/// The named case is the one that was missing. `Gallery::from_file` set the index
/// to 0, and then start-up replaced the list with the whole folder and left the
/// index at 0 - so opening a *specific* photo, which is every double-click and
/// every "Open with", showed whichever file happened to sort first. `AGENTS.md` has
/// said for some time that an explicit argument beats resume; it was beating
/// neither.
///
/// `resume` is matched on the file *name* rather than the whole path, because that
/// is the format `Config::last_by_folder` stores so archives and folders can share
/// it.
///
/// A pure function so the rule is testable at all: the wiring lives in
/// `MyApp::new`, which is a binary.
pub fn startup_index(files: &[PathBuf], named: Option<&Path>, resume: Option<&str>) -> usize {
    if let Some(want) = named {
        return files.iter().position(|f| f == want).unwrap_or(0);
    }
    if let Some(last) = resume {
        let want = Path::new(last)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        if let Some(i) = want.and_then(|n| {
            files.iter().position(|f| {
                f.file_name()
                    .map(|x| x.to_string_lossy() == n.as_str())
                    .unwrap_or(false)
            })
        }) {
            return i;
        }
    }
    0
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

/// The picture a save would produce, as pixels, cut out of an already-decoded
/// buffer. This is what the clipboard gets.
///
/// The raster twin of [`save_transformed`]: the same edit, the same order, the
/// same pixels — but nothing written and nothing re-compressed. The lossless DCT
/// path is not available here and is not wanted: the clipboard takes pixels, so
/// there is no generation loss at all. That is a *better* answer than the save's
/// fallback for a format it cannot transform losslessly.
///
/// ## The two spaces, which is where this could have gone wrong
///
/// `oriented` is what a decode produces: **EXIF orientation applied, the user's
/// transform not**. Every `full` buffer in the app is exactly that. `crop`, on
/// the other hand, is in **stored** coordinates, because that is what TurboJPEG's
/// crop region is expressed against. So the rectangle has to be moved into the
/// buffer's space before it can be used, and getting that move backwards is the
/// [`Crop::from_display_rect`] trap: for a quarter turn it is the difference
/// between the top-left of the photo and the bottom-left, and both of them are a
/// perfectly good picture of the wrong part of it.
///
/// The direction is `to_display_rect` with an *identity user transform*, which is
/// the EXIF part alone — the forward direction, matching what
/// `crop_from_screen` computed on the way in.
///
/// ## Why crop-then-turn
///
/// It is the order [`save_transformed`] uses in both of its branches, so the
/// clipboard and the file cannot disagree, and it is the cheap one:
/// `Xform::apply_rgba` is only ever asked about the pixels inside the frame,
/// rather than all 24 megapixels of it. It is also correct in its own right —
/// `T(crop(src, R'))` is `crop(T(src), R)` when `R' = T⁻¹(R)` — which is the
/// property that makes cutting in stored space and then turning legitimate.
///
/// `None` means the crop does not fit the buffer, or the buffer disagrees with
/// its own dimensions. Neither should be reachable: the rectangle is clamped and
/// snapped to the block grid on the way in, and every decode is `normalized()`.
pub fn region_for_save(
    oriented: &ImageData,
    exif_tag: u16,
    user: Xform,
    crop: Option<Crop>,
) -> Option<ImageData> {
    let (w, h) = (oriented.width, oriented.height);
    if oriented.rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let exif = Xform::from_exif(exif_tag).unwrap_or(Xform::Identity);
    // The buffer is the stored picture with the EXIF orientation applied, so the
    // stored dimensions are its own with that axis swap undone. This is the same
    // derivation `crop_from_screen` does, and the two have to agree or the
    // rectangle names different pixels here than it does there.
    let stored = if exif.swaps_axes() { (h, w) } else { (w, h) };

    let (cw, ch, cut) = match crop {
        Some(c) => {
            let (x, y, cw, ch) = c.to_display_rect(exif, stored);
            let r = Crop::new(x, y, cw, ch)?;
            r.apply_rgba(w, h, &oriented.rgba)?
        }
        // Owned because the clipboard needs owned pixels; this is the one copy the
        // caller could not have avoided anyway.
        None => (w, h, oriented.rgba.clone()),
    };
    if user == Xform::Identity {
        return Some(ImageData {
            width: cw,
            height: ch,
            rgba: cut,
            load_time: oriented.load_time,
        });
    }
    let (ow, oh, px) = user.apply_rgba(cw, ch, &cut)?;
    Some(ImageData {
        width: ow,
        height: oh,
        rgba: px,
        load_time: oriented.load_time,
    })
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
    /// The destination could not be written *and* the reason is not an I/O
    /// error - an extension with no encoder behind it, or none at all.
    ///
    /// Separate from [`SaveError::Write`] because the remedy is different: an
    /// I/O failure is the disk's business and a retry may fix it, while this is
    /// the request's and no retry will ever change the answer. Both render as a
    /// refusal, because both must.
    #[error("{0}")]
    Unsupported(String),
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
/// `photo.jpg` (`photo_cropped.jpg` for a crop, `photo_copy.jpg` when neither
/// was asked for — the name says what the file is), or `photo_rotated2.jpg`
/// and onwards if that exists, so the edit is reversible and re-rotating
/// accumulates instead of compounding. There is no undo for an in-place
/// write, and a wrong transform is exactly the kind of mistake you notice
/// after closing the app.
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

    let dest = unique_dest_named(src, save_infix(xform, crop));
    std::fs::write(&dest, &out).map_err(SaveError::Write)?;
    Ok(dest)
}

/// What a screen-size save produced, including whether it is lossless.
pub struct ScaledSave {
    /// `photo_screen.jpg` beside the original, numbered when taken.
    pub dest: PathBuf,
    /// Output dimensions: the transformed picture scaled to fit `max_long`.
    pub width: u32,
    pub height: u32,
    /// PNG, BMP and TIFF only. A JPEG screen save decodes and re-encodes at
    /// quality 85, which is a generation of compression — the caller must
    /// name that in its notice rather than calling it lossless.
    pub lossless: bool,
}

/// As [`save_transformed`], but downscaled to fit `max_long` px on the long
/// side instead of kept full-res — the "save what is on screen" to Ctrl+S's
/// "save the file". Same crop and turn, same naming rule apart from the
/// `_screen` infix, same refusals.
///
/// JPEG cannot be lossless here: there is no DCT-domain downscale to an
/// arbitrary size, so it decodes, downscales and re-encodes. That is the
/// behaviour the user asked for by asking for screen size rather than a bug,
/// and `lossless == false` is how the caller knows to say "re-encoded".
/// WebP, GIF, HEIC and JXL are still refused outright: a screen-size save of
/// those would be a *quiet* second generation, which is what the refusal is
/// for. Never upscales: a picture smaller than the display is written as-is.
pub fn save_transformed_scaled(
    src: &Path,
    user: Xform,
    crop: Option<Crop>,
    max_long: u32,
) -> Result<ScaledSave, SaveError> {
    let dest = unique_dest_named(src, "screen");
    save_transformed_to(src, user, crop, &dest, DEFAULT_JPEG_QUALITY, max_long)
}

/// The JPEG quality both re-encode paths use when the caller has no opinion.
///
/// **85, and it is stated here rather than tuned here**, because this path is
/// lossy by construction and the number only moves artefacts around. It is the
/// de-facto viewer default (JPEGView's `JPEGSaveQuality`) and it was the
/// number [`save_transformed_scaled`] already used, so naming it once is what
/// keeps the screen-save and the save-as from drifting apart.
pub const DEFAULT_JPEG_QUALITY: u8 = 85;

/// Write the current edit to `dest`, re-encoded, at a chosen quality and size.
///
/// **The destination's extension picks the format**, not the source's: a PNG
/// saved to `out.jpg` is written as JPEG, because the alternative is a file
/// whose extension lies about its contents — and a viewer that writes one is
/// worse than useless, since the *next* program to open it fails on a file that
/// looks fine in a file listing.
///
/// `max_long` of 0 means "do not resize"; anything else is a bound on the long
/// side and **never upscales**, so a 256px source asked for 1000 stays 256px.
/// Upscaling would invent pixels nobody had, and this codebase's whole premise
/// is that what you see is what the file has.
///
/// `quality` is used for JPEG only. PNG and WebP-lossless are lossless formats
/// and the number is ignored rather than being an error, so a dialog that
/// always shows the slider does not have to hide it per format.
///
/// **Never overwrites.** `Ctrl+S` picks a `_rotated` name that does not exist;
/// save-as is the path where the user named the destination, so the *user's*
/// intent is already "this exact path", and if it exists the honest thing is
/// to report it rather than silently write `_rotated2` somewhere they are not
/// looking. The caller has already asked the user to confirm.
pub fn save_transformed_to(
    src: &Path,
    user: Xform,
    crop: Option<Crop>,
    dest: &Path,
    quality: u8,
    max_long: u32,
) -> Result<ScaledSave, SaveError> {
    let bytes = std::fs::read(src).map_err(SaveError::Read)?;
    let fmt = format_for_dest(dest)?;
    let lossless = fmt != image::ImageFormat::Jpeg;
    let is_jpeg = is_jpeg_bytes(&bytes);
    // `image`'s JPEG decoder does not apply EXIF orientation, so these are
    // stored pixels and `display_xform` is the same composition the lossless
    // path uses — the crop below is in display space either way.
    let exif_tag = if is_jpeg { exif_orientation(src) } else { 0 };
    let xform = display_xform(exif_tag, user);
    let img = image::load_from_memory(&bytes).map_err(|e| SaveError::Decode(format!("{e}")))?;
    let (w, h) = (img.width(), img.height());
    let rgba = img.to_rgba8();
    let crop = match crop {
        Some(c) if !c.is_full(w, h) => Some(
            c.clamped_to(w, h)
                .ok_or_else(|| SaveError::Decode("crop fell outside the image".into()))?,
        ),
        _ => None,
    };
    let (cw, ch, cropped) = match crop {
        Some(c) => c
            .apply_rgba(w, h, &rgba.into_raw())
            .ok_or_else(|| SaveError::Decode("crop fell outside the image".into()))?,
        None => (w, h, rgba.into_raw()),
    };
    let (ow, oh, px) = xform
        .apply_rgba(cw, ch, &cropped)
        .ok_or_else(|| SaveError::Decode("buffer did not match its dimensions".into()))?;
    // **`max_long == 0` means no resize at all**, and that is not the same as a
    // resize to 1px: `downscale_image` treats its argument as a target, so
    // `max_long.max(1)` on a 0 would quietly shrink every save-as to a
    // thumbnail. The scale is therefore computed here rather than handed over,
    // so "leave it alone" is a value the pipeline can actually express.
    let scaled = if max_long == 0 || ow.max(oh) <= max_long {
        ImageData {
            width: ow,
            height: oh,
            rgba: px,
            load_time: std::time::Duration::ZERO,
        }
    } else {
        downscale_image(
            &ImageData {
                width: ow,
                height: oh,
                rgba: px,
                load_time: std::time::Duration::ZERO,
            },
            max_long,
        )
    };
    let (sw, sh) = (scaled.width, scaled.height);
    let mut buf = Vec::new();
    if fmt == image::ImageFormat::Jpeg {
        // JPEG has no alpha: drop it the same way every 32→24-bit save path
        // here does, rather than asking the encoder to guess.
        let rgb: Vec<u8> = scaled
            .rgba
            .chunks_exact(4)
            .flat_map(|px| [px[0], px[1], px[2]])
            .collect();
        let mut cursor = std::io::Cursor::new(&mut buf);
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality);
        enc.encode(&rgb, sw, sh, image::ExtendedColorType::Rgb8)
            .map_err(|e| SaveError::Decode(format!("re-encoding failed: {e}")))?;
    } else {
        let out_img = image::RgbaImage::from_raw(sw, sh, scaled.rgba)
            .ok_or_else(|| SaveError::Decode("downscaled buffer was malformed".into()))?;
        let mut cursor = std::io::Cursor::new(&mut buf);
        // **`quality` is deliberately not passed here, and that is not an
        // oversight.** `image` 0.25 has one JPEG encoder with a quality knob
        // and no lossy encoder for anything else, so a WebP "quality 40" would
        // be a lossless WebP wearing a label that says otherwise — and the
        // `lossless` flag below would then be the only honest thing in the
        // result. Silently honouring the number on one format and ignoring it
        // on the others, without saying which, is worse: the user sets 40 for
        // file size and gets the opposite, with nothing to tell them.
        //
        // So PNG, BMP and TIFF are genuinely lossless, WebP is lossless too,
        // and the quality slider is JPEG's. This is the "a setting that does
        // nothing on most formats" problem, and the honest fix is for the
        // dialog to say so rather than for the encoder to pretend.
        image::DynamicImage::ImageRgba8(out_img)
            .write_to(&mut cursor, fmt)
            .map_err(|e| SaveError::Decode(format!("re-encoding failed: {e}")))?;
    }
    // EXIF orientation is baked into the pixels now, so carry the tag reset
    // the lossless path does — for JPEG only, where a tag may exist. **Only when
    // the destination is a JPEG**: writing an orientation tag into a PNG that
    // has no Exif block to begin with would be a corrupt file, and the old code
    // got this right only because the destination always matched the source.
    if is_jpeg && fmt == image::ImageFormat::Jpeg {
        neutralize_exif_orientation(&mut buf);
    }
    std::fs::write(dest, &buf).map_err(SaveError::Write)?;
    Ok(ScaledSave {
        dest: dest.to_path_buf(),
        width: sw,
        height: sh,
        lossless,
    })
}

/// The output format, decided by the destination's extension.
///
/// **An extension we cannot encode is an error, not a guess.** Falling back to
/// PNG would write correct pixels under a name that says otherwise, and
/// guessing JPEG would do the same for a `.tga`; either way the user finds out
/// from another program, not from here.
///
/// WebP is deliberately encoded as **lossless**: `image` 0.25 has no lossy
/// WebP encoder, and a "lossy" WebP that is secretly lossless would make the
/// `lossless` flag on the result a lie in the one direction that costs the user
/// nothing (a bigger file) rather than the one that costs them a generation of
/// compression.
fn format_for_dest(dest: &Path) -> Result<image::ImageFormat, SaveError> {
    let ext = dest
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Ok(image::ImageFormat::Jpeg),
        "png" => Ok(image::ImageFormat::Png),
        "webp" => Ok(image::ImageFormat::WebP),
        "bmp" => Ok(image::ImageFormat::Bmp),
        "tif" | "tiff" => Ok(image::ImageFormat::Tiff),
        "" => Err(SaveError::Unsupported(
            "the destination has no extension, so there is no format to write".into(),
        )),
        other => Err(SaveError::Unsupported(format!(
            "cannot write .{other} - choose .jpg, .png, .webp, .bmp or .tiff"
        ))),
    }
}

/// The infix a saved copy is named for: what the file **is**, from the resolved
/// values rather than the caller's.
///
/// `_cropped` if a crop survived, else `_rotated` if the composed transform is
/// not the identity, else `_copy`.
///
/// **Named for what happened, so an unedited save is a `_copy` and never a
/// `_rotated`.** This was a reported bug and the sharpest version of "read the
/// value at the wrong level": the save-as panel picked its infix from the
/// *user's* edit (`crop.is_some() ? "cropped" : "rotated"`), which cannot
/// distinguish "turned" from "untouched", so an unedited image was named
/// `_rotated` while `Ctrl+S` — running the identical rule through
/// [`save_transformed`] — named the very same picture `_copy`.
///
/// **Two things must be composed before this is asked, and both callers do it.**
/// A crop that covers the whole picture is not a crop (the save path resolves
/// that to `None` first), and `xform` must be the *composed* transform from
/// [`display_xform`], which folds in the EXIF tag — so a phone photo stored
/// sideways is `_rotated` on its own, and "Identity" means nothing turned
/// *whatever was asked for*. Passing a raw `Xform` here is the mistake, and it
/// is the one that produced the report.
///
/// It is a function rather than inline code because two features now name files
/// — `Ctrl+S` and save-as — and a rule duplicated between them is a rule that
/// will answer differently to the same question.
pub fn save_infix(xform: Xform, crop: Option<Crop>) -> &'static str {
    if crop.is_some() {
        "cropped"
    } else if xform != Xform::Identity {
        "rotated"
    } else {
        "copy"
    }
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

/// Two stacks per path: what has been done, and what can be redone.
///
/// **Per path, not one global list**, and that is the whole of the design
/// decision. Undo means "undo on *this* picture": someone who turns three photos
/// and then walks back to the first expects undo there to undo that photo's turn,
/// not to walk them through all three in reverse. A single global stack answers a
/// question nobody asked and gets the common case wrong.
///
/// `record` takes the *current* edit and must be called **before** the change, so
/// `undo` can hand back the state the image was in before it. That ordering is
/// the interface's contract and the reason a caller cannot get it wrong by
/// forgetting a "previous value" argument: there isn't one to pass.
///
/// Recording also clears redo for that path, because a new edit has rewritten
/// the future. That is what makes redo's absence after a new edit correct rather
/// than a bug.
///
/// The entries are whole [`Edit`]s rather than *operations*: pressing "rotate"
/// twice and then undoing once must land back on the single rotated picture, not
/// on an intermediate half-turn, and only storing states can say that. The price
/// is the ordering rule above.
#[derive(Debug, Clone)]
pub struct EditHistory {
    /// Recorded states for each path, oldest first, so the cap drops the deepest
    /// one by index rather than by searching.
    past: HashMap<PathBuf, Vec<Edit>>,
    /// States pushed aside by [`EditHistory::undo`]. A stack, not a log: undo
    /// pushes and redo pops from the same end.
    redo: HashMap<PathBuf, Vec<Edit>>,
    /// Steps kept **per path**. Bounded by count because a viewer left open over
    /// a large folder would otherwise grow this for every edit on every picture,
    /// and an `Edit` is a whole crop plus a transform.
    cap: usize,
}

impl Default for EditHistory {
    fn default() -> Self {
        Self {
            past: HashMap::new(),
            redo: HashMap::new(),
            cap: 50,
        }
    }
}

impl EditHistory {
    /// Push `current` — the state the image is in **right now** — onto `path`'s
    /// undo stack, so the change about to happen can be undone later. Recording
    /// for one path discards that path's redo stack.
    ///
    /// Over the cap, the **oldest** entry goes. Refusing the newest instead would
    /// mean a rotate that silently does not become undoable once the history is
    /// full, which is far worse than forgetting a step from an hour ago.
    pub fn record(&mut self, path: &Path, current: Edit) {
        let past = self.past.entry(path.to_path_buf()).or_default();
        past.push(current);
        if past.len() > self.cap {
            past.remove(0);
        }
        self.redo.remove(path);
    }

    /// Is there a recorded step for `path` to walk back through?
    pub fn can_undo(&self, path: &Path) -> bool {
        self.past.get(path).is_some_and(|v| !v.is_empty())
    }

    /// Is there a step for `path` to walk forward through? False both when there
    /// was never an undo here and when a later edit threw the future away.
    pub fn can_redo(&self, path: &Path) -> bool {
        self.redo.get(path).is_some_and(|v| !v.is_empty())
    }

    /// Undo one step on `path`, returning the state to restore, or `None` when
    /// there is nothing recorded. `current` — the state as it stands now — goes
    /// onto the redo stack, which is what makes `redo` a mirror rather than a
    /// second, separately-kept answer.
    ///
    /// `None` leaves both stacks alone: a key press is not a promise that
    /// anything is there, so an undo with nothing to undo is a no-op rather than
    /// an error. It is also why this cannot be written as an index the caller
    /// passes — the empty case has to be answerable without the caller having
    /// checked first.
    pub fn undo(&mut self, path: &Path, current: Edit) -> Option<Edit> {
        let previous = self.past.get_mut(path)?.pop()?;
        self.redo
            .entry(path.to_path_buf())
            .or_default()
            .push(current);
        Some(previous)
    }

    /// The mirror of [`EditHistory::undo`]: `current` goes back onto the undo
    /// stack and the popped redo entry is the state to restore. `None` when the
    /// future is empty, which is the ordinary answer after a new edit.
    pub fn redo(&mut self, path: &Path, current: Edit) -> Option<Edit> {
        let next = self.redo.get_mut(path)?.pop()?;
        self.past
            .entry(path.to_path_buf())
            .or_default()
            .push(current);
        Some(next)
    }

    /// How many steps `path` can currently undo — for tests and diagnostics.
    pub fn len_for(&self, path: &Path) -> usize {
        self.past.get(path).map_or(0, Vec::len)
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
    // HEIC and AVIF keep their colour in an ISO-BMFF `colr` box, which the `heic`
    // crate reads for us. Sniffed here rather than passed in, so a caller that
    // knows only "not a JPEG, not a PNG" still gets a real answer.
    if is_heif_bytes(bytes) {
        return heif_color_info(bytes);
    }
    // A GIF has no colour profile chunk at all — its colours come from a
    // palette. UntaggedSrgb by the same convention untagged PNG and JPEG get,
    // not NotInspected.
    if is_gif_bytes(bytes) {
        return ColorInfo::plain(ColorSpace::UntaggedSrgb);
    }
    // A WebP has no ICC profile chunk at all - its colours come from the
    // VP8/VP8L bitstream. The honest answer is UntaggedSrgb, by the same
    // convention untagged PNG and JPEG already get, not NotInspected.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return ColorInfo::plain(ColorSpace::UntaggedSrgb);
    }
    ColorInfo::not_inspected()
}

/// Read a HEIC/AVIF's `colr` box and classify whatever profile is in it.
///
/// `DecoderConfig::extract_icc` is the whole reason this is not a container walk:
/// it returns the raw ICC bytes, or `None` when the file states its colour with
/// **`nclx` parameters instead of a profile**. That distinction is the reason the
/// answer is not "assume sRGB": an `nclx` file is a real possibility, and iPhone
/// HEIC is frequently Display P3 through exactly that route, so assuming would
/// report a wide-gamut photo as sRGB — which is the failure this whole stage
/// exists to prevent. `None` stays [`ColorSpace::NotInspected`].
///
/// Verified on real files: of four HEIF/AVIF images, two carry a 536-byte ICC
/// profile that classifies as Display P3 from its primaries with **no `desc`
/// tag**, and two have no profile at all.
fn heif_color_info(bytes: &[u8]) -> ColorInfo {
    match heic::DecoderConfig::new().extract_icc(bytes) {
        Ok(Some(profile)) => classify_icc(&profile),
        Ok(None) => ColorInfo::not_inspected(),
        // A container we cannot parse is not a reason to claim anything about
        // its colour.
        Err(_) => ColorInfo::not_inspected(),
    }
}

/// [`color_info_from_bytes`] for a file on disk.
pub fn color_info(path: &Path) -> ColorInfo {
    let Ok(bytes) = std::fs::read(path) else {
        return ColorInfo::plain(ColorSpace::Unknown);
    };
    let is_jpeg = is_jpeg_bytes(&bytes);
    let is_png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    color_info_from_bytes(&bytes, is_jpeg, is_png)
}

/// Classify every image in `folder`, in natural name order.
///
/// Exists so a library can be audited from a command line instead of by clicking
/// through the metadata panel one file at a time, which is the only other way to
/// reach [`color_info`]. It is a pure function of the folder, so it is testable;
/// the presentation is the caller's.
///
/// **This reads every file whole**, because [`color_info`] does, and for a folder
/// of 1000 images that is a few tens of megabytes and a few seconds. Fine for a
/// command somebody asked for, wrong for anything on a startup path.
pub fn scan_colour(folder: &Path) -> Vec<(PathBuf, ColorInfo)> {
    scan_folder_sorted(folder, SortMode::Name)
        .into_iter()
        // Archives are listed by a folder scan and are **not** auditable by
        // this: `color_info` reads the whole file, so auditing a 200 MB `.cbz`
        // would pull all of it into memory to report that a container has no
        // colour profile of its own. The images inside it are audited when it
        // is opened.
        .filter(|p| !is_archive_path(p))
        .map(|p| {
            let info = color_info(&p);
            (p, info)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Wide-gamut to sRGB: Display P3, opt-in
// ---------------------------------------------------------------------------
//
// The renderer uploads `ColorImage` as `Rgba8UnormSrgb` and egui wants a
// gamma-space surface, so the pipeline is 8-bit sRGB end to end whatever the file
// says. A Display P3 photo therefore displays without conversion and reads
// flatter than it should — 229 of 1001 images in a real library. This is the
// conversion, and it is **off by default**: it changes how photos look, so it is
// something to ask for rather than something to inherit.
//
// Two things that are easy to get wrong and are therefore spelled out:
//
// * **The primaries are derived, not transcribed.** The matrix below is built
//   from the two spaces' chromaticities. Hand-copying a published matrix is the
//   `Xform::Transverse` failure mode: it compiles, it type-checks, and it is
//   wrong in a way nothing catches. `the_derived_matrix_reproduces_the_icc_primaries_we_measured`
//   checks the derivation against the D50-adapted primary read out of 192 real
//   P3 profiles, which is a fact about files rather than about this code.
// * **The conversion happens in linear light.** Applying the matrix to gamma-space
//   values is the classic mistake, and it produces dark, muddy images rather than
//   an error. Hence the two transfer-function LUTs either side of it.

/// Whether wide-gamut pixels are converted to sRGB as they are decoded.
///
/// A process-wide toggle like `set_linear_resample`, and for the same reasons:
/// it is a decode-time decision, the workers are already threads, and plumbing a
/// parameter through every decode entry point would put the setting in the way of
/// five call sites that each have to remember it.
static WIDE_GAMUT_CONVERSION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Turn conversion of Display P3 pixels to sRGB on or off. **Default: off.**
///
/// Takes effect on the next decode; like the linear-light toggle it does not
/// re-decode what is already cached, so switch it on and move to another image
/// (or restart) to see it everywhere.
pub fn set_wide_gamut_conversion(on: bool) {
    WIDE_GAMUT_CONVERSION.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the conversion on? Mostly for tests and the settings panel.
pub fn wide_gamut_conversion() -> bool {
    WIDE_GAMUT_CONVERSION.load(std::sync::atomic::Ordering::Relaxed)
}

/// sRGB's primaries as CIE 1931 chromaticities (x, y).
pub const SRGB_XY: [(f64, f64); 3] = [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)];
/// Display P3's primaries, likewise. Same blue as sRGB; wider red and green.
pub const P3_XY: [(f64, f64); 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];
/// Both spaces are referred to D65, which is why converting between them needs no
/// chromatic adaptation: the white point is shared, and the primaries are the only
/// thing that differs. (The D50 in `P3_D50_RED` is an *ICC* convention for how
/// profiles store primaries, and it is what the detection side compares against —
/// see `the_derived_matrix_reproduces_the_icc_primaries_we_measured`.)
pub const D65_XY: (f64, f64) = (0.3127, 0.3290);

/// CIE xyY (Y = 1) to XYZ.
pub fn xy_to_xyz(xy: (f64, f64)) -> [f64; 3] {
    let (x, y) = xy;
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// The RGB→XYZ matrix for a set of primaries and a white point.
///
/// Standard derivation: each primary becomes a column of XYZ scaled to Y = 1,
/// then the white point is expressed in that basis and folded back in as the
/// per-column scale.
pub fn rgb_to_xyz(primaries: [(f64, f64); 3], white: (f64, f64)) -> [[f64; 3]; 3] {
    // Rows are components, columns are primaries — that is the orientation the
    // colour equation is written in, and getting it wrong solves the *transposed*
    // system, which has an answer, just not this one. It is worth spelling out
    // because the failure is invisible: the result is a full set of plausible
    // numbers, `invert3` still round-trips to the identity, and white ends up
    // somewhere other than white.
    let mut m = [[0.0; 3]; 3];
    for (c, xy) in primaries.iter().enumerate() {
        let xyz = xy_to_xyz(*xy);
        for r in 0..3 {
            m[r][c] = xyz[r];
        }
    }
    // Solve M * s = white, then scale each column by its own s.
    let s = mul_vec(&invert3(&m), xy_to_xyz(white));
    for r in 0..3 {
        for c in 0..3 {
            m[r][c] *= s[c];
        }
    }
    m
}

fn mul_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for r in 0..3 {
        out[r] = m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2];
    }
    out
}

fn mul_mat(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    mul_mat_ref(a, b)
}

/// [`mul_mat`] by value, so the tests can use it without cloning.
pub fn mul_mat_ref(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    out
}

/// [`p3_to_srgb_matrix`], inverted: linear sRGB → linear Display P3.
///
/// Present so the round trip can be asserted, and so the matrix can be undone.
pub fn srgb_to_p3_matrix() -> [[f64; 3]; 3] {
    invert3(p3_to_srgb_matrix())
}

/// Inverse of a 3×3 by cofactors. Adequate here because the inputs are
/// well-conditioned colour matrices; a general solver would be a dependency for
/// one call site. Public because the ICC cross-check in `tests/color_profiles.rs`
/// needs to invert the Bradford matrix.
/// Inverse of a 3×3: the adjugate over the determinant.
///
/// Written as "cofactor at (k, r)" rather than a flat list of terms, because the
/// flat form has the off-diagonal entries in **transposed** order and the
/// transposition is invisible — it type-checks, it returns plausible numbers, and
/// `inv * m` is *not* the identity, so everything downstream is wrong by a few
/// percent while still looking like a colour matrix. `rows.zip(cols)` also
/// deliberately preserves order rather than sorting, which a filter-map over a
/// range would not.
pub fn invert3(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    // Signed minor: element (row, col) of the cofactor matrix.
    let cof = |row: usize, col: usize| -> f64 {
        let rows: Vec<usize> = (0..3).filter(|r| *r != row).collect();
        let cols: Vec<usize> = (0..3).filter(|c| *c != col).collect();
        let minor = m[rows[0]][cols[0]] * m[rows[1]][cols[1]]
            - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
        if (row + col) % 2 == 0 {
            minor
        } else {
            -minor
        }
    };
    // Expand along the first row: det = sum_j m[0][j] * C[0][j].
    let det = m[0][0] * cof(0, 0) + m[0][1] * cof(0, 1) + m[0][2] * cof(0, 2);
    let inv = 1.0 / det;
    let mut out = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            // adj[r][c] == C[c][r]: the adjugate is the transpose of the
            // cofactor matrix. Getting this backwards is the whole bug.
            out[r][c] = cof(c, r) * inv;
        }
    }
    out
}

/// The linear-light Display P3 → sRGB matrix, derived once.
///
/// Equals the matrix published as `lin_display_p3_to_lin_srgb`, and is built here
/// from the primaries instead so that it cannot drift from them.
pub fn p3_to_srgb_matrix() -> &'static [[f64; 3]; 3] {
    static M: std::sync::OnceLock<[[f64; 3]; 3]> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let srgb = rgb_to_xyz(SRGB_XY, D65_XY);
        let p3 = rgb_to_xyz(P3_XY, D65_XY);
        mul_mat(&invert3(&srgb), &p3)
    })
}

/// LUT resolution for the transfer functions.
///
/// 12 bits, matching the resampler's LUTs. The quantisation is in *linear* light,
/// so the error it introduces is at most one index step of the encode LUT, which
/// is under 1/255 anywhere the encode curve is steep enough to matter.
const TF_STEPS: usize = 4096;

/// sRGB electro-optical transfer function, exact.
fn srgb_to_linear_exact(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB opto-electronic transfer function, exact.
fn linear_to_srgb_exact(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_to_linear_lut() -> &'static [f32; TF_STEPS] {
    static L: std::sync::OnceLock<[f32; TF_STEPS]> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        let mut l = [0f32; TF_STEPS];
        for (i, v) in l.iter_mut().enumerate() {
            *v = srgb_to_linear_exact(i as f64 / (TF_STEPS - 1) as f64) as f32;
        }
        l
    })
}

fn linear_to_srgb_lut() -> &'static [f32; TF_STEPS] {
    static L: std::sync::OnceLock<[f32; TF_STEPS]> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        let mut l = [0f32; TF_STEPS];
        for (i, v) in l.iter_mut().enumerate() {
            *v = (linear_to_srgb_exact(i as f64 / (TF_STEPS - 1) as f64) * 255.0).round() as f32;
        }
        l
    })
}

/// Convert 8-bit sRGB-encoded Display P3 pixels to sRGB, in place.
///
/// Alpha is untouched. Out-of-gamut results are clamped, which is the only thing
/// that *can* be done with them: a saturated P3 primary has no sRGB equivalent, and
/// clipping it is what every other tool does. This is lossy for those colours by
/// construction — which is the honest argument for the setting being off by
/// default.
pub fn convert_wide_gamut_rgba(rgba: &mut [u8]) {
    let m = p3_to_srgb_matrix();
    let dec = srgb_to_linear_lut();
    let enc = linear_to_srgb_lut();
    // The LUTs are `TF_STEPS` wide and the samples are 8-bit, so an index has to
    // be scaled across. Reading `dec[v]` directly decodes 255 as 255/4095 — about
    // 6% grey — which turns every white pixel dark grey and every photo into a
    // silhouette. The `* (TF_STEPS - 1) / 255` is the whole fix.
    let to_index = |v: u8| (v as usize * (TF_STEPS - 1)) / 255;
    let last = (TF_STEPS - 1) as f32;

    // Rayon, like the resampler: this runs on the decode path and a full-res
    // 24 MP buffer is 96 MB of pixels, which is 600 ms on one thread. The
    // per-pixel work is identical either way — no reduction, no ordering — so the
    // parallel version cannot differ from the serial one.
    rgba.par_chunks_mut(4).for_each(|px| {
        let r = dec[to_index(px[0])];
        let g = dec[to_index(px[1])];
        let b = dec[to_index(px[2])];
        // The matrix itself: linear P3 -> linear sRGB. Out-of-range on purpose.
        let lr = m[0][0] as f32 * r + m[0][1] as f32 * g + m[0][2] as f32 * b;
        let lg = m[1][0] as f32 * r + m[1][1] as f32 * g + m[1][2] as f32 * b;
        let lb = m[2][0] as f32 * r + m[2][1] as f32 * g + m[2][2] as f32 * b;
        for (slot, v) in [(0, lr), (1, lg), (2, lb)] {
            let v = v.clamp(0.0, 1.0) * last;
            px[slot] = enc[v as usize].clamp(0.0, 255.0) as u8;
        }
    });
}

/// Convert `out` in place, but only if the setting is on and the file really is
/// wide gamut.
///
/// **Both checks are after the early return**, so the default costs nothing at
/// all: no second read of the file, no classification, no arithmetic. `color_info`
/// reads the file again, which is free against the page cache but not free, and
/// there is no reason for an opt-in feature to pay that when it is off.
pub(crate) fn apply_wide_gamut_conversion(path: &Path, out: &mut PreviewAndFull) {
    if !wide_gamut_conversion() {
        return;
    }
    // `WideGamut` and nothing else. A profile we could not classify must not be
    // pushed through a matrix on a guess, and an untagged file is already sRGB.
    if color_info(path).space != ColorSpace::WideGamut {
        return;
    }
    convert_wide_gamut_rgba(&mut out.preview.rgba);
    if let Some(full) = out.full.as_mut() {
        convert_wide_gamut_rgba(&mut full.rgba);
    }
    if let Some(anim) = out.anim.as_mut() {
        for frame in &mut anim.frames {
            convert_wide_gamut_rgba(&mut frame.rgba);
        }
    }
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

/// First JPEG COM segment in `bytes`, as text.
///
/// Walks markers from SOI and stops at SOS — scan data follows it, so anything
/// shaped like a marker past it is pixels, not segments — at EOI, or at the
/// first malformed length. Every read is bounds-checked: this parses untrusted
/// input, so a truncated file is `None`, never a panic on a worker.
pub fn jpeg_comment(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 1 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        // Skip fill bytes: any number of 0xFF is one marker prefix.
        while i < bytes.len() && bytes[i] == 0xFF {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let marker = bytes[i];
        i += 1;
        match marker {
            // Standalone markers carry no length.
            0xD8 | 0xD9 | 0x01 | 0xD0..=0xD7 => {
                if marker == 0xD9 {
                    return None;
                }
            }
            // Start of scan: pixel data follows, so no segment past here.
            0xDA => return None,
            _ => {
                if i + 2 > bytes.len() {
                    return None;
                }
                let len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
                if len < 2 || i + len > bytes.len() {
                    return None;
                }
                if marker == 0xFE {
                    if let Some(text) = clean_comment_text(&bytes[i + 2..i + len]) {
                        return Some(text);
                    }
                }
                i += len;
            }
        }
    }
    None
}

/// EXIF UserComment payload: an 8-byte charset code (`ASCII\0\0\0`,
/// `UNICODE\0`, …) followed by the text. Unknown codes are read as bytes —
/// most writers put ASCII there whatever the code says — and a payload that
/// is empty after cleaning is no comment, not an empty line.
pub fn decode_user_comment(payload: &[u8]) -> Option<String> {
    if payload.len() < 8 {
        return clean_comment_text(payload);
    }
    let (code, text) = payload.split_at(8);
    if code == b"UNICODE\0" {
        let units: Vec<u16> = text
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s: String = char::decode_utf16(units.iter().copied())
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        clean_comment_text(s.as_bytes())
    } else {
        clean_comment_text(text)
    }
}

/// Comment for the metadata panel: ImageDescription first, else UserComment.
/// One line — a comment that is empty after cleaning is absent, not blank.
pub fn exif_comment(exif: &nom_exif::Exif) -> Option<String> {
    if let Some(s) = exif
        .get(nom_exif::ExifTag::ImageDescription)
        .and_then(|v| v.as_str())
        .and_then(|s| clean_comment_text(s.as_bytes()))
    {
        return Some(s);
    }
    exif
        .get(nom_exif::ExifTag::UserComment)
        .and_then(|v| v.as_undefined())
        .and_then(decode_user_comment)
}

/// Text out of a comment payload: UTF-8 where it is valid, Latin-1 mapped
/// where it is not (a camera writing Latin-1 is ordinary, and lossy mojibake
/// beats dropping the line), trimmed of NULs and whitespace.
fn clean_comment_text(bytes: &[u8]) -> Option<String> {
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    };
    let s = s.trim_matches(['\0', ' ', '\t', '\r', '\n']).to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
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

/// Full-resolution decode of `path`, for the zoom/pan tiles and the clipboard.
///
/// `page` is 0 for everything but a page of a multi-page TIFF, and is threaded
/// into the same page-aware path the preview uses rather than having a second
/// implementation that could disagree with it.
pub fn load_image_full(path: &Path, page: u32) -> Result<ImageData, LoadError> {
    load_page_preview_full(path, MAX_TEXTURE_DIM, page).map(|r| {
        // The preview is already the full image whenever the source fitted under
        // the texture cap, which is the only way a full decode is asked for.
        r.full.unwrap_or(r.preview)
    })
}

/// True if the bytes look like a JPEG (magic bytes), regardless of file name.
pub fn is_jpeg_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// Image dimensions sniffed from the file content, not the extension.
/// Cheap: only the header is parsed.
///
/// Also the reason [`page_dims`] can read a prefix instead of the whole file: a
/// stream too short to carry a format's dimension marker is a `None` here rather
/// than a wrong number, so a header that falls outside the prefix costs one
/// fallback read instead of a mismeasurement. See [`PROBE_PREFIX_BYTES`].
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

/// What a decode is *for*, which decides how much of it is kept.
///
/// A grid cell and the main view asked for the same function with the same target
/// and used to get the same answer: a full-resolution buffer, *retained and sent
/// back to the UI thread*, for a 150 px cell. For a 25 MP PNG that is 100 MB
/// crossing a channel to be dropped, and for a 17000 px scan it is a 5851 KB GPU
/// texture — 512 of which is the thumbnail cache's ceiling, so 2.9 GB. The cell
/// genuinely never wants any of it, so the request now says so.
///
/// What this does **not** claim: that a cell never allocates at source size. For
/// PNG, HEIC, JXL and TIFF the only reduced-size read the decoders in this tree
/// offer is "decode it all, then resize", and that full-size buffer is still
/// allocated inside `dynamic_to_image_data` and thrown away there. Changing that
/// needs a row-streaming decode per format and is not attempted here; what is gone
/// is the *retention*, the second downscale, and the 44x oversized texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// The image on screen. `full` may be filled, and it rides along on the
    /// preview so a second decode is not needed to zoom.
    View,
    /// One grid cell: a small picture, nothing else. Never returns `full` or
    /// `anim`, and the result is at most the requested target on its long side.
    Thumb,
}

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

/// A format that can carry more than one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimatedFormat {
    Gif,
    /// Animated PNG. A *plain* PNG is the same container with no `acTL` chunk, so
    /// "is this animated" is a question about the file and not the extension.
    Png,
    /// Animated WebP. Same story: a still WebP is a VP8/VP8L still, not an ANIM
    /// chunk.
    Webp,
}

/// The animated format these bytes are, if any.
///
/// Content-based like every other format decision here, so a PNG saved as `.jpg`
/// still animates. Cheap: magic bytes only, no header parse, no decode.
pub fn animated_format(bytes: &[u8]) -> Option<AnimatedFormat> {
    if is_gif_bytes(bytes) {
        return Some(AnimatedFormat::Gif);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(AnimatedFormat::Png);
    }
    // RIFF container, and the form type at offset 8 has to be WEBP. Checking the
    // "RIFF" alone would match WAV and AVI.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(AnimatedFormat::Webp);
    }
    None
}

/// Upper bound on the pages walked in one TIFF.
///
/// A corrupt or hostile file can chain IFDs indefinitely, and counting them is a
/// `next_image()` per page. 4096 is far past any real document, and the point of
/// the cap is that the walk terminates on *this* machine rather than on whatever
/// the file claims.
pub const MAX_TIFF_PAGES: u32 = 4096;

/// How many pages this file has — 1 for anything that is not a readable TIFF.
///
/// **This runs on every file you navigate to**, so the non-TIFF case has to be
/// nearly free, and it is: four bytes are read to check the magic, and a
/// photo folder is not TIFFs. The first version of this read the whole file with
/// `fs::read` before sniffing, which would have meant pulling a 10 MB JPEG into
/// memory every single time you pressed the right arrow.
///
/// **Still not called during a folder scan.** A scan would open every file in the
/// folder, and start-up is the thing this project spends the most effort on. The
/// gallery remembers the answer, so this is paid once per file visited rather
/// than once per visit.
///
/// Returns 1 rather than an error for everything unreadable, because "not a
/// multi-page TIFF" and "a TIFF we cannot read" mean the same thing to every
/// caller here: one page.
pub fn tiff_page_count(path: &Path) -> u32 {
    // Four bytes, then give up. This is the whole reason the function is safe to
    // call on every navigation.
    let mut magic = [0u8; 4];
    if std::fs::File::open(path)
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut magic)
        })
        .is_err()
    {
        return 1;
    }
    if std::fs::File::open(path)
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut magic)
        })
        .is_err()
    {
        return 1;
    }
    if !is_tiff_bytes(&magic) {
        return 1;
    }
    // A TIFF's IFD chain is at the front of the file and `next_image` reads
    // directories rather than pixels, so this is a few KB of reads even for a
    // 50 MB scan. `BufReader` keeps it that way: the crate seeks a lot.
    let Ok(file) = std::fs::File::open(path) else {
        return 1;
    };
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::BufReader::new(file)) else {
        return 1;
    };
    // `Decoder::new` has already read the first IFD, so the count starts at one.
    let mut n = 1u32;
    while dec.more_images() && n < MAX_TIFF_PAGES {
        if dec.next_image().is_err() {
            // A truncated chain: report what was really there rather than
            // guessing, and rather than failing the file outright.
            break;
        }
        n += 1;
    }
    n
}

/// [`tiff_page_count`] over bytes already in hand, for callers that have the
/// whole file. Reads the first IFD chain out of the slice, so it costs nothing
/// beyond the header — the pixels are never touched.
pub fn tiff_page_count_bytes(bytes: &[u8]) -> u32 {
    if !is_tiff_bytes(bytes) {
        return 1;
    }
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)) else {
        return 1;
    };
    let mut n = 1u32;
    while dec.more_images() && n < MAX_TIFF_PAGES {
        if dec.next_image().is_err() {
            break;
        }
        n += 1;
    }
    n
}

/// How many bytes of a file [`page_dims`] reads.
///
/// 64 KB, chosen because every format's dimension marker sits near the front
/// *usually* rather than always: PNG's IHDR is at byte 8 and a GIF's screen
/// descriptor at byte 6, both fixed; a JPEG's SOF0 has to follow whatever metadata
/// segments precede it, and the spec caps a segment's payload at 65533 bytes — so a
/// camera that fills an APP1 right up to the limit puts SOF0 a few bytes **past**
/// this. This is therefore a budget that is nearly always sufficient and
/// provably not always sufficient, which is what the fallback below is for.
///
/// **A miss here is a `None`, never a wrong answer.** [`image_dims_bytes`] either
/// finds a format's marker or reports that it did not, so a header that happens to
/// sit further out falls through to a whole-file read rather than being measured
/// wrong. That distinction is the whole reason the prefix read can be aggressive:
/// the expensive path is only ever the price of being unlucky, and being unlucky
/// costs one read of a file that is rare.
///
/// The fallback is not claimed to be unreachable, and no test asserts that it is —
/// building a header past 64 KB to exercise it would mean writing a multi-megabyte
/// fixture to cover a branch that costs nothing to be correct about.
pub const PROBE_PREFIX_BYTES: usize = 65_536;

/// Read up to [`PROBE_PREFIX_BYTES`] from `path`.
///
/// **Tolerates a short file**, which is most of them: a preallocated buffer is
/// filled in a loop and truncated to what actually arrived, so a 300-byte PNG
/// comes back as 300 bytes rather than as an error. `read_exact` is the wrong tool
/// for that — it fails on a file shorter than the buffer, and a small image is not
/// a failure.
///
/// A loop rather than one `read`, because a single `read` is free to return short
/// on a file that is not short.
fn read_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; PROBE_PREFIX_BYTES];
    let mut filled = 0usize;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

/// [`image_dims_bytes`] over the whole file, for a header that did not fit in
/// [`PROBE_PREFIX_BYTES`]. Costs a `fs::read` of the file, so it is the fallback
/// and not the path.
fn image_dims_path(path: &Path) -> Option<(u32, u32)> {
    image_dims_bytes(&std::fs::read(path).ok()?)
}

/// The stored dimensions of page `page`, without decoding it.
///
/// **Stored, not display-oriented.** The EXIF tag is *not* applied here, and that
/// is deliberate. `decode_page` reads the *same* pair with the same
/// [`image_dims_bytes`] call and applies the tag itself, so `orig` and this agree by
/// construction rather than by convention — one source measurement, presented in two
/// forms, because the two callers each need a different one.
///
/// **So do not "fix" a caller by folding the tag in here.** The swap is its own
/// inverse, so a second application is not a doubled value to be noticed: it is the
/// identity, which quietly puts the *stored* pair back in the shape cache and
/// restores exactly the silent failure [`Gallery::learn_shapes`] exists to prevent.
/// Applying the tag is that function's job, and it is load-bearing rather than
/// cosmetic.
///
/// Header only. This runs on arrival at a file, so it has to cost about what
/// [`tiff_page_count`] costs rather than what a decode costs: a bounded read and a
/// format sniff. The whole-file fallback is the exception, and only for a header
/// that does not fit in the prefix.
///
/// `page > 0` is only meaningful for a TIFF, the one format in this tree with
/// pages, and it walks the IFD chain because a document's pages may differ in size.
/// Nothing else is asked for page > 0: an animation's frames all belong to one
/// file, and a later page of one is answered by the decode itself, which has that
/// page's real dimensions in hand.
pub fn page_dims(path: &Path, page: u32) -> Option<(u32, u32)> {
    page_dims_and_jpeg(path, page).map(|(dims, _)| dims)
}

/// [`page_dims`], plus whether the file is a JPEG.
///
/// One function rather than two because the JPEG flag falls out of the bytes the
/// read already returned, and the condition it gates is the one `decode_page`
/// applies the EXIF orientation under — so this is the file's answer, from the same
/// bytes as its dimensions, rather than a second sniff that could disagree with them.
///
/// **It does not save the whole open.** `learn_shapes` still opens the file again
/// for `exif_orientation`, because `nom_exif::read_exif` takes a path and not bytes.
/// That call is ~0.1-0.2 ms against a 10-17 ms `fs::read`, and it happens once per
/// file rather than once per visit, so this is still a net win — but the honest
/// version is "one read for the dimensions *and* the format", not "one read instead
/// of two, full stop".
///
/// The obvious follow-up is an `exif_orientation_bytes(&[u8])` over the prefix. Not
/// done here: `find_embedded_thumbnail` already walks the APP1 block in hand, so the
/// work would be to hand *that* walk's answer out rather than to reach for `nom_exif`
/// a third time.
fn page_dims_and_jpeg(path: &Path, page: u32) -> Option<((u32, u32), bool)> {
    if page > 0 {
        return tiff_page_dim(path, page).map(|dims| (dims, false));
    }
    let buf = read_prefix(path).ok()?;
    let jpeg = is_jpeg_bytes(&buf);
    image_dims_bytes(&buf)
        .or_else(|| image_dims_path(path))
        .map(|dims| (dims, jpeg))
}

/// Page `page`'s stored dimensions out of a TIFF's IFD chain, or `None`.
///
/// A walk with the same shape as [`tiff_page_count`]'s — the crate reads
/// directories, never pixels, and `BufReader` keeps it a few KB of reads on a
/// 50 MB scan. `Decoder::new` has already read IFD 0, so `page` further steps
/// land on the `page`th.
///
/// Bounded by the chain rather than by [`MAX_TIFF_PAGES`]: `more_images` is false
/// at the end, and the crate's own cycle detector rejects an IFD that points at
/// itself, so a caller passing an absurd `page` gets `None` rather than a walk
/// that does not terminate.
fn tiff_page_dim(path: &Path, page: u32) -> Option<(u32, u32)> {
    let mut dec = tiff::decoder::Decoder::new(std::io::BufReader::new(
        std::fs::File::open(path).ok()?,
    ))
    .ok()?;
    for _ in 0..page {
        if !dec.more_images() || dec.next_image().is_err() {
            return None;
        }
    }
    dec.dimensions().ok()
}

/// Decode page `page` (0-based) of a TIFF, downscaled to `max_dim`.
///
/// Goes through the `tiff` crate rather than `image`, and that is not a
/// preference: `image` 0.25's `TiffDecoder` implements only `ImageDecoder`, whose
/// `read_image` *consumes* the decoder, and it is not an `AnimationDecoder`. There
/// is no way to ask it for page 2. The `tiff` crate has `more_images` and
/// `next_image`, which is why it is a direct dependency.
///
/// The colour types handled are the ones that are a straight reinterpretation of
/// the decoded buffer, plus CMYK — which needs a real conversion and gets one by
/// asking `image` to do it. Palette, CMYKA, YCbCr, Lab and multiband pages are
/// **not**: applying a colormap or a colour transform correctly is `image`'s job
/// and there is no route to it for those types, while guessing would produce
/// pages that are subtly, silently the wrong colours — which for a scanned
/// document means invisible but for a photo means wrong. Those return an error,
/// which surfaces the same way any other decode failure does, rather than quietly
/// showing something else.
fn decode_tiff_page(
    bytes: &[u8],
    page: u32,
    max_dim: u32,
    start: std::time::Instant,
) -> Result<ImageData, LoadError> {
    // The `tiff` crate has its own error type and `LoadError` has no `From` for
    // it. Routing through `Other` keeps the message, which matters: a multi-page
    // file that cannot be read is otherwise an unexplained blank panel.
    fn tiff_err(e: tiff::TiffError) -> LoadError {
        LoadError::Other(format!("tiff: {e}"))
    }
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)).map_err(tiff_err)?;
    for _ in 0..page {
        if !dec.more_images() {
            return Err(LoadError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("page {page} requested, file has fewer"),
            )));
        }
        dec.next_image().map_err(tiff_err)?;
    }
    let (w, h) = dec.dimensions().map_err(tiff_err)?;
    let kind = dec.colortype().map_err(tiff_err)?;
    let result = dec.read_image().map_err(tiff_err)?;

    /// Straight reinterpretation of the decoded buffer, then the one conversion
    /// every page shares.
    ///
    /// Most types are a plain reinterpretation and nothing more. CMYK is not, and
    /// goes through `image` — see [`cmyk_via_image`]. Palette, CMYKA, YCbCr, Lab
    /// and multiband still return an error: there is no conversion we can hand to
    /// `image` for them, and approximating a colormap or a colour transform here
    /// would produce pages that are subtly and silently the wrong colours — for a
    /// photo, wrong; for a scanned document, invisible. An error is visible; that
    /// is the whole reason the refusal exists.
    ///
    /// The 16-bit buffers arrive as native-endian `u16` (the crate converts), so
    /// those are handled properly rather than reinterpreted from bytes.
    fn to_rgba8(
        w: u32,
        h: u32,
        kind: tiff::ColorType,
        result: tiff::decoder::DecodingResult,
    ) -> Result<image::RgbaImage, LoadError> {
        use tiff::ColorType as T;
        use tiff::decoder::DecodingResult as D;
        let bad = |what: &str| {
            LoadError::Other(format!(
                "TIFF page is {kind:?}, which is not supported past the first page ({what})"
            ))
        };
        let need = |got: usize, want: usize| -> Result<(), LoadError> {
            if got == want {
                Ok(())
            } else {
                Err(bad(&format!("{got} samples for {want} pixels")))
            }
        };
        let img: image::DynamicImage = match (kind, result) {
            (T::Gray(8), D::U8(b)) => {
                need(b.len(), (w * h) as usize)?;
                image::GrayImage::from_raw(w, h, b).ok_or_else(|| bad("bad size"))?.into()
            }
            (T::GrayA(8), D::U8(b)) => {
                need(b.len(), (w * h * 2) as usize)?;
                image::ImageBuffer::<image::LumaA<u8>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::RGB(8), D::U8(b)) => {
                need(b.len(), (w * h * 3) as usize)?;
                image::RgbImage::from_raw(w, h, b).ok_or_else(|| bad("bad size"))?.into()
            }
            (T::RGBA(8), D::U8(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                image::RgbaImage::from_raw(w, h, b).ok_or_else(|| bad("bad size"))?.into()
            }
            (T::Gray(16), D::U16(b)) => {
                need(b.len(), (w * h) as usize)?;
                image::ImageBuffer::<image::Luma<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::GrayA(16), D::U16(b)) => {
                need(b.len(), (w * h * 2) as usize)?;
                image::ImageBuffer::<image::LumaA<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::RGB(16), D::U16(b)) => {
                need(b.len(), (w * h * 3) as usize)?;
                image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::RGBA(16), D::U16(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            // CMYK: the conversion belongs to `image`, so `image` does it. See
            // `cmyk_via_image` for why that means re-wrapping the page.
            (T::CMYK(8), D::U8(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                cmyk_via_image::<tiff::encoder::colortype::CMYK8>(w, h, &b)?.into()
            }
            (T::CMYK(16), D::U16(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                cmyk_via_image::<tiff::encoder::colortype::CMYK16>(w, h, &b)?.into()
            }
            // Palette, CMYKA, YCbCr, Lab and multiband: no conversion path we can
            // hand to `image`, so they stay refused. Those, and any mismatch
            // between the declared colour type and the samples that arrived.
            _ => return Err(bad("unsupported colour type")),
        };
        Ok(img.to_rgba8())
    }

    /// Convert a CMYK page by handing it to `image` through a one-page TIFF.
    ///
    /// `image` owns the CMYK→RGB maths — `(255 - c) * (1 - k/255)`, verified
    /// against `cmyk_to_rgb` in `image`'s TIFF decoder — and it is not ours to
    /// restate. An approximation here is how a scanned document comes out a
    /// slightly wrong shade of every colour, invisibly, which is the failure the
    /// original refusal of these types was protecting against.
    ///
    /// It cannot simply be pointed at the original file, because its TIFF decoder
    /// only ever reads page 0 — that is the entire reason this function reaches
    /// for the `tiff` crate in the first place. So the samples already in hand
    /// are re-wrapped in a fresh single-page TIFF and *that* is decoded. The cost
    /// is one copy of the page, on a path only reached for a colour type the
    /// plain reinterpretation cannot express.
    ///
    /// Generic over the sample type so the 8- and 16-bit cases share it; the
    /// `tiff` encoder takes flat samples either way, so nothing is repacked.
    fn cmyk_via_image<C: tiff::encoder::colortype::ColorType>(
        w: u32,
        h: u32,
        samples: &[C::Inner],
    ) -> Result<image::DynamicImage, LoadError>
    where
        [<C as tiff::encoder::colortype::ColorType>::Inner]: tiff::encoder::TiffValue,
    {
        let mut out = std::io::Cursor::new(Vec::new());
        // The encoder finishes (and writes the end-of-IFD marker) when it drops,
        // at the end of this statement, which is before `out` is read back.
        tiff::encoder::TiffEncoder::new(&mut out)
            .map_err(tiff_err)?
            .write_image::<C>(w, h, samples)
            .map_err(tiff_err)?;
        image::load_from_memory_with_format(&out.into_inner(), image::ImageFormat::Tiff)
            .map_err(|e| LoadError::Other(format!("tiff: {e}")))
    }

    let mut data = ImageData {
        width: w,
        height: h,
        rgba: to_rgba8(w, h, kind, result)?.into_raw(),
        load_time: start.elapsed(),
    };
    if data.width.max(data.height) > max_dim {
        data = downscale_image(&data, max_dim);
    }
    Ok(normalized(data))
}

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

/// Playback-speed bounds: 0.1× is slow motion, 8× the ceiling. Zero would
/// freeze on frame 0 and negative runs time backwards — neither is a speed.
pub fn clamp_anim_speed(speed: f32) -> f32 {
    speed.clamp(0.1, 8.0)
}

/// Current frame index and time until the next boundary (in real time, for
/// repaint scheduling) for a playing animation, given elapsed playback time
/// scaled by `speed` (1.0 = file timing). Zero/short delays are floored to
/// 20 ms so a broken GIF can't spin the render loop.
pub fn anim_frame_state(anim: &Animation, elapsed: Duration, speed: f32) -> (usize, Duration) {
    let speed = clamp_anim_speed(speed);
    let scaled = elapsed.mul_f32(speed);
    let total = anim.total_duration().as_millis().max(1);
    let t = scaled.as_millis() % total;
    let mut acc = 0u128;
    for (i, d) in anim.delays.iter().enumerate() {
        let dm = d.as_millis().max(20);
        if t < acc + dm {
            return (i, Duration::from_millis((acc + dm - t) as u64).div_f32(speed));
        }
        acc += dm;
    }
    (
        anim.frames.len().saturating_sub(1),
        Duration::from_millis(20).div_f32(speed),
    )
}

/// One frame step for a paused animation (`,` / `.`, YouTube-style):
/// the frame after/before `idx`, wrapping at both ends. Animations loop,
/// so stepping never gets stuck. Zero frames (unreachable in practice —
/// an animation with no frames is never built) stays 0 rather than
/// dividing by zero.
pub fn step_frame(n: usize, idx: usize, forward: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if forward {
        (idx + 1) % n
    } else {
        (idx + n.saturating_sub(1)) % n
    }
}

/// When frame `idx` starts, from loop origin: the floored delays before it.
///
/// Resuming a paused animation rewinds `start` by this, so playback
/// continues mid-loop instead of restarting — the same flooring
/// `total_duration` uses, or the resume point and the paint clock disagree
/// about which frame is on screen.
pub fn anim_frame_start(anim: &Animation, idx: usize) -> Duration {
    let ms: u64 = anim
        .delays
        .iter()
        .take(idx.min(anim.delays.len()))
        .map(|d| d.as_millis().max(20) as u64)
        .sum();
    Duration::from_millis(ms)
}

/// A zoom preset (keys `1`–`4`) as a source-pixel ratio: 1.0 is 100%.
/// Clamped to the sane range so a hand-edited config cannot park a key at
/// 0× (a divide-by-zero downstream) or 1000×. The draw path clamps again
/// against the fit scale, so this is config hygiene, not layout.
pub fn zoom_preset(presets: &[f32; 4], i: usize) -> f32 {
    presets[i.min(3)].clamp(0.1, 32.0)
}

/// Zoom percentage for the status readout: 100% is one source pixel per
/// physical pixel (`scale * ppp == 1`), whatever the window is. Against 1:1
/// rather than against fit, so the number means the same on a panorama at
/// 6% and a thumbnail at 400% — and so it stays put when the window moves
/// but the zoom does not.
pub fn zoom_percent(scale: f32, ppp: f32) -> u32 {
    (scale * ppp * 100.0).round().max(0.0) as u32
}

/// One axis of the tile loop's source range: which buffer pixels the visible
/// interval covers, clamped to the buffer.
///
/// `buf_to_src` is source pixels per buffer pixel — 1.0 when the tile buffer
/// is the full decode, 2.0 when it is a half-res DCT decode (a source past
/// `MAX_TEXTURE_DIM`, whose full texture would not fit the GPU). Dividing by
/// it is what keeps the range in *buffer* pixels: without it the range is
/// named in source pixels and every tile is cut from the wrong place.
pub fn tile_axis_range(
    vis_min: f32,
    vis_max: f32,
    paint_min: f32,
    scale: f32,
    buf_to_src: f32,
    buf_dim: u32,
) -> (u32, u32) {
    let k = buf_to_src.max(f32::EPSILON);
    let lo = ((vis_min - paint_min) / scale / k).floor().max(0.0) as u32;
    let hi = ((vis_max - paint_min) / scale / k).ceil().max(0.0) as u32;
    (lo.min(buf_dim), hi.min(buf_dim))
}

/// Where one tile lands on screen: the tile's buffer span, pitched up to
/// source pixels, laid out from the page's paint origin at `scale`.
///
/// The pitch-up is the same `buf_to_src` ratio as [`tile_axis_range`], and
/// for the same reason: a tile cut from a half-res buffer and laid out at
/// source pitch lands every tile shifted and squeezed, composing a small
/// copy of the whole image over the image's own top-left. At 1.0 this is
/// exactly the old arithmetic, so full-res sources paint bit-identical
/// rects.
pub fn tile_draw_rect(
    paint_min: (f32, f32),
    px: u32,
    py: u32,
    tw: u32,
    th: u32,
    scale: f32,
    kx: f32,
    ky: f32,
) -> [f32; 4] {
    [
        paint_min.0 + px as f32 * kx * scale,
        paint_min.1 + py as f32 * ky * scale,
        tw as f32 * kx * scale,
        th as f32 * ky * scale,
    ]
}

/// Window size hugging `content` (the picture as drawn, in points): the gap
/// between the window's inner size and the central area is the side panels
/// and bars, which keep whatever they have — so adding the same gap back
/// wraps the picture with the chrome unchanged. Clamped to the monitor (an
/// 8× spread is bigger than any screen) and floored (degenerate content
/// must not collapse the window into something ungrabbable).
pub fn fit_window_size(
    content: (f32, f32),
    inner: (f32, f32),
    central: (f32, f32),
    monitor: Option<(f32, f32)>,
) -> (f32, f32) {
    let chrome = (
        (inner.0 - central.0).max(0.0),
        (inner.1 - central.1).max(0.0),
    );
    let mut out = (content.0 + chrome.0, content.1 + chrome.1);
    if let Some(m) = monitor {
        out.0 = out.0.min(m.0);
        out.1 = out.1.min(m.1);
    }
    (out.0.max(200.0), out.1.max(120.0))
}

/// Decode an animation into composited frames, capped at the limits above.
///
/// Dispatch is on [`AnimatedFormat`], not on the extension, and the `image` crate
/// composites partial frames and applies disposal methods for all three.
///
/// **Why the format is passed in rather than sniffed here:** for PNG and WebP the
/// "is it actually animated" question needs the header, and asking for the header
/// of a *still* image is not free for WebP, whose `into_frames` decodes a full
/// frame. [`animation_frame_count`] is the cheap check and is called first.
fn load_animation(
    bytes: &[u8],
    max_dim: u32,
    start: std::time::Instant,
    format: AnimatedFormat,
) -> Result<Animation, LoadError> {
    let cur = std::io::Cursor::new(bytes);
    match format {
        AnimatedFormat::Gif => {
            collect_animation(image::codecs::gif::GifDecoder::new(cur)?, max_dim, start)
        }
        AnimatedFormat::Png => {
            // `apng()` is infallible in practice; it is `Result` upstream.
            let png = image::codecs::png::PngDecoder::new(cur)?;
            collect_animation(png.apng()?, max_dim, start)
        }
        AnimatedFormat::Webp => {
            collect_animation(image::codecs::webp::WebPDecoder::new(cur)?, max_dim, start)
        }
    }
}

/// Decode only the first frame of an animation, for a grid cell.
///
/// One `next()` and no loop, so this cannot hang on a truncated animation — the
/// same reason [`is_animated`] asks for two frames rather than counting. A cell
/// shows one picture, and the picture it has to match is `frames[0]` of
/// [`load_animation`], so this returns the same pixels without decoding the rest.
///
/// Returns `None` rather than an error so the caller can fall through to the
/// normal single-image path, which is what a *still* file of an animated format
/// needs anyway.
fn first_frame(
    bytes: &[u8],
    max_dim: u32,
    start: std::time::Instant,
    format: AnimatedFormat,
) -> Option<ImageData> {
    use image::AnimationDecoder;
    let cur = std::io::Cursor::new(bytes);
    let frame = match format {
        AnimatedFormat::Gif => image::codecs::gif::GifDecoder::new(cur)
            .ok()?
            .into_frames()
            .next()?,
        AnimatedFormat::Png => image::codecs::png::PngDecoder::new(cur)
            .ok()?
            .apng()
            .ok()?
            .into_frames()
            .next()?,
        AnimatedFormat::Webp => image::codecs::webp::WebPDecoder::new(cur)
            .ok()?
            .into_frames()
            .next()?,
    };
    let frame = frame.ok()?;
    let mut data = ImageData {
        width: frame.buffer().width(),
        height: frame.buffer().height(),
        rgba: frame.buffer().clone().into_raw(),
        load_time: start.elapsed(),
    };
    if data.width.max(data.height) > max_dim {
        data = downscale_image(&data, max_dim);
    }
    Some(normalized(data))
}

/// Whether this file has more than one frame, without decoding more than two of
/// them.
///
/// This is the cheap gate, and it is the difference between one decode and two: a
/// still WebP handed to `into_frames` yields one *decoded* frame which the caller
/// throws away and then decodes the file again.
///
/// **It asks for two frames, not for the count, and both of those words are load
/// bearing.**
///
/// *Asking for two* rather than counting: the first version of this was
/// `into_frames().count()`, which decodes the whole animation and then the real
/// decode does it again — so a 240-frame GIF was decoded twice before the first
/// pixel appeared. The answer only needs to be "more than one?".
///
/// *Two* rather than "iterate": a **truncated** GIF makes `into_frames` yield `Err`
/// and then keep yielding it, and `count()` never stops — so a 7.9 MB GIF that is
/// missing its last byte hung the worker forever. The image never painted, the
/// stall watchdog fired at 6 s, and the app sat there with six neighbour previews
/// decoded and none of the one you asked for. It is in the benchmark folder, so
/// this was not hypothetical. `take(2)` cannot hang whatever the file does.
///
/// PNG and WebP do not need the frames at all: both answer from the header.
pub fn is_animated(bytes: &[u8], format: AnimatedFormat) -> bool {
    use image::AnimationDecoder;
    let cur = std::io::Cursor::new(bytes);
    match format {
        AnimatedFormat::Gif => {
            let Ok(dec) = image::codecs::gif::GifDecoder::new(cur) else {
                return false;
            };
            two_ok_frames(dec.into_frames())
        }
        AnimatedFormat::Png => {
            // `is_apng` is a header read. A plain PNG must not be mistaken for a
            // one-frame animation, which would decode it twice.
            let Ok(png) = image::codecs::png::PngDecoder::new(cur) else {
                return false;
            };
            png.is_apng().unwrap_or(false)
        }
        AnimatedFormat::Webp => image::codecs::webp::WebPDecoder::new(cur)
            .map(|d| d.has_animation())
            .unwrap_or(false),
    }
}

/// True when the first two frames both decode. Two `next` calls and no loop, so it
/// terminates on any input including a file whose frame data runs out.
fn two_ok_frames<'a, I: Iterator<Item = image::ImageResult<image::Frame>>>(mut it: I) -> bool {
    matches!(it.next(), Some(Ok(_))) && matches!(it.next(), Some(Ok(_)))
}

/// Shared body of [`load_animation`], generic over the three decoders because
/// `image` gives them no common concrete type.
fn collect_animation<'a, D: image::AnimationDecoder<'a>>(
    dec: D,
    max_dim: u32,
    start: std::time::Instant,
) -> Result<Animation, LoadError> {
    // No `use image::AnimationDecoder` needed: the bound on `D` above already
    // puts the trait in scope for `into_frames`.
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
        // A single frame that is itself over the cap is not a truncated animation,
        // it is a decompression bomb, and the running-total check above only
        // stops *after* it has been allocated. One 20000x20000 APNG frame is
        // 1.6 GB, and memory exhaustion here presents as a worker that never
        // answers rather than as an error. Checked before the downscale, because
        // that is the size that was actually allocated.
        if data.rgba.len() > MAX_ANIM_BYTES {
            break;
        }
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

// ---------------------------------------------------------------------------
// Grid thumbnails: the on-disk cache
// ---------------------------------------------------------------------------
//
// **Why this exists at all, because the interesting number is a negative one.**
// A 95 MP baseline JPEG takes ~700-800 ms to decode at 1/8 scale on this box, and
// there is no cheaper way to get the whole picture out of it: every pixel needs
// every DCT coefficient, so the entropy decode is the floor for *any* conformant
// decoder. Measured, interleaved, min of 4:
//
//   0_booklet_inside.jpg  95.7 MP baseline 4:2:0, 27 MB
//     fs::read 13 ms | DCT 1/8 833 ms | DCT 1/1 1242 ms | cell texture 5851 KB
//
// The DCT path saves RAM, not time — which is why the embedded EXIF thumbnail
// (`decode_embedded_thumbnail`, 15-40x) is the only real answer for the files that
// have one, and why the files that do not were the residue with nowhere to go.
//
// So the fix is not a faster decode. It is *not decoding it again*: a folder of
// large scans is opened over and over, and the second open should cost a file
// read. That is a cache, and the design of one is almost entirely about the ways
// it can be **wrong**, which is what the rest of this section is about:
//
// * A stale entry must be a **miss**, never the wrong picture. The stamp
//   (mtime + length) lives in the entry and is checked on every read, so a file
//   edited on disk is re-decoded even if nothing remembered to invalidate it.
// * A hash collision must be a **miss**, not a neighbour's thumbnail. The
//   source path is stored in the entry and compared, so 64 bits of name is not
//   the only thing standing between two files and each other's pictures.
// * A half-written entry must be **unreadable**. Entries are written to a
//   temporary name and renamed into place, so a reader — including a second
//   instance of the app — sees either the old entry or the new one.
// * Every failure is a **miss**. An unwritable cache directory, a full disk, a
//   corrupt PNG: each one costs a decode, and none of them may fail the decode.
//
// What this does *not* do is make the first visit to a folder of large scans
// fast. It cannot, and the honest number for a cold folder of 24 baseline scans
// is still ~24 x 700 ms / 4 workers. See `tests/bench_browse.rs`
// (`thumb_cache_ab`) for that A/B.

/// Every entry starts with this, so a stray file, a directory, or a truncated
/// write from a killed process is a miss and not a decode of whatever is there.
const THUMB_MAGIC: [u8; 4] = *b"IVTC";

/// Bumped whenever the header layout changes, which makes every existing entry a
/// miss instead of being misread.
const THUMB_HEADER_VERSION: u8 = 1;

/// magic(4) + version(1) + reserved(3) + width(4) + height(4) + mtime(8)
/// + len(8) + target(4) + path_len(2)
const THUMB_HEADER_FIXED: usize = 4 + 1 + 3 + 4 + 4 + 8 + 8 + 4 + 2;

/// The last 12 bytes of every complete PNG: a zero-length `IEND` chunk and its
/// CRC. See [`read_cached_thumbnail`] for why this is load-bearing.
const PNG_IEND: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];

/// Cap on the whole cache directory.
///
/// 256 MB is 3 000-19 000 cells at the 13-80 KB an entry measures (see
/// [`encode_png`]), so it holds a large scan library rather than a photo archive,
/// and it is trimmed oldest-first when it is reached. The number is stated in the
/// settings panel next to the toggle, because a cache that silently takes a
/// quarter of a disk is not something to enable quietly.
pub const THUMB_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// How many writes happen between trims. A trim walks the whole cache, so doing
/// it on every write would be absurd; doing it never would let the cache grow
/// without bound, which is the failure mode a cache has to be designed against.
const THUMB_TRIM_EVERY: u64 = 64;

/// Fraction of the cap a trim aims back down to, so it is not repeated on the
/// very next write.
const THUMB_TRIM_TO_NUM: u64 = 4;
const THUMB_TRIM_TO_DEN: u64 = 5;

/// Root of the on-disk thumbnail cache.
///
/// **Local, not roaming, and deliberately not next to the config.** These are
/// this machine's decodes of files that may well be on a network share, they are
/// worthless on another machine, and a profile folder is the wrong place for
/// something this size.
pub fn thumb_cache_root() -> PathBuf {
    if let Some(dir) = portable_dir() {
        return dir.join("thumbs");
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local)
            .join("image-viewer")
            .join("thumbs")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join(".cache")
            .join("image-viewer")
            .join("thumbs")
    } else {
        PathBuf::from("image-viewer-thumbs")
    }
}

/// On/off for the whole cache, so a user who wants the viewer to write nothing
/// outside its config folder can say so. Mirrors [`set_linear_resample`]: a
/// global the workers read, set once from config at start-up.
static THUMB_CACHE_ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_thumbnail_cache(on: bool) {
    THUMB_CACHE_ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the on-disk cache in use? Public so a test can save and restore it around
/// assertions that only hold with it off.
pub fn thumbnail_cache_enabled() -> bool {
    THUMB_CACHE_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// What identifies the source file an entry was made from.
///
/// mtime + length, the same pair the file-change watcher compares
/// (`Stamp` in main.rs). Length is in the pair because a save that lands inside
/// one filesystem timestamp tick changes the bytes without changing the time,
/// and that is the case that would otherwise show a stale picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbStamp {
    pub mtime_secs: u64,
    pub len: u64,
}

pub fn thumb_stamp(path: &Path) -> Option<ThumbStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(ThumbStamp {
        mtime_secs,
        len: meta.len(),
    })
}

/// Header of one cache entry: everything needed to decide whether it may be
/// used, and nothing that is not.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ThumbHeader {
    width: u32,
    height: u32,
    stamp: ThumbStamp,
    target: u32,
    path: String,
}

/// Little-endian on purpose, so an entry written on one machine reads the same on
/// another and the format does not depend on this host's byte order.
fn encode_thumb_header(h: &ThumbHeader) -> Vec<u8> {
    let path = h.path.as_bytes();
    let path_len = u16::try_from(path.len()).unwrap_or(u16::MAX);
    let mut b = Vec::with_capacity(THUMB_HEADER_FIXED + path.len());
    b.extend_from_slice(&THUMB_MAGIC);
    b.push(THUMB_HEADER_VERSION);
    b.extend_from_slice(&[0u8; 3]);
    b.extend_from_slice(&h.width.to_le_bytes());
    b.extend_from_slice(&h.height.to_le_bytes());
    b.extend_from_slice(&h.stamp.mtime_secs.to_le_bytes());
    b.extend_from_slice(&h.stamp.len.to_le_bytes());
    b.extend_from_slice(&h.target.to_le_bytes());
    b.extend_from_slice(&path_len.to_le_bytes());
    b.extend_from_slice(path);
    b
}

/// Split an entry into its header and payload. `None` for anything that is not
/// an entry this version wrote, including a short read, a bad magic, a version
/// from the future, or a `path_len` that does not fit — all of which are simply
/// "no entry here".
fn decode_thumb_header(bytes: &[u8]) -> Option<(ThumbHeader, &[u8])> {
    if bytes.len() < THUMB_HEADER_FIXED || bytes[..4] != THUMB_MAGIC {
        return None;
    }
    if bytes[4] != THUMB_HEADER_VERSION {
        return None;
    }
    // Bounds are already settled by the length check above: the largest read ends
    // at 38, which is exactly `THUMB_HEADER_FIXED`. Spelled out rather than
    // `try_into().ok()?` per field because a closure cannot return `Option` here
    // and the invariant is worth stating once.
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
    let path_len = usize::from(u16::from_le_bytes(bytes[36..38].try_into().unwrap()));
    let start = THUMB_HEADER_FIXED;
    let end = start.checked_add(path_len)?;
    let path = std::str::from_utf8(bytes.get(start..end)?).ok()?.to_owned();
    Some((
        ThumbHeader {
            width: u32_at(8),
            height: u32_at(12),
            stamp: ThumbStamp {
                mtime_secs: u64_at(16),
                len: u64_at(24),
            },
            target: u32_at(32),
            path,
        },
        bytes.get(end..)?,
    ))
}

/// Where one source file's entry lives: `<root>/<2 hex>/<16 hex>.png`.
///
/// **The name is a hash of the path and nothing else, on purpose.** The stamp that
/// decides whether the entry is still good lives inside the entry, so a file that
/// is edited re-uses and overwrites its own slot instead of leaving one orphan
/// behind per save — which is the difference between a cache that is bounded by
/// the library size and one that is bounded by how many times you have hit save.
///
/// The two-hex fanout is not cosmetic either: a library of 30 000 files in one
/// directory is a single NTFS directory big enough to make `read_dir` and every
/// `create_dir` in it measurably slower.
pub fn thumb_cache_entry(root: &Path, path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let name = format!("{:016x}", h.finish());
    root.join(&name[..2]).join(format!("{name}.png"))
}

/// Read a cached thumbnail from `root`, or `None` for a miss of any kind.
///
/// Takes the root rather than resolving [`thumb_cache_root`] so a test can point
/// it at a temp directory; the default-root path is [`load_grid_thumbnail`].
pub fn read_cached_thumbnail(root: &Path, path: &Path, target: u32) -> Option<ImageData> {
    let start = std::time::Instant::now();
    let entry = thumb_cache_entry(root, path);
    let bytes = std::fs::read(&entry).ok()?;
    let (header, payload) = decode_thumb_header(&bytes)?;
    // Every one of these is a miss, not a repair: a wrong picture in a grid is
    // worse than a blank cell, and the decode that replaces it is the thing this
    // cache exists to make rare.
    if header.stamp != thumb_stamp(path)? {
        return None;
    }
    if header.target != target || header.path != path.to_string_lossy() {
        return None;
    }
    // **The payload must be complete before it is decoded.** This check is here
    // because the obvious version of this function does not have it, and the test
    // that found it says why: `image`'s PNG decoder accepts a stream whose last
    // 64 bytes are missing, and hands back a full-size image with the tail zeroed.
    // So a truncated entry — a half-copied file, a backup tool mid-write, a
    // hydrated OneDrive placeholder — produced a *hit*, and a grid cell with a
    // black band across it, with no error anywhere. PNG's own terminator is the
    // exact and free way to ask "is all of this here?": a complete file ends with
    // a zero-length IEND chunk and its CRC. (Bit flips inside the compressed data
    // are a different question, and the `png` crate's per-chunk CRC answers those.)
    if !payload.ends_with(&PNG_IEND) {
        return None;
    }
    let img = image::load_from_memory_with_format(payload, image::ImageFormat::Png).ok()?;
    let rgba = img.to_rgba8();
    // The header's dims are a claim; the payload is the truth, and `normalized`
    // is what keeps a disagreement from becoming an assert in
    // `ColorImage::from_rgba_unmultiplied` on the UI thread.
    let data = normalized(ImageData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        load_time: start.elapsed(),
    });
    if (data.width, data.height) != (header.width, header.height) {
        return None;
    }
    Some(data)
}

/// Write a thumbnail into the cache at `root`. Best effort: every failure is
/// silent, and none of them may affect the decode the caller already has.
///
/// Takes the root for the same reason as [`read_cached_thumbnail`].
pub fn write_cached_thumbnail(root: &Path, path: &Path, target: u32, data: &ImageData) {
    let Some(stamp) = thumb_stamp(path) else {
        return;
    };
    let header = ThumbHeader {
        width: data.width,
        height: data.height,
        stamp,
        target,
        path: path.to_string_lossy().into_owned(),
    };
    let Ok(png) = encode_png(&data.rgba, data.width, data.height) else {
        return;
    };
    let entry = thumb_cache_entry(root, path);
    let Some(parent) = entry.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut bytes = encode_thumb_header(&header);
    bytes.extend_from_slice(&png);
    // Write beside the entry and rename into place. `fs::rename` replaces on
    // Windows, and a reader therefore sees either the previous entry or this one
    // — never a half-written one, which matters because a second instance of the
    // app reads these too.
    //
    // The temporary name is *not* made unique per writer, and that is deliberate:
    // two writers of one entry are writing the same bytes (same path, same stamp,
    // same target ⇒ the same header and the same encode), so clobbering the
    // temporary file cannot produce a wrong entry — at worst one `rename` finds its
    // source already gone and gives up, leaving the other's entry installed. A
    // per-writer name would instead leave an orphan behind whenever a process died
    // between the two calls.
    let tmp = entry.with_extension("tmp");
    if std::fs::write(&tmp, &bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &entry).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    maybe_trim_thumb_cache(root);
}

/// RGBA -> PNG, losslessly, and *without storing information the viewer does not
/// use*.
///
/// Lossless is the point and not an accident: a cache hit has to be **the same
/// pixels** as the decode it replaced, or the grid would quietly show something
/// the main view cannot reproduce — and a test could not tell the difference. That
/// is also why a lossy thumbnail format (a q90 JPEG would be ~8x smaller) was
/// rejected: it trades a property that is cheap to keep and easy to assert for one
/// that is neither.
///
/// The alpha drop is not a compromise. An opaque thumbnail stores as RGB, which
/// is 3 bytes per pixel before deflate instead of 4, and a cell draws an opaque
/// image identically either way; the reader puts the alpha back at 255, so the
/// RGBA the app receives is byte-for-byte what the decode produced. A thumbnail
/// that *does* have alpha keeps it.
///
/// What a cell costs on disk, measured: **13-80 KB, call it 1.5 bytes per pixel**.
/// Deflate is simply not good at photographic content, which is what a thumbnail
/// of a photo is, and [`THUMB_CACHE_MAX_BYTES`] is chosen with that in mind.
fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, image::ImageError> {
    use image::codecs::png::{FilterType, PngEncoder};
    use image::ExtendedColorType;
    use image::ImageEncoder;
    let mut out = Vec::new();
    {
        let enc =
            PngEncoder::new_with_quality(&mut out, THUMB_PNG_COMPRESSION, FilterType::Adaptive);
        if buffer_has_alpha(rgba) {
            enc.write_image(rgba, width, height, ExtendedColorType::Rgba8)?;
        } else {
            let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
            for px in rgba.chunks_exact(4) {
                rgb.extend_from_slice(&px[..3]);
            }
            enc.write_image(&rgb, width, height, ExtendedColorType::Rgb8)?;
        }
    }
    Ok(out)
}

/// The one tunable number in the stored form, and its table is why it is not
/// simply "the best compression available". Deflate effort, measured on real
/// thumbnails (bytes / encode time, min of 3, release):
///
/// | thumb | `Default` | `Fast` | `Best` |
/// |---|---|---|---|
/// | 256x84, wide scan | 35 734 B / 11.3 ms | 34 938 B / **0.75 ms** | 35 649 B / 26.8 ms |
/// | 107x160, embedded | 24 794 B / 6.8 ms | 32 994 B / **0.54 ms** | 24 328 B / 12.6 ms |
/// | 256x170 | 79 346 B / 21.0 ms | 79 613 B / **1.41 ms** | 79 193 B / 28.7 ms |
///
/// `Best` buys under 1% of size for 15-36x the time, and that time lands *before*
/// the cell's message is sent, so it is added to the one latency a user feels on a
/// cold open. `Default` is the same size as `Best` and just as slow. `Fast` is the
/// only one of the three that is not paying for nothing; the embedded-thumbnail row
/// is the one case where it costs size, and 8 KB on a file that takes 12 ms to
/// decode is not a trade worth optimising.
const THUMB_PNG_COMPRESSION: image::codecs::png::CompressionType =
    image::codecs::png::CompressionType::Fast;

static THUMB_PUTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Trim the cache off a background thread, every [`THUMB_TRIM_EVERY`] writes.
///
/// Off-thread because a trim walks the whole directory: on a full cache that is
/// tens of thousands of `read_dir` and `metadata` calls, and doing it inline would
/// add that to one unlucky thumbnail's latency. Detached rather than joined
/// because nothing waits on it — at worst the process exits mid-trim, which
/// leaves the cache slightly over its cap and perfectly valid.
fn maybe_trim_thumb_cache(root: &Path) {
    let n = THUMB_PUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if n % THUMB_TRIM_EVERY != 0 {
        return;
    }
    let root = root.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("iv-thumb-trim".into())
        .spawn(move || {
            prune_thumb_cache(&root, THUMB_CACHE_MAX_BYTES);
        });
}

/// Delete oldest-first until the cache is under `cap * THUMB_TRIM_TO_NUM/DEN`.
/// Returns the number of bytes left.
///
/// Public so a test can trim a synthetic cache and assert the result, rather than
/// trusting that a background thread eventually did it. Synchronous by design —
/// the caller decides what thread it runs on.
pub fn prune_thumb_cache(root: &Path, cap: u64) -> u64 {
    let target = cap / THUMB_TRIM_TO_DEN * THUMB_TRIM_TO_NUM;
    let mut entries: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = Vec::new();
    let mut total = 0u64;
    let Ok(fanouts) = std::fs::read_dir(root) else {
        return 0;
    };
    for fanout in fanouts.flatten() {
        let Ok(files) = std::fs::read_dir(fanout.path()) else {
            continue;
        };
        for f in files.flatten() {
            let Ok(meta) = f.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            total = total.saturating_add(meta.len());
            // An entry whose mtime cannot be read sorts as oldest, which is the
            // safe direction: it is the one most likely to be junk.
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            entries.push((mtime, meta.len(), f.path()));
        }
    }
    if total > target {
        entries.sort_by_key(|(mtime, _, _)| *mtime);
        for (_, len, path) in entries {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
        // Drop the fanout directories the trim just emptied, so the cache does not
        // slowly accumulate 256 near-empty folders. `remove_dir` refuses a
        // directory that still has anything in it, so the only casualty is a second
        // instance that created a fanout microseconds ago and has not written yet:
        // its rename fails, it caches nothing this once, and the next request for
        // that cell writes it. A cache that occasionally misses is the design.
        if let Ok(fanouts) = std::fs::read_dir(root) {
            for fanout in fanouts.flatten() {
                let _ = std::fs::remove_dir(fanout.path());
            }
        }
    }
    total
}

/// Drop `path`'s cached thumbnail, if it has one.
///
/// Called when the watcher sees the file change. Correctness does **not** depend
/// on this: the stamp in the entry already makes a changed file a miss. It is
/// here so a re-saved file does not sit on disk taking up space for a session
/// that may never open that folder again.
pub fn forget_thumbnail(path: &Path) {
    if !thumbnail_cache_enabled() {
        return;
    }
    let _ = std::fs::remove_file(thumb_cache_entry(&thumb_cache_root(), path));
}

/// The picture one grid cell shows, from the cache when it can be.
///
/// This is the whole grid-thumbnail entry point: a cache hit is a small file read,
/// a miss is [`load_thumbnail`] plus a write. Both are best-effort around the
/// edges — if the cache cannot be used the result is exactly what it was before
/// this existed.
pub fn load_grid_thumbnail(path: &Path, target: u32) -> Result<ImageData, LoadError> {
    if !thumbnail_cache_enabled() {
        return load_thumbnail(path, target);
    }
    let root = thumb_cache_root();
    match read_cached_thumbnail(&root, path, target) {
        Some(data) => {
            perf::count(perf::C_THUMB_CACHE_HIT, 1);
            Ok(data)
        }
        None => {
            perf::count(perf::C_THUMB_CACHE_MISS, 1);
            let data = load_thumbnail(path, target)?;
            write_cached_thumbnail(&root, path, target, &data);
            Ok(data)
        }
    }
}

// ---------------------------------------------------------------------------
// Spread overrides: what the user decided, persisted
// ---------------------------------------------------------------------------
//
// The detector above reads a page's aspect ratio, which is a good guess and blind
// to three things: a landscape insert that is not a scan of two pages, a page
// cropped enough that its ratio no longer matches its partner's, and what a given
// book wants at the front. So the user gets the last word — nudge how many
// leading pages stand alone, and flip any one page between a page and a spread —
// and this is where those decisions live.
//
// It is a small file, and it borrows the thumbnail cache's rules wholesale rather
// than inventing its own:
//
// * **Local, not roaming.** `%LOCALAPPDATA%\image-viewer\spreads.json`, beside
//   the thumbnail cache and *not* beside `config.json`. These are this machine's
//   reading of files that may be on a network share; they are worthless on
//   another machine, and a profile folder is the wrong place for them.
// * **A half-written store is unreadable.** Written to `spreads.json.tmp` and
//   renamed, and `fs::rename` replaces on Windows, so a reader — a second
//   instance of the app — sees either the old file or the new one.
// * **Every failure is an empty override set.** A missing file, a malformed one,
//   a path that cannot be read at all. Nothing a user or an editor does to this
//   file may be able to stop the app starting, and no failure here is worth a
//   warning: the loss is a pairing the user can re-press, and the alternative is
//   refusing to open a folder over a cache file.
//
// Two levels of key, and the second is a trap rather than a choice:
//
// * **The gallery's root, as the caller states it** ([`spread_root_key`]). One
//   store holds every gallery, so a save reads the whole file and replaces exactly
//   one entry: the folder, or the archive that an open gallery was extracted from.
// * **A bare file name and a page number**, inside that entry. A member of an
//   archive has no stable directory to record — the `%TEMP%` directory it was
//   extracted into changes on every re-extract — so the directory component is
//   dropped and the *root key* carries the gallery instead. Within one gallery
//   the names are unique: a folder is scanned flat, and `extract_zip` flattens
//   and de-duplicates the entries it writes.

/// What identifies a gallery's entry in the store: the root path, as a string.
///
/// Deliberately a bare path and not a "folder or archive" enum. The mistake the
/// `%TEMP%` rule exists to prevent is made *natural* by that choice rather than
/// impossible: there is no field here to put an archive's extraction directory into,
/// so reaching for it takes a deliberate argument instead of filling something in.
/// It is still reachable — a caller can write any string it likes and the load will
/// not notice — which is why [`spread_root_key`] is where the choice is actually
/// made and tested.
type SpreadStore = std::collections::BTreeMap<String, GallerySpreads>;

/// Where the overrides live: `%LOCALAPPDATA%\image-viewer\spreads.json`.
///
/// **Local, not roaming, and deliberately not beside the config.** These are this
/// machine's decisions about files that may well be on a network share, they are
/// worthless on another machine, and a profile folder is the wrong place for
/// something the user never asked to sync. `Config::default_path` is the opposite
/// case and does live in `%APPDATA%`; keeping the two apart is the whole point of
/// `AGENTS.md`'s "a cache of any sort is `%LOCALAPPDATA%`".
///
/// `HOME` is the same fallback [`thumb_cache_root`] uses, for the one environment
/// where it is not `LOCALAPPDATA`. **No third arm**, unlike that function: it
/// falls back to a directory beside the working copy, which is the right answer
/// for a disposable cache and the wrong one for a file a person is meant to open
/// and edit. `None` means "do not persist", and the caller keeps
/// the decisions in memory for the session — the only cost is that they are
/// forgotten at exit, which is a far smaller failure than an app that will not
/// start.
pub fn spreads_root() -> Option<PathBuf> {
    if let Some(dir) = portable_dir() {
        return Some(dir.join("spreads.json"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        Some(PathBuf::from(local).join("image-viewer").join("spreads.json"))
    } else if let Some(home) = std::env::var_os("HOME") {
        Some(PathBuf::from(home).join(".cache").join("image-viewer").join("spreads.json"))
    } else {
        None
    }
}

/// Which gallery `archive`/`folder` names, as the store's key.
///
/// **For an open archive this is the archive's own path and never the `%TEMP%`
/// directory its members were extracted into.** That directory's name is a hash of
/// the archive's path, mtime and length, so it is stable only until the archive is
/// touched — an override keyed by it would appear to work for as long as nobody
/// edited the `.cbz` and then silently lose every override the first time they
/// did. `MyApp` holds both, so the choice has to be made here rather than at each
/// call site.
///
/// `None` only for the **empty** path, which is the one value that cannot name a
/// gallery: the empty string is a real key, so every gallery with no name would
/// share one entry and each one's overrides would answer for the others. Note that
/// "no parent" is *not* the test — `Path::new(".")` has no parent and is not empty,
/// so it keys as `"."`.
///
/// A function rather than an expression at the one call site that needs it, because
/// `MyApp` holds both paths and the wrong reading of which one is the gallery's is
/// silent: overrides keyed by the extraction directory load fine right up until the
/// archive is edited.
pub fn spread_root_key(archive: Option<&Path>, folder: &Path) -> Option<String> {
    let root = archive.unwrap_or(folder);
    (!root.as_os_str().is_empty()).then(|| root.to_string_lossy().into_owned())
}

/// The user's decisions about one gallery: which pages take a whole turn, and how
/// many leading pages stand alone.
///
/// Two independent things because they are two independent decisions. `lead_lone`
/// is a property of the *book* and applies to the whole gallery; `pages` is a
/// property of a *page* and names individual exceptions. They travel together
/// only because they are written to the same place, and [`Self::shape_for`] is the
/// one function a pairing asks.
///
/// `Default` is spelled out rather than derived, and it is [`LEAD_LONE_DEFAULT`]
/// rather than `0`. A derived `Default` would make "this gallery has no overrides"
/// mean "pair the cover with page 2", and `load_spread_overrides` hands out exactly
/// that value whenever the store is missing or unreadable — so every gallery would
/// open with its cover paired to the page after it, with nothing on screen to say
/// why.
///
/// `#[serde(default)]` is on the **container**, so both fields fall back to
/// *this* `Default` and there is only one spelling of the default to keep
/// correct: a file written before a field existed loads with the same values a
/// fresh value has. There is deliberately no `deny_unknown_fields`, so a store
/// from an older build — or one with a note in it — still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GallerySpreads {
    /// How many leading pages are shown one to a turn, by flat ordinal.
    /// Bounds: `0..=` [`LEAD_LONE_MAX`], enforced by [`Self::nudge_lead_lone`]
    /// and again on load.
    pub lead_lone: u32,
    /// Per-page overrides, keyed by **bare file name and page number**.
    ///
    /// Not keyed by path, for the archive reason above; not keyed by page alone,
    /// because a multi-page document's pages are separate decisions.
    #[serde(with = "spread_pages")]
    pub pages: HashMap<(String, u32), Shape>,
}

impl Default for GallerySpreads {
    fn default() -> Self {
        GallerySpreads {
            lead_lone: LEAD_LONE_DEFAULT,
            pages: HashMap::new(),
        }
    }
}

/// The on-disk form of [`GallerySpreads::pages`]: file name -> page -> shape.
///
/// A map keyed by a `(String, u32)` **pair** is not something `serde_json` can
/// write: it refuses any map key that is not a string, and no derive avoids the
/// key. So the pair is split into two nested objects and the page becomes a decimal
/// string, which is also what keeps the file editable by hand: nested objects with
/// string keys, rather than one flat map keyed by an invented separator nobody could
/// type.
///
/// ```text
/// "pages": { "001.png": { "0": "double", "7": "single" } }
/// ```
///
/// **An unparseable page key is dropped, not fatal.** One mistyped key must not
/// cost a reader every override in the file, which is what the
/// `unwrap_or_default` in [`load_spread_overrides`] would otherwise do — the same
/// reasoning as a corrupt cache entry being a miss rather than a wrong answer.
mod spread_pages {
    use super::Shape;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::{BTreeMap, HashMap};

    /// `file name -> page as a string -> shape`. `BTreeMap` so the written file's
    /// key order is stable, which is what makes it reviewable in a diff.
    type ByFile = BTreeMap<String, BTreeMap<String, Shape>>;

    pub fn serialize<S: Serializer>(
        pages: &HashMap<(String, u32), Shape>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut by_file: ByFile = BTreeMap::new();
        for ((name, page), shape) in pages {
            by_file
                .entry(name.clone())
                .or_default()
                .insert(page.to_string(), *shape);
        }
        by_file.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<HashMap<(String, u32), Shape>, D::Error> {
        let by_file = ByFile::deserialize(deserializer)?;
        let mut out = HashMap::with_capacity(by_file.len());
        for (name, per_page) in by_file {
            for (page, shape) in per_page {
                // Dropped, as the module docs say. The alternative — failing the
                // whole parse — hands back an empty store for one bad key.
                if let Ok(page) = page.parse::<u32>() {
                    out.insert((name.clone(), page), shape);
                }
            }
        }
        Ok(out)
    }
}

impl GallerySpreads {
    /// The user's decision about one page, or `None` when there is none.
    ///
    /// `None` is the ordinary answer and means "ask the detector": a page with no
    /// override pairs by its aspect ratio like any other. So this is not a
    /// fallback that guesses — an override *replaces* the detector rather than
    /// breaking its ties.
    ///
    /// Also `None` for a path with no file name, which cannot be in the store at
    /// all: [`Self::set_shape`] refuses to write one.
    pub fn shape_for(&self, path: &Path, page: u32) -> Option<Shape> {
        self.pages.get(&(file_name_of(path), page)).copied()
    }

    /// Record the user's decision about one page.
    ///
    /// **A path with no file name is refused rather than stored.** `file_name_of`
    /// answers `""` for one, and the store is keyed by that name — so every such
    /// path would share the entry `("", page)` and one would silently overwrite the
    /// others' decisions. Nothing reachable today produces such a path (a folder scan
    /// and `extract_zip` both name every file they yield), but the same asymmetry
    /// [`spread_root_key`] refuses for a *root* is worth refusing here too, and the
    /// cost is one entry that could never have been addressed anyway.
    pub fn set_shape(&mut self, path: &Path, page: u32, shape: Shape) {
        let name = file_name_of(path);
        if name.is_empty() {
            return;
        }
        self.pages.insert((name, page), shape);
    }

    /// Flip one page away from `detected`, store the answer, and return it.
    ///
    /// **`detected` is the whole answer, and it is the caller's**: what the page is
    /// judged to be *now* — an existing override if there is one, the aspect ratio
    /// otherwise. Two presses therefore flip and unflip, because the second press is
    /// handed the first press's answer.
    ///
    /// It does not give the store a second vote, which is what makes the returned
    /// shape worth anything: a `toggle` that read its own last answer back would
    /// flip away from the *stored* shape, and those two only differ when the stored
    /// override and the caller's judgement have parted company — an override set
    /// against a page whose file has since been re-exported. The same call would
    /// then answer two different questions, and only one of them was asked.
    /// `tests/spreads.rs::toggling_a_page_reports_the_shape_it_landed_on` pins it.
    pub fn toggle(&mut self, path: &Path, page: u32, detected: Shape) -> Shape {
        let next = match detected {
            Shape::Single => Shape::Double,
            Shape::Double => Shape::Single,
        };
        self.set_shape(path, page, next);
        next
    }

    /// Move the leading-lone offset by `by`, clamped to `0..=`[`LEAD_LONE_MAX`].
    ///
    /// Clamped in `i64` rather than with `saturating_*` on `u32`, because the
    /// arithmetic has to happen before it is bounded: `0u32` minus `i32::MIN` wraps
    /// to about two billion, which is *in* range and absurd, and the result would be
    /// a gallery of nothing but single pages.
    pub fn nudge_lead_lone(&mut self, by: i32) {
        let next = self.lead_lone as i64 + by as i64;
        self.lead_lone = next.clamp(0, LEAD_LONE_MAX as i64) as u32;
    }

    /// Drop every override for one file, for a file whose contents changed.
    ///
    /// **Every page, not one**, because the keys name pages: a path-keyed `remove`
    /// would be a no-op for any page past the first.
    ///
    /// This is not the same as forgetting a *measurement*, and the difference is
    /// why it is right to drop an override at all. A stale dimension is a wrong
    /// answer to a question about the current file; a stale override is a decision
    /// about pixels that no longer exist — a 12-page document re-exported as one
    /// page has no page 3 to be a spread. And because an override beats the
    /// detector outright, keeping it would hold a page a spread for the session no
    /// matter what the file now contains.
    ///
    /// Entries for a *name* that has left the gallery are not pruned by anything
    /// here: this store cannot tell a renamed file from a deleted one, and they are
    /// a few bytes each in a file the user can delete wholesale.
    pub fn forget_file(&mut self, path: &Path) {
        let name = file_name_of(path);
        self.pages.retain(|(n, _), _| *n != name);
    }
}

/// The overrides for one gallery out of the store. **Never fails.**
///
/// Every error is the same value: a missing file, a read error, and a parse that
/// did not produce the expected shape are all `GallerySpreads::default()`. That is
/// the whole contract, and it is why this returns the value rather than a `Result`
/// — a caller that could handle the error by refusing to open a folder would be a
/// worse app. Two refusals join them: a store over [`SPREADS_MAX_BYTES`] is not read
/// at all, and an entry over [`SPREADS_MAX_PAGES`] is emptied of its pages.
///
/// **An over-cap store is honoured as empty, and [`save_spread_overrides`] treats
/// the same file as untouchable.** The asymmetry is deliberate and is the whole
/// point of stating it in both places: returning empty here is safe because this
/// function only *reads*. The decisions are not honoured this session, the pairings
/// fall back to the detector, and the file is still on disk for next time. A save
/// cannot afford the same shrug, because its read is the first half of a
/// read-modify-write — so it refuses over the cap and writes nothing. Do not
/// "simplify" this by having one helper serve both.
///
/// `root` names the gallery the way [`spread_root_key`] produced it; `store` is
/// the file, which the caller passes in so a test can point it at a temporary
/// directory instead of the user's `%LOCALAPPDATA%`.
pub fn load_spread_overrides(store: &Path, root: &Path) -> GallerySpreads {
    // Checked before the read, not by reading and measuring after: this runs while
    // a gallery is opening, on the UI thread, and the point of the cap is to bound
    // that work rather than to notice it afterwards. A directory's metadata reads
    // fine and is tiny, so it falls through to the read below and fails there, as it
    // should.
    match std::fs::metadata(store) {
        Ok(m) if m.len() > SPREADS_MAX_BYTES => return GallerySpreads::default(),
        Ok(_) => {}
        Err(_) => return GallerySpreads::default(),
    }
    let Ok(text) = std::fs::read_to_string(store) else {
        return GallerySpreads::default();
    };
    let all: SpreadStore = serde_json::from_str(strip_bom(&text)).unwrap_or_default();
    let mut g = all
        .get(root.to_string_lossy().as_ref())
        .cloned()
        .unwrap_or_default();
    // Clamped, not obeyed. This is a file a person can open and edit, so a
    // `lead_lone` of 99 is a thing that will happen, and obeying it stands the
    // first 99 pages of the gallery alone.
    g.lead_lone = g.lead_lone.min(LEAD_LONE_MAX);
    // All of them or none, as `SPREADS_MAX_PAGES` says: keeping a subset would
    // mean keeping a different subset on every launch. `lead_lone` is untouched —
    // it is one number, it is not what the cap is about, and a gallery whose pages
    // were all dropped still has a front matter the reader chose.
    if g.pages.len() > SPREADS_MAX_PAGES {
        g.pages.clear();
    }
    g
}

/// How big the store may be before [`load_spread_overrides`] stops reading it — and
/// before [`save_spread_overrides`] stops writing to it.
///
/// **A read on the UI thread, which is why it is bounded at all.** It happens while
/// a gallery is being opened (`MyApp::refresh_spreads`), on a file whose size
/// nothing else bounds.
///
/// The value is deliberately generous rather than tight, and deliberately *not*
/// justified by a bytes-per-entry figure: the store holds one small object per
/// gallery the user has ever overridden something in, and nothing in this app grows
/// it except `set_shape`. What the cap buys is that a hand-edited or hostile file
/// cannot make the app read an arbitrary amount of someone's disk.
///
/// Over the cap the load yields **nothing**, not a truncated read: a half-file does
/// not parse, so "what was readable" of an oversized store is nothing either way.
///
/// **A threshold, not a write invariant — and the two directions differ because they
/// must.** A load may treat an over-cap store as empty (it only reads; the file is
/// still there); a save may not (its read is the first half of a read-modify-write,
/// so writing what it could not read would destroy what it could not read). What
/// nothing does is keep *writes* under the cap: a save whose serialised result would
/// exceed it is not refused, and can leave a store that every later save then
/// refuses. See [`save_spread_overrides`] for what that costs and why it is accepted.
pub const SPREADS_MAX_BYTES: u64 = 1 << 20;

/// Most page overrides one gallery's entry may hold, whatever the file's size.
///
/// The size cap already bounds what a *file* can produce, so this is the second
/// line of defence for the in-memory store — and it exists because the two degrade
/// differently. It is deliberately **all of them or none**: past the cap the whole
/// page map is dropped rather than truncated to some of its entries, because a
/// `HashMap`'s iteration order is randomised, so a truncated load would hand back a
/// *different* set of overrides on every launch and nothing would ever look wrong.
/// `lead_lone` survives; it is one number and it is not what the cap is about.
pub const SPREADS_MAX_PAGES: usize = 20_000;

/// The store's own name plus `.tmp.<pid>` — the half-written-entry guard, made
/// unique per process.
///
/// **Per process, not per call.** A fixed name is what the thumbnail cache uses and
/// it is safe *there* because two writers of one cache entry are writing the same
/// bytes: same path, same stamp, same target, so the same payload. That reasoning
/// does not transfer, because this file is neither content-addressed nor
/// whole-overwritten — it is a **read-modify-write**, so two instances saving
/// different galleries milliseconds apart both open the same name with `Create` /
/// `TRUNCATE`, interleave their writes, and each renames the torn result over
/// `spreads.json`. The reader guarantee (`fs::rename` replaces, atomically) holds
/// throughout; what it hands over is a mixture. Then, because an unparseable store
/// is treated as empty and replaced, the next override press rewrites the file with
/// one gallery's entry and **every other gallery's decisions are gone**, silently
/// and with no second copy anywhere. Per-process naming means a second instance
/// writes its own temporary and renames a complete file over the first one's.
///
/// Per *process* rather than per call is enough because the app saves from one
/// thread — the UI — and the collision that matters is between two instances of the
/// app, not between two of its own threads.
///
/// **Appended, not `with_extension`.** `spreads.json`.with_extension("tmp") is
/// `spreads.tmp`: a different name for the same file, and one that reads as a
/// different file in a directory listing.
///
/// Takes the id rather than reading [`std::process::id`] itself so the naming is a
/// pure function of its arguments and a test can compare two ids' names. Public for
/// that reason, as [`thumb_cache_entry`] is.
pub fn spreads_tmp(store: &Path, pid: u32) -> PathBuf {
    let mut name = store.as_os_str().to_os_string();
    name.push(format!(".tmp.{pid}"));
    PathBuf::from(name)
}

/// Write one gallery's overrides into the store.
///
/// **A read-modify-write, not a whole-file overwrite.** One file holds every
/// gallery, so writing one gallery's entry over the top of it would delete every
/// other gallery's decisions — with no error and nothing on screen to say so. A
/// store that cannot be *parsed* is treated as empty and replaced, which is the
/// same rule [`load_spread_overrides`] reads it by.
///
/// **An over-cap store is LEFT ALONE, and that is the opposite of what a load
/// does with one.** [`load_spread_overrides`] over [`SPREADS_MAX_BYTES`] returns
/// empty: the file is simply not honoured this session, which is visible (the
/// pairings are the detected ones) and recoverable (the file is still there, and
/// shrinking or deleting it brings the decisions back). A save cannot do that,
/// because saving *is* this read: read-modify-write against a store it could not
/// read would replace that gallery's real entry — and every other gallery's, since
/// a parse failure takes the whole file — with what it read, which is nothing. That
/// is the same silent, unrecoverable loss [`spreads_tmp`] exists to prevent, one
/// level down, and it is why this returns an error over the cap instead of writing.
///
/// So: **no bytes are written, the file is left exactly as it was, and the caller
/// is told the decision was not persisted.** The app's caller ignores that error —
/// the decisions are still in memory for the session — but the contract is
/// `Err`, because a save that did not happen should not report success.
///
/// A store within the cap but over [`SPREADS_MAX_PAGES`] for one gallery is a
/// different case and is *not* refused: it parses, so replacing that one entry
/// loses only that entry, which is what the user just asked to change anyway.
///
/// **What is accepted, deliberately, because it is the mirror of the rule above.**
/// Nothing refuses a write whose serialised result would exceed
/// [`SPREADS_MAX_BYTES`], so a save *can* leave the store over the cap — and after
/// that every later save refuses, until the file is shrunk or deleted. Nothing is
/// ever corrupted: the decisions stay in the file, and they come back the moment it
/// is under the cap. So the visible consequence is that the override key appears to
/// do nothing, which is bounded and points the safe way — the alternative rule
/// (refuse the oversized write) would mean refusing to save at all once a store had
/// drifted over, which loses the very decisions the cap exists to protect. The
/// threshold is deliberately not an invariant on writes; see [`SPREADS_MAX_BYTES`].
pub fn save_spread_overrides(
    store: &Path,
    root: &Path,
    g: &GallerySpreads,
) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(store) {
        if meta.len() > SPREADS_MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "the overrides file is {} bytes, over the {SPREADS_MAX_BYTES}-byte \
                     cap, so it was not read and not written",
                    meta.len()
                ),
            ));
        }
    }
    let mut all: SpreadStore = std::fs::read_to_string(store)
        .ok()
        .as_deref()
        // Stripped rather than parsed raw, and this is the load-bearing of the two
        // sites: an unparseable store is *replaced*, so a BOM that made this parse
        // fail would turn the next override press into a delete of every other
        // gallery's entry. See `strip_bom`.
        .and_then(|text| serde_json::from_str(strip_bom(text)).ok())
        .unwrap_or_default();
    all.insert(root.to_string_lossy().into_owned(), g.clone());

    if let Some(parent) = store.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&all)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // Write beside the store and rename into place, the thumbnail cache's rule:
    // `fs::rename` replaces on Windows, so a reader sees either the old file or
    // this one, never a half-written one. The temporary carries this process's id
    // (`spreads_tmp`) so a second instance of the app cannot tear this write — the
    // one thing that is *not* inherited from the thumbnail cache, and the reason
    // this is not a fixed name.
    //
    // A partial temporary is never renamed, so nothing can read one, and it is not
    // deleted on failure either: the next save from this process overwrites the same
    // name.
    let tmp = spreads_tmp(store, std::process::id());
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, store)
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
    load_page_preview_full(path, preview_max_dim, 0)
}

/// [`load_image_preview_full`] for a specific page of a multi-page file.
///
/// `page` is 0-based and only TIFF has more than one; every other format ignores
/// it, because a GIF's "pages" are frames and are handled by the animation path
/// above. Page 0 goes through exactly the same code it always did — the existing
/// single-page function is this one with a zero — so nothing about how the common
/// case decodes has changed.
///
/// **Every decode the app does goes through here**, which is why the wide-gamut
/// conversion is applied by the wrapper rather than at the five places inside
/// `decode_page` that return: preview, full, the DCT path, the embedded EXIF
/// thumbnail, and animations all leave through one of them, and one that forgets is
/// exactly the bug this shape exists to make impossible. Tiles are cut from the
/// converted `full`, so they come along for free, and so does the clipboard.
pub fn load_page_preview_full(
    path: &Path,
    preview_max_dim: u32,
    page: u32,
) -> Result<PreviewAndFull, LoadError> {
    let mut out = decode_page(path, preview_max_dim, page, Purpose::View)?;
    apply_wide_gamut_conversion(path, &mut out);
    Ok(out)
}

/// The picture one grid cell shows: at most `target` on the long side, and never
/// a full-resolution image.
///
/// A cell is one picture at a fixed small size, so unlike the main view it gains
/// nothing from the full image — and the wide-gamut conversion is applied by the
/// same wrapper that applies it everywhere else, because a cell that converted its
/// neighbour differently from the image it opens would be its own bug.
///
/// **The downscale is here rather than inside `decode_page` on purpose.** It is the
/// one step that has to happen after *every* one of that function's return points,
/// and the same argument [`load_page_preview_full`] makes about the conversion. It
/// is also not optional: `decode_jpeg_scaled` can only pick 1/8, 1/4, 1/2 or 1, so
/// the smallest reduction that still covers a 256 px cell is 2100 px for a 17000 px
/// scan — 6 MB of texture for a 150 pt cell, and 44x what was asked for. The main
/// view deliberately does not do this: a preview is drawn at whatever density the
/// display can use, and a cell is not.
///
/// Ordered before the conversion on purpose: 166 MP/s over 2100x700 is 9 ms, over
/// 256x84 it is 0.2 ms.
pub fn load_thumbnail(path: &Path, target: u32) -> Result<ImageData, LoadError> {
    let mut out = decode_page(path, target, 0, Purpose::Thumb)?;
    if out.preview.width.max(out.preview.height) > target {
        out.preview = downscale_image(&out.preview, target);
    }
    apply_wide_gamut_conversion(path, &mut out);
    Ok(out.preview)
}

fn decode_page(
    path: &Path,
    preview_max_dim: u32,
    page: u32,
    purpose: Purpose,
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

    // A page after the first, of a multi-page TIFF. Ahead of everything else
    // because no other format has pages, and a TIFF is not animated, so the
    // animation path below cannot claim it.
    //
    // `orig` is deliberately left as the *file's* first-page dimensions: it is
    // what the window fits the image into and what the zoom maths uses, and
    // pages of one document are near enough always the same size. It is
    // recomputed from the decoded page below, because a document whose pages
    // differ in size is legal and showing page 3 in page 1's frame is wrong.
    if page > 0 && is_tiff_bytes(&bytes) {
        let data = decode_tiff_page(&bytes, page, max_dim, start)?;
        let dims = (data.width, data.height);
        return Ok(PreviewAndFull {
            preview: data,
            full: None,
            anim: None,
            orig: Some(dims),
            below_target: false,
            exif_tag: tag,
            crop_grid,
        });
    }

    // An animation: decode the whole loop once; no second full decode, no tiles.
    //
    // GIF, APNG and animated WebP all land here, and all three used to be handled
    // differently — in fact APNG and animated WebP were not handled at all, and
    // showed their first frame forever, silently. Whether it is an animation at
    // all is asked first, because a still image of an animated format must not be
    // decoded twice: once as a "one frame animation" and again by the normal path
    // below.
    if let Some(fmt) = animated_format(&bytes) {
        if purpose == Purpose::Thumb {
            // A cell is one picture. Decoding the loop to show frame 0 is the same
            // mistake as decoding full-res for a 150 px cell, and worse: an APNG or
            // animated WebP is capped at 240 frames and 256 MB, so a grid of cells
            // could decode hundreds of megabytes to display a few kilobytes.
            //
            // `is_animated` is deliberately *not* asked first. It costs two frame
            // decodes to answer a question a cell does not have: whether the file
            // has one frame or many, the first frame is the same picture either
            // way, and the fall-through below would decode it a second time.
            if let Some(data) = first_frame(&bytes, max_dim, start, fmt) {
                let dims = (data.width, data.height);
                return Ok(PreviewAndFull {
                    preview: data,
                    full: None,
                    anim: None,
                    orig: Some(dims),
                    below_target: true,
                    exif_tag: tag,
                    crop_grid,
                });
            }
        } else if is_animated(&bytes, fmt) {
            if let Ok(anim) = load_animation(&bytes, PREVIEW_TEXTURE_DIM, start, fmt) {
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
        }
        // A still image of an animated format, or one that would not decode:
        // fall through to the normal path, which handles it once.
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
        // A grid cell: decode at the cell's size and stop. Ahead of the
        // `MAX_FULL_PIXELS` arm below because it subsumes it — that arm decodes at
        // `max_dim` too — and because a 256 px request has no use for the
        // full-resolution `ImageData` that used to be built, returned and sent
        // across a channel for every PNG, HEIC and JXL in a folder.
        _ if purpose == Purpose::Thumb => {
            let mut preview = normalized(decode_bytes(&bytes, max_dim, start)?);
            apply_exif_orientation(&mut preview, tag);
            // A cell is always below source density by construction, and nothing
            // reads this for a cell; `true` says so rather than implying a
            // re-decode is worth issuing.
            (preview, None, true)
        }
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

/// What undecodable bytes actually are, for the failure message — used only
/// when no decoder was even attempted, so a corrupt file of a supported
/// kind keeps the decode error instead of gaining a wrong explanation.
///
/// Extensions lie routinely (an MP4 renamed `.gif`), and the guesser's
/// "format could not be determined" answers neither "what is it" nor "can
/// I do anything about it". Naming the container answers both: nothing here
/// will ever decode it, and the remedy is a different program.
fn unsupported_error(bytes: &[u8]) -> LoadError {
    if bytes.is_empty() {
        return LoadError::Other("unsupported file: empty (0 bytes)".into());
    }
    if let Some(brand) = isobmff_brand(bytes) {
        // `is_heif_bytes` brands never reach here — they decode above — so a
        // remaining `ftyp` is something else in the same container family,
        // overwhelmingly video or audio.
        let name = match brand {
            b"isom" | b"iso2" | b"iso3" | b"iso4" | b"iso5" | b"iso6" | b"mp41"
            | b"mp42" | b"mp4v" | b"avc1" => "MP4 video",
            b"M4V " | b"M4VH" => "M4V video",
            b"M4A " => "M4A audio",
            b"qt  " => "QuickTime video",
            b"3gp4" | b"3gp5" | b"3gp6" | b"3g2a" => "3GP video",
            _ => "",
        };
        if name.is_empty() {
            let brand = String::from_utf8_lossy(brand);
            let brand: String = brand
                .chars()
                .map(|c| if c.is_ascii_graphic() { c } else { '?' })
                .collect();
            return LoadError::Other(format!(
                "unsupported file: ISO-BMFF container (brand '{brand}'), not a decodable image"
            ));
        }
        return LoadError::Other(format!(
            "unsupported file: {name} (ISO-BMFF brand '{}'), not a decodable image",
            String::from_utf8_lossy(brand)
        ));
    }
    // RIFF with a non-WEBP form: WAV and AVI start the same way, which is
    // why `looks_like_supported_image` demands the form type — and why the
    // message can name exactly which one it is.
    if bytes.starts_with(b"RIFF") && bytes.len() >= 12 {
        let form = String::from_utf8_lossy(&bytes[8..12]);
        let name = match &bytes[8..12] {
            b"WAVE" => "WAV audio",
            b"AVI " => "AVI video",
            _ => "",
        };
        if !name.is_empty() {
            return LoadError::Other(format!("unsupported file: {name}, not a decodable image"));
        }
        return LoadError::Other(format!(
            "unsupported file: RIFF container (form '{form}'), not a decodable image"
        ));
    }
    // Short ASCII magics: vector, document and archive containers. Checked
    // after the binary ones because none of them can overlap.
    let texty: &[(&[u8], &str)] = &[
        (b"<svg", "SVG vector image"),
        (b"%PDF", "PDF document"),
        (b"PK\x03\x04", "ZIP archive"),
        (b"Rar!", "RAR archive"),
        (b"7z\xBC\xAF\x27\x1C", "7z archive"),
        (b"ID3", "MP3 audio"),
        (b"OggS", "Ogg audio/video"),
        (b"MZ", "Windows executable"),
    ];
    // Leading whitespace/BOM is skipped: an SVG with an XML prolog is still
    // an SVG, and the alternative is calling pretty-printed XML unrecognized.
    let mut head = bytes;
    while head.starts_with(b"\xEF\xBB\xBF") {
        head = &head[3..];
    }
    while !head.is_empty() && matches!(head[0], b' ' | b'\t' | b'\r' | b'\n') {
        head = &head[1..];
    }
    for (magic, name) in texty {
        if head.starts_with(magic) {
            return LoadError::Other(format!("unsupported file: {name}, not a decodable image"));
        }
    }
    LoadError::Other(format!(
        "unsupported file type (starts with {:02X?})",
        &bytes[..bytes.len().min(8)]
    ))
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

    let img = match image::load_from_memory(bytes) {
        Ok(img) => img,
        // Bytes no decoder claims: say what they are instead of parroting the
        // guesser's "could not be determined". Gated on the magic, so a
        // corrupt file of a supported kind keeps the decode error — which is
        // the honest message there — rather than gaining a wrong explanation.
        Err(e) => {
            if looks_like_supported_image(bytes) || is_jxl_bytes(bytes) || is_heif_bytes(bytes) {
                return Err(e.into());
            }
            return Err(unsupported_error(bytes));
        }
    };
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

/// How much wider than the Nyquist minimum the resampling kernel runs when
/// `Smooth` is on. **This is the strength axis, and it is the only one this
/// codebase has.**
///
/// `FAST_STRENGTH` is exactly the width the kernel had before the setting
/// existed, so `Fast` is the old behaviour and not a third flavour of it.
/// `SMOOTH_STRENGTH` is measured rather than chosen: see
/// `tests/unit.rs::print_square_lattice_sweep` and `print_real_page_strength_sweep`
/// for the sweep it came out of, and `smooth_costs_edge_sharpness_and_the_cost_is_bounded`
/// for the cost it was accepted at.
///
/// **Why a multiplier on `filter_scale` and not on the tap count.**
/// `contributions` already divides the kernel argument by `filter_scale`, so
/// widening the *span* while leaving `filter_scale` alone evaluates every extra
/// tap at exactly zero and is bit-identical - that is a measured result, not an
/// inference. Widening `filter_scale` widens the argument *and* the span
/// together, so the extra taps are real. And it is the one lever that reaches
/// both halves of the resize: the pre-reduction's own `filter_scale` and the
/// final pass's.
pub const FAST_STRENGTH: f32 = 1.0;
pub const SMOOTH_STRENGTH: f32 = 2.0;

/// The strength a worker sees, as a float in an atomic. Workers read it; the UI
/// thread writes it, from the settings toggle.
static DOWNSCALE_STRENGTH: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(SMOOTH_STRENGTH.to_bits());

/// Set the kernel strength directly. `set_downscale_smoothing` is the user-facing
/// spelling; this exists so a test can put an arbitrary strength on the axis and
/// sweep it.
pub fn set_downscale_strength(strength: f32) {
    DOWNSCALE_STRENGTH.store(strength.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The kernel strength in force, as read by the resampler.
pub fn downscale_strength() -> f32 {
    f32::from_bits(DOWNSCALE_STRENGTH.load(std::sync::atomic::Ordering::Relaxed))
}

/// The `Smooth` / `Fast` toggle. `true` is `SMOOTH_STRENGTH`, `false` is
/// `FAST_STRENGTH`, which is what the path did before either existed.
pub fn set_downscale_smoothing(on: bool) {
    set_downscale_strength(if on { SMOOTH_STRENGTH } else { FAST_STRENGTH });
}

/// Is the strength at least `SMOOTH_STRENGTH`? Public so tests can save and
/// restore it around assertions that only hold on one side of the toggle, the
/// same reason `linear_resample_enabled` exists.
pub fn downscale_smoothing() -> bool {
    downscale_strength() >= SMOOTH_STRENGTH
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

/// Width or height the linear-light prefilter reduces to: **at most
/// [`PREFILTER_FACTOR`]x the target**, and never larger than the dimension itself.
///
/// **This is also the threshold [`needs_refine`] judges an over-sized texture by**,
/// and that sharing is the point rather than tidiness: the factor is what the
/// pipeline filters at, so a texture more than that above the target it is painted
/// at is a picture nothing in this crate has low-passed — and at a request above
/// the source's own long side the pre-reduction cannot fire at all, which is how
/// a texture ends up **unfiltered altogether**.
///
/// This is the memory bound for `linear_resize`. The 12-bit linear conversion is
/// the expensive step, so it runs on the pre-reduced buffer rather than on the
/// full-resolution source: on a 6.9 MP page that is 26.7 MB instead of 55 MB,
/// and deleting the step roughly doubles the conversion cost as well. It is a
/// function rather than an inline expression so a test can pin it — nothing
/// observable from outside says how big the intermediate is, and a single kernel
/// pass scores the same or better on every quality measurement, so a deleted
/// pre-reduction is invisible except in memory.
///
/// `saturating_mul`, not `*`: `target` is a display dimension and the largest
/// this codebase can be handed is `MAX_TEXTURE_DIM`, but the arithmetic should
/// not depend on that staying true.
/// **This is a memory bound and not a quality knob, which is worth being explicit
/// about because it was read as a quality knob for a long time** — it is the
/// number the reported 1440p moiré was blamed on. Measured: `contributions`
/// derives `filter_scale` from each stage's *own* ratio, so the stages of a split
/// decimation multiply back to the reduction exactly (`s1 * s2 == R`), and
/// lowering this factor cannot widen the low-pass — it only changes how large the
/// buffer the 12-bit conversion runs over is. What sets the width is
/// [`MIN_FILTER_SCALE`].
pub fn prefilter_dim(dim: u32, target: u32) -> u32 {
    dim.min(target.saturating_mul(PREFILTER_FACTOR)).max(1)
}

/// Source span `(start, len)` covered by each of `n` output samples when
/// Separable **kernel** pre-reduction of an RGBA8 buffer to `pre_w x pre_h`,
/// averaging in 12-bit linear light and handing back sRGB8 at exactly the size
/// the box this replaced used.
///
/// **Why a kernel and not a box.** A box average's sinc sidelobes sit near
/// -13 dB and do not roll off, so at the 2.879x reduction of a 2223x3109 page
/// (the manga half-panel target of 1080) the prefilter was aliasing *more* than
/// the Hermite pass it fed. Measured as the output amplitude of a 100-code
/// sinusoid — i.e. the transfer function — the box left 21-29 codes in the band
/// below source period 4 px, where a widened kernel leaves 2-11, and the two
/// tie above source period ~6 px. At every reduction from 2x to 16x the kernel
/// is lower or equal.
///
/// **Why linear light, and not sRGB.** A weighted average is exposure-preserving
/// only in the space it averages in. Averaging *gamma-encoded* values of a 50%
/// near-Nyquist dither gives ~128 — the pattern's own sRGB mean — where the
/// tone's true average reflectance is 50% linear, i.e. sRGB ~188. Measured on
/// real manga pages, an sRGB-space kernel prefilter came out **5-7 codes darker
/// per page** than the box it replaced, which is a far worse bug than the
/// sparkle it removes. In linear light the same prefilter lands within 0.3
/// codes of what a single kernel pass over the whole source produces, i.e. it
/// stops second-guessing the space the rest of the path works in.
///
/// The taps are converted per tap rather than by buffering the source in 12-bit:
/// a full-resolution 12-bit buffer is 55 MB on a 6.9 MP page against 26.7 MB
/// for the intermediate, and AGENTS.md's memory section is explicit that this
/// conversion must not run over the full-resolution source. Row-converting
/// first (one `u16` row per worker) was measured and is *slower* — 565-707 ms
/// against 428-519 ms on the 6.9 MP page — so the LUT stays in the inner loop.
///
/// Both passes are row-parallel, which the box's vertical pass could not be: it
/// summed a sliding window down a column accumulator, so it ran on one thread.
/// That, not the arithmetic, is where the speedup comes from.
///
/// **`floor` is a parameter rather than a read, and `linear_resize` passes a
/// literal `0.0` for it.** The pre-reduction runs at exactly
/// `PREFILTER_FACTOR`, so `scale == 0.5` and `filter_scale == 2.0` already —
/// above any floor worth having — and a floor here can therefore only widen a
/// kernel that already meets it. Measured, not inferred: handing this stage the
/// real floor changes the output at R 2.02 and above, i.e. at every reduction the
/// shipped threshold already covers.
fn kernel_reduce_linear(data: &ImageData, pre_w: u32, pre_h: u32, strength: f32, floor: f32) -> Vec<u8> {
    let (to_lin, to_srgb) = lin_tables();
    let (sw, sh) = (data.width as usize, data.height as usize);
    let (pw, ph) = (pre_w as usize, pre_h as usize);
    let (kern, support) = kernel(downscale_filter());
    let horizontal = contributions(sw as u32, pw as u32, &kern, support, strength, floor);
    let vertical = contributions(sh as u32, ph as u32, &kern, support, strength, floor);

    // Horizontal: source width -> pre_w, at full height.
    let mut tmp = vec![0u8; pw * sh * 4];
    let src_stride = sw * 4;
    let src = &data.rgba;
    tmp.par_chunks_mut(pw * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let row = &src[y * src_stride..(y + 1) * src_stride];
            for (x, con) in horizontal.iter().enumerate() {
                let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
                for &(si, w) in con {
                    let p = &row[si * 4..si * 4 + 4];
                    r += to_lin[p[0] as usize] as f32 * w;
                    g += to_lin[p[1] as usize] as f32 * w;
                    b += to_lin[p[2] as usize] as f32 * w;
                    a += (p[3] as f32) * 16.0 * w;
                }
                let o = x * 4;
                out_row[o] = to_srgb[r.max(0.0).min(4095.0) as usize];
                out_row[o + 1] = to_srgb[g.max(0.0).min(4095.0) as usize];
                out_row[o + 2] = to_srgb[b.max(0.0).min(4095.0) as usize];
                out_row[o + 3] = ((a.max(0.0).min(4080.0) / 16.0).round()) as u8;
            }
        });
    if ph == sh {
        return tmp;
    }

    // Vertical: source height -> pre_h. Alpha is linear by definition and needs
    // no LUT; it rides the same 0..4080 scale `lin` uses for it.
    let mut out = vec![0u8; pw * ph * 4];
    let tmp_stride = pw * 4;
    out.par_chunks_mut(tmp_stride)
        .enumerate()
        .for_each(|(y, out_row)| {
            for x in 0..pw {
                let i = x * 4;
                let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
                for &(si, w) in &vertical[y] {
                    let p = &tmp[si * tmp_stride + i..si * tmp_stride + i + 4];
                    r += to_lin[p[0] as usize] as f32 * w;
                    g += to_lin[p[1] as usize] as f32 * w;
                    b += to_lin[p[2] as usize] as f32 * w;
                    a += (p[3] as f32) * 16.0 * w;
                }
                out_row[i] = to_srgb[r.max(0.0).min(4095.0) as usize];
                out_row[i + 1] = to_srgb[g.max(0.0).min(4095.0) as usize];
                out_row[i + 2] = to_srgb[b.max(0.0).min(4095.0) as usize];
                out_row[i + 3] = ((a.max(0.0).min(4080.0) / 16.0).round()) as u8;
            }
        });
    out
}


/// Resample in 12-bit linear light with a separable kernel.
///
/// Large reductions get a **kernel pre-reduction** first, down to at most 2x the
/// target ([`prefilter_dim`]). Measured on a 40 MP source this took the resample
/// from ~790ms to well under 200ms, and it is a quality win too: a big reduction
/// straight into Lanczos undersamples and aliases, because its ~6 taps end up
/// spread over thousands of source pixels.
///
/// Two things about the shape are load-bearing and both were measured rather than
/// chosen. **The intermediate size is the memory bound**: the 12-bit conversion
/// runs on the pre-reduced buffer, never on the full-resolution source (55 MB vs
/// 26.7 MB on a 6.9 MP page), so the pre-reduction cannot be deleted for quality
/// — a single kernel pass scores the *same or better* on every quality probe and
/// still costs twice the memory. **The prefilter averages in linear light**:
/// `kernel_reduce_linear` is a wrapper for the same reason
/// `apply_wide_gamut_conversion` is, because the conversion has to be applied at
/// the one place the values are combined rather than at each of a function's
/// return points.
///
/// The final pass therefore still sees `dFactor` 2.0000, unchanged from when the
/// prefilter was a box — its **tap count**, however, is whatever the strength
/// asks for: Hermite's 4-5 at `FAST_STRENGTH` and about twice that at
/// `SMOOTH_STRENGTH`, which is the whole of the extra ~25 ms a resize pays.
fn linear_resize(data: &ImageData, new_w: u32, new_h: u32) -> Vec<u8> {
    let (to_lin, to_srgb) = lin_tables();

    // **One read of the strength, one value, both halves.** The pre-reduction
    // and the final passes each call `contributions`, and the pre-reduction's
    // output is the final pass's input - so a strength read separately in each
    // would let the two halves disagree, i.e. resize with a prefilter of one
    // width feeding a final pass of another. Read once, thread it.
    let strength = downscale_strength();
    // **One read of the floor, one value, both axes.** The same argument and the
    // same count-checked test as the strength above: the two axes are separate
    // `contributions` calls, so a per-call read would let a toggle pressed
    // between them produce a picture low-passed one way horizontally and another
    // vertically.
    let floor = min_filter_scale();
    let pre_w = prefilter_dim(data.width, new_w);
    let pre_h = prefilter_dim(data.height, new_h);

    // Pass 1: reduce to at most 2x the target, in linear light, then convert
    // *that* to 12-bit linear. The conversion is the expensive part, so it must
    // not run over the full-resolution source.
    let t0 = std::time::Instant::now();
    let (small, lw, lh): (Cow<'_, [u8]>, u32, u32) = if pre_w < data.width || pre_h < data.height {
        (
            // **The literal `0.0`, and it is load-bearing** - see
            // `kernel_reduce_linear`'s floor parameter. Passing `floor` here
            // instead is the mistake this line is written to prevent, and
            // `the_floor_reaches_both_final_axes_and_only_those` is the check.
            Cow::Owned(kernel_reduce_linear(data, pre_w, pre_h, strength, 0.0)),
            pre_w,
            pre_h,
        )
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
    let horizontal = contributions(lw, new_w, &kern, support, strength, floor);
    let vertical = contributions(lh, new_h, &kern, support, strength, floor);

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
            "  linear_resize {}x{}->{}x{}: prefilter={:?} to_linear={:?} weights={:?} hpass={:?} vpass={:?}",
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
    strength: f32,
    floor: f32,
) -> Vec<Vec<(usize, f32)>> {
    let mut out = Vec::with_capacity(dst_len as usize);
    let scale = dst_len as f32 / src_len as f32;
    // When downscaling, widen the filter by the inverse ratio (anti-aliasing),
    // then by `strength` on top: one multiplier reaching the pre-reduction and
    // the final passes alike, rather than a per-call-site decision that could
    // disagree with itself.
    //
    // **`floor` is [`MIN_FILTER_SCALE`] on the final pass and `0.0` on the
    // pre-reduction, and the asymmetry is the entire mechanism.** `scale` is each
    // stage's *own* ratio and the product of a split's two stages is the
    // reduction exactly, so the floor can only ever bind on a stage whose ratio
    // is above `1 / MIN_FILTER_SCALE` — which, for the pre-reduction, is no stage
    // at all, since it reduces by exactly `PREFILTER_FACTOR`. Hence `max` here
    // and `0.0` there, and hence a no-op on every reduction the prefilter
    // already covers.
    let filter_scale = if scale < 1.0 { (strength / scale).max(floor) } else { 1.0 };
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

/// The archive containers this app can open.
///
/// A member listing is the only thing every one of these promises: read the
/// names, count them, decide which are images. What comes *out* of a member is
/// the reader's business and is not uniform — see [`extract_archive`].
pub const ARCHIVE_EXTENSIONS: [&str; 4] = ["zip", "cbz", "7z", "rar"];

/// True if the path looks like an image archive this app can open.
///
/// **Listing an archive is not decoding it.** This predicate decides what a
/// folder scan yields; [`is_browsable`] adds the images to it, and neither puts
/// an archive in [`SUPPORTED_EXTENSIONS`], which is what the decoders and
/// [`extract_zip`] filter members with. A zip in there would make a container
/// something the image decoders get handed.
pub fn is_archive_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| ARCHIVE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Turn one archive member's name into the file name it will be written as, or
/// `None` if it should not be written at all.
///
/// **Shared by all three readers**, which is the point: the gallery inside an
/// archive is the same whichever container it came from, so a `.rar` and a
/// `.cbz` of the same pages produce the same files in the same order.
///
/// Three filters, each load-bearing:
///  * **directories** — an archive member may name a folder.
///  * **non-images** — by [`SUPPORTED_EXTENSIONS`], which is deliberately *not*
///    the listing predicate. A `.zip` inside a `.zip` is not extracted, so an
///    archive can never contain a browsable archive and the gallery is one level
///    deep by construction.
///  * **illegal and reserved names** — entries may carry characters NTFS
///    refuses, plus the trailing dot or space it silently strips, so the name is
///    compared *after* cleaning rather than before.
fn archive_member_name(raw: &str) -> Option<String> {
    let name = std::path::Path::new(raw).file_name()?.to_str()?;
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let safe: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20 {
                '_'
            } else {
                c
            }
        })
        .collect();
    // NTFS silently drops a trailing dot or space, so `page.png ` would collide
    // with `page.png` after cleaning - compare what will actually be on disk.
    let trimmed = safe.trim_end_matches(['.', ' ']).to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Name an extracted member uniquely, case-insensitively.
///
/// Two archive members can flatten to the same file name — `ch1/page01.jpg`
/// twice, or `page01.jpg` and `PAGE01.JPG` — and Windows treats those as one
/// file, so the second would silently overwrite the first and the gallery would
/// be a page shorter than the archive with no error anywhere. Returns `false`
/// when the name is taken.
fn claim_member_name(seen: &mut std::collections::HashSet<String>, safe: &str) -> bool {
    seen.insert(safe.to_lowercase())
}

/// A file the app now owns a copy of, and the directory that copy lives in.
///
/// The directory is a separate value because **it is the unit that is deleted**:
/// the caller hands it back when the app moves on, and `remove_dir_all` is the
/// only cleanup there is. Two copies of this struct are never live at once -
/// `MyApp` holds one `Option` - so the ownership is a single slot, the same slot
/// discipline the archive's temp dir already uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropCopy {
    pub dir: PathBuf,
    pub file: PathBuf,
    /// **The copy was already in `dir` and no bytes were moved.**
    ///
    /// Recorded because it is the difference between a drop that costs two
    /// `stat` calls and a drop that costs a read and a write of the whole file,
    /// and because a status bar that says "copied" when nothing was copied is
    /// lying about the one thing the user just waited for.
    pub reused: bool,
}

/// Where owning a dropped file *would* put its copy, and whether it is already
/// there — the decision, with no bytes moved.
///
/// **Split from the copy so the copy can happen on a worker.** The decision is
/// two `stat` calls and a hash, which is microseconds and is safe anywhere; the
/// copy is a read and a write of a file whose size the app does not know, which
/// is hundreds of milliseconds and is safe nowhere but off the UI thread. See
/// `own_dropped`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropCopyPlan {
    pub dir: PathBuf,
    pub file: PathBuf,
    /// A copy of exactly this source is already in `dir`.
    pub reuse: bool,
}

impl DropCopyPlan {
    /// Carry out the plan: copy `src` into `file` **unless a valid copy is
    /// already there**.
    ///
    /// **The validity check is made here rather than trusted from the plan.**
    /// `plan.reuse` was decided at plan time and the bytes move later, so
    /// re-deriving the source's length here is what makes the answer a property
    /// of the file rather than of a value latched one message ago — the same
    /// "derive it, do not remember it" rule the rest of this crate is built on.
    /// Trusting the flag would mean a plan made before a 500 MB copy cannot be
    /// wrong about it; re-checking cannot.
    pub fn materialise(&self, src: &Path) -> Result<DropCopy, String> {
        let want = std::fs::metadata(src)
            .map_err(|e| format!("{}: {e}", src.display()))?
            .len();
        let reused = copy_is_current(&self.file, want);
        if !reused {
            std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
            // Re-owning must be idempotent, and `copy` writes through an existing
            // destination rather than refusing it. Removing first is what makes a
            // second drop of the same file a clean overwrite instead of leaving a
            // stale tail. Only on the copy path: a reused copy is not touched at
            // all, which is the whole point of `reuse`.
            let _ = std::fs::remove_file(&self.file);
            std::fs::copy(src, &self.file).map_err(|e| format!("{}: {e}", self.file.display()))?;
        }
        Ok(DropCopy {
            dir: self.dir.clone(),
            file: self.file.clone(),
            reused,
        })
    }
}

/// Is there already a copy of exactly `want` bytes at `dst`?
///
/// **A length check, and the length is half the identity.** `drop_copy_dir`
/// hashes the source's **path, mtime and length**, so reaching this function
/// with `dst` inside the planned directory already means "same path, same
/// mtime, same length" — the same `(mtime, len)` stamp the folder watcher calls
/// a change, so the reuse check is exactly as strong as change detection
/// anywhere else in this app. A rewrite that preserves *both* mtime and length
/// is invisible here, and is invisible to the watcher too; there is no stamp
/// that catches it.
///
/// A length check and not an existence check, because a half-written copy from a
/// killed session is a file that exists and is the wrong thing to show.
fn copy_is_current(dst: &Path, want: u64) -> bool {
    std::fs::metadata(dst).is_ok_and(|m| m.is_file() && m.len() == want)
}

/// Decide what owning `src` would do, without moving any bytes.
///
/// Pure apart from the two `stat` calls it needs to answer the question, so the
/// app can ask it on the UI thread and do the copy on a worker — or ask it and
/// the copy in the same place, which is what `own_dropped` is.
pub fn plan_drop_copy(src: &Path) -> Result<DropCopyPlan, String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", src.display()));
    }
    let name = src
        .file_name()
        .ok_or_else(|| format!("{} has no file name", src.display()))?;
    let dir = drop_copy_dir(src, &meta);
    let file = dir.join(name);
    Ok(DropCopyPlan {
        reuse: copy_is_current(&file, meta.len()),
        dir,
        file,
    })
}

/// Take a copy of a file that the app cannot expect to still be there, and say
/// where the copy is.
///
/// **A drag-and-drop is a hand-off, not a reference.** Explorer does not pass a
/// path into a zip's member and leave it there: it extracts the member to a
/// transient temp file, hands *that* path to the drop target, and deletes it a
/// moment later. So the source of a drop is transient **by construction**, and an
/// app that opens one in place is holding a path to a file with an expiry date on
/// it. What that cost, measured (`tests/drops.rs` and the trace in
/// `.superpowers/sdd/2026-10-02-manga-spread-mode/dropped-temp-file-vanishes.md`):
/// on the app's own 2 s poll the vanished file was a *changed* file, so
/// `invalidate_path` dropped the texture, `rescan` removed it from the gallery and
/// the re-requested decode failed into a `failed` entry that is permanent for the
/// session - shown for about a second, then gone with no way back.
///
/// Copying is the same shape the archive path already uses (`%TEMP%\image-viewer`
/// keyed by content), for the same reason: a member of an archive and a member
/// dragged out of one are the same kind of hand-off, and only one of them used to
/// be handled.
///
/// **A real `copy`, not a hard link** - and the reason is a measurement, because
/// the link was the obvious cheaper answer and it is wrong.
///
/// A hard link survives the *deletion* this function exists for: unlinking one
/// name leaves the data reachable through the other. So it passes the property
/// under test and still fails the thing that property is for. A link **shares the
/// file**, so any in-place write to the source mutates the "copy" too - verified
/// on this box, both ways a writer can do it:
///
/// ```text
/// dst after in-place rewrite: [82, 69, 76, ...]   // "REPLACED, longer contents"
/// dst after rename-over:      [82, 69, 76, ...]   // same
/// ```
///
/// The app would then be showing - and `Ctrl+S` would write - bytes under a path
/// it believes it owns, which is precisely the stale-or-aliasing class this
/// codebase treats as worse than slow (`AGENTS.md` on the watcher's stale
/// texture: "keeping old pixels for a re-exported file is the one outcome that
/// looks like a bug"). A snapshot is the whole point; an alias is not a snapshot.
///
/// **What it costs, honestly: a read and a write of the dropped file, once per
/// drop — and not on the UI thread.** `own_dropped` is the *blocking* form: the
/// plan, then the copy. The app does not call it from the drop handler. It calls
/// [`plan_drop_copy`] on the UI thread — two `stat` calls and a hash — hands the
/// copy to a worker thread, and opens the copy when the worker reports in. The
/// reason is the first line of `AGENTS.md`: this app never does I/O of
/// unbounded size on the thread that paints. Measured on this box, the
/// synchronous form cost **3.8 ms at 5 MB, 26 ms at 50 MB and 279 ms at 500 MB**
/// (`bench_browse drop_copy_ab`, release, warm cache, NVMe) — the 500 MB figure
/// is eighteen frames at 60 Hz, and it is linear in a size the app has no way to
/// know. A user should be told either way, which is why `MyApp::start_drop_copy`
/// puts "Copying…" in the status bar.
///
/// **A repeated drop of an unchanged file moves no bytes at all**, which is what
/// `reuse` is for and what the same benchmark pins: before the check existed the
/// repeat cost 3.2 / 24 / 244 ms against the cold arm's 3.8 / 26 / 279 ms —
/// the same work, because the directory was reused and the *copy* was remade.
/// Now it is two `stat` calls. The directory name is a hash of the source's
/// **path, mtime and length** (`drop_copy_dir`), which is what makes that
/// statement true rather than hopeful: reaching the reuse check at all means the
/// source has the same path, mtime and length as the copy's.
///
/// **What it does not fix.** If Explorer is still finalising the extraction when
/// the drop lands, the copy captures a partial file. That was a hypothesis
/// considered here and **not observed**: the trace of the real failure shows the
/// file decoded and painted correctly at frame 2 and was blanked at frame 14
/// because its *stamp changed when it vanished*. Mid-write is not a mechanism
/// this fix claims to address.
///
/// The name is a hash of the source's **path, mtime and length**, like
/// `MyApp::zip_temp_for`: two drops of the same unchanged file land in the same
/// directory, so the second one **reuses** the copy rather than remaking it (see
/// `copy_is_current`), and a file that changed under us gets a new directory
/// rather than silently overwriting a copy something else may be reading.
pub fn own_dropped(src: &Path) -> Result<DropCopy, String> {
    plan_drop_copy(src)?.materialise(src)
}

/// Where a dropped file's copy goes: `%TEMP%\image-viewer\drop\<16 hex>`.
///
/// **Under the OS temp directory and not under `%APPDATA%`**, for the reason
/// every cache in this app lives there: these are this machine's copies of files
/// that may be anywhere, they are worthless elsewhere, and a re-drop overwrites
/// rather than accumulating.
pub fn drop_copy_dir(src: &Path, meta: &std::fs::Metadata) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    meta.modified().ok().map(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).hash(&mut h);
    meta.len().hash(&mut h);
    std::env::temp_dir()
        .join("image-viewer")
        .join("drop")
        .join(format!("{:016x}", h.finish()))
}

/// Drop the copies this app owns. **Every failure is swallowed on purpose**:
/// a leftover directory in `%TEMP%` is harmless, and the alternative is an error
/// on a path that is already shutting down.
pub fn forget_drop_copy(copy: &DropCopy) {
    let _ = std::fs::remove_dir_all(&copy.dir);
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
        let Some(safe) = archive_member_name(f.name()) else {
            continue;
        };
        if !claim_member_name(&mut seen, &safe) {
            continue;
        }
        let mut out = std::fs::File::create(dest.join(&safe)).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
        extracted += 1;
    }
    Ok((extracted, encrypted))
}

/// Extract image entries from a 7z archive. Same contract as [`extract_zip`].
///
/// **Encrypted entries cannot be counted the way zip counts them**: 7z reports
/// one archive-level flag, so a password-protected `.7z` arrives here as an
/// error from the reader rather than as a per-member skip. That is the honest
/// shape for it — a whole-archive refusal rather than a partial gallery.
pub fn extract_7z(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    use sevenz_rust2::ArchiveReader;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut reader = ArchiveReader::open(archive, sevenz_rust2::Password::empty())
        .map_err(|e| e.to_string())?;
    let mut extracted = 0;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut names: Vec<(String, bool)> = Vec::new();
    reader
        .for_each_entries(|entry, _| {
            names.push((entry.name().to_string(), entry.is_directory()));
            Ok(true)
        })
        .map_err(|e| e.to_string())?;
    for (name, is_dir) in names {
        if is_dir {
            continue;
        }
        let Some(safe) = archive_member_name(&name) else {
            continue;
        };
        if !claim_member_name(&mut seen, &safe) {
            continue;
        }
        // `read_file` decodes the whole member into memory. 7z members are
        // solid by default, so a reader that streams would decode the members
        // before it anyway; the difference is only the transient buffer, and a
        // comic page is a few MB.
        let bytes = reader.read_file(&name).map_err(|e| e.to_string())?;
        std::fs::write(dest.join(&safe), &bytes).map_err(|e| e.to_string())?;
        extracted += 1;
    }
    Ok((extracted, 0))
}

/// Extract image entries from a RAR archive. Same contract as [`extract_zip`].
///
/// **Only stored (uncompressed) members come out, and that is a limit of the
/// `rar` crate rather than a choice.** Its extractor has an unpack step marked
/// `todo`, so a *compressed* member's stored bytes would be written out as if
/// they were the page — a file that is not a JPEG, which the decoders then
/// reject with an error the user has no way to interpret.
///
/// `rar` 0.4 exposes exactly one entry point, [`rar::Archive::extract_all`],
/// which writes every member of the archive into a directory and hands back the
/// parsed structure. So this extracts to a scratch directory, then applies the
/// same member-name and content rules the other two readers apply *before* the
/// bytes reach the gallery. The scratch directory is deleted either way.
///
/// A RAR4 file fails inside `extract_all` (it parses RAR5 headers), which
/// surfaces as an error rather than as an empty gallery — the honest outcome
/// for a format this crate does not read.
pub fn extract_rar(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    // A sibling of `dest`, never inside it: `extract_all` writes member names
    // verbatim, so a member called `sub/page.jpg` creates directories, and a
    // scratch dir under `dest` would be scanned by the gallery.
    let scratch = dest.with_extension("rar-scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let parsed = rar::Archive::extract_all(
        archive.to_string_lossy().as_ref(),
        scratch.to_string_lossy().as_ref(),
        "",
    );
    let archive = match parsed {
        Ok(a) => a,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(format!("Cannot read this RAR ({e})"));
        }
    };
    // The name list is authoritative over the directory walk: a member whose
    // name is not a safe flat file name must not become a gallery entry, and a
    // compressed one must not become one either.
    let mut kept: Vec<(String, PathBuf)> = Vec::new();
    for block in &archive.files {
        let name = block.name.clone();
        let Some(safe) = archive_member_name(&name) else {
            continue;
        };
        let path = scratch.join(Path::new(&name).file_name().unwrap_or_default());
        if path.is_file() {
            kept.push((safe, path));
        }
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut extracted = 0;
    let mut skipped = 0;
    for (safe, path) in kept {
        if !claim_member_name(&mut seen, &safe) {
            continue;
        }
        match verify_extracted_image(&path) {
            Ok(true) => {
                std::fs::copy(&path, dest.join(&safe)).map_err(|e| e.to_string())?;
                extracted += 1;
            }
            // **The compression case.** The bytes are not an image, so the
            // member is counted as skipped and the status line says why. Left in
            // place it would become a gallery entry that fails to decode with a
            // message about a corrupt file, which is both untrue and useless.
            Ok(false) => skipped += 1,
            Err(_) => skipped += 1,
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok((extracted, skipped))
}

/// True if these leading bytes are an image this app can decode.
///
/// **Content, never a file name**, which is the codebase's rule for choosing a
/// decoder and is what makes it worth having here too: a member can arrive under
/// a `.png` name carrying bytes that are not a picture. It exists as a public
/// function because the archive readers need it *after* extracting, and that
/// check is what stops an undecompressed member from becoming a gallery entry
/// that later fails to decode with a message about a corrupt file.
pub fn looks_like_supported_image(head: &[u8]) -> bool {
    is_jpeg_bytes(head)
        || head.starts_with(b"\x89PNG\r\n\x1a\n")
        || is_gif_bytes(head)
        // `RIFF` alone is not WebP: WAV and AVI start the same way, so the form
        // type four bytes in is the part that decides it.
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP"))
        || is_qoi_bytes(head)
        || is_tiff_bytes(head)
        || is_bmp_bytes(head)
        || is_ico_bytes(head)
}

/// True if a just-extracted file really is an image, read from its own bytes.
fn verify_extracted_image(path: &Path) -> Result<bool, std::io::Error> {
    use std::io::Read;
    let mut head = [0u8; 16];
    let mut f = std::fs::File::open(path)?;
    let n = f.read(&mut head)?;
    Ok(looks_like_supported_image(&head[..n]))
}

/// Extract every image member of any supported archive into `dest`.
///
/// **The three readers are not equally capable, and this function does not
/// pretend otherwise.** Returns `(extracted, skipped)` where `skipped` counts
/// members the reader could not produce — encrypted for zip, compressed for
/// rar — so the caller can say how many and why rather than showing a short
/// gallery with no explanation.
pub fn extract_archive(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "zip" | "cbz" => extract_zip(archive, dest),
        "7z" => extract_7z(archive, dest),
        "rar" => extract_rar(archive, dest),
        other => Err(format!("Unsupported archive: .{other}")),
    }
}

/// How many images an archive holds, without extracting it.
///
/// **A header read, not a decode.** zip and 7z read their central directory
/// and RAR walks its file headers; no member payload is touched, so this is
/// bounded by the header rather than by the archive. It is what lets the status
/// line say `vol1.cbz - 42 images` and a grid cell draw a label, and it runs on
/// the UI thread, so it must stay this cheap.
///
/// How many images an archive holds, without extracting it.
///
/// **A header read, not a decode.** zip and 7z read their central directory
/// and RAR walks its file headers; no member payload is touched, so this is
/// bounded by the header rather than by the archive. It is what lets the status
/// line say `vol1.cbz - 42 images` and a grid cell draw a label, and it runs on
/// the UI thread, so it must stay this cheap.
///
/// **It counts what extraction would produce, not how many members the archive
/// holds.** Two members can flatten to the same file name — `ch1/01.png` and
/// `ch2/01.PNG` both become `01.png`, and Windows has one file there — so a
/// plain member tally says 3 for a 2-page archive. The number on screen would
/// then disagree with the gallery beside it, and the caller has no way to tell
/// which one is lying. Hence the same [`claim_member_name`] the extractors use.
///
/// `None` for anything that cannot be read: the caller has a path, not a
/// problem to solve, and the error belongs at open time.
pub fn archive_image_count(archive: &Path) -> Option<usize> {
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "zip" | "cbz" => {
            let f = std::fs::File::open(archive).ok()?;
            let mut z = zip::ZipArchive::new(f).ok()?;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut n = 0usize;
            for i in 0..z.len() {
                if let Ok(entry) = z.by_index(i) {
                    if entry.is_dir() {
                        continue;
                    }
                    if let Some(safe) = archive_member_name(entry.name()) {
                        if claim_member_name(&mut seen, &safe) {
                            n += 1;
                        }
                    }
                }
            }
            Some(n)
        }
        "7z" => {
            use sevenz_rust2::{ArchiveReader, Password};
            let mut reader = ArchiveReader::open(archive, Password::empty()).ok()?;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut n = 0usize;
            reader
                .for_each_entries(|entry, _| {
                    if !entry.is_directory() {
                        if let Some(safe) = archive_member_name(entry.name()) {
                            if claim_member_name(&mut seen, &safe) {
                                n += 1;
                            }
                        }
                    }
                    Ok(true)
                })
                .ok()?;
            Some(n)
        }
        "rar" => {
            // The one public entry point extracts to a directory, so counting
            // without writing is not offered. This is a header read plus a
            // scratch directory, which is still bounded by the member list and
            // not by the payload.
            let scratch = std::env::temp_dir().join(format!(
                "iv-rar-count-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&scratch);
            let archive = rar::Archive::extract_all(
                archive.to_string_lossy().as_ref(),
                scratch.to_string_lossy().as_ref(),
                "",
            )
            .ok()?;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let n = archive
                .files
                .iter()
                .filter(|b| {
                    let written = scratch
                        .join(std::path::Path::new(&b.name).file_name().unwrap_or_default())
                        .is_file();
                    match (archive_member_name(&b.name), written) {
                        (Some(safe), true) => claim_member_name(&mut seen, &safe),
                        _ => false,
                    }
                })
                .count();
            let _ = std::fs::remove_dir_all(&scratch);
            Some(n)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Manga spreads: a page's shape, and the walk over turns
// ---------------------------------------------------------------------------
//
// A *turn* is what the screen shows at once: one page, or two pages facing each
// other. A page that is itself a scan of two printed pages takes a whole turn
// alone and **re-pairs everything after it**, which is the whole reason the walk
// below cannot be a pairing table computed once and stored - one wide page in
// the middle of a book moves every turn behind it.
//
// ## The invariant: the cursor is always a turn anchor
//
// An *anchor* is a page a turn opens on. Every function in this section is
// written assuming the position handed to it is an anchor, and that assumption
// is the design rather than a convenience: it is what makes "is this page
// alone?" one local test instead of a question about everything behind it.
//
// The tempting formulation is "can this page pair with the next one?", asked at
// whatever page the cursor happens to be on. It is wrong, and wrong in the one
// case a reader notices:
//
//     page 4 is portrait and page 5 is a fold-out, so 4 "cannot pair with 5",
//     so 4 is alone.  False: 4 is the second half of {3, 4}.
//
// Nothing local to a page can tell "the first half of a pair" from "alone",
// because that depends on what happened *behind* it. So the question is only
// ever asked at an anchor, where by construction nothing paired from behind,
// and `next_anchor`/`prev_anchor` are what keep the cursor at one. `lone`
// carries this in its doc comment; that comment is load-bearing.
//
// ## No page map, and why that is affordable
//
// Mihon keeps a `TreeSet` of isolated page indices, because it looks up an
// arbitrary prefix of a chapter of unknown length. Aidoku keeps a set *and* an
// `shiftIsolation(after:, by:)` to repair its indices when it physically inserts
// split pages. Kavita precomputes the whole pairing table server-side and
// invalidates it when a file changes. None of that is needed here, and each of
// those costs is a cost this design would really pay: a precomputed table has
// to be rebuilt whenever a dimension is learned, a file is re-exported, or the
// filter changes, and a stale entry is a *wrong pairing*, which is invisible
// until you are three pages into the wrong book.
//
// Instead every query is relative to a cursor that is always an anchor.
// Forward is O(1): one `turn_len`. **Backward is not, and cannot be** - it
// rebuilds the tiling forward from page 0, so it costs the distance from the
// front in in-memory reads over cached dimensions. See `prev_anchor` for why
// there is no local rule for it, which is the one place this design's
// "no page map" argument does not hold. The questions those need - step, key,
// dimensions, override - are exactly `PageCtx`.
//
// ## The irreducible ambiguity, and why it is survivable
//
// A single landscape page (a splash page, a fold-out insert) and a scan of two
// printed pages are geometrically identical, both about 1.414, and no
// aspect-ratio detector can separate them. Every reader in the field has this
// bug: Paperback #251, "the double page always start on the left panel"; Aidoku
// #939, wide images "forced into double-page layout, resulting in black screen".
//
// Two things contain it. `pairable`'s tolerance check means a landscape page
// will not pair with a portrait one, so it takes its own turn rather than being
// squashed into a half. And a turn's halves are drawn at one shared scale, so a
// partner that turns out to be wrong shows with a gap under it rather than
// distorted. The detector's only job is **whether to look for a partner**,
// never which side; a wrong guess costs a pairing, not a broken picture. A
// user override beats the detector outright, which is why `PageCtx` asks for one
// separately instead of folding it into the dimensions.

/// A screen rectangle as `[x, y, width, height]`. Plain floats rather than
/// `egui::Rect` so it can be tested: this module has no egui dependency on
/// purpose.
pub type Rect4 = [f32; 4];

/// The aspect ratio above which an untagged page is taken to be a scan of two
/// printed pages, and so takes a whole turn to itself.
///
/// 1.2 and not 1.0, which is what most readers use: a real page is about 0.707
/// and a real spread about 1.414, so a threshold of 1.0 sits on the midpoint and
/// would call a square-ish single page a spread. Above the threshold, not at or
/// below it - a page exactly at 1.2 is a page.
pub const SPREAD_ASPECT: f32 = 1.2;

/// How far two pages' aspect ratios may differ and still be one turn.
///
/// Mihon's `pairAspectTolerance`. This is the check that keeps a landscape page
/// out of a pair rather than squashing it into a half, and 0.1 is loose enough
/// that two scans of the same book - slightly cropped, slightly rotated -
/// still pair.
pub const PAIR_ASPECT_TOLERANCE: f32 = 0.1;

/// How many pages at the front of a gallery are shown one to a turn: the cover
/// alone, then pairs.
///
/// 1 because that is how a book opens, and because the most-reported pairing
/// failure in every mature reader is the opposite case - a scan whose blank
/// front-matter page was dropped, which wants 0. It is a setting because which
/// one is right depends on the book and not on the format.
pub const LEAD_LONE_DEFAULT: u32 = 1;

/// The most leading pages [`LEAD_LONE_DEFAULT`] can be raised to.
///
/// A bound rather than a free number because the setting is nudged with a key,
/// and a gallery is only ever asked about its first few pages - past that the
/// question "is this page front matter?" has stopped having an answer.
pub const LEAD_LONE_MAX: u32 = 4;

/// What a page looks like, which decides whether it takes a whole turn.
///
/// Serialised, because the user's override for one page is written to disk (see
/// [`GallerySpreads`]) into a file meant to be read and edited by a person: the
/// format is chosen so that a shape can be typed by hand. `rename_all =
/// "lowercase"` is what makes the documented spelling (`"double"`) the one that
/// *loads*; without it serde writes `"Double"` and refuses `"double"`, so a
/// hand-edited file silently becomes no overrides at all — the same trap
/// `WgpuBackend` fell into, which `AGENTS.md` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    Single,
    Double,
}

/// One page of the flat sequence a gallery walks.
///
/// A position rather than a key, because the walk is arithmetic: `step` and
/// `flat` are what it needs, and both are cheaper over an index than over a
/// `HashMap` lookup. The same pair plus a path is what every pixel cache is
/// keyed on, so nothing here has to invent an identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PagePos {
    /// Which file in the gallery.
    pub index: usize,
    /// Which page within a multi-page document, 0-based. Always 0 for an
    /// ordinary image.
    pub page: u32,
}

/// The four facts the pairing walk needs, and nothing else.
///
/// A trait rather than a parameter struct because the real implementation is a
/// `Gallery` plus a shape cache and the test implementation is a `Vec<Shape>`,
/// and neither can be built from the other. Every method is a read: the walk
/// makes no decision that cannot be recomputed from these, which is what keeps
/// a wrong pairing debuggable.
///
/// No default bodies on purpose - a default would be a guess about a gallery
/// this section cannot see, and the guess would be silently wrong for archives
/// and for a `page_counts` that has not been learned yet.
pub trait PageCtx {
    /// Step the flat page cursor by `by` pages, honouring `loop_back`. `None`
    /// past either end when looping is off. **The only primitive that moves.**
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos>;

    /// Step the flat page cursor without wrapping: `None` past either end even
    /// when `loop_back` is on.
    ///
    /// **Both this and [`Self::step`] exist because two questions were being
    /// asked with one method**, and they have different right answers:
    ///
    /// * *"Does this page have a partner within this gallery?"* - **must not see
    ///   across the wrap.** A turn is a spread on screen, and a spread that
    ///   pairs the last page of a volume with its own first is not a spread of
    ///   that book: turning forward off the last spread would show page 4
    ///   beside page 1. So with looping on the **last turn truncates to one
    ///   page**, and the tiling is exactly the tiling the same gallery would
    ///   have with looping off.
    /// * *"Where does the cursor go next?"* - **must wrap**, or `loop_back` does
    ///   nothing at all.
    ///
    /// `step` answers the second and this answers the first, so [`lone`] - which
    /// asks whether a partner exists - uses this one, and only
    /// [`next_anchor`] uses `step`.
    ///
    /// It also removes a self-comparison that had no meaning: with wrapping, a
    /// one-page gallery's "next page" was page 0 again, and asking whether it
    /// pairs with itself said yes.
    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos>;

    /// The key for a position, for the shape cache and the loader. `None` for a
    /// position that does not exist.
    fn key(&self, pos: PagePos) -> Option<ImgKey>;

    /// Cached display dimensions. `None` = not learned yet, and that is a state
    /// the pairing is built to survive rather than an error.
    fn dims(&self, pos: PagePos) -> Option<(u32, u32)>;

    /// The user's override for this page, if any. Beats the detector outright.
    fn override_shape(&self, pos: PagePos) -> Option<Shape>;

    /// The page's ordinal in the whole gallery, so it can be compared with
    /// `lead_lone`. `None` when a preceding file's page count is not yet
    /// learned - only ever asked about the first few pages of a gallery, where
    /// the cursor's having arrived there guarantees the counts are known.
    fn flat(&self, pos: PagePos) -> Option<usize>;
}

/// What a page looks like: the user's override if there is one, otherwise
/// whether it is wider than a real page is.
///
/// **The dimensions must be the ones the page _displays_ at.** A phone scan
/// stored 1600x900 with EXIF orientation 6 displays as 900x1600, and the stored
/// pair's ratio is 1.78 - above [`SPREAD_ASPECT`] - so *every* page in the
/// volume is judged a spread, every page takes a whole turn to itself, and
/// nothing ever pairs. That failure is silent and it lands on the largest
/// category of real manga library there is, which is why whatever fills
/// [`PageCtx::dims`] applies the orientation tag instead of reporting the
/// stored size.
///
/// Unknown dimensions are [`Shape::Single`], the optimistic reading: an
/// unmeasured page is not a wide one.
///
/// A zero height is never a division by zero - `h > 0` is checked before the
/// ratio is formed, not after.
pub fn shape_of(pos: PagePos, ctx: &dyn PageCtx) -> Shape {
    if let Some(s) = ctx.override_shape(pos) {
        return s;
    }
    match ctx.dims(pos) {
        Some((w, h)) if h > 0 && (w as f32 / h as f32) > SPREAD_ASPECT => Shape::Double,
        _ => Shape::Single,
    }
}

/// Whether two adjacent pages can share a turn.
///
/// Three conditions, and each one is a way the obvious alternative is wrong:
///
/// * **Neither page is a spread.** A scan of two printed pages is a whole turn
///   by itself; pairing it would put two real pages and one scan in one
///   viewport, all three squashed.
/// * **Their ratios agree to within [`PAIR_ASPECT_TOLERANCE`].** This is what
///   keeps a landscape page out of a pair - Paperback #251's "the double page
///   always start on the left panel" and Aidoku #939's "forced into double-page
///   layout, resulting in black screen" - without any content-based detection,
///   which is out of scope because it would need a decode before the pairing is
///   known.
/// * **An unknown dimension never blocks the pair.** Optimism here costs a
///   pairing that the shared scale makes visible as a gap, and strictness costs
///   a turn that never opens.
///
/// The two ways a ratio can be unavailable are therefore treated differently,
/// and the asymmetry is deliberate:
///
/// * **No dimensions at all** pairs. There is nothing to compare against, and
///   refusing would strand a turn for as long as the dimension read takes.
/// * **A height of zero** does not pair. The page was measured and still has no
///   ratio, so it is not missing data - it is a page that cannot be sized.
///
/// Both directions of that are the safe ones: a lone turn is visible on screen,
/// a wrong partner is not. `a_shape_at_the_threshold_is_not_a_spread` pins the
/// zero-height case for both `shape_of` and this.
///
/// What this deliberately does **not** answer is whether `a` is the first half
/// of its turn. That is [`lone`]'s question, and `lone` may only be asked at an
/// anchor.
///
/// **Four [`PageCtx::dims`] calls per invocation, down from six.** Two are the
/// hoisted locals below and two are inside the two [`shape_of`] calls; the old
/// spelling of the three conditions called `dims` six times for the same two
/// pages. It is not tidiness: `dims` is a `HashMap` lookup keyed on a `PathBuf`,
/// and [`prev_anchor`] calls this once per page of an O(n) walk.
///
/// **The remaining two are the obvious follow-up** - `shape_of` re-reads what
/// the locals already hold, so passing the dimensions in would make this two
/// calls. Not done here because `shape_of` is public and takes a `PagePos`, and
/// changing its signature for this is Task 2's call rather than a local tidy.
pub fn pairable(a: PagePos, b: PagePos, ctx: &dyn PageCtx) -> bool {
    let (da, db) = (ctx.dims(a), ctx.dims(b));
    if shape_of(a, ctx) == Shape::Double || shape_of(b, ctx) == Shape::Double {
        return false;
    }
    // An unknown dimension never blocks the pair, so the tolerance check is only
    // reached when both are known.
    match (da, db) {
        (Some((aw, ah)), Some((bw, bh))) if ah > 0 && bh > 0 => {
            (aw as f32 / ah as f32 - bw as f32 / bh as f32).abs() <= PAIR_ASPECT_TOLERANCE
        }
        (Some((_, 0)), _) | (_, Some((_, 0))) => false,
        _ => true,
    }
}

/// Does the turn anchored at `s` hold only that page?
///
/// **VALID ONLY AT AN ANCHOR** - a page some turn opens on - which is the
/// cursor's only ever position, by the invariant this section is built on. At a
/// non-anchor this function is *wrong*, and the example is worth more than the
/// rule:
///
/// ```text
/// page 4 is portrait, page 5 is a landscape insert
///   -> 4 "cannot pair with 5", so this answers "4 is alone"
///   -> but 4 is the second half of {3, 4}
/// ```
///
/// Asking whether `j` pairs with `j+1` cannot tell "the first half of a pair"
/// from "alone", because the answer depends on what happened *behind* `j`. At
/// an anchor nothing paired from behind, so the same local test is a complete
/// answer. **This is the one thing here that must not be "simplified"** into a
/// predicate evaluated wherever the cursor happens to be: every such
/// formulation passes a forward-only test, because forward pairing never looks
/// back, and is wrong from the first landscape page onwards - a wrong pairing
/// being invisible until you are three pages into the wrong book.
///
/// The same asymmetry is why [`prev_anchor`] cannot ask this question either,
/// and does not try: it rebuilds the tiling forward from page 0 instead of
/// testing the page before the cursor, because every local test of that page has
/// already been shown wrong. Its doc comment lists the three that were tried.
///
/// Three reasons to be alone, in the order they are asked:
///
/// * **Front matter.** The first `lead_lone` pages of the gallery, because a book
///   opens on its cover alone. `0` for a scan whose blank first page was dropped,
///   which is the most-reported pairing complaint in every mature reader (Komga
///   #123, #2132; OpenComic #456; Paperback #251). The test is [`is_lead`]'s one
///   step, not a count of the pages before it - same rule, and it is the rule's
///   cost and its behaviour under an unlearned page count that make it that way.
/// * **A spread**, which takes a whole turn and re-pairs everything after it.
///   **This arm is redundant with `pairable`'s own `shape_of(a) != Double` check,
///   and is kept for legibility.** Deleting it was tried and changes no answer on
///   any input, because the third arm's `!pairable` is true for every wide page
///   anyway. It stays because it is the only statement of the re-pairing rule
///   visible at the point where re-pairing is decided, and because someone will
///   otherwise try to "remove the duplication". The arm that is genuinely
///   load-bearing is the first one: deleting that breaks the trace,
///   `leading_pages_can_be_lone`, `backwards_agrees_with_forwards` and the
///   round-trip property at once.
/// * **Nothing to pair with**: the next page is a spread, it is a different
///   shape of page, or there is no next page at all.
///
/// The partner test asks [`PageCtx::step_no_wrap`], not `step`, and that one
/// choice is what stops a turn straddling the wrap: with `loop_back` on, the
/// last anchor has no partner, so the last turn of a volume is one page and the
/// next press moves to page 1. A reader expects the last spread to be the last
/// page alone, not that page beside the first one.
pub fn lone(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> bool {
    is_lead(anchor, lead_lone, ctx)
        || shape_of(anchor, ctx) == Shape::Double
        || match ctx.step_no_wrap(anchor, 1) {
            Some(next) => !pairable(anchor, next, ctx),
            None => true,
        }
}

/// Whether `pos` is one of the gallery's first `n` pages - the front matter a book
/// opens alone.
///
/// **One step, not a count.** "`pos` has fewer than `n` pages before it" and "stepping
/// back `n` pages runs off the front" are the same statement, so this asks
/// [`PageCtx::step_no_wrap`] rather than [`PageCtx::flat`]. Both reasons for that were
/// found by measurement and by reading what `flat` does with an unknown count:
///
/// * **O(1), where `flat` is O(files).** [`Gallery::flat`] sums the page counts of
///   every file before `pos`, with a `PathBuf`-keyed lookup per element, and `lone` is
///   called once per anchor of every walk - so one backward step over a folder of 2000
///   loose images spent **117-385 ms** in it, measured in release on the development
///   box. `n` is at most [`LEAD_LONE_MAX`] (4), so this is a single `step_no_wrap` call
///   and not a loop of them.
/// * **It cannot refuse to answer.** `flat` is `None` whenever a *preceding* file's
///   page count has not been learned, and the spelling this replaces guarded that with
///   `is_some_and`, which then answered "not front matter". So a front page that
///   happened to sit behind an unlearned file lost its front-matter arm and paired with
///   the page after it - a wrong pairing, silent, and the kind this whole section is
///   built to avoid. `step_no_wrap` asks the position arithmetic, which is always
///   answerable, so the page is judged by where it is rather than by what is known
///   about the files ahead of it.
///
/// **Two boundaries, and neither earns a guard.** `n == 0` asks for
/// `step_no_wrap(pos, 0)`, which is `Some(pos)` for any valid position, so the answer
/// is "no" - right, since nothing can be among the first zero pages. And a position
/// that does not exist also answers `None` and would be called front matter: `lone` is
/// only ever asked at an anchor, which is a position a walk produced and so exists, so
/// that cannot arise - and a guard there would be a branch that cannot change an output.
fn is_lead(pos: PagePos, n: u32, ctx: &dyn PageCtx) -> bool {
    ctx.step_no_wrap(pos, -(n as isize)).is_none()
}

/// How many pages the turn anchored at `s` holds: one, or two.
///
/// There is no `Turn` type on purpose. A turn is `[anchor] + maybe [anchor+1]`,
/// which every caller can already express, and a struct over it would only add
/// a conversion to get wrong.
pub fn turn_len(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> usize {
    if lone(anchor, lead_lone, ctx) {
        1
    } else {
        2
    }
}

/// The other page of the turn anchored at `anchor`, or `None` when the turn holds
/// one page. `anchor` itself is never returned: it is the cursor's job.
///
/// **This is the whole of the turn's second half in one expression**, and it lives
/// here rather than in the app because every piece of it is a rule and every rule
/// in this section is testable from a test. The app's version is the mode gate
/// plus one call.
///
/// Three decisions, each of which has a wrong answer that looks right:
///
/// * **`turn_len` decides whether there is one at all**, and it is the same
///   question [`lone`] answers, so there is no second opinion to reconcile. A
///   lone turn - front matter, a spread, or a last page with nothing after it -
///   has no other half and must not borrow one.
/// * **`step_no_wrap(anchor, 1)`, not [`next_anchor`].** A turn is
///   `[anchor] + maybe [anchor + 1]`, so its second page is the anchor's
///   *successor*; `next_anchor` is the turn **after** this one, a whole
///   `turn_len` away, which is two pages forward whenever the answer is `Some`.
///   It is also documented as the only caller of the *wrapping* step, and a turn
///   must not straddle the wrap - so using it here would both paint the wrong
///   page and contradict [`lone`]'s own reason for asking
///   `step_no_wrap`. (The wrapping step taken *on its own* is a different matter,
///   and equivalent: see the last paragraph.)
/// * **`anchor` can never come back**, and that is a property of the step rather
///   than a check: `step_no_wrap` does not wrap, so one page forward is either
///   the next page of this document (`page + 1`) or the first page of the next
///   file (`index + 1`), and neither can be the position already named. A
///   guard would be a branch that cannot change an output.
///
/// **`None` for a one-page gallery is the same property, not a special case.**
/// `step_no_wrap` runs off the end and answers `None`, so a lone page cannot be
/// reported as its own partner - which is the artefact that made the two
/// questions ("does this page have a partner" and "where does the cursor go")
/// need separate steps in the first place.
///
/// **`step` would give the same answer here, and that is not an unpinned
/// constraint - it is an equivalence.** There is no test that can tell this
/// function's step from the wrapping one, and looking for one is a waste of a
/// review round: the two differ only at the two ends of a gallery, and this
/// function never reaches an end.
///
/// * It steps at all only when `turn_len == 2`, that is when [`lone`] is false.
/// * [`lone`]'s front-matter arm and its `Double` arm each make it true on their
///   own, so when it is false it must be the third arm that failed - which means
///   `step_no_wrap(anchor, 1)` did return a position, and [`pairable`] accepted
///   it.
/// * And `step_no_wrap` answers `None` only at the end of the gallery, where
///   `lone` is true and `turn_len` is 1, so the function has already returned.
///
/// So an interior anchor is the only one that gets here, and at an interior anchor
/// `step` and `step_no_wrap` are the same walk. `step_no_wrap` is the spelling to
/// keep anyway, for two reasons that are about the code rather than the answer: it
/// says out loud that a turn must not straddle the wrap, which is the property
/// [`lone`] is built on; and it keeps this function asking `lone`'s own question,
/// so the two cannot drift apart.
pub fn turn_partner(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<ImgKey> {
    if turn_len(anchor, lead_lone, ctx) == 1 {
        return None;
    }
    ctx.step_no_wrap(anchor, 1).and_then(|pos| ctx.key(pos))
}

/// The anchor after `a`, or `None` past the end with looping off.
///
/// Forward is one addition **from an anchor**, and that is the reason this design
/// needs no pairing table: the next anchor is this turn's length away, and a turn's
/// length is a local test.
///
/// **What it costs the app, which is not this.** `MyApp::turn_forward` calls
/// [`next_turn`], not this, and that snaps to the anchor first — so the navigation a
/// press performs is one `anchor_of` walk plus this one addition. See `anchor_of`'s
/// doc for the corrected cost and `TODO.md` for the deferred table.
///
/// **The only caller of the wrapping [`PageCtx::step`].** The wrap belongs here
/// and nowhere else: [`lone`] asks for a partner with `step_no_wrap`, so a turn
/// never straddles the seam, and this is where a looping gallery's cursor
/// actually gets back to page 0. That split is what makes a looping gallery's
/// tiling identical to the same gallery's tiling with looping off, so the
/// partition property holds for both.
pub fn next_anchor(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    ctx.step(a, turn_len(a, lead_lone, ctx) as isize)
}

/// How many anchors a tiling walk will consider before giving up.
///
/// **A bound on a walk that terminates on its own, not the walk's cost.** Every
/// walk bounded by this exits as soon as [`PageCtx::step_no_wrap`] returns `None`
/// at the front or the back of the gallery, whatever `loop_back` says, so this
/// only ever bites on a gallery larger than it names. Its value is deliberately far
/// above any plausible gallery rather than tuned: being generous costs nothing,
/// because the loop cannot reach the number without the answer already being known.
pub const PREV_SCAN_LIMIT: usize = 1 << 20;

/// The anchor of the turn before `a`'s turn, or `None` when there is none.
///
/// **`None` has two causes, and they are different questions.** It answers `None` at
/// the first turn - there is no turn before it - **and** for a position that is not
/// an anchor at all: the walk compares successors, so a second half is never the `a`
/// it is looking for, the walk runs off the end, and the answer is the same `None`.
/// So `prev_anchor(a).is_none()` does **not** mean "`a` is the first anchor", and a
/// caller that needs that has to ask [`anchor_of`] first and compare:
///
/// ```text
/// 8 single-page files, lead_lone = 1  ->  turns {0} {1,2} {3,4} {5,6} {7}
///   a = (2, 0): the second half of {1,2}, and there are two turns behind it.
///     prev_anchor walks 0 -> 1 -> 3 -> 5 -> 7 -> off the end, and answers None.
///   a = (0, 0): the first anchor, and there is nothing behind it.
///     prev_anchor answers None for the same reason and the same value.
/// ```
///
/// **A cursor can be on a second half without this section's help**, which is why
/// that distinction is load-bearing rather than theoretical: three ways into the app
/// assign the cursor by file index rather than by turn - a grid pick, a start-up or
/// resume, and a `Del` that keeps the freed index - and at the default
/// `lead_lone` of 1 an ordinary folder of single pages has a lone cover, so "file 3"
/// is page 2 of the turn `{1,2}`. Reading `None` as "at the front" is how a backward
/// step from such a position jumps to the last spread of the volume, which is what
/// [`prev_turn`] exists to prevent.
///
/// **Backward is O(distance from the front), and it has to be.** Forward is one
/// addition, because an anchor knows its own turn length without asking anything.
/// Going backwards there is no such thing to ask, and the reason is the same
/// asymmetry this whole section is built on:
///
/// ```text
/// pages [Single, Single, Single, Double], lead_lone = 0
///   forward:  {0,1} paired | {2} alone (3 is wide) | {3} wide
///   so page 2 is an ANCHOR - it went alone because nothing was ahead of it.
///   but pairable(1, 2) is true (both portrait), so "could 2 pair with the page
///   behind it?" cannot tell that apart from "2 is the second half of {1,2}".
/// ```
///
/// So this **rebuilds the tiling forward from page 0** and returns the anchor
/// that led to `a`. Every step is in-memory arithmetic over dimensions that are
/// already cached, and the walk is only ever as long as the distance back to the
/// first page - which for a manga volume is a few hundred cheap reads against a
/// decode measured in hundreds of milliseconds.
///
/// **Do not "optimise" this back into a two-candidate local test.** There is no
/// such rule. Three were tried and all three are wrong, each on
/// `[Single, Single, Single, Double]`:
///
/// * `lone(b)` - wrong at `lead_lone = 1`, returns 2 where the answer is 1.
/// * `shape(b) == Double || !pairable(b-1, b) || front(b)` - wrong at
///   `lead_lone = 0`, returns 1 where the answer is 2.
/// * "is `b` an anchor, i.e. does its own `turn_len` land on `a`?" - wrong at
///   `lead_lone = 1`, returns 2 where the answer is 1.
///
/// Anchor-ness of `b` depends on `b-1`'s turn length, which depends on `b-2`'s,
/// so it is only settled by walking back to the front. The forward direction
/// escapes this because it always starts from a known anchor; the backward
/// direction has no such starting point, which is why it walks instead.
///
/// **Returns `None` at the first anchor even when `loop_back` is on.** There is
/// no turn before the first one; whether the *app* should jump back to the last
/// turn of a looping gallery is the navigation layer's decision, and
/// [`prev_turn`] makes it. This function only ever reports the tiling - and, as
/// its summary says, it reports `None` for a non-anchor as well, so it is
/// [`anchor_of`] that decides whether a `None` means the front of the gallery.
pub fn prev_anchor(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    let mut anchor = PagePos { index: 0, page: 0 };
    for _ in 0..PREV_SCAN_LIMIT {
        // `step_no_wrap`, so the walk ends at the front of the gallery rather
        // than cycling: there is no previous turn before the first one, and
        // whether the *app* wraps to the last turn on a looping gallery is the
        // navigation layer's decision, not this function's.
        let next = match ctx.step_no_wrap(anchor, turn_len(anchor, lead_lone, ctx) as isize) {
            Some(n) => n,
            None => return None,
        };
        // Compared against the SUCCESSOR, and that is the whole contract: the
        // answer to "the anchor before `a`" is the anchor whose turn *ends* by
        // handing over to `a`.
        //
        // **Two edits that look similar are not the same, and only one of them
        // changes behaviour.** Adding `if anchor == a { return None }` at the top
        // of this loop, and leaving the check below, is *equivalent*: this loop
        // only ever tests `anchor` values the walk has already passed, so the top
        // check can match only the first anchor - whose answer is `None` anyway,
        // which is what the `None` arm above already returned. That edit was run
        // and nothing failed. **Replacing** the check below with a top-of-loop
        // `if anchor == a { return Some(anchor) }` is a different edit and is
        // *not* equivalent: it returns `Some(first_anchor)` where the answer is
        // `None`, stranding the backward walk on a one-turn gallery.
        //
        // Kept as written because it is the one that says what it means, and
        // because the guard is on the *property* rather than the spelling:
        // `every_anchor_is_reached_from_the_anchor_before_it` asserts that
        // whatever this returns is an anchor whose successor is `a`.
        if next == a {
            return Some(anchor);
        }
        anchor = next;
    }
    None
}

/// The turn anchor at or before `pos`: the anchor of the turn `pos` is on.
///
/// **Total, because the turns partition the gallery.** Every page is in exactly one
/// turn and the walk from the first anchor visits every anchor, so the answer always
/// exists for a position that does - which is what makes it usable as the thing to
/// call *before* asking anything else about a cursor.
///
/// **It walks forward, for the same reason [`prev_anchor`] walks forward**, and the
/// two together are the whole of anchor arithmetic: this one answers "which turn is
/// this page on", and `prev_anchor` answers "which turn is before that one". Asking
/// them of a position that is not an anchor is exactly what [`prev_anchor`] cannot
/// do, and the failure it causes is worse than a wrong page number: see its summary.
///
/// **Total does not mean correct for a position that does not exist.** A `pos` past
/// the end of the gallery is answered with the last turn's anchor, because that is
/// the walk's final answer and inventing a second failure mode for it would only give
/// callers something else to get wrong. Every caller in this crate passes a position
/// the walk produced or a cursor, and [`Gallery::goto_page`] refuses an index past the
/// end of the list, so nothing can observe the difference.
///
/// **O(distance from the *front*), not from `pos` — and the distinction is load
/// bearing, so it is worth stating plainly.** The walk starts at `(0, 0)`, so this
/// costs one iteration per *turn before the answer*, not one per page between the
/// front and `pos`. It therefore reads the same cached dimensions the rest of the
/// section does and nothing else: no I/O, no decode, and **it stops at the turn `pos`
/// is on rather than reading to the end of the volume**.
///
/// **An earlier version of this doc said "O(distance from `pos`), like every other
/// walk here", and it was wrong.** So was "forward is O(1)", which
/// [`next_anchor`]'s own doc repeats and which is the claim that lets a reader
/// believe no pairing table is needed: it is true of `next_anchor` *from an anchor*,
/// and the navigation the app actually performs is [`next_turn`], which **calls this
/// function first**. At the far end of a 2000-file folder a forward step is therefore
/// ~1000 iterations of `turn_len`, not one addition — still microseconds of in-memory
/// arithmetic against a decode measured in hundreds of milliseconds, and still not a
/// hang, but it is not the bound it was documented as. The deferred fix (a per-gallery
/// anchor list) and the corrected cost are recorded in `TODO.md`; do not "optimise"
/// this into a local test, for [`prev_anchor`]'s reasons.
pub fn anchor_of(pos: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> PagePos {
    let mut anchor = PagePos { index: 0, page: 0 };
    for _ in 0..PREV_SCAN_LIMIT {
        if anchor == pos {
            return anchor;
        }
        // The turn holds two pages when `lone` says so, and its second one is
        // `step_no_wrap(anchor, 1)` - the same spelling [`turn_partner`] uses, and
        // for the same reason: the wrapping step would answer across the seam.
        if turn_len(anchor, lead_lone, ctx) == 2 && ctx.step_no_wrap(anchor, 1) == Some(pos) {
            return anchor;
        }
        // `turn_len` is asked twice for the same anchor, which is one wasted local
        // test per iteration - not hoisted because hoisting it would only move the
        // cost inside the loop, and the loop's cost is the walk, not this call.
        let Some(next) = ctx.step_no_wrap(anchor, turn_len(anchor, lead_lone, ctx) as isize) else {
            return anchor;
        };
        anchor = next;
    }
    anchor
}

/// The anchor of the gallery's final turn.
///
/// **Forward, not backward, and for [`prev_anchor`]'s reason read from the other
/// end.** Which turn is last is a fact about the whole tiling, and nothing asked
/// *at* the gallery's last page can answer it. `lone` there always says "alone" -
/// it looks for a successor and there is none - so it says the same thing whether
/// the last page opens the final turn or closes it. `prev_anchor` there answers
/// `None` when the last page is a second half and the turn *before* the last one
/// when it is not, and neither is the answer. So the walk starts at the front and
/// reads forwards, the same way [`prev_anchor`] does.
///
/// **It does not see the wrap, and that is what makes a looping gallery's tiling
/// the tiling it would have with looping off** - the property the round-trip test
/// in `tests/spreads.rs` asserts. It walks with [`PageCtx::step_no_wrap`] for the
/// same reason [`prev_anchor`] does: a turn must not name a page twice, and across
/// the seam it would. It is also what makes the walk **end by itself**: a wrapping
/// step on a looping gallery never answers `None`, so the loop would run to
/// [`PREV_SCAN_LIMIT`] instead - measured at ~25 s before the test fails, which is
/// the cost of getting this line wrong rather than of getting it right.
///
/// `None` for a gallery with nothing in it, checked rather than assumed: a gallery
/// with no files has no last turn, and `(0, 0)` is not a page of it.
pub fn last_anchor(lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    // `by` of 0 runs no iterations, so this is the "is there a first page at all"
    // question answered by the same bounds check every step does. Not a formality: a
    // gallery with no files has no page 0, and a walk that started at `(0, 0)` without
    // asking would answer `Some` for it.
    let mut anchor = ctx.step_no_wrap(PagePos { index: 0, page: 0 }, 0)?;
    for _ in 0..PREV_SCAN_LIMIT {
        let Some(next) = ctx.step_no_wrap(anchor, turn_len(anchor, lead_lone, ctx) as isize) else {
            return Some(anchor);
        };
        anchor = next;
    }
    None
}

/// Where one forward *step* lands: the anchor of the next turn.
///
/// **[`next_anchor`] snapped to the anchor first**, and that is the whole difference
/// between the two. `next_anchor` is a question about the tiling asked of a position
/// the caller believes to be an anchor; this is the navigation layer's step, asked of
/// a cursor that may not be one - and at a second half, `turn_len` is the second half's
/// own answer, so `next_anchor` returns a position one page past the turn on screen and
/// every step after that is off by the same page.
///
/// The snap costs one walk from the front ([`anchor_of`]); `next_anchor` itself is one
/// `turn_len`, so a forward step is that walk and nothing else. It is the same walk
/// [`prev_anchor`] makes for a backward step, and the same price in both directions —
/// paid to be callable with any cursor, rather than the alternative: a navigation layer
/// that silently mis-navigates on every second half, which at the default `lead_lone`
/// of 1 is every other page of an ordinary folder.
pub fn next_turn(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    next_anchor(anchor_of(a, lead_lone, ctx), lead_lone, ctx)
}

/// Where one backward *step* lands: the anchor of the turn before the one `a` is on,
/// or - when that is the gallery's first turn and this context's step would wrap -
/// the gallery's last turn.
///
/// **`prev_anchor` is asked first, and its `Some` is a proof rather than a lucky
/// answer.** The walk compares successors, so it can only return `Some` for a position
/// that *is* an anchor - a second half is never the successor it is looking for. So
/// when it answers, there is nothing to disambiguate and no second walk to pay for,
/// which is the case every step after the first is in (the walks always land on an
/// anchor, and `MyApp::goto` snaps whatever did not).
///
/// **`None` is the ambiguous one, and that is the bug this function was corrected
/// for.** `None` means "the first turn" *or* "`a` is not an anchor at all", and three
/// ways into the app put the cursor in the second case without asking: a grid pick
/// leaves `page` alone, a start-up or resume lands on page 0, and a `Del` keeps the
/// freed index. At the default `lead_lone` of 1 an ordinary folder of single pages
/// tiles `{0} {1,2} {3,4} {5,6} {7}`, so "the third file" is page 2 - the second half
/// of `{1,2}` - and reading its `None` as "at the front" sent a backward step to the
/// last spread of the volume, with `loop_back` on **and** off. [`anchor_of`] is what
/// separates the two, and it is only asked when there is a `None` to interpret.
///
/// **A second half of the *first* turn still reaches the wrap arm**, because its own
/// anchor is the first anchor and there is nothing before that either. Written as a
/// fall-through rather than an early return, and the case is real rather than
/// theoretical: on a two-file gallery with `loop_back` on, page 1 is the first turn's
/// second half, and the answer is the only turn in the volume.
///
/// **The wrap is decided by asking [`PageCtx::step`], not by a `looping` argument**,
/// so `loop_back` stays the single setting that decides it and this cannot disagree
/// with [`Gallery::prev`] about whether the cursor wraps at all. **The question is
/// asked of the anchor, not of `a`**, because `a` may be a second half: one page back
/// from page 1 of `{0,1}` is page 0 of the same gallery, not past its front, so asking
/// `a` would wrap (or refuse) where the answer is "nowhere". A backward step at the
/// front with looping off still stops, which is the whole of what `loop_back` has ever
/// meant.
pub fn prev_turn(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos> {
    if let Some(prev) = prev_anchor(a, lead_lone, ctx) {
        return Some(prev);
    }
    let at = anchor_of(a, lead_lone, ctx);
    if at != a {
        // A second half: the answer is the turn before the one *it* is on, which is
        // a second walk and the price of accepting a cursor that is not on an anchor.
        if let Some(prev) = prev_anchor(at, lead_lone, ctx) {
            return Some(prev);
        }
        // `at` is the first anchor and `a` its turn's second half: still the front.
    }
    // If a one-page step backwards from the first turn would wrap, the cursor's next
    // position backwards is the last turn; if it would not, nothing moves.
    ctx.step(at, -1)?;
    last_anchor(lead_lone, ctx)
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
    /// Which page of the current file, 0-based. Non-zero only for pages of a
    /// multi-page TIFF.
    ///
    /// **A cursor, not a list.** The alternative — splicing an entry per page into
    /// `files` — was rejected because `files` is the *unfiltered* shape that the
    /// filter, the resume point, the rescan and `all_files` are all keyed on, and
    /// growing it means growing the index arithmetic in every one of them. A
    /// cursor keeps `files` exactly as it was, so all of that is untouched and
    /// only the arithmetic in `next`/`prev` learns about pages.
    page: u32,
    /// How many pages each visited file has. Only ever holds files that have been
    /// opened, and pruned whenever `files` is rebuilt, so it cannot grow without
    /// bound as you browse.
    page_counts: std::collections::HashMap<PathBuf, u32>,
    /// Display dimensions of each page whose shape has been asked for, keyed by
    /// [`ImgKey`].
    ///
    /// **Per page, not per file**, and the reason is a document with a fold-out. A
    /// manga volume scanned as one multi-page TIFF can hold a landscape double-page
    /// spread among portrait singles: page 3 is a spread, page 4 is a page, and one
    /// entry for the file would judge both by page 0 and give every one of them the
    /// wrong turn. `ImgKey { path, page }` is the identity every pixel cache here
    /// already uses, and `PageCtx::dims` already has one in hand via `key(pos)`, so
    /// this is the keying that needed no new concept.
    ///
    /// What was traded for it: a 200-page document is measured as its pages are
    /// reached rather than in one read, which is the same bargain `plan_jobs` makes
    /// about prefetching only page 0 of a neighbour — you arrive at a later page by
    /// pressing right, and `learn_shapes` runs on that arrival.
    ///
    /// Cleared with `page_counts` whenever `files` is rebuilt, for the same reason a
    /// count must not outlive the list it describes: these are keyed on paths and
    /// would otherwise accumulate every page the session ever opened.
    shapes: std::collections::HashMap<ImgKey, (u32, u32)>,
}

/// A filter needle interpreted as a rating query rather than a name substring.
///
/// Checked before the substring fallback, in this order: `stars:N`, `stars:N+`,
/// `flagged` / `unflagged`, `tag:NAME` (case-insensitive substring of the tag
/// list). Both `stars:` forms mean *at least* N — the pinned test has `stars:4`
/// matching a 5-star file, so `stars:N` is not an equality check and the `+`
/// is accepted but redundant. The needle handed here is already trimmed and
/// lowercased, so `tag:` matching lowercases only the stored tags.
enum RatingTerm {
    StarsAtLeast(u8),
    Flagged,
    Unflagged,
    Tag(String),
}

/// Parse an already-trimmed-and-lowercased needle as a rating query.
///
/// `flagged` / `unflagged` match only the whole needle: `flagged pics` stays a
/// name substring, since a word that merely leads a longer needle is not the
/// query. `stars:` and `tag:` match on prefix; an empty `tag:` (`tag:` alone)
/// is not a query and falls back to substring, while an unparseable `stars:`
/// value is not a query either and likewise falls back to substring.
fn parse_rating_term(needle: &str) -> Option<RatingTerm> {
    if let Some(rest) = needle.strip_prefix("stars:") {
        if let Some(min) = rest.strip_suffix('+') {
            return min.parse::<u8>().ok().map(RatingTerm::StarsAtLeast);
        }
        return rest.parse::<u8>().ok().map(RatingTerm::StarsAtLeast);
    }
    if needle == "flagged" {
        return Some(RatingTerm::Flagged);
    }
    if needle == "unflagged" {
        return Some(RatingTerm::Unflagged);
    }
    if let Some(name) = needle.strip_prefix("tag:") {
        if !name.is_empty() {
            return Some(RatingTerm::Tag(name.to_string()));
        }
    }
    None
}

fn rating_term_matches(term: &RatingTerm, ratings: &Ratings) -> bool {
    match term {
        RatingTerm::StarsAtLeast(n) => ratings.rating.stars >= *n,
        RatingTerm::Flagged => ratings.rating.flag,
        RatingTerm::Unflagged => !ratings.rating.flag,
        RatingTerm::Tag(name) => ratings
            .tags
            .iter()
            .any(|t| t.to_lowercase().contains(name)),
    }
}

impl Gallery {
    pub fn new(folder: &Path) -> Self {
        Self {
            files: scan_folder(folder),
            index: 0,
            loop_back: true,
            page: 0,
            page_counts: std::collections::HashMap::new(),
            shapes: std::collections::HashMap::new(),
        }
    }

    /// An empty gallery.
    ///
    /// For the path where the caller is about to install its own list - start-up
    /// with `load_all_on_startup` calls [`scan_folder_sorted`] immediately after.
    /// [`Self::new`] would walk the directory and have the result thrown away,
    /// which on a 1001-image folder is ~50 ms before the window is even up.
    pub fn empty() -> Self {
        Self {
            files: Vec::new(),
            index: 0,
            loop_back: true,
            page: 0,
            page_counts: std::collections::HashMap::new(),
            shapes: std::collections::HashMap::new(),
        }
    }

    /// Create a gallery containing a single file.
    pub fn from_file(path: PathBuf) -> Self {
        Self {
            files: vec![path],
            index: 0,
            loop_back: true,
            page: 0,
            page_counts: std::collections::HashMap::new(),
            shapes: std::collections::HashMap::new(),
        }
    }

    /// The current page's position in the flat page sequence, which is what the
    /// pairing walk is expressed in terms of.
    pub fn page_pos(&self) -> PagePos {
        PagePos {
            index: self.index,
            page: self.page,
        }
    }

    pub fn current(&self) -> Option<&Path> {
        self.files.get(self.index).map(|p| p.as_path())
    }

    /// The page of the current file to decode. 0 for everything but pages of a
    /// multi-page TIFF past the first.
    pub fn current_page(&self) -> u32 {
        self.page
    }

    /// Set page counts positionally, for `files` as it currently stands.
    ///
    /// **For tests.** The app learns counts one file at a time with
    /// [`Self::learn_pages`] as you arrive at them, which is the right shape
    /// because it costs four bytes for a non-TIFF. A test about *navigation*
    /// should not also depend on the IFD walker being correct — when that breaks,
    /// every navigation test fails for the wrong reason and the actual fault is
    /// buried. Counting has its own test.
    pub fn set_page_counts<I: IntoIterator<Item = u32>>(&mut self, counts: I) {
        self.page_counts.clear();
        for (path, n) in self.files.iter().zip(counts) {
            self.page_counts.insert(path.clone(), n.max(1));
        }
        self.page = 0;
    }

    /// How many files have a remembered page count. So a test can assert the map
    /// was actually emptied rather than merely that nothing broke.
    pub fn known_page_counts(&self) -> usize {
        self.page_counts.len()
    }

    /// How many **pages** have remembered dimensions. So a test can assert a
    /// `forget_shapes` really dropped *every* page of a document rather than one —
    /// the count is the only place that difference is visible, since a per-page map
    /// and a per-file one answer the same single-page query identically.
    pub fn known_shapes(&self) -> usize {
        self.shapes.len()
    }

    /// The position `key` names, as the inverse of [`PageCtx::key`].
    ///
    /// **Needed because "what is on screen" is a key and "which turn is it on" is a
    /// position**, and the pairing is asked entirely in positions. A turn's second
    /// half is derived from an anchor, so deriving it for the picture the app is
    /// painting means turning that key back into a position on every frame.
    ///
    /// **A linear scan, and that is the trade.** `set_filtered` already searches
    /// `files` this way for the same reason — a key does not know its index — so this
    /// is not a new shape, only a new caller. It is a `PathBuf` comparison per file,
    /// pointer-equal for the common case where the app asks about the cursor's own
    /// file; [`Self::page_pos`] answers that without the scan at all, which is why the
    /// app checks that first.
    ///
    /// **Only the path is matched, and the page is trusted**, for [`Self::goto_page`]'s
    /// reason: every caller in this crate hands over a key that some `key(pos)` built,
    /// and inventing a second page-range rule here is a place for the two to disagree.
    /// A key naming a page past the end of its file therefore answers a position
    /// rather than `None`.
    ///
    /// `None` when the path is not in `files` at all — a file removed while it was
    /// still on screen, or one from a gallery that has since been rebuilt.
    pub fn pos_of(&self, key: &ImgKey) -> Option<PagePos> {
        let index = self.files.iter().position(|f| f == &key.path)?;
        Some(PagePos {
            index,
            page: key.page,
        })
    }

    /// Is the cursor on the first position of this listing?
    ///
    /// **About the whole listing, page included** — an archive's listing is its
    /// members, so "the first page of the archive" is `page == 0 && index == 0`
    /// and not merely the first file's first page. A reader who has to press
    /// `←` twice to get out of a multi-page document has been left in it.
    pub fn at_front(&self) -> bool {
        self.page == 0 && self.index == 0
    }

    /// Is the cursor on the **last** position of this listing?
    ///
    /// The mirror of [`Self::at_front`], and the reason an archive can be left
    /// forwards as well as backwards: without it, `→` at an archive's last
    /// member is `pos_next`'s ordinary end-of-list wrap, which inside an archive
    /// wraps to the archive's *own* first member. The reader is then cycling
    /// through one volume's pages and can never reach the next archive in the
    /// folder — which reads as "only the first zip loads".
    ///
    /// The page half is what makes it correct for a multi-page TIFF at either
    /// end: `index + 1 == len` is the last *file*, not the last *position*.
    pub fn at_back(&self) -> bool {
        !self.files.is_empty()
            && self.index + 1 == self.files.len()
            && self.page + 1 >= self.pages_in_current()
    }

    /// Whether advancing past either end wraps around. Separate from the field so
    /// a test can turn it off without reaching into the struct.
    pub fn set_loop_back(&mut self, on: bool) {
        self.loop_back = on;
    }

    /// How many pages the current file has, as far as this gallery knows. 1 until
    /// [`Self::learn_pages`] has been asked about the file.
    pub fn pages_in_current(&self) -> u32 {
        self.current()
            .and_then(|p| self.page_counts.get(p))
            .copied()
            .unwrap_or(1)
            .max(1)
    }

    /// Find out how many pages `path` has and remember it.
    ///
    /// Called when a file becomes current, not during a scan — see
    /// [`tiff_page_count`] for why the cost has to stay off the folder walk.
    ///
    /// **Answers from the cache when it can.** This runs on *every* navigation, so
    /// the first version — which always re-read, because "asking is cheap" was
    /// true of the four bytes and not of the `File::open` around them — put a file
    /// open on the critical path of every single keypress. Browsing back and forth
    /// over three photos is three opens, three closes and three reads to re-learn
    /// something already known.
    pub fn learn_pages(&mut self, path: &Path) -> u32 {
        if let Some(n) = self.page_counts.get(path) {
            return *n;
        }
        let n = tiff_page_count(path);
        self.page_counts.insert(path.to_path_buf(), n);
        if self.current() == Some(path) {
            self.page = self.page.min(n.saturating_sub(1));
        }
        n
    }

    /// Forget every page count. Called whenever `files` is rebuilt, so the map
    /// cannot outlive the list it describes — a stale count would put the cursor
    /// on a page that no longer exists.
    fn forget_pages(&mut self) {
        self.page_counts.clear();
        // The dimensions go with the counts, and for the same reason plus one
        // more: they are keyed on paths, so keeping them would grow with every
        // file the session ever opened rather than with `files`.
        self.shapes.clear();
        self.page = 0;
    }

    /// Find out how large `pos` displays at and remember it for the shape test — **and
    /// the next page's, because the pairing reads one page ahead.**
    ///
    /// A position rather than a `(path, page)` pair because the next-page step needs
    /// the file's *index*: finding it from a path would be a scan of `files` on the
    /// critical path of every keypress, and [`Self::page_pos`] already has it.
    ///
    /// **The page asked about, not page 0.** `pos.page` is 0 for every ordinary
    /// image, so this is unchanged for a photo folder; for a multi-page document it is
    /// the difference between knowing that page 3 is a landscape spread and judging it
    /// by whatever page 0 happened to be. The caller is `goto`, so this is the "learn
    /// on arrival" rule the page count has always followed, applied to the page that
    /// actually arrived.
    ///
    /// ## Why the next page too, and why it is not optional
    ///
    /// [`lone`] decides a turn by asking about `anchor + 1`, and an unknown `+1` reads
    /// as [`Shape::Single`] — the optimistic default. So a turn is only correct if the
    /// *next* page's dimensions are right, and learning only the page under the cursor
    /// gets it wrong in the worst possible way: **arriving at page 5 changes page 5's
    /// shape, which changes `lone`, which changes the turn the reader is already
    /// looking at.** A spread that re-flows mid-read is worse than one that was wrong
    /// from the start, because the reader watched it change.
    ///
    /// Three things bound the cost, so this is not a general "read ahead" policy:
    ///
    /// * **Only for a file known to have more than one page.** `pages_in` is 1 for
    ///   every ordinary image and for every file whose count has not been learned, and
    ///   an ordinary single-page file — the common case, and the whole of a photo
    ///   folder — therefore pays exactly what it paid before.
    /// * **One page, and inside the same document.** `step_no_wrap` rather than
    ///   `step`, so the last page does not learn the *first page of the last file*;
    ///   and a hop that crosses a file boundary is dropped, because the question is
    ///   the next page of *this* document. Nothing is lost by that: the next file's
    ///   first page is reported by its own preview landing (`note_shapes`) or measured
    ///   when the cursor gets there.
    /// * **Cache-first, per page**, so an already-known next page costs one map
    ///   lookup.
    ///
    /// **Which of those bounds is load-bearing, measured rather than assumed:** the
    /// same-document one. Dropping the page-count bound is a **no-op mutation** — a
    /// one-page file's next page is necessarily in a *different* file, so the
    /// same-document bound catches it too, and all 28 tests in `spreads.rs` stay green
    /// with the page-count check deleted (run twice, including after the bounds test
    /// was reordered). The page-count bound is kept because it states the read-ahead's
    /// scope where it is decided and skips one in-memory hop, **not** because anything
    /// depends on it — so do not read it as protection that is not there. The
    /// same-document bound, by contrast, is caught by
    /// `the_next_page_learn_stops_at_the_document`.
    ///
    /// **The EXIF orientation tag is applied here, and skipping it is the one
    /// mistake in this feature that is silent, total, and lands on real
    /// libraries.** A phone scan is stored landscape and tagged to display
    /// portrait: stored 1600x900 with orientation 6, the *stored* pair's ratio is
    /// 1.78, above [`SPREAD_ASPECT`], so a cache that stored what [`page_dims`]
    /// reported would call every page in the volume a spread. Every page would
    /// take a whole turn to itself, nothing would ever pair, and nothing on screen
    /// would say why. That is why [`page_dims`] reports the *stored* size and this
    /// is a separate step rather than one folded-together function:
    /// `decode_page` reads the same pair and applies the same tag to produce
    /// `PreviewAndFull::orig`, so the two routes agree by construction rather than
    /// by convention. **Do not fold the tag into `page_dims` to "fix" this** — the
    /// swap is its own inverse, so applying it a second time is not a wrong value to
    /// be spotted but the identity, which puts the *stored* pair in the cache and
    /// reproduces the failure above exactly.
    ///
    /// The tag is read under exactly the condition `decode_page` applies it under —
    /// JPEG only — because the dimensions have to be the ones the *pixels* end up
    /// at. A WebP or HEIC carrying an orientation tag is displayed unrotated here
    /// (the `heic` crate applies the container transforms itself), so judging it by
    /// a rotated size would be a second, quieter version of the same bug.
    ///
    /// **Answers from the cache when it can**, for [`Self::learn_pages`]' reason
    /// and with more force: this runs on *every* navigation, and unlike the four
    /// bytes of a page count this is a bounded read plus a format sniff plus, for
    /// a JPEG, an APP1 seek. Re-reading per visit would put a file open on the
    /// critical path of every keypress. The cache check is **per page**, so pressing
    /// right through a document measures each page once and no more.
    ///
    /// Does nothing for a page whose dimensions cannot be read. An unmeasured page is
    /// [`Shape::Single`] and pairs optimistically, which is the state the whole
    /// pairing is built to survive, so there is no error to report and nothing to
    /// retry: the arrived preview's `orig` will fill this in via
    /// [`Self::note_shapes`].
    pub fn learn_shapes(&mut self, pos: PagePos) {
        self.learn_one_page(pos);
        if self.pages_in(pos.index) <= 1 {
            return;
        }
        let Some(next) = self.step_no_wrap(pos, 1) else {
            return;
        };
        if next.index != pos.index {
            return;
        }
        self.learn_one_page(next);
    }

    /// The measurement half of [`Self::learn_shapes`], for one page.
    ///
    /// Separate so the page being learned and the page learned *for* go through
    /// byte-identical code: two copies of "read the header, apply the tag, store" is
    /// exactly where a fix to one and not the other would live, and the whole reason
    /// the EXIF swap is load-bearing is that both copies must do it.
    fn learn_one_page(&mut self, pos: PagePos) {
        let Some(path) = self.files.get(pos.index).cloned() else {
            return;
        };
        let key = ImgKey {
            path: path.clone(),
            page: pos.page,
        };
        if self.shapes.contains_key(&key) {
            return;
        }
        let Some(((w, h), jpeg)) = page_dims_and_jpeg(&path, pos.page) else {
            return;
        };
        // 0 for anything but a JPEG, matching `decode_page`: the tag is only ever
        // applied there, and `exif_orientation` costs a seek out of the file.
        let tag = if jpeg { exif_orientation(&path) } else { 0 };
        self.insert_shapes(key, if orientation_swaps_dims(tag) { (h, w) } else { (w, h) });
    }

    /// Record the size a page **displays** at, from a decode that already knows.
    ///
    /// A neighbour's preview reports its own dimensions on its way past, which makes
    /// this the cheapest source of the pairing's input there is, and it is what fills
    /// a page in before the cursor arrives at it.
    ///
    /// **Fill-only: this never replaces an entry that is already there.** `learn_shapes`
    /// measures the source header and that is the better number, so a decode's
    /// dimensions only fill a gap.
    ///
    /// The alternative — last writer wins — would let a decode that reported a
    /// *downscaled* size replace a measurement, and `learn_shapes` short-circuits on
    /// the entry existing, so the better number could never come back. **Refusing is
    /// not free, and the trade is worth stating:** an approximation that arrives
    /// first is kept and is never re-measured. What makes that acceptable is the next
    /// paragraph — route 1 agrees with `learn_shapes` by construction, so the common
    /// case has nothing to displace, and routes 2-4 are ratio-equivalent, so
    /// `shape_of` reads them correctly either way. The case this actually gives up
    /// is a route-4 fallback — a file whose header the decode could not sniff at all,
    /// where the preview size is kept instead of a later header measurement.
    ///
    /// **What the caller passes, precisely.** `dims` must already be
    /// display-oriented, and there are four routes into it. Only the first is a
    /// source measurement:
    ///
    /// 1. [`PreviewAndFull::orig`] as `decode_page` builds it for an ordinary
    ///    decode: `image_dims_bytes` plus the JPEG-only tag swap. A measurement, and
    ///    the same one [`Self::learn_shapes`] would compute from the same header — so
    ///    the two routes agree by construction rather than by convention. The
    ///    DCT-preview and embedded-thumbnail paths report this too: they change the
    ///    *pixels*, not the reported source size.
    /// 2. An **animation**: `orig` comes from `anim.frames[0]`, and
    ///    `collect_animation` has already downscaled that frame to
    ///    [`PREVIEW_TEXTURE_DIM`](crate::PREVIEW_TEXTURE_DIM) (4096). An animation
    ///    wider than that records a downscaled size.
    /// 3. A **page past the first of a TIFF**: `orig` is that page's *decoded*
    ///    dimensions, and `decode_tiff_page` has already downscaled it to the
    ///    request's target.
    /// 4. The **caller's fallback** for a file whose header could not be sniffed at
    ///    all, which is the size of the *decoded preview*.
    ///
    /// **Routes 2-4 are approximations, and fill-only is what makes them safe.** All
    /// the downscaling in them goes through `downscale_image`, which picks *one* scale
    /// factor and applies it to both axes, so the aspect ratio survives up to integer
    /// rounding — and the aspect ratio is the only thing
    /// [`shape_of`](crate::shape_of) reads. So an approximation that gets here first
    /// is still read correctly by `shape_of`, and one that arrives second is refused.
    /// The number in the cache is not always the source number; the *verdict* it
    /// produces is, which is all `dims` is asked for.
    ///
    /// Consequently a measurement taken from a decode that was already in flight
    /// carries that decode's bytes, so it inherits the preview cache's exposure to a
    /// result landing after the file was rewritten. Nothing here does anything about
    /// that: decodes are never abandoned, and the same exposure already exists for
    /// the texture those pixels become.
    pub fn note_shapes(&mut self, key: ImgKey, dims: (u32, u32)) {
        if self.shapes.contains_key(&key) {
            return;
        }
        self.shapes.insert(key, dims);
    }

    /// [`Self::note_shapes`] for a caller that has already established there is no
    /// entry — [`Self::learn_shapes`], which is the only one.
    fn insert_shapes(&mut self, key: ImgKey, dims: (u32, u32)) {
        self.shapes.insert(key, dims);
    }

    /// Forget every page of one file's remembered dimensions, so the next read
    /// measures the file's current contents.
    ///
    /// **Every page, not one.** The entries are keyed by [`ImgKey`], so a
    /// path-keyed `remove` would be a no-op for any page past the first — and a
    /// re-exported 200-page document would keep every one of its old shapes, with
    /// only page 0 ever re-measured. The same "all pages, not one" rule
    /// `invalidate_path` already applies to `previews`.
    ///
    /// **Must be called from `invalidate_path`,** beside `forget_page_count`. The
    /// file's *contents* changed, and only the caller knows that: a re-export can
    /// change a page's aspect from portrait to landscape, and the pairing is built
    /// to trust what it was told. Without this the shapes are frozen for the
    /// session and **every turn after that file stays mis-paired**, with no error
    /// and nothing on screen to say so.
    pub fn forget_shapes(&mut self, path: &Path) {
        self.shapes.retain(|key, _| key.path != path);
    }

    /// Forget one file's page count, and clamp the cursor if it was on that file.
    ///
    /// For a file whose *contents* changed. A 12-page document re-exported as one
    /// page has to stop offering 11 pages that do not exist, and the cursor has to
    /// come off page 12 rather than leaving `next` to walk off the end of a phantom
    /// document.
    pub fn forget_page_count(&mut self, path: &Path) {
        self.page_counts.remove(path);
        if self.current() == Some(path) {
            self.page = 0;
        }
    }

    /// Move to a file by index, starting at its first page.
    ///
    /// A *file* index, deliberately: a jump to a file has no page to go to, and
    /// resuming should not restore a page either, because the page count is
    /// learned on arrival and a remembered one could name a page that a rewritten
    /// file no longer has.
    pub fn goto_index(&mut self, index: usize) {
        if index < self.files.len() {
            self.index = index;
            self.page = 0;
        }
    }

    /// Move to a `(file, page)` position, as the turn walk produces one.
    ///
    /// The counterpart of [`Self::next`]/[`Self::prev`] for a position that is not
    /// one page away: manga mode's navigation hands the app the *anchor* of the next
    /// turn, and in a multi-page document that is a page in the middle of the file,
    /// which `next` can only reach by arriving there one page at a time.
    ///
    /// **A position is a whole answer, so this does not clamp the page** to
    /// `pages_in_current`. Every caller gets its position out of [`PageCtx::step`]
    /// or [`PageCtx::step_no_wrap`], which already respect the count, so a clamp
    /// here would be a second rule that silently disagrees with the walk's - and a
    /// disagreement here is a cursor somewhere the reader did not ask for.
    ///
    /// **Only a file index past the end of the list is refused**, and it changes
    /// nothing. That case cannot come out of the walk, which answers `None` past
    /// either end; it is here for a caller holding a stale position, because
    /// `next`/`prev` from an out-of-range index wrap to the front rather than
    /// refusing when `loop_back` is on - and this is the one of the three that does
    /// nothing either way.
    pub fn goto_page(&mut self, pos: PagePos) {
        if pos.index < self.files.len() {
            self.index = pos.index;
            self.page = pos.page;
        }
    }

    /// Rebuild [`Self::files`] from `all`, keeping only names matching `needle`.
    ///
    /// The match is a case-insensitive substring of the file name; an empty or
    /// whitespace-only needle means "no filter". The folder is scanned flat, so
    /// there is no directory component to match beyond the name.
    ///
    /// Before the substring fallback, a needle that parses as a rating query is
    /// decided from the sidecar instead of the name: `stars:N` and `stars:N+`
    /// (both: at least N), `flagged`, `unflagged`, or `tag:NAME`
    /// (case-insensitive substring of the tag list). A file with no sidecar
    /// reads as `stars:0` and not-flagged, so `stars:1+` excludes it rather
    /// than erroring; a damaged sidecar is likewise treated as unrated, so a
    /// broken sidecar can never hide its image. An unparseable `stars:` value
    /// (`stars:x`, `stars:`) is not a query and falls back to a name
    /// substring, which matches nothing unless a file is so named.
    ///
    /// Sidecars are read once per candidate per call. This runs once per filter
    /// change (never per frame), and only rating needles touch the disk at all,
    /// so there is no cross-call cache to go stale when a rating is edited
    /// while a filter is active.
    ///
    /// The current image is kept if it survived, otherwise the index falls back
    /// to the nearest surviving position (clamped, never out of range). Returns
    /// true if the current image changed, which is the caller's cue to re-goto.
    pub fn set_filtered(&mut self, all: &[PathBuf], needle: &str) -> bool {
        let current = self.current().map(|p| p.to_path_buf());
        let needle = needle.trim().to_lowercase();
        self.files = if needle.is_empty() {
            all.to_vec()
        } else if let Some(term) = parse_rating_term(&needle) {
            all.iter()
                .filter(|p| rating_term_matches(&term, &Ratings::read(p).unwrap_or_default()))
                .cloned()
                .collect()
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
        // The current file survived, so its page survives too and the user keeps
        // their place in a document they are part-way through. If it did not, the
        // index moved to a different file and the page must go with it.
        if self.current() != current.as_deref() {
            self.page = 0;
        }
        self.current() != current.as_deref()
    }

    /// Advance one image, or one page of a multi-page file.
    ///
    /// Within a document this moves by page; at the last page it moves to the
    /// first page of the next file. That is the whole point of making this a
    /// cursor: pressing right 199 times walks a 200-page scan and then carries on
    /// into the next folder entry, which is what every document viewer does and
    /// what nobody has to be told.
    ///
    /// Returns whether the view actually moved - `false` at the end with
    /// `loop_back` off, so the caller can skip the work that a real change would
    /// justify.
    pub fn next(&mut self) -> bool {
        let Some(to) = self.pos_next(self.index, self.page) else {
            return false;
        };
        self.index = to.index;
        self.page = to.page;
        true
    }

    /// The position one page on from `(index, page)`, or `None` past the end when
    /// `loop_back` is off.
    ///
    /// [`Self::next`]'s arithmetic, written against a position rather than the
    /// cursor so `PageCtx::step` can ask about a page it is not on **and get the
    /// same answer**. There is one implementation of these rules on purpose: `prev`
    /// from page 0 landing on the *last* page of the previous file is a
    /// deliberate rule that exists because two presses per file feels broken, and a
    /// second copy of it would be a second place for it to be wrong - silently, and
    /// only in manga mode.
    ///
    /// It is a separate function rather than a clone-and-call because
    /// `PageCtx::step` has to answer about a position the cursor is **not** on, and
    /// the state `next`/`prev` read beyond `(index, page)` — `files` and
    /// `page_counts` — is already available on `&self`. A clone would be a way of
    /// manufacturing a `&mut self` cursor to answer a question that needs neither
    /// the cursor nor the mutation.
    fn pos_next(&self, index: usize, page: u32) -> Option<PagePos> {
        if self.files.is_empty() {
            return None;
        }
        if page + 1 < self.pages_in(index) {
            return Some(PagePos {
                index,
                page: page + 1,
            });
        }
        // Last page (or an ordinary file): on to the next one, at its first page.
        match index.checked_add(1) {
            Some(i) if i < self.files.len() => Some(PagePos {
                index: i,
                page: 0,
            }),
            _ if self.loop_back => Some(PagePos {
                index: 0,
                page: 0,
            }),
            _ => None,
        }
    }

    /// Go back one image, or one page of a multi-page file.
    ///
    /// Symmetric with [`Self::next`]: from page 0 it goes to the *last* page of
    /// the previous file, not its first. Anything else makes stepping backwards
    /// through a folder need two presses per file, which is a small thing that
    /// feels broken.
    pub fn prev(&mut self) -> bool {
        let Some(to) = self.pos_prev(self.index, self.page) else {
            return false;
        };
        self.index = to.index;
        self.page = to.page;
        true
    }

    /// The position one page back from `(index, page)`, or `None` before the front
    /// when `loop_back` is off. [`Self::pos_next`]'s counterpart, for
    /// [`Self::prev`]'s reasons.
    fn pos_prev(&self, index: usize, page: u32) -> Option<PagePos> {
        if self.files.is_empty() {
            return None;
        }
        if page > 0 {
            return Some(PagePos {
                index,
                page: page - 1,
            });
        }
        // The new file's page count is only known if it was visited, which it was,
        // since the user is stepping back through where they came. `pages_in`
        // falls back to 1 for anything unknown, so this is always in range.
        match index.checked_sub(1) {
            Some(i) => Some(PagePos {
                index: i,
                page: self.pages_in(i).saturating_sub(1),
            }),
            None if self.loop_back => {
                let last = self.files.len() - 1;
                Some(PagePos {
                    index: last,
                    page: self.pages_in(last).saturating_sub(1),
                })
            }
            None => None,
        }
    }

    /// How many pages the file at `index` has, as far as this gallery knows.
    /// 1 until [`Self::learn_pages`] has been asked about it.
    ///
    /// [`Self::pages_in_current`] is the same question about the *cursor's*
    /// file, which is not the same thing: the pairing asks about pages the
    /// cursor is not on, and answering with the current file's page count would
    /// make a three-page document's last page look like an ordinary file's.
    pub fn pages_in(&self, index: usize) -> u32 {
        self.files
            .get(index)
            .and_then(|p| self.page_counts.get(p))
            .copied()
            .unwrap_or(1)
            .max(1)
    }

    /// The page `by` pages from `pos`, without honouring `loop_back`.
    ///
    /// This is [`Self::next`] and [`Self::prev`]'s page arithmetic with the
    /// wrap taken out, which is what [`PageCtx::step_no_wrap`] needs so a
    /// gallery's last turn cannot straddle the wrap. `None` past either end
    /// whatever `loop_back` says.
    ///
    /// Written against a position rather than `&mut self` so the pairing can
    /// ask about a page it is not currently on; the caller in Task 2's
    /// `PageCtx for Gallery` is a one-liner over this.
    ///
    /// **`by` may be more than one page**, because `prev_anchor`'s walk steps by
    /// a whole turn, which is 1 or 2. Handled as a loop rather than a `match` on
    /// 1 and -1: a version that only understood single steps returns `None` for
    /// every turn of two, which is the same shape of bug as using
    /// `pages_in_current()` for another file's page count - correct-looking, and
    /// invisible until a real gallery is walked.
    ///
    /// Every caller in this crate passes 1, 2 or -1. A `by` of `isize::MIN`
    /// would loop 2^63 times, so it is not defended against: no caller can
    /// produce one, and the alternative - a saturating bound - would be a branch
    /// that cannot change an answer for any reachable input.
    pub fn step_no_wrap(&self, pos: PagePos, by: isize) -> Option<PagePos> {
        if pos.index >= self.files.len() {
            return None;
        }
        let mut cur = pos;
        for _ in 0..by.unsigned_abs() {
            cur = match by.signum() {
                1 if cur.page + 1 < self.pages_in(cur.index) => {
                    PagePos { index: cur.index, page: cur.page + 1 }
                }
                // Last page (or an ordinary file): on to the next one, at its
                // first page. `None` at the last file rather than wrapping.
                1 => cur.index.checked_add(1).filter(|&i| i < self.files.len()).map(|index| PagePos {
                    index,
                    page: 0,
                })?,
                -1 if cur.page > 0 => PagePos { index: cur.index, page: cur.page - 1 },
                // From page 0 of a file: the *last* page of the previous one, not
                // its first, matching `prev`. `None` at the first file rather
                // than wrapping.
                -1 => cur.index.checked_sub(1).map(|index| PagePos {
                    index,
                    page: self.pages_in(index) - 1,
                })?,
                _ => return None,
            };
        }
        Some(cur)
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
        self.load_all_from(&scan_folder(folder));
    }

    /// Load all images from the folder tree, preserving the current image's
    /// position. Same contract as [`Self::load_all_from_folder`], over the
    /// recursive walk instead of one level.
    pub fn load_all_from_folder_recursive(&mut self, folder: &Path) {
        self.load_all_from(&scan_folder_recursive(folder));
    }

    /// Install a scanned list, keeping the current image selected when it is
    /// still in it. Both loaders above are this with different scans, so a
    /// position that survives in one survives in the other.
    fn load_all_from(&mut self, new_files: &[PathBuf]) {
        // Remember current image path
        let current_path = match self.current() {
            Some(p) => p.to_path_buf(),
            None => PathBuf::new(),
        };

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

        self.files = new_files.to_vec();
        self.index = new_index;
        // The list these counts described is gone.
        self.forget_pages();
    }
}

/// Should this keypress step out of the open archive?
///
/// **Both edges, and `can_leave` before either.** The forward edge is the one
/// that was missing, and its absence is what trapped the reader: `→` at an
/// archive's last page had nowhere to go but back to its own first page, so a
/// folder of archives behaved like a folder of one.
///
/// `can_leave` is the parent's own answer to "is there anywhere to go", and it
/// is asked *before* leaving. An archive with `loop_back` off whose entry is the
/// folder's last must not be left by a press that has nowhere to land —
/// otherwise the reader is dropped into the parent folder and the press looks
/// broken, which is a worse answer than staying put.
///
/// Pure, and pure for the usual reason: the key handling that applies it is in
/// the binary, unreachable from a test, so the rule it exists to apply is the
/// part worth being able to test.
pub fn leave_archive_at(forward: bool, at_front: bool, at_back: bool, can_leave: bool) -> bool {
    can_leave && if forward { at_back } else { at_front }
}

/// The real [`PageCtx`]: a gallery, its page counts, and the shape cache.
///
/// Each method answers from something that already exists — the position
/// arithmetic `next`/`prev` and `step_no_wrap` share, or the two caches — which
/// is the point of the trait having no default bodies: a default would be a guess
/// about a gallery this section cannot see, and the guess would be silently wrong
/// for archives and for a `page_counts` that has not been learned yet.
///
/// Two traps this impl walks straight into, both answered *here* rather than left
/// for the next reader:
///
/// * **`pages_in`, never `pages_in_current`.** The latter is about `self.index`,
///   so it is right for the cursor and wrong for every other position - and the
///   pairing spends its whole life asking about positions it is not on.
/// * **`flat` reads `page_counts` directly, not through `pages_in`.** The other
///   way round: `pages_in` answers 1 for a file it knows nothing about, which is
///   the right default for "how many pages does this file have" and a lie for
///   "how many pages come before this one".
impl PageCtx for Gallery {
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
        if from.index >= self.files.len() {
            // `pos_next`/`pos_prev` would answer `Some(0, 0)` here rather than
            // `None`: an index past the end finds no next file and falls into the
            // `loop_back` arm. A position that does not exist has nothing to step
            // from, and wrapping to the front would say a page exists that does not.
            return None;
        }
        // A loop rather than a `match` on 1 and -1, for the reason
        // `Gallery::step_no_wrap` gives: `prev_anchor`'s walk steps by a whole
        // turn, and a turn is 1 or 2. `by` of 0 runs no iterations, so it answers
        // `Some(from)`, which is also what `step_no_wrap` does.
        //
        // `by.unsigned_abs()` is undefended here for the reason it is undefended
        // there: no caller can produce `isize::MIN` without writing it out by hand,
        // and a saturating bound would be a branch that cannot change an output for
        // any reachable input. **Unlike `step_no_wrap`'s loop, this one could spin**
        // — `pos_next` and `pos_prev` both wrap when `loop_back` is on, so a `by` of
        // `isize::MIN` would not return. Nothing reaches that: the only caller of
        // `PageCtx::step` is `next_anchor`, which passes `turn_len`, and `turn_len`
        // returns 1 or 2 by construction.
        let mut cur = from;
        for _ in 0..by.unsigned_abs() {
            cur = if by < 0 {
                self.pos_prev(cur.index, cur.page)?
            } else {
                self.pos_next(cur.index, cur.page)?
            };
        }
        Some(cur)
    }

    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
        Gallery::step_no_wrap(self, from, by)
    }

    fn key(&self, pos: PagePos) -> Option<ImgKey> {
        self.files.get(pos.index).map(|path| ImgKey {
            path: path.clone(),
            page: pos.page,
        })
    }

    /// The cached display dimensions, and **not** a re-read.
    ///
    /// A lookup and nothing else, because this is on the pairing's hot path:
    /// `prev_anchor` calls `pairable` once per page of its O(n) walk, and each of
    /// those makes four `dims` calls. Reading a file here would turn a query into
    /// disk I/O in the middle of a keypress.
    ///
    /// **Keyed by the position's own [`ImgKey`], not by its file** — so a page whose
    /// dimensions have not been measured yet is `None` while its neighbour is
    /// `Some`, and that is the whole point: a document with a landscape spread among
    /// portrait pages has to be able to say so.
    ///
    /// `None` is a state the pairing is built to survive - an unmeasured page is
    /// `Single` and pairs optimistically - so it is not an error here, and there is
    /// nothing to fall back to.
    fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
        self.key(pos).and_then(|key| self.shapes.get(&key)).copied()
    }

    /// Always `None` today. The user's override is a separate feature and it is
    /// asked for separately rather than folded into `dims`, because it beats the
    /// detector outright - including when the detector is confidently wrong, which
    /// is the case the override exists for.
    fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
        None
    }

    /// The page's ordinal in the whole gallery, and `None` when a preceding file's
    /// page count is unknown.
    ///
    /// `page_counts.get(..)?` rather than [`Gallery::pages_in`]: that answers 1 for
    /// an unknown file, and substituting its default would turn "I do not know how
    /// many pages this document has" into a confidently wrong ordinal. The walk only
    /// ever asks about the leading pages, where the cursor's having arrived
    /// guarantees the counts are known - so this is an optimisation in practice and
    /// a lie avoided in principle.
    fn flat(&self, pos: PagePos) -> Option<usize> {
        if pos.index >= self.files.len() {
            return None;
        }
        let mut n = pos.page as usize;
        for path in &self.files[..pos.index] {
            n += *self.page_counts.get(path)? as usize;
        }
        Some(n)
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