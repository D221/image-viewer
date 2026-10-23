//! Module `commands`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use commands::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// `1` keeps the shipped meaning (100%), then doubling to 8×.
pub(crate) fn default_zoom_presets() -> [f32; 4] {
    [1.0, 2.0, 4.0, 8.0]
}

/// Three seconds: JPEGView's middle preset, and slow enough to actually see
/// the picture.
pub(crate) fn default_slideshow_secs() -> u32 {
    3
}

/// File timing.
pub(crate) fn default_anim_speed() -> f32 {
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
    // Shipped unbound on purpose: Space is the fit/100% toggle (`fit_alt`,
    // JPEGView's own Space key), and a bare navigation-Space would fight it in
    // the same hand. The action stays rebindable — and is the hook for anyone
    // who wants Space navigating again — but nothing shipped reads it, so a
    // blank override here resolves to *unbound*, not to the shipped chord.
    ("next_alt", "", "Next image (alt)"),
    ("page_next", "PageDown", "Next image (page)"),
    ("page_prev", "PageUp", "Previous image (page)"),
    ("first", "Home", "First image"),
    ("last", "End", "Last image"),
    ("grid", "G", "Toggle gallery grid"),
    ("panels", "I", "Toggle all panels"),
    ("pane_left", "L", "Toggle left pane"),
    ("pane_right", "P", "Toggle right pane"),
    ("fit", "F", "Fit view mode"),
    ("fit_alt", "Space", "Fit view mode (alt)"),
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
/// deleting the action — *unless the shipped default is itself blank*
/// (`next_alt`: deliberately unbound so Space can be the zoom toggle), in
/// which case the chain ends at *unbound*, because there is no shipped chord
/// for a typo to fall back to.
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

pub(crate) fn default_true() -> bool {
    true
}

/// The shipped grid cell size, and the value an old config without the key
/// comes up on.
pub(crate) fn default_grid_cell() -> f32 {
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
pub(crate) fn strip_bom(text: &str) -> &str {
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
            pixel_art: false,
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
            preview_store: true,
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
            title_strip: 0.0,
            fit_window_mode: false,
            fit_mode: false,
            remember_view: false,
            show_panels: true,
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


