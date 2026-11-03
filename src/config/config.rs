//! Module `config`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use config::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

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

    /// **Pixel-art sampling**: content textures are uploaded with **nearest
    /// on magnify, linear on minify**, so a sprite drawn larger than its own
    /// pixels shows squares of its texels instead of a smear, and a zoomed-out
    /// picture (fit or smaller) keeps the averaging filter — a minifying
    /// nearest sample drops pixel rows and shears straight lines, which is a
    /// worse look than the smoothing it replaces. Off by default: magnified
    /// nearest on a photo is exactly the blocky look nobody wants as the
    /// shipped filter.
    ///
    /// The choice names the **two sampler axes and not a zoom level**, so it
    /// is made once when a texture is uploaded and never has to be un-done
    /// when the zoom later crosses 100% — egui picks the sampler per axis at
    /// draw time. What a flip *does* have to touch is textures already
    /// uploaded (they keep the samplers they arrived with), and the settings
    /// arm walks the same `refilter` shape the Smooth toggle does.
    ///
    /// This is the decision and its mapping lives in the binary
    /// ([`pixel_art_sampling`], which returns a plain enum: this module keeps
    /// no egui dependency on purpose).
    #[serde(default)]
    pub pixel_art: bool,

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

    /// Keep decoded previews of large pictures in
    /// `%LOCALAPPDATA%\image-viewer\previews`.
    ///
    /// On by default because the decode of a large scan is the floor of this
    /// crate — 690 ms for a 28 MB JPEG here — and a revisit, a slideshow or a
    /// back-flip pays it again for nothing. Measured, the same picture from this
    /// store is on screen in 22-50 ms.
    ///
    /// Off means nothing is written outside the config folder and every large
    /// picture is decoded again, every time.
    #[serde(default = "default_true")]
    pub preview_store: bool,

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

    /// Favourite folders: bookmarked locations the reader can jump to from
    /// Settings. Absolute paths; a favourite whose folder no longer exists is
    /// silently skipped when opened.
    #[serde(default)]
    pub favourite_folders: Vec<PathBuf>,

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

    /// The title strip's height (ui points) as measured when the title bar
    /// was last hidden. The strip exists only while the window is decorated,
    /// so a hide is the one moment it can be measured — and showing it back
    /// needs it, to put the outer frame where the client area the user was
    /// looking at stays put ([`outer_top_for_decoration_toggle`]). Persisted
    /// like `RendererPref.probed`'s measured number is: it is machine
    /// geometry, not a preference.
    #[serde(default)]
    pub title_strip: f32,

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

    /// Fit-width as a sticky mode: every image scaled so its width fills the
    /// window, letting the height overflow so you pan vertically — the natural
    /// mode for tall pages and scans. Mutually exclusive with fit mode —
    /// turning one on switches the other off. Off by default.
    #[serde(default)]
    pub fit_width_mode: bool,

    /// Remembered window size, so a restart opens where you left it. Validated
    /// against the current monitors on load — a saved position on a monitor
    /// that no longer exists is dropped and the window is centred instead.
    #[serde(default)]
    pub window_size: Option<(u32, u32)>,

    /// Remembered window position (top-left, physical pixels).
    #[serde(default)]
    pub window_pos: Option<(i32, i32)>,

    /// Remembered maximized state.
    #[serde(default)]
    pub window_maximized: bool,

    /// **Remember the view per image**: an image you zoomed or panned comes
    /// back the way you left it — zoom **and** pan restored — while an image
    /// never touched keeps today's behaviour (zoom carried, pan reset), for
    /// the same reason carrying is the shipped rule: a carried offset shifts
    /// the next picture by the old one's drag.
    ///
    /// Memory is **per `(file, page)`** like every cache that holds decoded
    /// pixels, and lasts only for the session — it is view state, not a
    /// preference: chrome (`show_panels`, `hide_title_bar`) persists because
    /// it is a taste about how much sits around the picture, a remembered
    /// zoom is not, it ends with the run. The two fit modes own the arriving
    /// view absolutely when they are on (their own arrival rules run after
    /// the restore), so this changes behaviour exactly where no mode already
    /// has an answer. Off by default: remembering is opt-in, and every plan
    /// the app makes out of the box is pinned against the behaviour it
    /// shipped with.
    ///
    /// `#[serde(default)]` for the reason every added field here carries it:
    /// a config written before this key existed still has to load.
    #[serde(default)]
    pub remember_view: bool,

    /// **The side-panel master, persisted.** `I` hides or shows both panes
    /// and remembers the choice between launches — the JPEGView-style
    /// minimal state: chrome off because the photo is enough. Unlike the
    /// per-image view memory above, this is a *preference*: how much chrome
    /// a person keeps around them is a taste, and `hide_title_bar` set the
    /// precedent that such a taste belongs in the config.
    ///
    /// Fullscreen is the one pattern that does **not** persist it, and that
    /// boundary is load-bearing: in fullscreen the master is an overlay the
    /// binary owns without saving — entering stashes the value, leaving
    /// restores the stash — so a session with a fullscreen detour cannot
    /// rewrite what the user chose, even though the exit path saves the
    /// config with the in-memory value and would otherwise stomp it.
    ///
    /// Default **on**, so a config from before this key existed keeps showing
    /// panels: `#[serde(default = "default_true")]`.
    #[serde(default = "default_true")]
    pub show_panels: bool,
}
