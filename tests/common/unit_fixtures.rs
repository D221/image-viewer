//! Shared fixtures for the unit suite targets.
//!
//! `tests/unit.rs` was split by subject (see `tests/source_size.rs`); the
//! helpers below are used by more than one target, so they live here rather
//! than in each. Every target compiles its own copy through
//! `#[path = "common/unit_fixtures.rs"]`, the pattern `tests/common/` already
//! uses.
//!
//! **The filter globals are process-wide:** `with_resample_at` takes the lock
//! once and pins all three settings, so no caller can nest two acquisitions
//! and hang on the non-reentrant mutex.
#![allow(dead_code)]

use image_viewer::{GRID_CELL, GRID_LABEL_H, grid_row_h};

/// `set_linear_resample` / `set_downscale_filter` are process-wide globals, and
/// `cargo test` runs the tests in a file concurrently. Anything that asserts
/// on a *specific* filter path has to take this lock and put the globals back,
/// or it silently tests whichever path another test last selected.
pub static FILTER_GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Set the filter globals at once, restore them after.
///
/// **One lock acquisition, never two.** These used to be separate helpers and
/// nesting them - `with_strength(s, || with_linear_resample(true, ..))` -
/// deadlocked on the second `lock()` of a non-reentrant `Mutex`, which is why
/// the strengths are swept through here rather than through a wrapper of its
/// own. A guard that only worked when the two settings were never changed
/// together would have been a guard that hung instead of testing anything.
///
/// `floor` is [`image_viewer::MIN_FILTER_SCALE`] by default. **`0.0` means the
/// pre-`MIN_FILTER_SCALE` behaviour** and is the arm several tests compare
/// against, so it has to be reachable without a second build - which is why the
/// floor is a runtime atomic and not a bare constant read at the call site.
pub fn with_resample_at<T>(linear: bool, strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
    let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let (was_linear, was_strength, was_floor) = (
        image_viewer::linear_resample_enabled(),
        image_viewer::downscale_strength(),
        image_viewer::min_filter_scale(),
    );
    image_viewer::set_linear_resample(linear);
    image_viewer::set_downscale_strength(strength);
    image_viewer::set_min_filter_scale(floor);
    let out = f();
    image_viewer::set_min_filter_scale(was_floor);
    image_viewer::set_downscale_strength(was_strength);
    image_viewer::set_linear_resample(was_linear);
    out
}

/// Run `f` with the downscale filter globals forced to `linear`, restoring the
/// previous setting afterwards.
pub fn with_linear_resample<T>(linear: bool, f: impl FnOnce() -> T) -> T {
    with_resample_at(linear, cached_default_strength(), image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit strength, everything else shipped.
#[allow(dead_code)]
pub fn with_strength<T>(strength: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(true, strength, image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit `MIN_FILTER_SCALE` and an explicit strength. `0.0`
/// disables the floor, which is the arm the fix is measured against, so the
/// strength is a **required** argument rather than a default: leaving it to be
/// read is how the "before" column came to be a `Smooth` column.
pub fn with_floor_at<T>(strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(true, strength, floor, f)
}

/// [`with_floor_at`] at the shipped strength.
#[allow(dead_code)]
pub fn with_floor<T>(floor: f32, f: impl FnOnce() -> T) -> T {
    with_floor_at(cached_default_strength(), floor, f)
}

// The three below are twins of `tests/resample.rs`'s: one test target cannot
// import from another, the `with_*` helpers above take the default strength,
// and the default is read rather than named so a probe that says "the default
// path" cannot end up reading whichever arm a sibling test last selected.

/// The default config, read from `Config::load` on an absent file.
pub fn default_config() -> image_viewer::Config {
    let tmp = tempfile::tempdir().unwrap();
    image_viewer::Config::load(&tmp.path().join("absent.json"))
}

pub fn default_strength() -> f32 {
    if default_config().smooth_downscale {
        image_viewer::SMOOTH_STRENGTH
    } else {
        image_viewer::FAST_STRENGTH
    }
}

pub fn cached_default_strength() -> f32 {
    static ONCE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *ONCE.get_or_init(default_strength)
}

/// Every library source file, concatenated - the whole crate except the binary.
///
/// `lib.rs` is a list of modules rather than the code (see `AGENTS.md` and
/// `tests/source_size.rs`), so a scan of `include_str!("../src/lib.rs")` would
/// read 127 lines of `pub mod` and find nothing. Read at run time rather than
/// with `include_str!`: following the modules is exactly what these scans have
/// to do, and a module added later must be covered without anyone remembering
/// to add it here.
pub fn main_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name == "main.rs" || name.starts_with("app/"))
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn lib_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name != "main.rs")
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file under `src/`, as `(relative name, text)`, sorted.
pub fn all_src_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out: Vec<(String, String)> = Vec::new();
    collect_src(&root, &root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn collect_src(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_src(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&path).unwrap()));
        }
    }
}

/// Build a minimal JPEG with an EXIF APP1 segment carrying the given
/// orientation tag, like a camera would.
pub fn make_jpeg_with_orientation(tag: u16) -> Vec<u8> {
    let img = image::DynamicImage::new_rgb8(3, 2);
    let mut jpeg = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();

    // TIFF header + IFD0 with a single SHORT entry: Orientation = tag.
    let mut tiff = Vec::new();
    tiff.extend(b"II");
    tiff.extend(42u16.to_le_bytes());
    tiff.extend(8u32.to_le_bytes()); // IFD0 offset
    tiff.extend(1u16.to_le_bytes()); // one entry
    tiff.extend(0x0112u16.to_le_bytes()); // Orientation tag
    tiff.extend(3u16.to_le_bytes()); // type SHORT
    tiff.extend(1u32.to_le_bytes()); // count
    tiff.extend((tag as u16).to_le_bytes()); // inline value
    tiff.extend(0u16.to_le_bytes()); // value padding
    tiff.extend(0u32.to_le_bytes()); // no IFD1

    let payload_len = (6 + tiff.len()) as u16;
    let mut app1 = vec![0xFF, 0xE1];
    app1.extend(payload_len.to_be_bytes());
    app1.extend(b"Exif\0\0");
    app1.extend(&tiff);

    let mut out = vec![0xFF, 0xD8]; // SOI
    out.extend(app1);
    out.extend(&jpeg[2..]); // rest of the JPEG
    out
}

/// Drive one headless egui pass and return its output.
///
/// egui 0.36 debug-asserts when a `TexturesDelta` is dropped unapplied
/// (`TexturesDelta::drop` in epaint). A headless pass emits the font-atlas
/// delta every frame and nothing applies it — in the real app eframe's
/// renderer does — so every test pass has to clear it. Tests that want the
/// returned `FullOutput` should go through this rather than `Context::run_ui`.
pub fn run_pass(
    ctx: &egui::Context,
    input: egui::RawInput,
    f: impl FnMut(&mut egui::Ui),
) -> egui::FullOutput {
    let mut out = ctx.run_ui(input, f);
    out.textures_delta.clear();
    out
}

/// Draw one row of `cols` cells exactly the way `gallery_grid`/`grid_cell` do.
/// `out` collects the cell rects, `hits` the clicked cell indices, `gap` the
/// horizontal `item_spacing` of the layout that actually placed the cells.
pub fn grid_row(
    ui: &mut egui::Ui,
    cols: usize,
    out: &mut Vec<egui::Rect>,
    hits: &mut Vec<usize>,
    gap: &mut f32,
) {
    egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
        ui,
        grid_row_h(GRID_CELL),
        1,
        |ui, _| {
            ui.horizontal(|ui| {
                *gap = ui.spacing().item_spacing.x;
                for i in 0..cols {
                    // Explicit allocation: this is the whole point of the test.
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(GRID_CELL, GRID_CELL + GRID_LABEL_H),
                        egui::Sense::click(),
                    );
                    if ui.is_rect_visible(rect) {
                        ui.painter().rect(
                            rect,
                            0.0,
                            egui::Color32::from_gray(40),
                            egui::Stroke::NONE,
                            egui::StrokeKind::Inside,
                        );
                    }
                    let inner = rect.shrink(4.0);
                    // `new_child`, not `scope_builder`: the cell above already
                    // advanced this row's cursor, and `scope_builder` would
                    // advance it a second time, stealing `item_spacing` from
                    // every following cell. 0.27's `child_ui` did not advance.
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(inner)
                            .layout(egui::Layout::top_down(egui::Align::Center)),
                    );
                    // Thumbnail area. The image itself is painted centred by
                    // hand (fit_scale_no_upscale + from_center_size), which is
                    // covered by test_fit_scale_no_upscale; a widget here cannot
                    // demonstrate it because widgets place at the cursor.
                    child.allocate_exact_size(
                        egui::vec2(inner.width(), GRID_CELL),
                        egui::Sense::hover(),
                    );
                    child.add(egui::Label::new("a-fairly-long-file-name.jpg").truncate());
                    out.push(resp.rect);
                    if resp.clicked() {
                        hits.push(i);
                    }
                }
            });
        },
    );
}

/// Every non-ASCII character the app can draw must be in the font it draws with.
///
/// This exists because of a bug you can see in a screenshot and nothing else
/// catches: the mirror buttons shipped as **blank boxes**. egui's default font is
/// a subset, and U+21CB / U+21C5 ("leftwards arrow over rightwards", "upwards
/// arrow over downwards") simply are not in it. A missing glyph is not a compile
/// error, not a warning, and not a failed assertion — it is a rectangle, and it
/// looks like a font bug rather than a missing-feature bug.
///
/// The same class had already shipped once: `Prev (←)` and `Next (→)` were blank
/// too, on the two most-used buttons in the app, since before any of this work.
/// So this checks the *sources* rather than a hand-kept list, which means a glyph
/// added to any string literal in either UI file is covered the moment it lands,
/// with no list to forget to update.
///
/// Scans for non-ASCII in any line containing a quote, which over-covers (comments
/// count). That is deliberate: a false positive here costs one word, and a false
/// negative costs a blank box.
/// Drop a trailing `//` comment, respecting string literals so a `//` inside a
/// URL does not truncate the line.
///
/// Needed because the comment explaining the missing-glyph bug has to be able to
/// *name* the glyphs it is about. Without this, the fix's own explanation keeps
/// the test red.
pub fn strip_line_comment(line: &str) -> &str {
    let b = line.as_bytes();
    let mut in_str = false;
    let mut in_chr = false;
    let mut esc = false;
    for i in 0..b.len() {
        let c = b[i];
        if esc {
            esc = false;
            continue;
        }
        match c {
            b'\\' if in_str || in_chr => esc = true,
            b'"' if !in_chr => in_str = !in_str,
            b'\'' if !in_str => in_chr = !in_chr,
            b'/' if !in_str && !in_chr && i + 1 < b.len() && b[i + 1] == b'/' => {
                return &line[..i];
            }
            _ => {}
        }
    }
    line
}

/// The body of the function whose signature is `sig`, by brace counting.
///
/// The same helper as `tests/common/spreads_fixtures.rs`, kept local rather than shared because
/// each test binary compiles separately, and these scans sit beside the
/// behaviour they belong to.
pub fn fn_body<'a>(src: &'a str, sig: &str) -> Option<&'a str> {
    let start = src.find(sig)? + sig.len();
    let mut depth = 0i32;
    let mut end = None;
    for (i, b) in src.as_bytes()[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end?;
    Some(&src[start..end])
}

/// `src` with every run of whitespace removed.
///
/// A needle that spans more than one line only matches if it happens to be
/// wrapped the way rustfmt wraps it today, so any scan that asks "is this
/// expression there" would break on a reformat that changes nothing else. Run
/// both sides through this and the wrap points stop mattering; only the tokens
/// are compared. Do not use it on a needle that must be single-line or on a
/// scan whose subject *is* the layout - the position tests below compare
/// offsets in the squashed text for the same reason.
pub fn squash(src: &str) -> String {
    src.split_whitespace().collect()
}

/// A real 2x2 PNG, built here rather than vendored: `image` can write PNG, so
/// `tests/fixtures/` is for formats nothing in the dependency set can produce.
pub fn archive_test_png() -> Vec<u8> {
    let img =
        image::RgbaImage::from_fn(2, 2, |x, y| image::Rgba([x as u8 * 80, y as u8 * 80, 40, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Build a zip/cbz of the given members. `deflate` is the default on purpose:
/// a *stored* zip would pass even if the reader only ever read headers.
pub fn write_test_zip(path: &std::path::Path, members: &[(&str, Vec<u8>)]) {
    let file = std::fs::File::create(path).unwrap();
    let mut zw = zip::ZipWriter::new(file);
    let opts: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in members {
        zw.start_file(*name, opts).unwrap();
        std::io::Write::write_all(&mut zw, bytes).unwrap();
    }
    zw.finish().unwrap();
}

/// Build a 7z of the given members, through the same crate that reads it back.
///
/// `compress_to_path` takes a *source path* rather than a list of entries, so
/// this writes the members out first and compresses the tree. That also means
/// the nested names (`ch1/01.png`) reach the archive as real directories, which
/// is what the flattening has to deal with.
pub fn write_test_7z(path: &std::path::Path, members: &[(&str, Vec<u8>)]) {
    let staging = path.with_extension("7z-staging");
    let _ = std::fs::remove_dir_all(&staging);
    for (name, bytes) in members {
        let p = staging.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    sevenz_rust2::compress_to_path(&staging, path).unwrap();
    let _ = std::fs::remove_dir_all(&staging);
}
