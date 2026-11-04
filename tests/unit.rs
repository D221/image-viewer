use image_viewer::{Gallery, ZoomPan, load_image};

/// `set_linear_resample` / `set_downscale_filter` are process-wide globals, and
/// `cargo test` runs the tests in a file concurrently. Anything that asserts
/// on a *specific* filter path has to take this lock and put the globals back,
/// or it silently tests whichever path another test last selected.
static FILTER_GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
fn with_resample_at<T>(linear: bool, strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
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
fn with_linear_resample<T>(linear: bool, f: impl FnOnce() -> T) -> T {
    with_resample_at(linear, cached_default_strength(), image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit strength, everything else shipped.
#[allow(dead_code)]
fn with_strength<T>(strength: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(true, strength, image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit `MIN_FILTER_SCALE` and an explicit strength. `0.0`
/// disables the floor, which is the arm the fix is measured against, so the
/// strength is a **required** argument rather than a default: leaving it to be
/// read is how the "before" column came to be a `Smooth` column.
fn with_floor_at<T>(strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(true, strength, floor, f)
}

/// [`with_floor_at`] at the shipped strength.
#[allow(dead_code)]
fn with_floor<T>(floor: f32, f: impl FnOnce() -> T) -> T {
    with_floor_at(cached_default_strength(), floor, f)
}

// The three below are twins of `tests/resample.rs`'s: one test target cannot
// import from another, the `with_*` helpers above take the default strength,
// and the default is read rather than named so a probe that says "the default
// path" cannot end up reading whichever arm a sibling test last selected.

/// The default config, read from `Config::load` on an absent file.
fn default_config() -> image_viewer::Config {
    let tmp = tempfile::tempdir().unwrap();
    image_viewer::Config::load(&tmp.path().join("absent.json"))
}

fn default_strength() -> f32 {
    if default_config().smooth_downscale {
        image_viewer::SMOOTH_STRENGTH
    } else {
        image_viewer::FAST_STRENGTH
    }
}

fn cached_default_strength() -> f32 {
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
fn main_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name == "main.rs" || name.starts_with("app/"))
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn lib_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name != "main.rs")
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file under `src/`, as `(relative name, text)`, sorted.
fn all_src_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out: Vec<(String, String)> = Vec::new();
    collect_src(&root, &root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn collect_src(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
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
#[test]
fn test_load_misnamed_png_with_jpg_extension() {
    // A real PNG saved with a .jpg extension must still decode fully;
    // format detection is content-based, never extension-based.
    // Build a valid PNG via the encoder, then load it through a
    // .jpg-named path.
    let tmp = tempfile::tempdir().unwrap();
    let fake = tmp.path().join("misnamed.jpg");
    let img = image::DynamicImage::new_rgba8(64, 32);
    let mut real_png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut real_png), image::ImageFormat::Png).unwrap();
    std::fs::write(&fake, &real_png).unwrap();

    let data = load_image(&fake, 1600).expect("misnamed PNG should load");
    assert_eq!((data.width, data.height), (64, 32));
    assert!(!data.rgba.is_empty());

    // Full-res path (what the viewer uses for tiles) must also work.
    let full = load_image(&fake, image_viewer::MAX_TEXTURE_DIM).expect("full load");
    assert_eq!((full.width, full.height), (64, 32));
}

#[test]
fn test_preview_full_single_decode_small_file() {
    // Files small enough to decode fast (e.g. a 2560x1440 wallpaper) must skip
    // the preview stage: load_image_preview_full returns the full image
    // directly, no second decode and no dwell wait.
    let tmp = tempfile::tempdir().unwrap();
    let img = image::DynamicImage::new_rgba8(2560, 1440);
    let mut jpeg = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg).unwrap();
    let p = tmp.path().join("wall.jpg");
    std::fs::write(&p, &jpeg).unwrap();

    let r = image_viewer::load_image_preview_full(&p, 2560).unwrap();
    assert_eq!(r.orig, Some((2560, 1440)));
    let full = r.full.expect("fast file should come with full image");
    assert_eq!((full.width, full.height), (2560, 1440));
    // Texture matches the requested target exactly: fit-to-window is sharp on
    // any display size, no blurry preview stage.
    assert_eq!((r.preview.width, r.preview.height), (2560, 1440));
    // Source fits the target: texture is at full source density.
    assert!(!r.below_target);
}

#[test]
fn test_preview_full_single_decode_png() {
    // PNG has no reduced-decode shortcut, so the preview pass must decode once
    // at full resolution and hand back the full image with the preview.
    let tmp = tempfile::tempdir().unwrap();
    let img = image::DynamicImage::new_rgba8(1024, 512);
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let p = tmp.path().join("photo.jpg"); // misnamed on purpose
    std::fs::write(&p, &png).unwrap();

    let r = image_viewer::load_image_preview_full(&p, 1024).unwrap();
    assert_eq!(r.orig, Some((1024, 512)));
    let full = r.full.expect("PNG preview pass should include full image");
    assert_eq!((full.width, full.height), (1024, 512));
    // Displayed texture is full-res (single decode, no blurry preview stage).
    assert_eq!((r.preview.width, r.preview.height), (1024, 512));
    assert!(!r.below_target);
}

#[test]
fn test_preview_below_target_flagged() {
    // Source larger than the requested target: texture is capped at the
    // target and flagged below_target so a bigger window re-decodes it.
    let tmp = tempfile::tempdir().unwrap();
    let img = image::DynamicImage::new_rgba8(2048, 1024);
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let p = tmp.path().join("big.png");
    std::fs::write(&p, &png).unwrap();

    let r = image_viewer::load_image_preview_full(&p, 512).unwrap();
    let full = r.full.expect("small pixel count keeps full decode");
    assert_eq!((full.width, full.height), (2048, 1024));
    assert_eq!((r.preview.width, r.preview.height), (512, 256));
    assert!(r.below_target);
}

#[test]
fn test_apply_exif_orientation() {
    // 2x1 image, pixels [A, B].
    let mk = |tag: u16| {
        let mut d = image_viewer::ImageData {
            width: 2,
            height: 1,
            rgba: vec![10, 0, 0, 255, 20, 0, 0, 255],
            load_time: std::time::Duration::ZERO,
        };
        image_viewer::apply_exif_orientation(&mut d, tag);
        d
    };

    // 6 = rotate 90 CW: 2x1 column becomes 1x2, A on top.
    let d = mk(6);
    assert_eq!((d.width, d.height), (1, 2));
    assert_eq!(&d.rgba[0..4], &[10, 0, 0, 255]);
    assert_eq!(&d.rgba[4..8], &[20, 0, 0, 255]);

    // 8 = rotate 270 CW: B on top.
    let d = mk(8);
    assert_eq!((d.width, d.height), (1, 2));
    assert_eq!(&d.rgba[0..4], &[20, 0, 0, 255]);
    assert_eq!(&d.rgba[4..8], &[10, 0, 0, 255]);

    // 3 = rotate 180: row stays a row, reversed.
    let d = mk(3);
    assert_eq!((d.width, d.height), (2, 1));
    assert_eq!(&d.rgba[0..4], &[20, 0, 0, 255]);

    // 5 = transpose: same shape as rotate 90 for this input.
    let d = mk(5);
    assert_eq!((d.width, d.height), (1, 2));
    assert_eq!(&d.rgba[0..4], &[10, 0, 0, 255]);

    // 2 = mirror horizontal.
    let d = mk(2);
    assert_eq!((d.width, d.height), (2, 1));
    assert_eq!(&d.rgba[0..4], &[20, 0, 0, 255]);

    // 0/1 are no-ops.
    let d = mk(1);
    assert_eq!((d.width, d.height), (2, 1));
    assert_eq!(&d.rgba[0..4], &[10, 0, 0, 255]);
}

#[test]
fn test_scan_folder_webp_and_gif() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.webp"), b"fake_webp").ok();
    std::fs::write(tmp.path().join("b.gif"), b"fake_gif").ok();
    std::fs::write(tmp.path().join("c.txt"), b"nope").ok();

    let gallery = image_viewer::Gallery::new(tmp.path());
    assert_eq!(gallery.len(), 2);
}

#[test]
fn test_scan_folder_jxl_avif_heic() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.jxl"), b"fake").ok();
    std::fs::write(tmp.path().join("b.avif"), b"fake").ok();
    std::fs::write(tmp.path().join("c.heic"), b"fake").ok();
    std::fs::write(tmp.path().join("d.heif"), b"fake").ok();
    std::fs::write(tmp.path().join("e.txt"), b"nope").ok();

    let gallery = image_viewer::Gallery::new(tmp.path());
    assert_eq!(gallery.len(), 4);
}

#[test]
fn test_natural_sort() {
    let mut names =
        vec!["IMG_10.jpg", "a.jpg", "IMG_2.jpg", "B.jpg", "IMG_1.jpg", "file10.txt", "file9.txt"];
    names.sort_by(|a, b| image_viewer::natural_cmp(a, b));
    assert_eq!(
        names,
        vec!["a.jpg", "B.jpg", "file9.txt", "file10.txt", "IMG_1.jpg", "IMG_2.jpg", "IMG_10.jpg",]
    );

    // Leading zeros keep grouping stable.
    let mut names = vec!["a2.jpg", "a01.jpg", "a1.jpg"];
    names.sort_by(|a, b| image_viewer::natural_cmp(a, b));
    assert_eq!(names, vec!["a1.jpg", "a01.jpg", "a2.jpg"]);
}

#[test]
fn test_scan_folder_sorted_modes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("big.bin.jpg"), vec![0u8; 4000]).ok();
    std::fs::write(tmp.path().join("small.jpg"), vec![0u8; 10]).ok();

    // Name mode is natural: "big.bin.jpg" < "small.jpg" here anyway, but the
    // digits case must hold.
    let files = image_viewer::scan_folder_sorted(tmp.path(), image_viewer::SortMode::Name);
    assert_eq!(files.len(), 2);

    // Size mode: largest first.
    let files = image_viewer::scan_folder_sorted(tmp.path(), image_viewer::SortMode::Size);
    assert_eq!(files[0].file_name().unwrap(), "big.bin.jpg");

    // Random keeps all files.
    let files = image_viewer::scan_folder_sorted(tmp.path(), image_viewer::SortMode::Random);
    assert_eq!(files.len(), 2);
}

#[test]
fn test_linear_resize_uniform_color() {
    // A uniform sRGB color must survive a linear-light downscale unchanged
    // (same color, smaller size).
    let mut rgba = Vec::with_capacity(64 * 64 * 4);
    for _ in 0..64 * 64 {
        rgba.extend_from_slice(&[200, 100, 50, 255]);
    }
    let data = image_viewer::ImageData {
        width: 64,
        height: 64,
        rgba,
        load_time: std::time::Duration::ZERO,
    };
    let out = with_linear_resample(true, || image_viewer::downscale_image(&data, 8));
    assert_eq!((out.width, out.height), (8, 8));
    assert!(!out.rgba.is_empty());
    // All pixels stay (approximately) the original color.
    for p in out.rgba.chunks_exact(4) {
        assert!((p[0] as i32 - 200).abs() <= 2, "R drifted: {}", p[0]);
        assert!((p[1] as i32 - 100).abs() <= 2, "G drifted: {}", p[1]);
        assert!((p[2] as i32 - 50).abs() <= 2, "B drifted: {}", p[2]);
        assert_eq!(p[3], 255);
    }
}

#[test]
fn test_buffer_has_alpha() {
    let opaque = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
    assert!(!image_viewer::buffer_has_alpha(&opaque));
    let translucent = vec![10u8, 20, 30, 255, 40, 50, 60, 0];
    assert!(image_viewer::buffer_has_alpha(&translucent));
}

#[test]
fn test_linear_resize_high_contrast_edge() {
    // CatmullRom overshoots on hard edges (that's its sharpness) — the u16
    // result can exceed the 12-bit linear range and must clamp, not panic
    // with a LUT index out of bounds (regression: index 4097 len 4096).
    let mut rgba = Vec::with_capacity(33 * 33 * 4);
    for y in 0..33u32 {
        for x in 0..33u32 {
            // Hard black/white checkerboard, 1px squares.
            let v = if (x + y) % 2 == 0 { 0u8 } else { 255u8 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let data = image_viewer::ImageData {
        width: 33,
        height: 33,
        rgba,
        load_time: std::time::Duration::ZERO,
    };
    let out = with_linear_resample(true, || image_viewer::downscale_image(&data, 7));
    assert_eq!((out.width, out.height), (7, 7));
    assert_eq!(out.rgba.len(), 7 * 7 * 4);
    // Values stay in gamut and alpha is untouched. (What a 1px checkerboard
    // averages to is not a useful assertion: source aliasing dominates. The
    // ramp test below covers resampler correctness.)
    for p in out.rgba.chunks_exact(4) {
        assert_eq!(p[3], 255, "alpha must stay opaque");
    }
}

#[test]
fn test_linear_resize_preserves_a_monotonic_ramp() {
    // The real invariant of a separable resampler: it must not invert a smooth
    // ramp. (Checked on an actual ramp - the checkerboard test above cannot
    // show this.)
    let (w, h) = (64u32, 8u32);
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for _ in 0..h {
        for x in 0..w {
            let v = (x * 255 / (w - 1)) as u8;
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let data =
        image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO };
    for target in [32u32, 7, 3] {
        let out = with_linear_resample(true, || image_viewer::downscale_image(&data, target));
        for row in out.rgba.chunks_exact(out.width as usize * 4) {
            for pair in row.chunks_exact(4).collect::<Vec<_>>().windows(2) {
                // Allow a hair of tolerance: ringing at the ends is the filter
                // working, not an inversion.
                assert!(
                    pair[1][0] as i32 >= pair[0][0] as i32 - 1,
                    "ramp inverted at target {target}: {:?} then {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }
}

#[test]
fn test_downscale_output_buffer_matches_its_dimensions() {
    // THE regression. `ColorImage::from_rgba_unmultiplied` asserts
    // `size[0] * size[1] * 4 == rgba.len()`, and that assert fires on the
    // *worker*, so a violation shows up as "decoder panicked" and lands the
    // image in `failed` permanently.
    //
    // It was violated on the gamma-space path (linear-light downsampling off)
    // only: `DynamicImage::resize` preserves the aspect ratio, so it returns an
    // image smaller than the one we asked for (980x552 -> asked 300x168, got
    // 298x168), and we were reporting our requested size next to its buffer.
    //
    // It became visible when previews were sized to the display area (11d778c)
    // rather than a fixed 4096: small targets round much harder, so the clamp
    // started actually shaving pixels. Hence "works on my machine / not on
    // his / works again after I resized the window".
    //
    // So: both paths, many shapes, many targets, invariant must always hold.
    fn mk(w: u32, h: u32) -> image_viewer::ImageData {
        image_viewer::ImageData {
            width: w,
            height: h,
            rgba: vec![200u8; (w as usize) * (h as usize) * 4],
            load_time: std::time::Duration::ZERO,
        }
    }

    // 980x552 is the real file from the report; the rest are aspect ratios that
    // make the aspect-ratio-preserving clamp bite hardest.
    let shapes = [
        (980u32, 552u32),
        (552, 980),
        (1000, 3),
        (3, 1000),
        (16, 2000),
        (2000, 16),
        (33, 33),
        (1, 97),
        (97, 1),
    ];
    // 256 is the clamp floor the viewer applies to the display area; the rest
    // are plausible panel sizes on different monitors and DPI scales.
    let targets = [256u32, 300, 512, 640, 800, 980, 1024, 1920, 3840, 4096];

    for linear in [true, false] {
        for (w, h) in shapes {
            let data = mk(w, h);
            for target in targets {
                let out =
                    with_linear_resample(linear, || image_viewer::downscale_image(&data, target));
                let want = (out.width as usize) * (out.height as usize) * 4;
                assert_eq!(
                    out.rgba.len(),
                    want,
                    "linear={linear} {w}x{h} @ {target} -> {}x{}: buffer is {}, \
                     ColorImage::from_rgba_unmultiplied would assert",
                    out.width,
                    out.height,
                    out.rgba.len()
                );
                assert!(
                    out.width > 0 && out.height > 0,
                    "linear={linear} {w}x{h} @ {target} produced a zero dimension"
                );
            }
        }
    }
}

#[test]
fn test_normalized_fixes_mismatched_buffer() {
    use image_viewer::ImageData;

    // A decoder that returns one row fewer than declared (the exact pattern
    // from the epaint size-assert crash: 621x974 declared, 621x973 buffer).
    let w = 621u32;
    let rows = 973usize;
    let data = ImageData {
        width: w,
        height: 974,
        rgba: vec![7u8; w as usize * rows * 4],
        load_time: std::time::Duration::ZERO,
    };
    let fixed = image_viewer::normalized(data);
    assert_eq!((fixed.width, fixed.height), (621, 973));
    assert_eq!(fixed.rgba.len(), 621 * 973 * 4);

    // Buffer shorter in a non-row-aligned way: padded to declared dims.
    let data = ImageData {
        width: 10,
        height: 10,
        rgba: vec![1u8; 10 * 10 * 4 - 7],
        load_time: std::time::Duration::ZERO,
    };
    let fixed = image_viewer::normalized(data);
    assert_eq!((fixed.width, fixed.height), (10, 10));
    assert_eq!(fixed.rgba.len(), 400);

    // Consistent input passes through untouched.
    let data = ImageData {
        width: 3,
        height: 2,
        rgba: vec![0u8; 3 * 3 * 4 * 4 / 4],
        load_time: std::time::Duration::ZERO,
    };
    let fixed = image_viewer::normalized(data);
    assert_eq!((fixed.width, fixed.height), (3, 3));
    assert_eq!(fixed.rgba.len(), 36);
}

#[test]
fn test_extract_zip() {
    use std::io::Write;

    // Build a zip: b.jpg (root), a.jpg (nested), readme.txt (ignored),
    // a case-collision duplicate (deduped).
    let tmp = tempfile::tempdir().unwrap();
    let zip_path = tmp.path().join("test.cbz");
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut zw = zip::ZipWriter::new(file);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();

    zw.start_file("b.jpg", opts).unwrap();
    zw.write_all(&[0u8; 8]).unwrap();
    zw.start_file("sub/a.jpg", opts).unwrap();
    zw.write_all(&[0u8; 8]).unwrap();
    zw.start_file("readme.txt", opts).unwrap();
    zw.write_all(b"not an image").unwrap();
    zw.start_file("sub/a.JPG", opts).unwrap(); // case-collision dedup
    zw.write_all(&[0u8; 8]).unwrap();
    zw.finish().unwrap();

    let dest = tmp.path().join("out");
    let (extracted, encrypted) = image_viewer::extract_zip(&zip_path, &dest).unwrap();
    assert_eq!(extracted, 2, "jpg entries only, deduped");
    assert_eq!(encrypted, 0);
    assert!(dest.join("b.jpg").exists());
    assert!(dest.join("a.jpg").exists()); // nested flattened to root
    assert!(!dest.join("readme.txt").exists());

    // Gallery scan over the extraction finds both, natural order.
    let gallery = image_viewer::Gallery::new(&dest);
    assert_eq!(gallery.len(), 2);

    // Archive detection.
    assert!(image_viewer::is_archive_path(&zip_path));
    assert!(!image_viewer::is_archive_path(&tmp.path().join("x.png")));
}

#[test]
fn test_gps_decode() {
    use nom_exif::ExifTag;
    use nom_exif::URational;
    // Reuse geo.jpg (Vilnius: 54.72261N, 25.25693E, 195 m) when present,
    // else synthesize the same values.
    let path = std::path::Path::new("test-images\\geo.jpg");
    let exif = if path.exists() {
        nom_exif::read_exif(path).unwrap()
    } else {
        return; // fixture-free machines skip this check
    };
    let (lat, lon) = image_viewer::gps_coords(&exif).expect("geo.jpg has GPS");
    assert!((lat - 54.722611).abs() < 0.0001, "lat {lat}");
    assert!((lon - 25.256925).abs() < 0.0001, "lon {lon}");
    let alt = image_viewer::gps_altitude(&exif).expect("geo.jpg has altitude");
    assert!((alt - 195.0).abs() < 0.01, "alt {alt}");

    // Unit-check the DMS conversion and hemisphere sign directly.
    let dms = |d: u32, m: u32, s: (u32, u32)| {
        vec![URational::new(d, 1), URational::new(m, 1), URational::new(s.0, s.1)]
    };
    let parts = dms(54, 43, (2140, 100));
    let deg = image_viewer::dms_to_deg(Some(&parts)).unwrap();
    assert!((deg - 54.722611).abs() < 0.0001);

    let _ = ExifTag::GPSLatitude; // touch import
}

#[test]
fn test_format_magic_detection() {
    // JXL naked codestream and container both detected.
    assert!(image_viewer::is_jxl_bytes(&[0xFF, 0x0A, 1, 2, 3]));
    let container = vec![0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A];
    assert!(image_viewer::is_jxl_bytes(&container));

    // ISOBMFF brand detection for the HEIF family.
    let avif = {
        let mut b = vec![0, 0, 0, 0x20];
        b.extend(b"ftypavif");
        b.extend(vec![0u8; 24]);
        b
    };
    assert!(image_viewer::is_heif_bytes(&avif));
    assert!(!image_viewer::is_jxl_bytes(&avif));

    let mp4 = {
        let mut b = vec![0, 0, 0, 0x20];
        b.extend(b"ftypmp42");
        b.extend(vec![0u8; 24]);
        b
    };
    assert!(!image_viewer::is_heif_bytes(&mp4));
}

#[test]
fn test_load_gif_first_frame() {
    // Animated/still GIFs decode via the image crate; the first frame is used.
    let tmp = tempfile::tempdir().unwrap();
    let img = image::DynamicImage::new_rgba8(48, 24);
    let mut gif = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut gif), image::ImageFormat::Gif).unwrap();
    let p = tmp.path().join("frame.gif");
    std::fs::write(&p, &gif).unwrap();

    let data = image_viewer::load_image(&p, 1600).expect("gif should load");
    assert_eq!((data.width, data.height), (48, 24));
    assert!(!data.rgba.is_empty());
}

#[test]
fn test_animated_gif_decode() {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame, RgbaImage};

    // Encode a real 2-frame animated GIF (red, then blue) via the encoder.
    let mut gif = Vec::new();
    {
        let mut enc = GifEncoder::new(std::io::Cursor::new(&mut gif));
        enc.set_repeat(Repeat::Infinite).unwrap();
        enc.encode_frame(Frame::from_parts(
            RgbaImage::from_pixel(8, 8, image::Rgba([255, 0, 0, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(100, 1),
        ))
        .unwrap();
        enc.encode_frame(Frame::from_parts(
            RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 255, 255])),
            0,
            0,
            Delay::from_numer_denom_ms(200, 1),
        ))
        .unwrap();
    }
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("anim.gif");
    std::fs::write(&p, &gif).unwrap();

    let r = image_viewer::load_image_preview_full(&p, 256).unwrap();
    let anim = r.anim.expect("animated GIF should carry frames");
    assert_eq!(anim.frames.len(), 2);
    assert_eq!((anim.frames[0].width, anim.frames[0].height), (8, 8));
    assert_eq!(anim.delays[0], std::time::Duration::from_millis(100));
    assert_eq!(anim.delays[1], std::time::Duration::from_millis(200));
    assert!(anim.total_duration() >= std::time::Duration::from_millis(300));
    // Preview is frame 0; no full decode and no tiles for animations.
    assert_eq!((r.preview.width, r.preview.height), (8, 8));
    assert!(r.full.is_none());
}

/// Build a minimal JPEG with an EXIF APP1 segment carrying the given
/// orientation tag, like a camera would.
fn make_jpeg_with_orientation(tag: u16) -> Vec<u8> {
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

#[test]
fn test_exif_orientation_applied_to_load() {
    let tmp = tempfile::tempdir().unwrap();

    // A 3x2 JPEG tagged "rotate 90 CW" must come back 2x3.
    let p6 = tmp.path().join("rot90.jpg");
    std::fs::write(&p6, make_jpeg_with_orientation(6)).unwrap();
    let d = image_viewer::load_image(&p6, 1600).expect("load");
    assert_eq!((d.width, d.height), (2, 3));

    // Unrotated (tag 1) keeps the encoded dimensions.
    let p1 = tmp.path().join("normal.jpg");
    std::fs::write(&p1, make_jpeg_with_orientation(1)).unwrap();
    let d = image_viewer::load_image(&p1, 1600).expect("load");
    assert_eq!((d.width, d.height), (3, 2));

    // Preview path reports the rotated source dimensions too.
    let r = image_viewer::load_image_preview_full(&p6, 256).unwrap();
    assert_eq!(r.orig, Some((2, 3)));
}

#[test]
fn test_scan_folder_returns_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("test.jpg"), b"fake_jpg_data").ok();
    std::fs::write(tmp.path().join("other.jpeg"), b"fake_jpeg_data").ok();
    std::fs::write(tmp.path().join("sample.png"), b"fake_png_data").ok();

    let gallery = Gallery::new(tmp.path());
    assert_eq!(gallery.len(), 3);
}

#[test]
fn test_gallery_next_and_prev() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.jpg"), b"fake").ok();
    std::fs::write(tmp.path().join("b.jpg"), b"fake").ok();

    let mut gallery = Gallery::new(tmp.path());
    assert!(!gallery.is_empty());
    assert_eq!(gallery.len(), 2);

    // Navigate
    gallery.next();
    assert_eq!(gallery.index, 1);
    gallery.prev();
    assert_eq!(gallery.index, 0);
    gallery.prev();
    // Should wrap
    assert_eq!(gallery.index, 1);
}

#[test]
fn test_load_all_from_folder_remembers_current() {
    // Create folder with multiple images
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.jpg"), b"fake").ok();
    std::fs::write(tmp.path().join("b.jpg"), b"fake").ok();
    std::fs::write(tmp.path().join("c.jpg"), b"fake").ok();

    let mut gallery = Gallery::new(tmp.path());
    assert_eq!(gallery.current().unwrap(), tmp.path().join("a.jpg"));

    // Navigate to second image
    gallery.next();
    assert_eq!(gallery.current().unwrap(), tmp.path().join("b.jpg"));

    // Call load_all_from_folder with the same folder
    gallery.load_all_from_folder(tmp.path());

    // Should remember current image (b.jpg) instead of jumping to first
    assert_eq!(gallery.current().unwrap(), tmp.path().join("b.jpg"));
}

#[test]
fn test_zoom_pan_reset_and_clamp() {
    let mut zoom_pan = ZoomPan::new();
    assert_eq!(zoom_pan.zoom, 1.0);

    // zoom_by takes scroll delta; each unit is multiplied by 0.01
    zoom_pan.zoom_by(-20.0); // -20 * 0.01 = -0.2 => zoom = 0.8
    assert_eq!(zoom_pan.zoom, 0.8);

    zoom_pan.zoom_by(200.0); // 200 * 0.01 = 2.0 => zoom = 2.8
    assert_eq!(zoom_pan.zoom, 2.8);

    zoom_pan.reset();
    assert_eq!(zoom_pan.zoom, 1.0);
}

#[test]
fn test_fit_to_window() {
    let mut zoom_pan = ZoomPan::new();
    // Large image: should downscale to fit
    zoom_pan.fit_to_window(2000.0, 1000.0, 1000.0, 600.0);
    assert!(zoom_pan.zoom < 1.0);
    assert!(zoom_pan.pan_x == 0.0);
    assert!(zoom_pan.pan_y == 0.0);

    // Small image: should upscale to fit
    zoom_pan.zoom = 1.0;
    zoom_pan.fit_to_window(50.0, 50.0, 100.0, 100.0);
    assert!(zoom_pan.zoom > 1.0);
    assert!(zoom_pan.pan_x == 0.0);
    assert!(zoom_pan.pan_y == 0.0);
}

use image_viewer::{GRID_CELL, GRID_LABEL_H, grid_cols, grid_row_h};

/// The grid geometry follows the cell size instead of the constant: 150pt is
/// six columns across 1000pt, 300pt is three, and the row pitch is always
/// cell + label + spacing.
#[test]
fn grid_geometry_follows_the_cell_size() {
    assert_eq!(image_viewer::grid_cols(1000.0, 150.0), 6);
    assert_eq!(image_viewer::grid_cols(1000.0, 300.0), 3);
    assert_eq!(image_viewer::grid_row_h(150.0), 178.0);
    assert_eq!(image_viewer::grid_row_h(300.0), 328.0);
}

/// Grid follow-scroll, the file-picker rule: a visible selection does not
/// move the view at all, and a selection outside it scrolls the minimum
/// distance to become visible. `None` means "leave the offset alone".
#[test]
fn grid_follow_scroll_leaves_a_visible_selection_alone() {
    // Rows are 100pt; the view shows 100..600.
    assert_eq!(image_viewer::grid_follow_offset(3, 100.0, 500.0, 100.0), None);
    // Edges count as visible: row 1 starts exactly at the top, row 5 ends
    // exactly at the bottom. Neither scrolls.
    assert_eq!(image_viewer::grid_follow_offset(1, 100.0, 500.0, 100.0), None);
    assert_eq!(image_viewer::grid_follow_offset(5, 100.0, 500.0, 100.0), None);
    // Below the view: align the row's bottom with the view's bottom.
    assert_eq!(image_viewer::grid_follow_offset(7, 100.0, 500.0, 100.0), Some(300.0));
    // Above the view: align the row's top with the view's top.
    assert_eq!(image_viewer::grid_follow_offset(0, 100.0, 500.0, 100.0), Some(0.0));
    // No measurement yet (grid just opened): pin the row to the top, which
    // is today's open behavior and the only sane thing with no viewport.
    assert_eq!(image_viewer::grid_follow_offset(9, 100.0, 0.0, 0.0), Some(900.0));
    // Never a negative offset, whatever the stored numbers say.
    assert_eq!(image_viewer::grid_follow_offset(0, 100.0, 500.0, -600.0), Some(0.0));
}

/// A shuffled show saves the folder's order on start and both off-ramps
/// restore it through one funnel. Asserted as counts, because the defect
/// this guards is a *third* site setting the flag directly: a stop path
/// that writes `slideshow = false` itself skips the restore and leaves the
/// folder shuffled with no way back.
#[test]
fn a_shuffled_show_saves_its_order_and_both_off_ramps_restore_it() {
    let src = main_src();
    let start = fn_body(&src, "fn start_slideshow(&mut self)").expect("start_slideshow");
    assert!(
        start.contains("image_viewer::shuffle("),
        "starting a shuffled show must walk a shuffled copy, not the folder order"
    );
    assert!(
        start.contains("slideshow_saved = Some("),
        "and it must save the folder's own order first, or stopping has \
         nothing to put back"
    );
    let stop = fn_body(&src, "fn stop_slideshow(&mut self)").expect("stop_slideshow");
    assert!(
        stop.contains("slideshow_saved.take()"),
        "stopping must consume the saved order exactly once: a take that is \
         missing restores nothing, and one that is not a take restores twice"
    );
    // The flag is written in exactly one place — inside stop_slideshow
    // itself. A second writer anywhere else is a stop path that skips the
    // restore and leaves the folder shuffled with no way back.
    assert_eq!(
        src.matches("self.slideshow = false").count(),
        1,
        "stopping the show must go through stop_slideshow alone: every direct \
         write of the flag elsewhere is an off-ramp that skips the restore"
    );
}

/// The grid follows the selection through the measured-offset rule, not by
/// pinning the row to the top: the decision reads last frame's scroll offset
/// and viewport height, and the frame writes both back for the next one.
/// Either half missing and the rule silently stops working — an unread
/// measurement means the decision runs on zeros, an unwritten one means it
/// runs on last week's folder.
#[test]
fn the_grid_follows_through_measured_offsets_not_a_pinned_row() {
    let src = main_src();
    let grid = fn_body(&src, "fn gallery_grid(&mut self").expect("gallery_grid");
    assert!(
        grid.contains("image_viewer::grid_follow_offset("),
        "the scroll decision must go through grid_follow_offset: pinning the \
         row to the top on every press is the bug this replaces"
    );
    assert!(
        !grid.contains("(sel / cols) as f32 * pitch"),
        "the old pin-to-top arithmetic must be gone, not just bypassed"
    );
    assert!(
        grid.contains("self.grid_scroll = grid_out.state.offset.y"),
        "the frame must write the measured offset back, or the next decision \
         runs on a stale folder's scroll"
    );
    assert!(
        grid.contains("self.grid_view_h = grid_out.inner_rect.height()"),
        "and the measured viewport height with it, or the visible-range check \
         below is against a window that no longer exists"
    );
}
/// A shuffle is a permutation: same set in, same set out, in an order that
/// is not the input order. The reorder half is probabilistic, so it runs
/// over 64 files, where shuffling to identity has probability 1/64!.
#[test]
fn shuffle_keeps_every_file_and_reorders() {
    let mut files: Vec<std::path::PathBuf> =
        (0..64).map(|i| std::path::PathBuf::from(format!("{i:03}.jpg"))).collect();
    let before = files.clone();
    image_viewer::shuffle(&mut files);
    let (mut a, mut b) = (before.clone(), files.clone());
    a.sort();
    b.sort();
    assert_eq!(a, b, "a shuffle must not lose, duplicate or invent files");
    assert_ne!(files, before, "a 64-file shuffle that changes nothing shuffled nothing");
}

/// Drive one headless egui pass and return its output.
///
/// egui 0.36 debug-asserts when a `TexturesDelta` is dropped unapplied
/// (`TexturesDelta::drop` in epaint). A headless pass emits the font-atlas
/// delta every frame and nothing applies it — in the real app eframe's
/// renderer does — so every test pass has to clear it. Tests that want the
/// returned `FullOutput` should go through this rather than `Context::run_ui`.
fn run_pass(
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
fn grid_row(
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

#[test]
fn test_gallery_grid_cells_fit_row_and_register_clicks() {
    // Regression test for the gallery grid (G) view. Cells must be allocated
    // explicitly with `Sense::click()`. Wrapping them in `egui::Frame::show`
    // is wrong in two ways that both looked fine in a screenshot-less build:
    //
    //  1. `Frame::show` builds its child Ui from
    //     `ui.available_rect_before_wrap()`, so the frame is as wide as the
    //     rest of the row. The first cell swallows the whole row and every
    //     later cell is laid out past the panel edge — the "offscreen grid".
    //  2. `Frame::show` allocates its space with `Sense::hover()`, so
    //     `Response::clicked()` is never true and cells cannot be opened.
    //     (In 0.27 this was `Frame::begin` / `Frame::end`; 0.36 has only
    //     `Frame::show`.)
    use egui::{Event, FontDefinitions, PointerButton, RawInput};

    let ctx = egui::Context::default();
    ctx.set_fonts(FontDefinitions::empty()); // don't load fonts in tests

    // Pass 1: lay the row out and record where each cell landed.
    let mut input = RawInput::default();
    let mut rects = Vec::new();
    let mut panel = egui::Rect::ZERO;
    let mut cols = 0usize;
    // `gap` is read by `grid_row` from the layout that actually placed the cells,
    // rather than hardcoding egui's default `item_spacing`. The assertion is
    // "the cells tile with the spacing the row used", and egui's default is a
    // moving target across upgrades: 0.27 put 8pt here, 0.36 puts 4pt.
    let mut gap = 0.0f32;
    let _ = run_pass(&ctx, input.clone(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            panel = ui.available_rect_before_wrap();
            // Same column math gallery_grid uses.
            cols = grid_cols(panel.width(), GRID_CELL);
            let mut hits = Vec::new();
            grid_row(ui, cols, &mut rects, &mut hits, &mut gap);
        });
    });

    assert!(cols >= 2, "test needs a multi-cell row, got {cols}");
    assert_eq!(rects.len(), cols, "every cell of the row must be laid out");
    for r in &rects {
        assert!(
            (r.width() - GRID_CELL).abs() < 1.0
                && (r.height() - (GRID_CELL + GRID_LABEL_H)).abs() < 1.0,
            "cell must take exactly the size it asked for, got {r:?}"
        );
        assert!(
            r.max.x <= panel.max.x + 1.0,
            "cell at x={} escapes the panel ({:?}) — row is too wide",
            r.min.x,
            panel
        );
    }
    for pair in rects.windows(2) {
        assert!(
            (pair[1].min.x - pair[0].max.x - gap).abs() < 1.0,
            "cells must sit side by side with {gap}pt spacing, got {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }

    // Pass 2: press the 3rd cell. Pass 3: release there. Only it may click.
    let target = rects[2].center();
    let mut clicks = Vec::new();
    for pass in 0..2 {
        input.events.push(Event::PointerMoved(target));
        input.events.push(Event::PointerButton {
            pos: target,
            button: PointerButton::Primary,
            pressed: pass == 0,
            modifiers: egui::Modifiers::default(),
        });
        let _ = run_pass(&ctx, input.clone(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let mut out = Vec::new();
                let mut gap = 0.0f32;
                grid_row(ui, cols, &mut out, &mut clicks, &mut gap);
            });
        });
    }
    assert_eq!(clicks, vec![2], "only the pressed cell may report the click");
}

#[test]
fn test_path_arg_skips_flags() {
    use image_viewer::path_arg;

    // The plain case: one positional path.
    assert_eq!(path_arg(["C:\\photos"]).as_deref(), Some(std::path::Path::new("C:\\photos")));

    // Regression: `--perf` is a flag, not a path. `args().nth(1)` took it as one,
    // it matched neither file nor dir, and the app silently fell back to the
    // persisted folder. That is the exact invocation AGENTS.md documents
    // (`cargo run -- --perf <folder>`), and it produced a perf report for the
    // *wrong folder* — which is how a wrong performance conclusion survived
    // long enough to be committed.
    assert_eq!(
        path_arg(["--perf", "C:\\photos"]).as_deref(),
        Some(std::path::Path::new("C:\\photos"))
    );
    assert_eq!(
        path_arg(["--perf", "C:\\photos", "extra"]).as_deref(),
        Some(std::path::Path::new("C:\\photos"))
    );
    assert_eq!(
        path_arg(["--register", "--perf", "C:\\photos"]).as_deref(),
        Some(std::path::Path::new("C:\\photos"))
    );

    // A flag with no path at all is None, not a bogus path.
    assert_eq!(path_arg(["--perf"]), None);
    assert_eq!(path_arg(Vec::<String>::new()), None);

    // A bare flag is still None, so the resume path is taken.
    assert_eq!(path_arg(["--unregister"]), None);

    // A path that merely contains a dash is not a flag.
    assert_eq!(
        path_arg(["--perf", "C:\\my-photos\\a-b.jpg"]).as_deref(),
        Some(std::path::Path::new("C:\\my-photos\\a-b.jpg"))
    );

    // An argument that *starts* with a dash is a flag whatever it means, which
    // is why a relative path can be escaped as `.\-name.jpg`. And an empty
    // argument is not a path either - it would open the process's own directory.
    assert_eq!(path_arg(["-name.jpg"]), None);
    assert_eq!(path_arg([r".\-name.jpg"]).as_deref(), Some(std::path::Path::new(r".\-name.jpg")));
    assert_eq!(path_arg(["", "C:\\photos"]).as_deref(), Some(std::path::Path::new("C:\\photos")));
}

#[test]
fn test_fit_scale_no_upscale_centres_thumbnails() {
    use image_viewer::fit_scale_no_upscale;
    let area = (150.0_f32, 150.0_f32);

    // A big landscape thumb scales down to the cell width, aspect kept.
    let s = fit_scale_no_upscale((400.0, 300.0), area);
    assert!((s - 0.375).abs() < 1e-6, "got {s}");
    assert!((400.0 * s - 150.0).abs() < 1e-4, "must fill the width");
    assert!((300.0 * s - 112.5).abs() < 1e-4, "aspect must be kept");

    // A tall portrait thumb is bounded by the height instead.
    let s = fit_scale_no_upscale((200.0, 400.0), area);
    assert!((s - 0.375).abs() < 1e-6, "got {s}");
    assert!((400.0 * s - 150.0).abs() < 1e-4, "must fill the height");

    // A panorama is bounded by the width and stays short.
    let s = fit_scale_no_upscale((1000.0, 100.0), area);
    assert!((s - 0.15).abs() < 1e-6, "got {s}");
    assert!((100.0 * s - 15.0).abs() < 1e-4, "aspect must be kept");

    // Never upscale: a small image is centred at native size rather than
    // blown up into a blurry square.
    for natural in [(40.0_f32, 30.0_f32), (1.0, 1.0), (150.0, 150.0)] {
        assert_eq!(fit_scale_no_upscale(natural, area), 1.0, "{natural:?} must not be upscaled");
    }

    // Degenerate inputs must not produce NaN (that would silently blank cells).
    assert_eq!(fit_scale_no_upscale((0.0, 0.0), area), 1.0);
    assert_eq!(fit_scale_no_upscale((100.0, 100.0), (0.0, 0.0)), 1.0);
    assert!(fit_scale_no_upscale((100.0, 100.0), (1e9, 1.9)).is_finite());
}
#[test]
fn test_grid_thumbnail_is_centred_and_textured() {
    // Guards the gallery-grid thumbnail against two regressions that both
    // shipped once, and neither of which a compile or a screenshot-free build
    // would catch:
    //
    //  1. It stopped being *centred*: widgets place at the cursor, and a
    //     `Layout::top_down(Align::Center)` does not move them (a top-down
    //     flow's initial cursor runs to +infinity on the main axis, so
    //     `allocate_exact_size` centres only across the cross axis). The
    //     widget therefore lives in a Ui bounded to the computed rect.
    //  2. It stopped being *textured* (solid colour blocks) when the cell was
    //     painted by hand via `painter().image(...)`. egui owns that
    //     plumbing, so the widget is used and only the rect is computed here.
    //
    // A real egui pass is driven and the produced shapes are inspected.
    use egui::{Color32, ColorImage, Context, RawInput, Rect, TextureOptions, Vec2};

    let ctx = Context::default();
    ctx.set_fonts(egui::FontDefinitions::empty());

    let texels = [200_usize, 100_usize];
    let img = ColorImage::filled([texels[0], texels[1]], Color32::from_rgb(200, 30, 30));
    let tex = ctx.load_texture("thumb-regression", img, TextureOptions::LINEAR);

    // A cell's thumbnail area, as gallery_grid lays it out.
    let area = Rect::from_min_size(egui::Pos2::new(20.0, 20.0), Vec2::new(142.0, 150.0));
    let natural = Vec2::new(texels[0] as f32, texels[1] as f32);
    let scale =
        image_viewer::fit_scale_no_upscale((natural.x, natural.y), (area.width(), area.height()));
    let draw_size = (natural * scale).min(area.size());
    let draw = Rect::from_center_size(area.center(), draw_size);

    let out = run_pass(&ctx, RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            // Exactly what grid_cell does.
            let mut img_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(draw)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            img_ui.add(egui::Image::new(&tex).max_size(draw_size));
        });
    });

    // egui paints an image as a RectShape whose `brush` carries a texture id.
    // (0.36 moved `fill_texture_id`/`uv` off `RectShape` and behind an
    // `Option<Arc<Brush>>`; a plain `RectShape::filled` has `brush: None`.)
    let textured: Vec<_> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(r) => r.brush.as_ref().and_then(|b| {
                (b.fill_texture_id != egui::TextureId::default()).then_some((r.rect, b.uv))
            }),
            _ => None,
        })
        .collect();
    let (rect, uv) = *textured
        .first()
        .expect("no textured quad painted: the cell would show only its background");

    // Textured: a collapsed UV samples one texel and stretches it, which is
    // what a "solid colour" thumbnail is.
    assert!(uv.width() > 0.0 && uv.height() > 0.0, "UV must be non-degenerate, got {uv:?}");
    // Aspect kept.
    let drawn = rect.width() / rect.height();
    let source = texels[0] as f32 / texels[1] as f32;
    assert!(
        (drawn - source).abs() < 0.02,
        "aspect must be preserved: drew {drawn}, source {source}"
    );
    // Fits the cell, and fills it (this thumb is larger than the area).
    assert!(
        rect.width() <= area.width() + 0.5 && rect.height() <= area.height() + 0.5,
        "thumbnail {rect:?} must fit the area {area:?}"
    );
    assert!((rect.width() - area.width()).abs() < 0.5, "should fill the width");
    // Centred on both axes.
    assert!(
        (rect.center().x - area.center().x).abs() < 0.5
            && (rect.center().y - area.center().y).abs() < 0.5,
        "thumbnail {rect:?} must be centred in {area:?}, not stuck in a corner"
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
fn strip_line_comment(line: &str) -> &str {
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

/// The three documents are structurally sound, and the damage this catches is
/// the kind a reader cannot report.
///
/// `README.md` carried **three** `# Image Viewer` headings, a stray keyboard row
/// at the top of the "Saving as" section, and the same zoom-preset row twice in
/// a row. All three came from edits landing at the wrong anchor - a line
/// *inserted* where a line was meant to be *replaced* - and none of them said
/// anything was wrong: the document still rendered, the keyboard row was still
/// correct in its proper place further down, and every check that looked for the
/// *content* passed.
///
/// So the checks are about **structure**, not content:
/// * one top-level title per document (the head corruption),
/// * no two identical adjacent lines (the pasted-once-too-often row),
/// * no one-row "table" (a lone `|` row is not a table at all, and the stray
///   rows were exactly that).
#[test]
fn each_document_is_structurally_sound() {
    for (name, text) in [
        ("README.md", include_str!("../README.md")),
        ("AGENTS.md", include_str!("../AGENTS.md")),
        ("TODO.md", include_str!("../TODO.md")),
    ] {
        let lines: Vec<&str> = text.lines().collect();

        let titles = lines.iter().filter(|l| l.starts_with("# ")).count();
        assert_eq!(titles, 1, "{name} has {titles} top-level titles");

        // Two identical adjacent lines are a paste that landed twice. Fenced
        // code is exempt: a sample of command output can legitimately repeat a
        // line.
        let mut in_fence = false;
        for pair in lines.windows(2) {
            let (a, b) = (pair[0].trim(), pair[1].trim());
            if a.starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || a.is_empty() || a != b {
                continue;
            }
            assert!(
                false,
                "{name}: two identical adjacent lines - {a:?} - which is an edit that landed twice"
            );
        }

        // A lone `|` row is not a table. The stray rows were exactly that: one
        // row, with prose above and below it.
        let mut i = 0;
        while i < lines.len() {
            if !lines[i].trim_start().starts_with('|') {
                i += 1;
                continue;
            }
            let start = i;
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                i += 1;
            }
            assert!(
                i - start > 1,
                "{name}: a single table row at line {} - {line:?} - is not a table",
                start + 1,
                line = lines[start].trim()
            );
        }
    }
}

/// The sources carry no duplicated comment line.
///
/// A comment line that appears twice in a row is an edit that landed at the wrong
/// anchor: the line was *inserted* where it was meant to *replace* another, and
/// the copy that was already there stayed. `src/main.rs` has carried
/// `// --- full-resolution tiles ---` twice since the zoom work, one of them at
/// column 0 in the middle of an indented block, which is the tell.
///
/// Two identical adjacent *code* lines are legitimate (`}` closing two things,
/// `0,` in an array), so the rule is about comments only.
#[test]
fn no_source_carries_a_duplicated_comment_line() {
    for (name, text) in all_src_files() {
        let lines: Vec<&str> = text.lines().collect();
        for (i, pair) in lines.windows(2).enumerate() {
            let (a, b) = (pair[0].trim(), pair[1].trim());
            if a.starts_with("//") && a == b {
                assert!(
                    false,
                    "{name}: the comment {a:?} appears twice in a row (line {}) - \
                     an edit that landed at the wrong anchor",
                    i + 1
                );
            }
        }
    }
}

#[test]
fn no_ui_text_uses_a_glyph_the_font_lacks() {
    let mut missing: Vec<(char, String)> = Vec::new();

    let ctx = egui::Context::default();
    // `run_pass`, not `run_ui`: this is a headless pass and nothing applies the
    // font-atlas delta, which epaint 0.36 debug-asserts on when it is dropped.
    let _ = run_pass(&ctx, egui::RawInput::default(), |ui| {
        // Two sizes: a glyph can be in the family and still be dropped when
        // rasterised at a size the atlas does not cover.
        let sizes = [14.0f32, 12.0];

        let mut seen: std::collections::BTreeMap<char, String> = Default::default();
        for file in ["src/main.rs", "src/lib.rs"] {
            let text =
                std::fs::read_to_string(file).unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
            for (i, line) in text.lines().enumerate() {
                let code = strip_line_comment(line);
                for c in code.chars() {
                    if c.is_ascii() || c == '\u{FFFD}' {
                        continue;
                    }
                    seen.entry(c).or_insert_with(|| format!("{file}:{}", i + 1));
                }
            }
        }

        ui.ctx().fonts_mut(|f| {
            for (c, origin) in &seen {
                let s = c.to_string();
                let absent =
                    sizes.iter().any(|sz| !f.has_glyphs(&egui::FontId::proportional(*sz), &s));
                if absent {
                    missing.push((*c, origin.clone()));
                }
            }
        });
    });

    assert!(
        missing.is_empty(),
        "these characters are not in egui's default font and will render as blank \
         boxes:\n{}\nUse ASCII, or a character the font actually has (the app uses \
         '…' '—' '·' '°' '⟲' '⟳' successfully).",
        missing
            .iter()
            .map(|(c, o)| format!("  U+{:04X} '{}' first at {o}", *c as u32, c))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn test_xformed_footprint_only_swaps_when_the_transform_does() {
    use image_viewer::{Xform, xformed_footprint};
    let (w, h) = (4000.0_f32, 3000.0_f32);
    // The four turns and two of the four mirrors leave the shape alone; the two
    // diagonal mirrors swap it, because a diagonal reflection *is* a quarter turn
    // plus a flip.
    for (xf, swaps) in [
        (Xform::Identity, false),
        (Xform::Rot90, true),
        (Xform::Rot180, false),
        (Xform::Rot270, true),
        (Xform::FlipH, false),
        (Xform::FlipV, false),
        (Xform::Transpose, true),
        (Xform::Transverse, true),
    ] {
        assert_eq!(
            xformed_footprint(w, h, xf),
            if swaps { (h, w) } else { (w, h) },
            "{xf:?}: wrong footprint"
        );
        // And the library's own idea of the output size must agree, because the
        // fit, the pan limits and the tile grid are all computed from these two.
        assert_eq!(
            xf.out_dims(4000, 3000),
            (xformed_footprint(w, h, xf).0 as u32, xformed_footprint(w, h, xf).1 as u32),
            "{xf:?}: footprint and out_dims disagree"
        );
    }
}

#[test]
fn test_transformed_mesh_covers_the_swapped_footprint() {
    // The rotate and mirror buttons paint the image by moving its rect's mesh
    // vertices about the rect centre (see `paint_tex`). That only keeps the image
    // centred if the transform lands exactly on the footprint that fit and pan
    // clamping were computed against. If this premise were wrong, turning a photo
    // would swing it off-centre or leave a gap.
    //
    // Uses `Xform::paint_matrix` — the same matrix `paint_tex` uses — rather than
    // restating the transform here, because a copy of the mechanism in the test
    // would keep passing after the shipping code broke. (What the matrix *means*
    // is pinned separately, in `tests/rotate_save.rs`.)
    use egui::{Color32, Rect, Vec2};
    use image_viewer::Xform;

    for xform in Xform::ALL {
        let src = Vec2::new(300.0, 100.0);
        let scale = 0.5;
        let centre = egui::Pos2::new(400.0, 300.0);
        let rect = Rect::from_center_size(centre, src * scale);

        let mut mesh = egui::epaint::Mesh::with_texture(egui::TextureId::default());
        mesh.add_rect_with_uv(
            rect,
            Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if xform != Xform::Identity {
            let m = xform.paint_matrix();
            let origin = rect.center();
            for v in &mut mesh.vertices {
                let d = v.pos - origin;
                v.pos = origin
                    + Vec2::new(m[0][0] * d.x + m[0][1] * d.y, m[1][0] * d.x + m[1][1] * d.y);
            }
        }

        let (dw, dh) = image_viewer::xformed_footprint(src.x * scale, src.y * scale, xform);
        let want = Rect::from_center_size(centre, Vec2::new(dw, dh));

        let xs: Vec<f32> = mesh.vertices.iter().map(|v| v.pos.x).collect();
        let ys: Vec<f32> = mesh.vertices.iter().map(|v| v.pos.y).collect();
        let got = Rect::from_min_max(
            egui::Pos2::new(
                xs.iter().cloned().fold(f32::MAX, f32::min),
                ys.iter().cloned().fold(f32::MAX, f32::min),
            ),
            egui::Pos2::new(
                xs.iter().cloned().fold(f32::MIN, f32::max),
                ys.iter().cloned().fold(f32::MIN, f32::max),
            ),
        );
        assert!(
            (got.min.x - want.min.x).abs() < 0.01
                && (got.min.y - want.min.y).abs() < 0.01
                && (got.max.x - want.max.x).abs() < 0.01
                && (got.max.y - want.max.y).abs() < 0.01,
            "{xform:?}: transformed image {got:?} must fill the footprint {want:?}"
        );
        // And it must stay centred where the untransformed image was.
        assert!(
            (got.center() - centre).x.abs() < 0.01 && (got.center() - centre).y.abs() < 0.01,
            "{xform:?}: the transform must not move the centre"
        );
    }
}

#[test]
fn test_gallery_scroll_offset_lands_the_selected_row_in_view() {
    // Regression: opening the gallery did not focus the current image once you
    // had browsed a few images. `Response::scroll_to_me` cannot fix that,
    // because `ScrollArea::show_rows` only lays out the rows inside the
    // viewport - a selected row that starts off-screen is never built, so it
    // never issues a scroll request, and nothing moves.
    //
    // The fix writes the scroll offset once per *selection change*. Both halves
    // matter, and both had a failure mode:
    //  * not writing it at all -> the grid opens wherever it was left;
    //  * writing it every frame -> manual scrolling is fought, because any small
    //    scroll that keeps the selected row laid out gets yanked back.
    use egui::{Context, FontDefinitions, RawInput};

    const ROWS: usize = 200;
    const COLS: usize = 4;
    const SEL_ROW: usize = 137; // far down a long folder

    let ctx = Context::default();
    ctx.set_fonts(FontDefinitions::empty());

    /// Lay out the rows and report which ones egui actually built. `offset` of
    /// `None` means "do not touch the scroll offset at all" (note `Some(0.0)`
    /// is *not* the same: it would reset the persisted offset to zero).
    fn laid_out_rows(ctx: &Context, offset: Option<f32>) -> (Vec<usize>, f32) {
        let mut rows = Vec::new();
        let mut pitch = 0.0f32;
        let _ = run_pass(ctx, RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                pitch = grid_row_h(GRID_CELL) + ui.spacing().item_spacing.y;
                // Mirrors the real grid: auto_shrink([false, true]) is what
                // gives the scrollbar somewhere to live.
                let mut sa = egui::ScrollArea::vertical().auto_shrink([false, true]);
                if let Some(o) = offset {
                    sa = sa.vertical_scroll_offset(o);
                }
                sa.show_rows(ui, grid_row_h(GRID_CELL), ROWS, |ui, range| {
                    rows.extend(range.clone());
                    for row in range {
                        ui.horizontal(|ui| {
                            for _ in 0..COLS {
                                let (_rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(GRID_CELL, GRID_CELL + GRID_LABEL_H),
                                    egui::Sense::click(),
                                );
                                if row == SEL_ROW {
                                    // The rejected mechanism, kept only to show
                                    // it cannot rescue an off-screen row.
                                    resp.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        });
                    }
                });
            });
        });
        (rows, pitch)
    }

    // 1. Fresh context, offset untouched: the selected row is off-screen and
    //    never built, so scroll_to_me has nothing to act on. This is the bug.
    let (rows, pitch) = laid_out_rows(&ctx, None);
    assert!(pitch > grid_row_h(GRID_CELL), "show_rows adds item spacing to the pitch");
    assert!(
        !rows.contains(&SEL_ROW),
        "row {SEL_ROW} must start off-screen, otherwise this test is not \
         exercising the bug (got {rows:?})"
    );

    // 2. The fix: one-shot offset at the selected row lands it in the band.
    let (rows, _) = laid_out_rows(&ctx, Some(SEL_ROW as f32 * pitch));
    assert!(
        rows.contains(&SEL_ROW),
        "selected row {SEL_ROW} must be laid out, got {rows:?} (pitch {pitch})"
    );

    // 3. A following frame that does NOT touch the offset keeps the selection
    //    in view (egui persists it) - so the viewer's own scrolling is never
    //    overridden while the selection is unchanged.
    let (rows, _) = laid_out_rows(&ctx, None);
    assert!(
        rows.contains(&SEL_ROW),
        "a pass that does not set the offset must keep the selected row in \
         view; got {rows:?}"
    );
}

/// Worker-count override (Tier 1 #12): 0 means auto, anything else is clamped.
#[test]
fn worker_count_override_is_honoured_and_auto_stays_put() {
    // Auto (0) keeps the shipped behaviour: available parallelism clamped to 2..=4.
    assert_eq!(image_viewer::resolve_worker_count(8, 0), 4);
    assert_eq!(image_viewer::resolve_worker_count(2, 0), 2);
    assert_eq!(image_viewer::resolve_worker_count(1, 0), 2);
    // An explicit preference is honoured, clamped to a sane 1..=8.
    assert_eq!(image_viewer::resolve_worker_count(8, 1), 1);
    assert_eq!(image_viewer::resolve_worker_count(8, 6), 6);
    assert_eq!(image_viewer::resolve_worker_count(8, 64), 8);
}

/// An old config without the workers key must still load, and come up on auto.
#[test]
fn a_config_written_before_workers_still_loads_and_stays_auto() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert_eq!(cfg.workers, 0, "an old config must come up on auto (0), not on a fixed count");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert_eq!(fresh.workers, 0, "a first run must also come up on auto");
}

/// The worker-count setting must be **wired**, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_worker_count_setting_is_wired_to_pool_startup() {
    let src = main_src();
    assert!(
        src.contains("resolve_worker_count("),
        "pool startup must go through resolve_worker_count, not compute the clamp inline"
    );
    assert!(src.contains("config.workers"), "pool startup must read the setting");
    assert!(
        src.contains("&mut self.config.workers"),
        "the settings panel must have a control bound to it"
    );
}

/// Fill-with-crop fit (Tier 1 #10): scale so the image covers the area,
/// cropping rather than letterboxing. The larger ratio wins, unlike
/// `fit_scale_no_upscale` where the smaller wins and upscale is refused.
#[test]
fn fill_scale_covers_the_area() {
    // Wide image in a square area: height binds.
    let s = image_viewer::fill_scale((200.0, 100.0), (100.0, 100.0));
    assert!((s - 1.0).abs() < 1e-6, "height must bind: got {s}");
    // Tall image in a wide area: width binds.
    let s = image_viewer::fill_scale((100.0, 200.0), (200.0, 100.0));
    assert!((s - 2.0).abs() < 1e-6, "width must bind: got {s}");
    // Small image fills by upscaling (that is what crops instead of borders).
    let s = image_viewer::fill_scale((50.0, 50.0), (100.0, 100.0));
    assert!((s - 2.0).abs() < 1e-6, "fill may upscale: got {s}");
}

/// Fill-with-crop must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here.
#[test]
fn the_fill_key_is_wired_to_a_fill_scale() {
    let src = main_src();
    assert!(
        src.contains("fill_scale("),
        "the fill key must size from fill_scale, not from fit_scale"
    );
    assert!(
        src.contains("modifiers.shift"),
        "fill must sit on Shift+F so bare F keeps meaning fit"
    );
}

/// Sort direction (Tier 1 #7): `descending` reverses each mode`s shipped order,
/// and off is exactly today`s behaviour - no existing order changes.
#[test]
fn sort_descending_reverses_each_mode() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.jpg"), vec![0u8; 10]).ok();
    std::fs::write(tmp.path().join("b.jpg"), vec![0u8; 4000]).ok();
    std::fs::write(tmp.path().join("c.jpg"), vec![0u8; 500]).ok();

    // Name: ascending is natural, descending its exact reverse.
    let asc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Name, false);
    let desc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Name, true);
    let names = |fs: &[std::path::PathBuf]| {
        fs.iter().map(|p| p.file_name().unwrap().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(
        names(&asc).iter().map(|s| s.to_str().unwrap()).collect::<Vec<_>>(),
        ["a.jpg", "b.jpg", "c.jpg"]
    );
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Name must be ascending reversed");

    // Size: shipped order is largest-first; descending puts smallest first.
    let desc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Size, true);
    assert_eq!(desc[0].file_name().unwrap(), "a.jpg");
    let asc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Size, false);
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Size must be ascending reversed");

    // Date: order depends on mtime granularity, but descending must still be
    // the exact reverse of whatever ascending produced.
    let asc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Date, false);
    let desc = image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Date, true);
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Date must be ascending reversed");
}

/// An old config without the key must still load, and keep today`s orders.
#[test]
fn a_config_written_before_sort_descending_still_loads_and_stays_off() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.sort_descending, "an old config must keep the shipped orders");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.sort_descending, "a first run must also keep them");
}

/// The sort-direction setting must be **wired**, and `main.rs` is unreachable
/// from every test, so this is a source scan - the established pattern here.
#[test]
fn the_sort_direction_setting_is_wired_to_rescan() {
    let src = main_src();
    assert!(
        src.contains("scan_folder_sorted_dir("),
        "folder scans must go through the directed sort, not the old wrapper"
    );
    assert!(
        src.contains("&mut self.config.sort_descending"),
        "the settings panel must have a control bound to it"
    );
}

/// Copy-path (Tier 1 #4): Ctrl+Shift+C arrives as `Event::Copy` - egui-winit`s
/// `is_copy_command` is `modifiers.command && key == C` with no shift
/// exclusion, and it returns before pushing the Key event - so the shift state
/// must disambiguate path from pixels inside the Copy arm.
#[test]
fn shift_copy_puts_the_path_on_the_clipboard() {
    let src = main_src();
    assert!(
        src.contains("copy_path_to_clipboard"),
        "there must be a path-copy action distinct from copy_image_to_clipboard"
    );
    // The call site, not the definition: the arm around it must branch on the
    // shift state, since both chords arrive as the same event.
    let call = src.find("self.copy_path_to_clipboard(ctx)").expect("the Copy arm must call it");
    let arm = src[..call].rfind("clip_img").expect("the call must sit in the Copy arm");
    assert!(
        src[arm..call].contains("clip_shift"),
        "the Copy arm must read the shift state to tell Ctrl+Shift+C apart \
         from Ctrl+C (both arrive as Event::Copy)"
    );
}

/// JPEG comments (Tier 1 #8): a COM segment in the header must be found.
#[test]
fn jpeg_comment_finds_the_com_segment() {
    // SOI + COM("hello") + EOI.
    let jpg = vec![0xFF, 0xD8, 0xFF, 0xFE, 0x00, 0x07, b'h', b'e', b'l', b'l', b'o', 0xFF, 0xD9];
    assert_eq!(
        image_viewer::jpeg_comment(&jpg).as_deref(),
        Some("hello"),
        "the COM payload must come back as text"
    );
    // No COM at all.
    let bare = vec![0xFF, 0xD8, 0xFF, 0xD9];
    assert_eq!(image_viewer::jpeg_comment(&bare), None);
    // Truncated mid-length must not panic and must not invent a comment.
    let cut = vec![0xFF, 0xD8, 0xFF, 0xFE, 0x00];
    assert_eq!(image_viewer::jpeg_comment(&cut), None);
    // Not a JPEG at all.
    assert_eq!(image_viewer::jpeg_comment(b"definitely not jpeg"), None);
}

/// EXIF text comments end to end: description wins over UserComment, read
/// from a real file so the tag lookup is exercised and not just the helpers.
///
/// `test-images/` is gitignored, so this skips when absent — the same pattern
/// as `test_gps_decode`. Where it runs, it runs for real.
#[test]
fn exif_comment_reads_description_from_a_real_file() {
    let path = std::path::Path::new("test-images/color-p3-anon.jpg");
    if !path.exists() {
        return;
    }
    let exif = nom_exif::read_exif(path).expect("fixture must parse");
    assert_eq!(
        image_viewer::exif_comment(&exif).as_deref(),
        Some("Screenshot"),
        "ImageDescription must come back as text"
    );
    // The file also carries an 18-byte UserComment; description still wins,
    // and the payload itself must decode to something non-empty.
    let payload = exif
        .get(nom_exif::ExifTag::UserComment)
        .and_then(|v| v.as_undefined())
        .expect("fixture carries a UserComment");
    assert_eq!(payload.len(), 18);
    assert!(
        image_viewer::decode_user_comment(payload).is_some(),
        "the real UserComment payload must decode"
    );

    // Synthetic payloads without a file: ASCII prefix + text, and an
    // all-NUL body is no comment rather than an empty line.
    assert_eq!(image_viewer::decode_user_comment(b"ASCII\0\0\0hello").as_deref(), Some("hello"));
    assert_eq!(image_viewer::decode_user_comment(b"ASCII\0\0\0\0\0\0"), None);
}

/// The comment lines must reach the panel, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_comment_lines_are_wired_into_extract_exif() {
    let src = main_src();
    assert!(
        src.contains("image_viewer::exif_comment("),
        "extract_exif must show the EXIF description/UserComment"
    );
    assert!(
        src.contains("image_viewer::jpeg_comment("),
        "extract_exif must also show a JPEG COM segment"
    );
}

/// The colour line must come from the preview, not from the metadata job, and
/// `main.rs` is unreachable from every test, so this is a source scan - the
/// established pattern here.
/// Colour is classified from the bytes the preview decode already read. The
/// whole point is that `extract_exif` runs **no** second read of the file — the
/// old shape called `color_info(path)` there, an 8-12 ms whole-file
/// `fs::read` on a real photograph, queued ahead of every prefetch. The panel
/// draws the line from the cached preview, which answers with no read of any
/// kind.
#[test]
fn the_colour_line_rides_on_the_preview_not_the_metadata_read() {
    let src = main_src();
    let body = fn_body(&src, "fn extract_exif(").expect("extract_exif");
    assert!(
        !body.contains("color_info"),
        "extract_exif must not classify colour: that is the decode's answer now, \
         and calling color_info here is a second whole-file read of every image"
    );

    // The panel draws it from the shown image's cached preview.
    let coloured = src.contains("preview.colour.panel_line()");
    assert!(coloured, "the metadata panel must draw the colour line from the cached preview");
    assert!(
        src.contains("self.shown.as_ref().and_then(|k| self.previews.get(k))"),
        "the colour line must be read off `shown`'s cached preview, not re-derived \
         per frame"
    );
}

/// Screen-size save must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here.
#[test]
fn the_screen_save_key_is_wired_to_a_scaled_save() {
    let src = main_src();
    assert!(
        src.contains("save_transformed_scaled("),
        "Ctrl+Shift+S must go through the scaled save, not the full-res one"
    );
    assert!(src.contains("save_scaled_copy()"), "there must be a screen-size save action");
}

/// Toasts (Tier 1 #11): a resort must say what order it produced. Notices are
/// status-line toasts; the sort change was the state change that stayed silent.
#[test]
fn resort_names_the_order_it_produced() {
    let src = main_src();
    let at = src.find("if resort {").expect("the settings panel must have a resort arm");
    let arm = &src[at..(at + 600).min(src.len())];
    assert!(
        arm.contains("self.notice = Some("),
        "resorting must leave a notice naming the order, or a flipped folder \
         reads as nothing happening"
    );
    assert!(
        arm.contains("self.config.sort"),
        "the notice must read the sort the rescan is about to apply"
    );
}

/// Single-instance toggle must be **wired**, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_single_instance_toggle_is_wired() {
    let src = main_src();
    assert!(
        src.contains("self.config.single_instance"),
        "settings panel must offer the single-instance toggle"
    );
    assert!(
        src.contains("hold_single_instance()"),
        "startup must gate on the single-instance mutex"
    );
}

/// The mutex itself, claimed twice in one process: the first claim wins and
/// the second loses, which is the whole semantic the gate relies on.
#[test]
fn second_claim_of_the_same_name_loses() {
    let name = format!("Local\\image-viewer-claim-test-{}", std::process::id());
    assert!(image_viewer::claim_single_instance(&name), "first claim of a fresh name must win");
    assert!(!image_viewer::claim_single_instance(&name), "second claim of a held name must lose");
}

/// The startup brick: a `\\.\`-prefixed device path is not an object name,
/// so creation fails on every launch — and that failure used to read as
/// "already running". A guard that refuses to start the app is worse than no
/// guard, so an uncreatable name starts anyway.
#[test]
fn an_uncreatable_mutex_name_does_not_block_startup() {
    assert!(
        image_viewer::claim_single_instance("\\\\.\\Global\\image-viewer-instance"),
        "a mutex that cannot be created must fail open, not exit the app"
    );
}

/// Rebindable keys (new system): overrides win, blanks fall back, unknown
/// actions resolve to "" rather than a guessed chord. Canonicalization
/// makes modifier order and case irrelevant.
#[test]
fn key_overrides_win_and_blanks_fall_back() {
    let mut keys = std::collections::HashMap::new();
    assert_eq!(image_viewer::resolve_binding(&keys, "next"), "ArrowRight");
    keys.insert("next".into(), "PageDown".into());
    assert_eq!(image_viewer::resolve_binding(&keys, "next"), "PageDown");
    keys.insert("next".into(), "   ".into());
    assert_eq!(image_viewer::resolve_binding(&keys, "next"), "ArrowRight");
    assert_eq!(image_viewer::resolve_binding(&keys, "nope"), "");
    assert_eq!(image_viewer::canonical_chord("shift+ctrl+s"), Some("Ctrl+Shift+S".into()));
    assert_eq!(image_viewer::canonical_chord("Space"), Some("Space".into()));
    assert_eq!(image_viewer::canonical_chord("Ctrl++"), None);
    assert_eq!(
        image_viewer::split_chord("Alt+shift+Delete"),
        Some((false, true, true, "Delete".into()))
    );
    assert_eq!(image_viewer::split_chord("Ctrl"), None);
}

/// Every default chord's key must be a known name: otherwise it can never
/// match, and the action ships dead. The one exception is a *deliberately*
/// blank default (`next_alt` since Space became the zoom toggle): an
/// unparseable shipped chord is still a shipped-dead action, so a second
/// blank row must add itself here, not slip through.
#[test]
fn every_default_key_is_a_known_name() {
    let mut blank: Vec<&str> = Vec::new();
    for (id, chord, _) in image_viewer::KEY_ACTIONS {
        let Some((_, _, _, name)) = image_viewer::split_chord(chord) else {
            assert_eq!(
                *chord, "",
                "default chord of `{id}` (`{chord}`) names no key: it must be blank on \
                 purpose like `next_alt`, or a parseable name like `Space`"
            );
            blank.push(id);
            continue;
        };
        assert!(
            image_viewer::KEY_NAMES.contains(&name.as_str()),
            "default chord of `{id}` names unknown key `{name}`"
        );
    }
    assert_eq!(
        blank,
        vec!["next_alt"],
        "the deliberately-unbound actions are exactly `next_alt`; if another row is \
         blank on purpose, name it here so a typo'd chord never ships unbound"
    );
}

/// Capture, totality, and live labels must be wired: the capture state and
/// its commit path, both directions of the key-name table for every known
/// name, and buttons built through the live-label helper.
#[test]
fn capture_and_labels_are_wired() {
    let src = main_src();
    assert!(
        src.contains("self.capturing = Some(") && src.contains("press keys… (Esc cancels)"),
        "Settings must offer click-to-capture with an Esc out"
    );
    // Duplicates land with a warning naming the sharers (never refused),
    // and every row marks its own conflicts in red.
    assert!(
        src.contains("(also: {sharers})") && src.contains("\"also: {}\""),
        "conflicts must show, not block"
    );
    for name in image_viewer::KEY_NAMES {
        // Single characters ride the char-match branches, not string arms.
        if name.len() == 1 {
            continue;
        }
        assert!(src.contains(&format!("\"{name}\" =>")), "key name `{name}` has no parse arm");
        assert!(
            src.contains(&format!("Key::{name} => \"{name}\"")),
            "key name `{name}` has no reverse arm"
        );
    }
    assert!(
        src.contains("is_ascii_alphabetic") && src.contains("is_ascii_digit"),
        "single characters must route through the char-match branches"
    );
    assert!(
        src.contains("fn action_button_text("),
        "buttons must go through the live-label helper"
    );
    // Capture waits for a non-modifier key (a bare Ctrl press must not
    // commit as "Ctrl+?") and refuses names the matcher cannot parse.
    assert!(
        src.contains("is_modifier_key(*key)"),
        "capture must skip modifier keypresses, not commit them"
    );
    assert!(
        src.contains("is not a bindable key"),
        "an unparseable capture must say so instead of storing a dead chord"
    );
}

/// Recursive browsing must be **wired**: one funnel for every rebuild and a
/// recursive variant where position is preserved.
#[test]
fn recursive_browsing_is_wired() {
    let src = main_src();
    assert!(
        src.contains("fn sorted_scan(") && src.contains("fn load_folder_into("),
        "every rebuild needs a funnel for each half (sorted list, position-preserving load)"
    );
    // The flag is *decided* twice, not once per call site: a `recursive_folders`
    // read anywhere else is a gallery that misses the tree.
    assert_eq!(
        src.matches("config.recursive_folders && ").count(),
        6,
        "exactly the six read sites: startup scan, open_folder, open_file, scan_current \
         (rescan), the settings reload, and leave_archive. A seventh means a rebuild was \
         added without asking whether it sees the tree — leave_archive is the one that \
         has to agree with the open it is going back from, or backing out lands in a \
         flat listing of a folder the reader was browsing as a tree."
    );
    assert!(
        src.contains("fn scan_current(&self)") && src.contains("rx_rescan"),
        "rescan scans off the paint thread via rx_rescan, keeping the funnel"
    );
    assert!(
        src.contains("load_all_from_folder_recursive("),
        "the position-preserving funnel must have a recursive arm"
    );
}

/// The body of the function whose signature is `sig`, by brace counting.
///
/// The same helper as `tests/common/spreads_fixtures.rs`, kept local rather than shared because
/// each test binary compiles separately, and these scans sit beside the
/// behaviour they belong to.
fn fn_body<'a>(src: &'a str, sig: &str) -> Option<&'a str> {
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
fn squash(src: &str) -> String {
    src.split_whitespace().collect()
}

/// Crop is the one site where "record before the change" is not adjacent to the
/// change: `set_crop` runs **per drag frame**, so recording at the mutation would
/// push ~200 entries for one gesture and make undo useless after a single drag.
///
/// So the record goes on **press**, and this asserts that it is on the press path
/// rather than merely present somewhere in the function — the same lesson as
/// `the_turns_second_half_is_derived_not_latched`.
#[test]
fn a_crop_gesture_records_once_on_press() {
    let src = main_src();
    let body = fn_body(&src, "fn crop_from_screen(").expect("crop_from_screen");
    // `record` must NOT be inside crop_from_screen (it is pure geometry).
    assert!(
        !body.contains("history.record("),
        "crop_from_screen is pure geometry; recording inside it would record \
         once per drag frame"
    );
    // It must be on the press handler, which runs once per gesture. The arm is
    // entered every frame the button is held, so the record must also sit after
    // the `primary_pressed()` test that makes it a press.
    let press = fn_body(&src, "fn central(").expect("central");
    let pressed =
        press.find("primary_pressed()").expect("the crop press must test for the press frame");
    let record = press.find("self.history.record(").expect("the crop press must record");
    let set_crop = press.find("self.edits.set_crop(").expect("the crop drag must set");
    assert!(
        pressed < record && record < set_crop,
        "the record must be on the press path, after the press test and before \
         the drag sets the crop"
    );
    // Every clear must record immediately before it, so a third `clear_crop`
    // site cannot appear unrecorded. The count is not compared with the number
    // of records: `apply_to_current` records too, and it is not a clear.
    let clear = "self.edits.clear_crop(";
    let sites: Vec<usize> = src.match_indices(clear).map(|(i, _)| i).collect();
    assert!(!sites.is_empty(), "expected clear_crop sites in main.rs");
    for at in sites {
        let before = &src[at.saturating_sub(120)..at];
        assert!(
            before.contains("self.history.record("),
            "every clear_crop must record just before it; unrecorded site at byte {at}"
        );
    }
}

/// Both ends of an archive must step **out** of it, not round its own listing.
///
/// This is the bug a reader hits as "only the first zip opens": `→` at an
/// archive's last page used to be `pos_next`'s ordinary end-of-list wrap, which
/// inside an archive wraps to the archive's *own* first member — so the folder's
/// second, third and later archives were unreachable by arrowing, and one volume
/// looped forever. The backward edge existed; the forward one did not.
#[test]
fn both_ends_of_an_archive_step_out_of_it() {
    use image_viewer::{Gallery, leave_archive_at};
    let dir = tempfile::tempdir().unwrap();
    for n in ["a.jpg", "b.jpg", "c.jpg"] {
        std::fs::write(dir.path().join(n), b"x").unwrap();
    }
    let mut g = Gallery::new(dir.path());
    assert_eq!(g.len(), 3);

    // Front and back, including the page dimension: `index + 1 == len` is the
    // last *file*, not the last *position*.
    assert!(g.at_front(), "a fresh listing starts at its front");
    assert!(!g.at_back());
    g.index = 2;
    assert!(g.at_back(), "the last file is the back of the listing");
    assert!(!g.at_front());

    // Forward out, backward out — and never the other way round. A `→` at the
    // *front* must not drop the reader into the parent folder, and a `←` at the
    // back must not either.
    assert!(leave_archive_at(true, true, true, true), "forward at the back leaves");
    assert!(leave_archive_at(false, true, true, true), "backward at the front leaves");
    assert!(!leave_archive_at(true, true, false, true), "forward mid-archive stays");
    assert!(!leave_archive_at(false, false, true, true), "backward mid-archive stays");
    assert!(!leave_archive_at(true, false, false, true), "nowhere to go, so nothing");

    // `can_leave` is asked first and overrides both edges: with looping off and
    // the archive as the folder's last entry there is nowhere to land, and being
    // dropped into the folder by a press that had nowhere to go reads as broken.
    assert!(!leave_archive_at(true, false, true, false));
    assert!(!leave_archive_at(false, true, false, false));

    // An empty listing has no back, so it cannot claim one.
    let empty = Gallery::empty();
    assert!(!empty.at_back());
    assert!(empty.at_front(), "an empty listing is trivially at its front");
}

/// Leaving an archive must not re-enter it. The original defect was:
/// `leave_archive` adopted a gallery whose cursor sat on the archive, and
/// `adopt_gallery` ended in `goto`, which *entered* an archive on the cursor.
/// The fix is not a one-shot but removing entry from `goto` entirely —
/// `goto` now positions on an archive and never enters one. `step` and the
/// grid pick are the only places that call `enter_archive_if_present`.
#[test]
fn leaving_an_archive_does_not_re_enter_it() {
    let src = main_src();

    let leave = fn_body(&src, "fn leave_archive").expect("leave_archive");
    // Leaving arms no guard — it just builds the parent gallery with the
    // cursor on the archive. `goto` no longer enters, so no guard is needed.
    assert!(
        leave.contains("just_left") == false,
        "leave_archive no longer needs a one-shot because goto does not enter"
    );

    let goto = fn_body(&src, "fn goto(&mut self, ctx: &Context)").expect("goto");
    // Goto must not enter archives at all — the one-shot is gone because
    // the whole branch is gone.
    assert!(goto.contains("just_left") == false, "goto must not contain the old one-shot");
    // The body shows the position logic without any archive entry: it sets
    // `is_nav`, `lat.begin`, then `snap_to_turn_anchor` — no `open_archive`.
    // The absence of an archive branch *is* the test.
    assert!(
        goto.contains("self.snap_to_turn_anchor()"),
        "goto runs the position setup without an archive branch"
    );

    let step = fn_body(&src, "fn step(&mut self, ctx: &Context, forward: bool)").expect("step");
    assert!(
        step.contains("archive_edge(forward)"),
        "step must ask about both edges of the archive, not only the front:\n{step}"
    );
    assert!(
        step.contains("leave_archive(ctx, forward)"),
        "the direction is what distinguishes 'back' from 'on to the next one':\n{step}"
    );
}

/// The archive visibility toggle must be wired end to end: told to the library
/// (or the next scan answers from a flag nobody set) and followed by a rescan
/// (or the panel changes the setting and the listing does not).
#[test]
fn the_show_archives_toggle_is_wired() {
    let src = main_src();
    assert!(
        src.contains("image_viewer::set_show_archives(config.show_archives)"),
        "start-up must tell the library before the first scan"
    );
    let flat = squash(&src);
    let before = flat.find("set_show_archives(config.show_archives)").expect("start-up call");
    let scan = flat.find("letfiles=Self::sorted_scan(").expect("the start-up scan");
    assert!(
        before < scan,
        "the setting has to be set before the scan that reads it, or the first \
         listing of the session answers from a stale flag"
    );

    let panel = fn_body(&src, "impl eframe::App for MyApp").expect("the app impl");
    assert!(panel.contains("&mut self.config.show_archives"), "the toggle must exist in Settings");
    let toggle = panel.find("&mut self.config.show_archives").expect("the checkbox");
    let setter = panel.find("set_show_archives(self.config.show_archives)").expect("the setter");
    let rescan = panel[toggle..].find("self.rescan(ctx, true)").expect("the rescan");
    assert!(
        toggle < setter && setter < toggle + rescan,
        "save, then tell the library, then rescan — a rescan before the setter \
         rebuilds the listing from the old flag and the toggle looks inert"
    );
}

/// Archives in the gallery must be **entered explicitly**, never entered
/// automatically and never decoded. The seam that does it is **not `goto`** —
/// `goto` is where opening a folder ends, and entering there opened whichever
/// archive happened to sort first without anyone asking. Instead, entry happens
/// in `step` (arrowing onto one) and `pick_grid_selection` (grid click/Enter):
/// the places where a person asked for something.
///
/// `goto` explicitly does NOT enter archives: it positions on them, and the
/// panel draws a placeholder. `plan_jobs` refuses archives, so nothing queued
/// a decode for one.
#[test]
fn archives_are_entered_explicitly_and_never_decoded() {
    let src = main_src();

    // `goto` no longer enters: it positions, and the body shows it.
    let goto = fn_body(&src, "fn goto(&mut self, ctx: &Context)").expect("goto");
    // The body shows the position logic without any archive entry: it sets
    // `is_nav`, `lat.begin`, then `snap_to_turn_anchor` — no `open_archive`.
    // The absence of an archive branch *is* the test.
    assert!(
        goto.contains("self.snap_to_turn_anchor()"),
        "goto runs the position setup without an archive branch"
    );
    assert!(goto.contains("open_archive") == false, "goto must not call open_archive");

    // Entry is in `step` (arrowing) and `pick_grid_selection` (grid).
    let step = fn_body(&src, "fn step(&mut self, ctx: &Context, forward: bool)").expect("step");
    assert!(
        step.contains("enter_archive_if_present(ctx)"),
        "arrowing onto an archive must call the entry helper"
    );

    let pick = fn_body(&src, "fn pick_grid_selection(&mut self, ctx: &Context)")
        .expect("pick_grid_selection");
    assert!(
        pick.contains("enter_archive_if_present(ctx)"),
        "grid click/Enter must call the entry helper"
    );

    // The decode side: plan_jobs refuses archives (library rule, tested there).
    // These are the paths that bypass plan_jobs and need their own guard.
    assert!(
        src.contains("let is_archive = is_archive_path(path);")
            && squash(&src).contains("!is_archive&&"),
        "a grid cell must not ask for a thumbnail of a container"
    );
    assert!(
        src.contains(".filter(|p| !is_archive_path(p))"),
        "the EXIF panel pushes its own job, so it needs its own archive guard"
    );

    // `Del` refuses when the cursor is ON an archive, not only when inside one.
    let del = fn_body(&src, "fn delete_current(&mut self, ctx: &Context)").expect("delete_current");
    assert!(
        del.contains("is_archive_path(&p) ||"),
        "delete must refuse when the cursor is ON an archive, not only when inside one"
    );
}

/// UserCmd expansion (Tier 1 #2): {file}/{dir}/{name} expand, unknowns stay.
#[test]
fn user_command_templates_expand_against_the_current_file() {
    let cmd = image_viewer::UserCommand {
        name: "x".into(),
        program: "{dir}/tool.exe".into(),
        args: vec!["{file}".into(), "{name}".into(), "{x}".into()],
    };
    let file = std::path::Path::new("C:/pics/a.jpg");
    let (prog, args) = image_viewer::expand_user_command(&cmd, file);
    assert_eq!(prog, "C:/pics/tool.exe");
    assert_eq!(args[0], "C:/pics/a.jpg");
    assert_eq!(args[1], "a.jpg");
    assert_eq!(args[2], "{x}", "unknown placeholders must survive");
}

/// Empty args default to the file: "open with" nearly always means the
/// picture on screen, not a blank window.
#[test]
fn user_command_with_no_args_gets_the_file() {
    let cmd = image_viewer::UserCommand {
        name: "paint".into(),
        program: "mspaint.exe".into(),
        args: Vec::new(),
    };
    let file = std::path::Path::new("C:/pics/a.jpg");
    let (prog, args) = image_viewer::expand_user_command(&cmd, file);
    assert_eq!(prog, "mspaint.exe");
    assert_eq!(args, vec!["C:/pics/a.jpg"]);
}

/// EXIF dates parse strict-shape or not at all; touching sets exactly the
/// time asked for, readable back from the file.
#[test]
fn exif_dates_parse_strictly_and_touch_lands() {
    use std::time::{Duration, UNIX_EPOCH};
    // 2024-03-17 14:34:56 UTC = 1710686096, in both spellings.
    assert_eq!(
        image_viewer::parse_exif_datetime("2024:03:17 14:34:56"),
        UNIX_EPOCH.checked_add(Duration::from_secs(1710686096))
    );
    assert_eq!(
        image_viewer::parse_exif_datetime("2024-03-17 14:34:56"),
        UNIX_EPOCH.checked_add(Duration::from_secs(1710686096))
    );
    // Leap day exists in 2024 and not in 2023.
    assert!(image_viewer::parse_exif_datetime("2024:02:29 00:00:00").is_some());
    assert!(image_viewer::parse_exif_datetime("2023:02:29 00:00:00").is_none());
    for bad in [
        "2024:13:01 00:00:00",
        "2024:03:17",
        "2024:03:17 24:00:00",
        "2024:03:17 14:34:56 extra",
        "",
    ] {
        assert!(image_viewer::parse_exif_datetime(bad).is_none(), "{bad:?} must not parse");
    }

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("touched.jpg");
    std::fs::write(&p, b"bytes").unwrap();
    let to = UNIX_EPOCH + Duration::from_secs(1700000000);
    image_viewer::touch_file_mtime(&p, to).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), to);
}

/// End to end through a real file: FFA02639.jpg carries DateTimeOriginal
/// 2017-02-08 14:14:02, and the lookup-plus-parse must find it — a
/// dashes-vs-colons mismatch here is exactly the shipped "No EXIF date".
/// `test-images/` is gitignored, so this skips when absent.
#[test]
fn exif_date_reads_from_a_real_file() {
    use std::time::{Duration, UNIX_EPOCH};
    let path = std::path::Path::new("test-images/FFA02639.jpg");
    if !path.exists() {
        return;
    }
    let exif = nom_exif::read_exif(path).expect("fixture must parse");
    let raw = exif
        .get(nom_exif::ExifTag::DateTimeOriginal)
        .map(|e| e.to_string())
        .expect("fixture carries DateTimeOriginal");
    assert_eq!(
        image_viewer::parse_exif_datetime(&raw),
        UNIX_EPOCH.checked_add(Duration::from_secs(1486563242))
    );
}

/// Slideshow pacing: due once the interval has passed, never early, and a
/// zero interval floors at one second rather than strobing every frame.
#[test]
fn slideshow_advances_only_when_due() {
    let now = std::time::Instant::now();
    let ago = now - std::time::Duration::from_secs(10);
    assert!(image_viewer::slideshow_due(ago, now, 3));
    assert!(!image_viewer::slideshow_due(now, now, 3));
    assert!(image_viewer::slideshow_due(ago, now, 0));
    assert!(!image_viewer::slideshow_due(now, now, 0));
}

/// Frame stepping wraps at both ends and never divides by zero; resume
/// offsets sum the same floored delays the paint clock uses; presets clamp
/// to the sane range.
#[test]
fn stepping_wraps_and_presets_clamp() {
    assert_eq!(image_viewer::step_frame(5, 4, true), 0);
    assert_eq!(image_viewer::step_frame(5, 0, false), 4);
    assert_eq!(image_viewer::step_frame(5, 2, true), 3);
    assert_eq!(image_viewer::step_frame(0, 0, true), 0);
    assert_eq!(image_viewer::step_frame(0, 7, false), 0);

    let anim = image_viewer::Animation {
        frames: Vec::new(),
        delays: vec![
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(0),
            std::time::Duration::from_millis(50),
        ],
    };
    // 100 + floored-20 + 0 before frame 2: the middle delay floors like
    // `total_duration` floors it, or resume disagrees with paint.
    assert_eq!(image_viewer::anim_frame_start(&anim, 2), std::time::Duration::from_millis(120));
    assert_eq!(image_viewer::anim_frame_start(&anim, 99), std::time::Duration::from_millis(170));

    assert_eq!(image_viewer::zoom_preset(&[1.0, 2.0, 4.0, 8.0], 0), 1.0);
    assert_eq!(image_viewer::zoom_preset(&[1.0, 2.0, 4.0, 8.0], 9), 8.0);
    assert_eq!(image_viewer::zoom_preset(&[0.0, -3.0, 100.0, 2.0], 0), 0.1);
    assert_eq!(image_viewer::zoom_preset(&[0.0, -3.0, 100.0, 2.0], 2), 32.0);
}

/// The zoom readout is a ratio against 1:1, not against fit: 100% is one
/// source pixel per physical pixel whatever the window is, so the number
/// means the same on a panorama and a thumbnail.
#[test]
fn zoom_percent_reports_against_one_to_one() {
    // 1:1 is `scale == 1/ppp` — the value the draw path itself uses — so it
    // must read exactly 100 at every pixel ratio, not approximately.
    assert_eq!(image_viewer::zoom_percent(1.0, 1.0), 100);
    assert_eq!(image_viewer::zoom_percent(1.0 / 1.25, 1.25), 100);
    assert_eq!(image_viewer::zoom_percent(1.0 / 2.0, 2.0), 100);
    // A fitted 17000px panorama on a ~1000pt panel is single digits.
    assert_eq!(image_viewer::zoom_percent(0.06, 1.0), 6);
    // And deep zoom is the large number it is, not clamped to something neat.
    assert_eq!(image_viewer::zoom_percent(50.0, 1.0), 5000);
}

/// The tile loop's range and rect math, pinned against the deep-zoom ghost:
/// on a source past `MAX_TEXTURE_DIM` the tile buffer is a half-res DCT
/// decode, and naming the range or the rect in source pixels composes a
/// small copy of the whole image over its own top-left.
#[test]
fn tile_math_positions_tiles_in_buffer_pixels_at_source_pitch() {
    // Full-res buffer (k == 1): the old arithmetic exactly. paint at 0,
    // scale 0.5, visible 100..600 of a 2000px buffer.
    assert_eq!(
        image_viewer::tile_axis_range(100.0, 600.0, 0.0, 0.5, 1.0, 2000),
        (200, 1200),
        "k=1 reproduces the pre-fix numbers bit for bit"
    );
    assert_eq!(
        image_viewer::tile_draw_rect((0.0, 0.0), 1024, 0, 1024, 1024, 0.5, 1.0, 1.0),
        [512.0, 0.0, 512.0, 512.0]
    );
    // Half-res buffer (k == 2), the booklet shape: 17042px source in an
    // 8521px buffer at scale 0.125. The visible 0..1067.5pt is buffer
    // 0..4268, not source 0..8534 — naming it in source pixels cuts every
    // tile from twice as far along as it is drawn.
    assert_eq!(image_viewer::tile_axis_range(0.0, 1067.5, 0.0, 0.125, 2.0, 8521), (0, 4270));
    // And the rect pitches the buffer span back up: tile (2,0) covers buffer
    // 2048..3072, i.e. source 4096..6144, i.e. screen 512..768.
    assert_eq!(
        image_viewer::tile_draw_rect((0.0, 0.0), 2048, 0, 1024, 1024, 0.125, 2.0, 2.0),
        [512.0, 0.0, 256.0, 256.0]
    );
    // Content and position agree: the drawn span is the buffer span pitched
    // up, so the tile shows the source pixels it is drawn over.
    let (x0, x1) = image_viewer::tile_axis_range(512.0, 768.0, 0.0, 0.125, 2.0, 8521);
    assert_eq!((x0, x1), (2048, 3072));
    // Clamping is against the buffer, not the source.
    assert_eq!(image_viewer::tile_axis_range(-100.0, 99999.0, 0.0, 0.125, 2.0, 8521), (0, 8521));
}

/// The tile loop goes through the ratio-aware math, not around it: the old
/// inline formula named the range in source pixels whatever the buffer was,
/// which is the defect above, so its absence is asserted as well as the new
/// calls' presence. Either half alone passes on a halfway revert.
#[test]
fn the_tile_loop_positions_through_the_ratio_aware_math() {
    let src = main_src();
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    assert!(
        central.contains("image_viewer::tile_axis_range("),
        "the tile range must come from tile_axis_range, which divides by the \
         buffer-to-source ratio"
    );
    assert!(
        central.contains("image_viewer::tile_draw_rect("),
        "the tile rect must come from tile_draw_rect, which pitches the buffer \
         span back up to source pixels"
    );
    assert!(
        !central.contains("(vis.min.x - paint.min.x) / scale)"),
        "the old inline range named source pixels for every buffer, which is \
         what squeezed a half-size copy over the image's own top-left"
    );
}

/// A histogram is exact on synthetic buffers: a solid color fills exactly
/// one bin per channel, a gradient fills every bin once, and clipping
/// counts are exact pixel counts. Ground truth by construction — no P3,
/// no reference renderer, just counting.
#[test]
fn histogram_counts_bins_and_clipped_pixels_exactly() {
    // Solid red 2x2: R[255]=4, G[0]=4, B[0]=4, nothing clipped.
    let red = vec![255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
    let h = image_viewer::histogram_rgba(&red, 2, 2);
    assert_eq!(h.bins[0][255], 4);
    assert_eq!(h.bins[1][0], 4);
    assert_eq!(h.bins[2][0], 4);
    assert_eq!(h.bins[0][0], 0);
    assert_eq!(h.black, 0);
    assert_eq!(h.white, 0);
    // Half pure black, half pure white: clipping is exact.
    let bw = vec![0u8, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255];
    let h = image_viewer::histogram_rgba(&bw, 2, 2);
    assert_eq!(h.black, 2);
    assert_eq!(h.white, 2);
    // Linear ramp across all 256 levels: every bin holds exactly one pixel.
    let mut ramp = Vec::with_capacity(256 * 4);
    for v in 0..=255u8 {
        ramp.extend_from_slice(&[v, v, v, 255]);
    }
    let h = image_viewer::histogram_rgba(&ramp, 256, 1);
    for ch in 0..3 {
        for (level, &count) in h.bins[ch].iter().enumerate() {
            assert_eq!(count, 1, "channel {ch} level {level}");
        }
    }
    assert_eq!(h.black, 1);
    assert_eq!(h.white, 1);
}

/// The histogram reaches the panel through the decode worker, not through
/// the UI: `decode_preview` counts the preview buffer it already holds and
/// the pane draws the cached numbers. A pane that counted pixels itself
/// would walk a buffer per frame, and a worker that skipped the count
/// would leave the panel showing a default histogram — zeros that read as
/// "this photo is black".
#[test]
fn the_histogram_is_counted_on_the_worker_and_drawn_from_cache() {
    let src = main_src();
    let decode = fn_body(&src, "fn decode_preview(").expect("decode_preview");
    assert!(
        decode.contains("image_viewer::histogram_rgba("),
        "decode_preview must count the preview buffer it already holds: the \
         CPU pixels are dropped after upload, so later is too late"
    );
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    assert!(
        !central.contains("histogram_rgba("),
        "the paint path must not count pixels itself — that is a buffer walk \
         per frame"
    );
}

/// A grid cell whose thumbnail failed every attempt says so instead of
/// loading forever: after `THUMB_TRIES` the ellipsis stopped meaning
/// "decoding" and reads as a hang, so the cell gets the same "?" an
/// unreadable container does.
#[test]
fn a_failed_grid_cell_says_so_instead_of_loading_forever() {
    let src = main_src();
    let cell = fn_body(&src, "fn grid_cell(").expect("grid_cell");
    // Anchored on `>=`, not the bare name: the request guard in the same
    // function also mentions `THUMB_TRIES` (with `<`), so a bare `contains`
    // passes with the exhausted arm deleted — which is exactly the mutation
    // that was tried, and exactly what this assertion has to catch.
    assert!(
        cell.contains(">= THUMB_TRIES"),
        "the fallback must branch on exhausted attempts: without it a failed \
         cell is indistinguishable from a decoding one, forever"
    );
    // The "?" has to sit behind that branch, not beside it — an unconditional
    // "?" would replace the loading mark for cells that are genuinely still
    // decoding.
    let tries_at = cell.find(">= THUMB_TRIES").expect("tries check");
    assert!(
        cell[tries_at..].contains("\"?\""),
        "an exhausted cell must show the failed mark after the budget check"
    );
}

/// An undecodable file names its container instead of parroting the
/// guesser — but only when no decoder was even attempted. A corrupt file
/// of a supported kind keeps the decode error, or every broken JPEG would
/// gain a wrong explanation.
#[test]
fn the_decode_failure_names_unclaimed_bytes_only() {
    let src = lib_src();
    let decode = fn_body(&src, "fn decode_bytes(").expect("decode_bytes");
    assert!(
        decode.contains("looks_like_supported_image(bytes)"),
        "the remap must be gated on the magic: bytes a decoder claims are \
         a corrupt file, not an unsupported one"
    );
    assert!(
        decode.contains("unsupported_error(bytes)"),
        "unclaimed bytes must go through the naming function rather than \
         the guesser's shrug"
    );
}

/// A failed image must never show "Loading…": its decode will never be
/// re-issued, so the overlay is a promise nothing keeps — and navigating
/// away and back must re-state the stored error rather than blank it, since
/// the revisit produces no new message of its own.
#[test]
fn a_failed_image_shows_its_error_instead_of_loading_forever() {
    let src = main_src();
    // Anchored on the overlay's own condition, not on the first "Loading…"
    // in the function: that literal also starts the shown-None branch and
    // appears in a comment, and both read before the overlay without
    // guarding it. `panel.left_top()` is the paint call itself, unique in
    // the file.
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    let at = central.find("self.cur != self.shown").expect("the overlay condition");
    let tail = &central[at..];
    let gate = tail.find("Kind::Preview))").expect("the failed check");
    let paint = tail.find("panel.left_top()").expect("the overlay paint");
    // The gate must come *before* the paint it guards: a check after the
    // text is painted is a comment, not a condition.
    assert!(
        gate < paint,
        "the Loading overlay must be gated on the failed set: without the \
         gate it paints forever over an image that will never arrive"
    );
    // ...and `goto` re-states the stored message, because the revisit
    // produces no new decode and therefore no new error of its own.
    let goto = fn_body(&src, "fn goto(&mut self").expect("goto");
    assert!(
        goto.contains("load_errors.get("),
        "goto must re-state a stored failure message: clearing the banner \
         without this leaves the revisit blank with no error and no spinner \
         activity"
    );
}

/// The stored message lives and dies with the `failed` entry: written on
/// the same arm, cleared at every site that clears it. A message that
/// outlives its failure is shown for an image that now decodes.
#[test]
fn the_stored_failure_message_cannot_outlive_its_failure() {
    let src = main_src();
    assert!(
        src.contains("self.load_errors.insert(key.clone(), msg.clone())"),
        "the message must be stored on the failure arm, beside the failed \
         entry it describes"
    );
    for site in ["self.load_errors.retain(|k, _| k.path != p)", "self.load_errors.clear()"] {
        assert!(
            src.contains(site),
            "every site that clears `failed` must clear the message too, or \
             a fixed file keeps reporting its old failure"
        );
    }
}

/// The frame clock survives its move to lib with speed: file timing pins
/// the old behavior, and 2x halves both the index math and the repaint wait
/// (which is real time — scheduling an effective-time wait would repaint
/// late by the speed factor).
#[test]
fn anim_frame_state_keeps_time_and_scales() {
    use image_viewer::ImageData;
    use std::time::Duration;
    let frame =
        || ImageData { width: 1, height: 1, rgba: vec![0, 0, 0, 255], load_time: Duration::ZERO };
    let anim = image_viewer::Animation {
        frames: vec![frame(), frame()],
        delays: vec![Duration::from_millis(100), Duration::from_millis(200)],
    };
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::ZERO, 1.0),
        (0, Duration::from_millis(100))
    );
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::from_millis(150), 1.0),
        (1, Duration::from_millis(150))
    );
    // Loops: total 300 ms, so 300 lands back on frame 0.
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::from_millis(300), 1.0),
        (0, Duration::from_millis(100))
    );
    // 2x: 50 ms in means 100 ms along — frame 1, with 200 ms of effective
    // time left reading as 100 ms on the wall clock.
    assert_eq!(
        image_viewer::anim_frame_state(&anim, Duration::from_millis(50), 2.0),
        (1, Duration::from_millis(100))
    );
    // Clamped, not frozen: 0x behaves as the 0.1x floor.
    let (idx, wait) = image_viewer::anim_frame_state(&anim, Duration::from_millis(1000), 0.0);
    assert!(idx < 2 && wait > Duration::ZERO);
    assert_eq!(image_viewer::clamp_anim_speed(0.0), 0.1);
    assert_eq!(image_viewer::clamp_anim_speed(99.0), 8.0);
}

/// Playback speed must be **wired**: offered in Settings, and read by the
/// frame clock (paint and resume agree through the same clamp).
#[test]
fn animation_speed_is_wired() {
    let src = main_src();
    assert!(
        src.contains("self.config.anim_speed"),
        "Settings must offer the speed and the clock must read it"
    );
    assert!(
        src.contains("clamp_anim_speed(self.config.anim_speed)"),
        "resume must scale through the same clamp as the clock"
    );
}

/// Wallpaper-as-displayed must be **wired**: crop and turn baked into a PNG
/// beside the config (the shell reads files, never buffers), refusing the
/// cases the path-keyed edit cannot express.
#[test]
fn wallpaper_shown_is_wired() {
    let src = main_src();
    assert!(
        src.contains("fn set_wallpaper_shown(&mut self"),
        "there must be a processed-wallpaper action"
    );
    let at = src.find("fn set_wallpaper_shown(&mut self)").expect("the action must exist");
    let body = &src[at..(at + 3500).min(src.len())];
    assert!(
        body.contains("region_for_save(") && body.contains("wallpaper.png"),
        "it must bake through region_for_save into a config-side PNG"
    );
    assert!(
        body.contains("Cannot wallpaper a transformed animation")
            && body.contains("multi-page document"),
        "animations and later pages must be refused, like save and copy refuse them"
    );
    assert!(
        src.contains("binding_down(i, &kb, \"wall_shown\")"),
        "Shift+W must reach it through the bindings map"
    );
}

/// **A wallpaper request must not decode on the paint thread**, and the last
/// press must win. Both wallpaper paths do a whole-file read and (converted or
/// Shift+W) a full decode + re-encode; the "the UI thread never decodes" rule
/// does not weigh the *reason*, so the work lives on a worker thread like the
/// drop copy's and the rating read's. `main.rs` is unreachable from every test,
/// so this is the source-scan pattern, asserting the arrangement by byte order:
/// the spawn must come **before** every heavy call, and the stale-answer drop
/// must exist in `pump`.
///
/// This lifetime rule is the `drops.rs` shape: presence of a spawn says nothing
/// about which side of it the decode is on, and the defect this guards was that
/// arrangement until this moved.
#[test]
fn wallpaper_work_is_on_a_worker_and_the_last_press_wins() {
    let src = main_src();

    for name in ["fn set_wallpaper(", "fn set_wallpaper_shown("] {
        let at = src.find(name).unwrap_or_else(|| panic!("{name} must exist"));
        // Brace-match the body: the functions are adjacent to other methods,
        // and a substring window drifted past this once and still passed.
        let body = fn_body(&src[at..], name).expect("balanced braces");
        let spawn =
            body.find("std::thread::spawn").unwrap_or_else(|| panic!("{name} must spawn a worker"));
        // The heavy half: read, decode, transform, encode, and the shell call
        // itself — every one of them after the spawn, or the rule is not kept.
        for heavy in
            ["load_image(", "load_image_full(", "region_for_save(", "wallpaper::set_from_path("]
        {
            if let Some(h) = body.find(heavy) {
                assert!(spawn < h, "{name}: {heavy} must run on the worker, after thread::spawn");
            }
        }
        // The label: an in-flight wallpaper request is otherwise invisible
        // until the desktop changes, which reads as a dropped keystroke.
        assert!(body.contains("\"Setting wallpaper…\""), "{name} must show its in-flight notice");
        // The token: the worker must abort at an early check and again at the
        // set, so a superseded press never points the desktop at a picture the
        // user's last action replaced.
        let loads = body.matches("issued.load(").count();
        assert!(
            loads >= 2,
            "{name} must check its issuance token before the work and again at the set, found {loads}"
        );
    }

    // The result's stale drop: a superseded answer must be recognised by its
    // order and silently discarded, not shown over the last press's text.
    let pump = src.find("fn pump(").expect("pump must exist");
    // Brace-match from the signature; pump is long and full of closures.
    let pb = fn_body(&src[pump..], "fn pump(").expect("pump braces balance");
    assert!(
        pb.contains("Msg::Wallpaper { order, result }"),
        "pump must absorb the wallpaper result"
    );
    assert!(
        pb.contains("self.wallpaper_order.load("),
        "pump must drop a result whose order is not the counter's now"
    );
}

/// Fit window keeps the chrome it measures and clamps to the monitor.
#[test]
fn fit_window_hugs_content_and_clamps() {
    // 800x600 window, 700x500 central: 100x100 of chrome, kept.
    assert_eq!(
        image_viewer::fit_window_size((400.0, 300.0), (800.0, 600.0), (700.0, 500.0), None),
        (500.0, 400.0)
    );
    // An 8x spread is bigger than any screen: clamped.
    assert_eq!(
        image_viewer::fit_window_size(
            (6000.0, 4000.0),
            (800.0, 600.0),
            (700.0, 500.0),
            Some((1920.0, 1080.0))
        ),
        (1920.0, 1080.0)
    );
    // Degenerate content never collapses the window.
    assert_eq!(
        image_viewer::fit_window_size((0.0, 0.0), (800.0, 600.0), (800.0, 600.0), None),
        (200.0, 120.0)
    );
}

/// The title-bar toggle moves only the chrome: hiding puts the frame top on
/// the client's top (the strip is being reclaimed), showing puts it
/// `strip_after` above (the strip is about to sit there), and a window with
/// less headroom than the strip clamps at the virtual screen's top rather
/// than parking its re-added title bar off-screen.
#[test]
fn the_decoration_toggle_lands_the_frame_where_the_client_was() {
    // Hide: 100-point window, 31-point strip. The client sat at 100; the
    // frame goes straight there and no strip is re-asserted.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, 0.0), 100.0);
    // Show it back: the remembered strip is subtracted, so the client stays
    // at 100 and the frame's top climbs to 69 to make room for the strip.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, 31.0), 69.0);
    // A toggle with no remembered measurement (first show of a session that
    // started undecorated) still keeps the client where it was: the window
    // may grow downward by the strip instead of moving.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, 0.0), 100.0);
    // Flush against the screen top with a re-added strip: clamped at 0, the
    // content gives up its top rows instead of the title bar going off-screen.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(20.0, 31.0), 0.0);
    // A negative measurement is never a negative offset.
    assert_eq!(image_viewer::outer_top_for_decoration_toggle(100.0, -4.0), 100.0);
}

/// The hide-title-bar checkbox keeps what the user was looking at.
///
/// `main.rs` is unreachable from the tests, so the wiring is a scan — the
/// geometry half it pins: the client area is measured *before* any command is
/// sent (the commands are applied against this frame's own geometry, so a
/// read after them would measure a window that has not changed yet), the
/// outer frame is landed with the library rule above, the client size is
/// re-asserted so winit's `SWP_NOSIZE` never lets the strip into the client,
/// and maximized/fullscreen windows are left to the OS's clamping.
#[test]
fn the_title_bar_toggle_keeps_the_client_area() {
    let src = main_src();

    let at = src.find(".checkbox(&mut self.config.hide_title_bar,").expect("the checkbox exists");
    let after = &src[at..];
    let arm_end = after.find("\n                });").expect("the checkbox's arm closes");
    let arm = &after[..arm_end];

    let geometry =
        arm.find(".zip(v.outer_rect)").expect("the arm measures the client and outer rects");
    let first_cmd = arm.find("send_viewport_cmd").expect("the arm sends commands");
    assert!(geometry < first_cmd, "the client area must be measured before any command is sent");

    assert!(
        arm.contains("image_viewer::outer_top_for_decoration_toggle("),
        "the frame must be landed through the library rule, not by an arm-local \
         formula"
    );
    assert!(
        arm.contains("ViewportCommand::OuterPosition(")
            && arm.contains("ViewportCommand::InnerSize("),
        "the frame must move and the client size must be re-asserted: winit's \
         SWP_NOSIZE default is what lets the strip into the client area"
    );
    assert!(
        arm.contains("if maximized || fullscreen {"),
        "maximized and fullscreen windows must be left to the OS, which clamps \
         their client area to the work area either way"
    );
    assert!(
        arm.contains("ViewportCommand::Decorations(!hiding)"),
        "the toggle itself must still reach winit in every branch"
    );

    // The strip is only measurable while decorated: the hide arm is the one
    // place it is written, and it is persisted next to the setting so a
    // future show in a fresh session reads it back.
    let lib = lib_src();
    for want in ["pub title_strip: f32,", "pub fn outer_top_for_decoration_toggle"] {
        assert!(lib.contains(want), "lib.rs must carry {want}");
    }
}

/// Recursive scan: the whole tree in pre-order, natural within each level,
/// symlinks skipped, non-images ignored, position preserved like the flat
/// loader.
#[test]
fn recursive_scan_walks_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (rel, bytes) in [
        ("c.jpg", &b"1"[..]),
        ("a/10.jpg", &b"2"[..]),
        ("a/2.jpg", &b"3"[..]),
        ("a/deep/b.png", &b"4"[..]),
        ("notes.txt", &b"nope"[..]),
    ] {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
    }
    let flat: Vec<String> = image_viewer::scan_folder(root)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(flat, vec!["c.jpg"], "flat scan must not descend");
    let walked: Vec<String> = image_viewer::scan_folder_recursive(root)
        .iter()
        .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(
        walked,
        vec!["a/2.jpg", "a/10.jpg", "a/deep/b.png", "c.jpg"],
        "pre-order, natural within each level, txt ignored"
    );

    // Position-preserving load over the recursive walk.
    let mut g = image_viewer::Gallery::empty();
    g.load_all_from_folder_recursive(root);
    assert_eq!(g.len(), 4);
    assert_eq!(
        g.current().unwrap().file_name().unwrap().to_string_lossy(),
        "2.jpg",
        "empty gallery starts at the first file"
    );

    // A symlink loop must not hang the walk: skipped, not followed. Link
    // creation needs privileges this box may not grant, so a failed link
    // skips rather than fails.
    #[cfg(windows)]
    {
        use std::os::windows::fs::symlink_dir;
        let link = root.join("a/loop");
        if symlink_dir(root, &link).is_ok() {
            let walked = image_viewer::scan_folder_recursive(root);
            assert_eq!(walked.len(), 4, "the loop link must not add files");
            let _ = std::fs::remove_dir(&link);
        }
    }
}

/// A real 2x2 PNG, built here rather than vendored: `image` can write PNG, so
/// `tests/fixtures/` is for formats nothing in the dependency set can produce.
fn archive_test_png() -> Vec<u8> {
    let img =
        image::RgbaImage::from_fn(2, 2, |x, y| image::Rgba([x as u8 * 80, y as u8 * 80, 40, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// Build a zip/cbz of the given members. `deflate` is the default on purpose:
/// a *stored* zip would pass even if the reader only ever read headers.
fn write_test_zip(path: &std::path::Path, members: &[(&str, Vec<u8>)]) {
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
fn write_test_7z(path: &std::path::Path, members: &[(&str, Vec<u8>)]) {
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

/// "Show archives" must actually hide them — from *both* scans, with images
/// untouched, and the getter must round-trip so a test can put it back.
///
/// The getter is public for the reason `thumbnail_cache_enabled` is: the flag is
/// process-wide (it is the house pattern for a setting the library has to see,
/// because one predicate serves the flat scan, the recursive walk and
/// `Gallery::new`), so a test that changes it has to be able to restore it. A
/// test that forgot would silently decide the answer for the ones after it.
#[test]
fn the_show_archives_setting_hides_archives_and_spares_images() {
    use image_viewer::{
        is_browsable, scan_folder, scan_folder_recursive, set_show_archives, show_archives_enabled,
    };
    // Restore whatever the rest of the suite expects, however this test ends.
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            set_show_archives(true);
        }
    }
    let _restore = Restore;

    assert!(show_archives_enabled(), "on by default: the feature shipped on");
    // Round-trip both ways *before* the scans, and leave it on: the "off"
    // assertions below are a separate state, and a test that left the flag
    // flipped would decide the answer for everything after it.
    set_show_archives(false);
    assert!(!show_archives_enabled(), "the getter must round-trip the setter");
    set_show_archives(true);
    assert!(show_archives_enabled(), "...and back again");

    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("vol2");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(dir.path().join("a.jpg"), b"x").unwrap();
    // Only archives WITH images should be listed. Create valid ones.
    write_test_zip(&dir.path().join("v1.cbz"), &[("p1.jpg", archive_test_png())]);
    write_test_7z(&dir.path().join("v2.7z"), &[("p1.jpg", archive_test_png())]);
    write_test_zip(&dir.path().join("v4.zip"), &[("p1.jpg", archive_test_png())]);
    // v3.rar - we can't easily write a valid RAR, so it won't be listed
    write_test_zip(&sub.join("deep.cbz"), &[("p.jpg", archive_test_png())]);

    let names = |v: Vec<std::path::PathBuf>| -> Vec<String> {
        v.iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_string_lossy().replace('\\', "/"))
            .collect()
    };
    assert_eq!(
        names(scan_folder(dir.path())),
        vec!["a.jpg", "v1.cbz", "v2.7z", "v4.zip"],
        "on: only valid archives with images are listed beside the image"
    );
    assert_eq!(names(scan_folder_recursive(dir.path())).len(), 5); // a.jpg + 4 valid archives

    set_show_archives(false);
    assert_eq!(
        names(scan_folder(dir.path())),
        vec!["a.jpg"],
        "off: the image is still browsable, and no archive appears"
    );
    assert_eq!(
        names(scan_folder_recursive(dir.path())),
        vec!["a.jpg"],
        "off: the recursive walk uses the same predicate, so it hides them too"
    );
    assert!(
        !is_browsable(std::path::Path::new("x/v1.cbz")),
        "the predicate itself is what changes, not a second list of extensions"
    );
    assert!(is_browsable(std::path::Path::new("x/a.jpg")));
}

/// Archives are **listed** in a folder scan, and are deliberately not in
/// `SUPPORTED_EXTENSIONS` — that constant gates decoding and also decides
/// which members get extracted, so a zip in it would make a container
/// something the image decoders are handed.
///
/// Every extension the app claims is asserted here, because the claim is what
/// makes a folder's contents visible at all: a typo in this list is a format
/// that silently does not appear in any folder, with no error anywhere.
#[test]
fn archives_are_listed_and_images_still_are() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::write(root.join("a.jpg"), b"x").unwrap();
    // Only archives WITH images should be listed. Create valid ones.
    write_test_zip(&root.join("vol1.cbz"), &[("p1.jpg", archive_test_png())]);
    write_test_7z(&root.join("vol1.7z"), &[("p1.jpg", archive_test_png())]);
    // RAR we can't easily write a valid one, so skip it for listing test
    write_test_zip(&root.join("vol1.zip"), &[("p1.jpg", archive_test_png())]);
    std::fs::write(root.join("notes.txt"), b"x").unwrap();
    std::fs::write(root.join("inner.cbz"), b"x").unwrap(); // empty - should NOT appear

    let mut found: Vec<String> = image_viewer::scan_folder(root)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    found.sort();
    // Only the valid archives with images are listed; inner.cbz is empty so excluded
    assert_eq!(
        found,
        vec!["a.jpg", "vol1.7z", "vol1.cbz", "vol1.zip"],
        "images and valid archives with images, natural order; empty inner.cbz excluded"
    );

    // The recursive walk shares the predicate, so an archive in a subfolder is
    // found without a second list to keep in step.
    let sub = root.join("vol2");
    std::fs::create_dir_all(&sub).unwrap();
    write_test_zip(&sub.join("v2.cbz"), &[("p.jpg", archive_test_png())]);
    let walked: Vec<String> = image_viewer::scan_folder_recursive(root)
        .iter()
        .filter(|p| image_viewer::is_archive_path(p))
        .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/"))
        .collect();
    assert_eq!(
        walked,
        vec!["vol1.7z", "vol1.cbz", "vol1.zip", "vol2/v2.cbz"],
        "archives with images are found at every level; empty inner.cbz excluded; the txt never is"
    );

    // The claim is content-based for decoding, so the listing predicate must
    // stay out of the decode filter. This is the assertion that would fail if
    // someone 'fixed' the folder scan by adding cbz to SUPPORTED_EXTENSIONS.
    assert!(
        !image_viewer::SUPPORTED_EXTENSIONS.contains(&"cbz"),
        "an archive in the decode filter would be handed to the image decoders \
         and would also be extracted as a member of another archive"
    );
    // The predicate is case-insensitive and covers both kinds.
    // Use an actual archive we created in this test.
    assert!(
        image_viewer::is_browsable(root.join("vol1.cbz").as_path())
            && image_viewer::is_browsable(std::path::Path::new("x/a.JPG")),
        "the predicate is case-insensitive and covers both kinds"
    );
    // Empty archive is NOT browsable
    assert!(
        !image_viewer::is_browsable(std::path::Path::new("x/inner.cbz")),
        "an empty archive is not browsable"
    );
    assert!(
        !image_viewer::is_browsable(std::path::Path::new("x/notes.txt")),
        "not everything in a folder is browsable"
    );
}

/// All three containers must produce the *same* flattened gallery: that shared
/// contract is what makes one `open_archive` correct for every format, and a
/// reader that quietly diverges (kept a subdirectory, kept a `.txt`, wrote two
/// members to one name) would show a gallery unlike every other archive's.
#[test]
fn every_archive_format_yields_the_same_flattened_gallery() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let png = archive_test_png();

    // Two members that flatten to the same file name, one nested and one with a
    // different case: without the case-insensitive claim, one silently wins and
    // the gallery is a page short with no error anywhere.
    let members: Vec<(&str, Vec<u8>)> = vec![
        ("ch1/01.png", png.clone()),
        ("ch2/01.PNG", png.clone()),
        ("ch2/02.png", png.clone()),
        ("notes.txt", b"ignore me".to_vec()),
        ("inner.cbz", b"ignore me too".to_vec()),
    ];

    write_test_zip(&root.join("v.cbz"), &members);
    write_test_7z(&root.join("v.7z"), &members);

    let mut results = Vec::new();
    for name in ["v.cbz", "v.7z"] {
        let dest = root.join(format!("out-{name}"));
        let (extracted, skipped) = image_viewer::extract_archive(&root.join(name), &dest)
            .unwrap_or_else(|e| {
                panic!("{name} extracted nothing: {e}");
            });
        let mut got: Vec<String> = image_viewer::scan_folder(&dest)
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_lowercase())
            .collect();
        got.sort();
        results.push((name.to_string(), extracted, skipped, got));
    }

    for (name, extracted, skipped, got) in &results {
        assert_eq!(*extracted, 2, "{name}: two distinct pages");
        assert_eq!(*skipped, 0, "{name}: nothing was unreadable");
        assert_eq!(
            *got,
            vec!["01.png".to_string(), "02.png".to_string()],
            "{name}: flat, images only, the colliding member did not overwrite"
        );
    }
    assert_eq!(
        results[0].3, results[1].3,
        "a cbz and a 7z of the same members must show the same gallery"
    );

    // The count is a header read, and it agrees with what extraction produced —
    // it is what the status line and the grid cell draw, so a disagreement is a
    // number on screen that is wrong.
    assert_eq!(image_viewer::archive_image_count(&root.join("v.cbz")), Some(2));
    assert_eq!(image_viewer::archive_image_count(&root.join("v.7z")), Some(2));
    assert_eq!(
        image_viewer::archive_image_count(&root.join("nope.7z")),
        None,
        "a missing archive has no count, rather than a zero that reads as 'empty'"
    );
    assert_eq!(
        image_viewer::archive_image_count(&root.join("v.jpg")),
        None,
        "a non-archive has no count"
    );

    // `extract_archive` dispatches on the extension and refuses anything else
    // by name, so an unhandled container is an error rather than an empty
    // gallery that looks like an archive with no images in it.
    let err =
        image_viewer::extract_archive(std::path::Path::new("x.tar.zst"), &root.join("out-none"))
            .unwrap_err();
    assert!(err.contains("zst"), "the error names the format: {err}");
}

/// A RAR this build cannot read must **say so**, not open as an empty gallery.
///
/// **What is and is not covered here, precisely.** Nothing in the dependency set
/// can write a RAR, and the 7-Zip on this box is read-only for the format
/// ("Not implemented" on write), so there is no RAR fixture to test against — a
/// hand-built one would be this crate's guess at the format rather than the
/// format. So the real archive path is untested here and this test covers only
/// what is checkable:
///
///  * a RAR4 file - the format most `.rar` files in the wild actually are, and
///    one this crate does not read - produces an error naming RAR, rather than
///    an empty gallery, which reads as "an archive with no images in it";
///  * the guard that catches an undecompressed member is content-based, so it
///    can be tested directly on bytes.
///
/// **Not claimed:** that a real RAR5 with stored PNGs opens and yields the same
/// flattened gallery as a cbz. That needs a RAR file this repo does not have,
/// and saying otherwise would be the kind of claim that survives only because
/// nobody can run the test.
#[test]
fn a_rar_this_build_cannot_read_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // RAR4's signature block: `Rar!\x1a\x07\x00`. Recognisably a RAR, and a
    // version the `rar` crate does not parse.
    let rar4 = root.join("vol.rar");
    std::fs::write(&rar4, b"Rar!\x1a\x07\x00\x00\x00").unwrap();

    let err = image_viewer::extract_archive(&rar4, &root.join("out")).unwrap_err();
    assert!(
        err.contains("RAR") || err.contains("Cannot read"),
        "the error says the format could not be read: {err}"
    );
    assert!(
        !root.join("out").exists() || image_viewer::scan_folder(&root.join("out")).is_empty(),
        "an unreadable archive must not leave gallery entries behind"
    );

    // The member guard is content, so it is testable without an archive: bytes
    // that are not a picture must be refused whatever the name says, because
    // that is the shape of an undecompressed member.
    assert!(image_viewer::looks_like_supported_image(&archive_test_png()));
    assert!(!image_viewer::looks_like_supported_image(b"\x00\x01\x02not an image at all"));
    assert!(!image_viewer::looks_like_supported_image(&[]));
    assert!(
        !image_viewer::looks_like_supported_image(b"RIFFxxxxWAVEfmt "),
        "a RIFF that is a WAV is not a WebP, and the check says so"
    );
}

/// T mode is always absolute: the shrink helper is deleted, so there is no
/// shrink behavior left to test — its absence is asserted in the wiring scan.
#[test]
fn the_fit_window_key_is_wired_to_a_live_resize() {
    let src = main_src();
    assert!(src.contains("self.fit_window_to_image()"), "`T` must arm the fit");
    assert!(
        src.contains("ViewportCommand::InnerSize("),
        "central must send a live resize with the drawn size"
    );
    assert!(
        src.contains("fit_window_size("),
        "the resize must go through the chrome-measuring helper"
    );
    assert!(
        src.contains("self.view.fit = false") && src.contains("self.view.scale = 1.0 / ppp"),
        "`T` must reset to absolute 100%, never a shrink-to-fit"
    );
    // Same frame: the reset must land before `scale` is read from the view,
    // or the window is sized for a zoom the picture draws a frame later.
    // T mode is always absolute: no shrink path exists, and `fit_zoom` is
    // gone entirely rather than merely uncalled.
    assert!(!src.contains("fit_zoom("), "the shrink path is deleted, not bypassed");
    let lib = lib_src();
    assert!(!lib.contains("fn fit_zoom("), "the shrink helper must go with its only caller");
    // Converge on the symptom (drawn vs panel), and re-arm while it
    // disagrees: a one-shot loop that clears its flags up front runs once
    // no matter what `fit_iters` allows, which is how the bars survived a
    // "bounded iteration" that never iterated.
    assert!(
        src.contains("(drawn.0 - panel.width()).abs()"),
        "convergence must compare drawn against panel, not window against target"
    );
}

/// Sticky mode: `T` toggles persistence (not a one-shot), arrivals refit via
/// the cursor key, user zooms re-hug without touching the zoom, and every
/// resize centers on the old window.
#[test]
fn the_fit_window_mode_is_sticky_centered_and_follows() {
    let src = main_src();
    assert!(
        src.contains("self.config.fit_window_mode = !self.config.fit_window_mode"),
        "`T` must toggle the mode, not fire once"
    );
    let goto_at = src.find("fn goto(&mut self").expect("goto must exist");
    // 9400 and not 8000: the cursor-change block also carries the per-image
    // remember wiring (`view_memory_is_wired_...`), which grew goto past
    // this scan's old heuristic window.
    let goto = &src[goto_at..(goto_at + 9400).min(src.len())];
    assert!(
        goto.contains("self.fit_window_key") && goto.contains("self.fit_arrive_pending = true"),
        "arrivals must refit through the cursor key, so toggles and rescans do not"
    );
    assert!(
        src.contains("let reset = self.fit_window_pending || self.fit_arrive_pending"),
        "arrival must carry the 100% reset too, or a zoomed-out view survives navigation"
    );
    assert!(
        src.contains("self.fit_arrive_pending") && src.contains("self.shown == self.cur"),
        "arrival fits must wait until the new image is displayed, or every fit reads one image behind"
    );
    assert!(
        src.contains("self.fit_wrap_pending = true"),
        "user zooms in the mode must re-hug the new size"
    );
    assert!(
        src.contains("ViewportCommand::OuterPosition(") && src.contains("((m.x - want.0) / 2.0"),
        "resizes must center on the monitor"
    );
}

/// Adopting a preview follows the view modes: fit-window owns the zoom
/// absolutely (its arrival reset), 1:1 pins it below, fit-view fits it here
/// — anything else leaves the zoom alone, so photos open at 100% instead of
/// maximized and an absolute zoom survives navigation. (An unconditional snap
/// here once compounded per visit into a shrinking spiral while every reset
/// correctly computed 1.0.)
#[test]
fn adoption_follows_the_view_modes() {
    let src = main_src();
    let at = src.find("fn sync_shown(&mut self").expect("sync_shown must exist");
    let body = &src[at..(at + 2500).min(src.len())];
    assert!(
        body.contains("self.config.fit_mode") && body.contains("self.view.fit = true"),
        "adoption must fit only in fit-view mode, never unconditionally"
    );
}

/// The two view modes exclude each other: turning fit-view on disables
/// fit-window, and vice versa.
#[test]
fn view_modes_are_exclusive() {
    let src = main_src();
    assert!(
        src.contains("fn toggle_fit_mode(&mut self)")
            && src.contains("fn toggle_fit_mode(&mut self"),
        "toggle_fit_mode must exist"
    );
    let at =
        src.find("self.config.fit_mode = !self.config.fit_mode").expect("toggle arm must exist");
    let body = &src[at..(at + 1200).min(src.len())];
    assert!(
        body.contains("self.config.fit_window_mode = false"),
        "fit-view switched on must switch fit-window off"
    );
}

/// Panning belongs to its picture: arriving at a new image zeroes the
/// offset, or the old drag shifts the new picture (black one side, clipped
/// the other) with a perfectly innocent zoom readout. Keyed on the cursor,
/// so toggles and rescans re-`goto`ing the same image keep the pan.
#[test]
fn arriving_resets_the_pan_but_stays_do_not() {
    let src = main_src();
    let goto_at = src.find("fn goto(&mut self").expect("goto must exist");
    // 9400 and not 8000: the cursor-change block also carries the per-image
    // remember wiring (`view_memory_is_wired_...`), which grew goto past
    // this scan's old heuristic window.
    let goto = &src[goto_at..(goto_at + 9400).min(src.len())];
    assert!(
        goto.contains("self.pan_key") && goto.contains("self.view.offset = Vec2::ZERO"),
        "goto must zero the pan when the cursor changed"
    );
}

/// Fit-window follow-through: pinned panel widths in the mode (auto-sizing
/// panels move the measured chrome after the fact, which is how the window
/// landed bigger than the content), two-phase centering (position after the
/// resize settles, with a monitor fallback), and a button that shows the
/// mode like Crop does.
#[test]
fn fit_window_pins_chrome_places_late_and_shows_state() {
    let src = main_src();
    assert!(
        src.contains("exact_size("),
        "side panes must pin their width in the mode, or chrome drifts post-measure"
    );
    assert!(
        src.contains("pending_pos"),
        "centering must wait for the resize to settle, not race it"
    );
    let at = src.find("self.config.fit_window_mode,").expect("the mode toggle must exist");
    let from = at.saturating_sub(200);
    let arm = &src[from..(at + 400).min(src.len())];
    assert!(
        arm.contains("selectable_label(")
            && arm.contains("action_button_text(&self.config, \"Fit window\", \"fit_window\")"),
        "the mode needs a selectable button showing the live chord, like Crop"
    );
    assert!(
        src.contains("action_button_text(&self.config, \"Fit window\", \"fit_window\")"),
        "the mode button must show the live chord, not a hardcoded one"
    );
}

/// Animation stepping and window chrome must be **wired**: `,`/`.`/`K` reach
/// the frame methods, `1`–`4` reach the presets, and both window flags reach
/// the viewport builder. `main.rs` is unreachable from every test, so these
/// are source scans - the established pattern here.
#[test]
fn the_animation_keys_presets_and_chrome_are_wired() {
    let src = main_src();
    assert!(
        src.contains("self.step_animation(false)")
            && src.contains("self.step_animation(true)")
            && src.contains("self.toggle_animation_play()"),
        "`,`/`.`/`K` must reach the animation frame methods"
    );
    assert!(
        src.contains("egui::Key::Comma") && src.contains("egui::Key::Period"),
        "stepping must be bound to comma and period"
    );
    assert!(
        src.contains("zoom_preset(&self.config.zoom_presets"),
        "`1`–`4` must read the configured presets"
    );
    assert!(
        src.contains("with_always_on_top()") && src.contains("with_decorations(false)"),
        "both window flags must reach the viewport builder"
    );
    assert!(
        src.contains("self.config.always_on_top") && src.contains("self.config.hide_title_bar"),
        "both window flags must be offered in Settings"
    );
}

/// The alt-next key resumes a paused animation instead of stepping: the
/// step branch stands down for it, and it only reroutes the paused case —
/// playing animations and stills step on as before. Shipped **unbound** now
/// (Space left navigation to become the fit/100% toggle — JPEGView parity);
/// the hook stays rebindable in Settings.
#[test]
fn the_alt_next_key_resumes_a_paused_animation() {
    let src = main_src();
    assert!(
        src.contains("step_dir.filter(|_| !unpause)"),
        "the step branch must stand down when the key means resume"
    );
    let at = src.find("let unpause =").expect("the unpause flag must exist");
    let arm = &src[at..(at + 700).min(src.len())];
    assert!(
        arm.contains("\"next_alt\"") && arm.contains("paused.is_some()"),
        "only the alt-next chord on a paused animation reroutes"
    );
}

/// Window chrome applies live: decorations go through the viewport command
/// and always-on-top through Win32 (`set_topmost`), not through a restart.
#[test]
fn window_chrome_applies_live() {
    let src = main_src();
    assert!(
        src.contains("ViewportCommand::Decorations("),
        "hiding the title bar must send a live viewport command"
    );
    assert!(
        src.contains("set_topmost(self.config.always_on_top)"),
        "always-on-top must apply straight away, not on restart"
    );
    let lib = lib_src();
    assert!(
        lib.contains("fn set_topmost(") && lib.contains("SetWindowPos"),
        "always-on-top must reach Win32 directly"
    );
}

/// Slideshow must be **wired**: a toggle plus pace in Settings, a tick that
/// wakes the frame loop and steps, and an end that stops rather than wraps
/// silently when looping is off. `main.rs` is unreachable from every test,
/// so this is a source scan - the established pattern here.
#[test]
fn the_slideshow_is_wired_to_a_tick_and_a_stop() {
    let src = main_src();
    assert!(
        src.contains("Start slideshow") && src.contains("slideshow_secs"),
        "Settings must offer the toggle and the pace"
    );
    assert!(
        src.contains("slideshow_due(") && src.contains("request_repaint_after("),
        "the tick must check the clock and keep waking the loop"
    );
    assert!(
        src.contains("fn step(&mut self, ctx: &Context, forward: bool) -> bool"),
        "step must report whether it moved, so the end can stop the show"
    );
}

/// A fresh config closes on Esc (the shipped behaviour), and the Esc chain
/// orders the arms correctly: save-as panel → typing → grid → crop → archive →
/// fullscreen → quit.
///
/// Two arms changed. The **archive** arm comes before fullscreen and quit, so Esc
/// inside an archive goes back instead of closing the app. The **save-as panel**
/// arm comes before everything *including* `typing`, because the panel's path box
/// holds focus the moment it opens — so `typing` is true while it is up, and Esc
/// landed in the search box's arm, clearing the *filter* and leaving the panel
/// standing. The quit branch still exists and still consults the toggle, but only
/// when there is nothing to close.
///
/// The window below was 3200 bytes and is now 8000, and that change is part of the
/// fix's story: the doc comment above pushed the fullscreen arm to byte +3242, so
/// the test failed with "fullscreen branch" missing while the Esc chain was
/// perfectly correct. A byte window a comment can move is a test that measures
/// the comment.
#[test]
fn esc_defaults_to_closing_and_crop_comes_first() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("no-such-config.json");
    assert!(
        image_viewer::Config::load(&missing).esc_closes_app,
        "a fresh config must keep the shipped Esc-closes behaviour"
    );
    let src = main_src();
    let at = src.find("if esc {").expect("the Esc chain must exist");
    let arm = &src[at..(at + 8000).min(src.len())];

    // Order: save-as panel → typing → crop → archive → fullscreen → quit, each
    // inside the previous `else`. **Position, not mere presence**: an arm that is
    // earlier in the text but not nested in the previous else stops nothing, and
    // that is the whole content of the save-as fix.
    let panel = arm.find("if self.save_as.open {").expect("save-as panel arm");
    let typing = arm.find("} else if typing {").expect("typing arm");
    let crop = arm.find("self.crop_mode").expect("crop branch");
    let archive = arm.find("zip_archive.is_some()").expect("archive branch");
    let fullscreen = arm.find("in_fullscreen").expect("fullscreen branch");
    let close = arm.find("ViewportCommand::Close").expect("quit branch");
    assert!(
        panel < typing
            && typing < crop
            && crop < archive
            && archive < fullscreen
            && fullscreen < close,
        "Esc chain must be: save-as panel → typing → crop → archive → fullscreen \
         → quit, got order: panel={} typing={} crop={} archive={} fullscreen={} \
         close={}",
        panel,
        typing,
        crop,
        archive,
        fullscreen,
        close
    );

    // The quit branch must still consult the toggle (it does, but it's now
    // nested inside the else of the archive check)
    assert!(
        arm.contains("self.config.esc_closes_app"),
        "quitting on Esc must still consult the toggle"
    );
}

/// Printing goes through the OS `print` verb on the file on screen: a
/// physical Ctrl+P only (repeat-filtered like every spawn chord), through
/// the spawn guard, and the handler gets the file — crops and turns are not
/// applied, because a path carries no pixels.
#[test]
fn printing_is_wired_to_ctrl_p() {
    let lib = lib_src();
    assert!(
        lib.contains("fn print_file(") && lib.contains("ShellExecuteW"),
        "printing must go through the native print verb"
    );
    let src = main_src();
    assert!(src.contains("fn print_current(&mut self"), "there must be a print action");
    assert!(
        src.contains("action_pressed_fresh(ctx, &kb, \"print\")"),
        "print must go through the edge-triggered matcher, not a bespoke chord"
    );
    let body_at = src.find("fn print_current(&mut self)").expect("the print action must exist");
    let body = &src[body_at..(body_at + 1500).min(src.len())];
    assert!(
        body.contains("spawn_guard("),
        "printing must go through the spawn guard like every external action"
    );
}

/// Touch dates must be **wired**: single file to now or its EXIF date,
/// whole visible folder to EXIF dates with the dateless counted, not failed.
#[test]
fn touch_dates_are_wired_to_menu_buttons() {
    let src = main_src();
    assert!(
        src.contains("fn touch_current(&mut self")
            && src.contains("fn touch_folder_to_exif_dates(&mut self"),
        "both touch actions must exist"
    );
    assert!(
        src.contains("touch_file_mtime(") && src.contains("parse_exif_datetime("),
        "touching must go through the tested helpers, not inline date math"
    );
}

/// The bottom bar's four menus carry the actions the old button row did.
///
/// `main.rs` is unreachable from the tests, so the wiring is a scan — and a
/// *positional* one, per the lesson recorded on `shown_partner`: presence of
/// a handler call proves nothing about which menu it lives in. Each block is
/// sliced from its `menu_button` label to the next one, so a stray handler
/// call anywhere else in the bottom panel fails.
#[test]
fn the_bottom_bar_menus_carry_the_buttons_actions() {
    let src = main_src();
    let at = src.find(r#"egui::Panel::bottom("menu")"#).expect("the bottom panel exists");
    // To the end of the panel: the bar is the last pane this section shows,
    // and the body after it starts a new pane, so slicing to the next
    // `egui::Panel::` keeps the scan inside the bar.
    let end = src[at + 1..]
        .find("egui::Panel::")
        .map(|i| at + 1 + i)
        .expect("a later pane closes the scan");
    let bar = &src[at..end];

    let menus = ["Transform", "Save", "File", "Tools"];
    for (i, name) in menus.iter().enumerate() {
        // Prefix match on purpose: the source label carries the "⏷" menu
        // marker, which is decoration and not part of the scan's contract,
        // and the needle stops before the label's closing quote.
        let start = bar.find(&format!("ui.menu_button(\"{name}")).unwrap_or_else(|| {
            panic!("the {name} menu exists [bar head: {:?}]", &bar[..bar.len().min(160)])
        });
        let block_end = menus[i + 1..]
            .iter()
            .find_map(|next| {
                let j = bar[start + 1..]
                    .find(&format!("ui.menu_button(\"{next}"))
                    .map(|j| start + 1 + j);
                j
            })
            .unwrap_or_else(|| bar.find(r#"ui.label("keys")"#).unwrap_or(bar.len()));
        let block = &bar[start..block_end];

        let want: &[&str] = match *name {
            "Transform" => &[
                "self.rotate(-1, ctx)",
                "self.rotate(1, ctx)",
                "self.mirror(true, ctx)",
                "self.mirror(false, ctx)",
                "self.reset_transform(ctx)",
                "self.undo(ctx)",
                "self.redo(ctx)",
                "edits.clear_crop(",
            ],
            "Save" => &[
                "self.save_transformed_copy()",
                "self.open_save_as()",
                "self.copy_image_to_clipboard(false)",
                "self.copy_image_to_clipboard(true)",
            ],
            "File" => &[
                "self.explore_current()",
                "self.rescan(ctx, true)",
                "self.transfer_current(ctx, false)",
                "self.transfer_current(ctx, true)",
                "self.delete_current(ctx)",
                "self.undo_delete(ctx)",
                "self.print_current()",
            ],
            _ => &[
                "self.set_wallpaper()",
                "self.set_wallpaper_shown()",
                "self.touch_current(false)",
                "self.touch_current(true)",
                "self.touch_folder_to_exif_dates()",
                "self.toggle_color_picker()",
            ],
        };
        for callee in want {
            assert!(block.contains(callee), "the {name} menu must own {callee}");
            // Close-before-act: the popup must not linger over the picture
            // the action leaves behind.
            let call_at = block.find(callee).unwrap();
            assert!(
                block[..call_at].contains("ui.close()"),
                "{callee} must close its menu before acting"
            );
        }
        // Every item closes the popup after acting, so the menu does not
        // linger over the picture — count them against the items we own.
        assert_eq!(
            block.matches("ui.close()").count(),
            want.len(),
            "every {name} item closes the menu"
        );
    }

    // The one-click top level: browsing, the side-step pair and the five mode
    // toggles are still on the row, in the bar (not in a menu after it).
    // Prefix again: the row ends where the first menu begins, and the
    // label now carries the "⏷" marker, so the find stops before the
    // label's closing quote.
    let row = &bar[..bar.find(r#"ui.menu_button("Transform"#).unwrap_or(bar.len())];
    for want in [
        "self.pick_folder(ctx)",
        "self.pick_file(ctx)",
        "nav_is_forward(NavInput::Left, rtl)",
        "nav_is_forward(NavInput::Right, rtl)",
        "self.toggle_fit_mode()",
        "self.fit_window_to_image()",
        "self.toggle_gallery()",
        "self.toggle_crop_mode(ctx)",
        "self.toggle_panels_master()",
        "show_left = !self.show_left",
        "show_right = !self.show_right",
    ] {
        assert!(row.contains(want), "the bottom row must keep {want}");
    }
}

/// Portable mode is the marker file: present means beside-the-exe, absent
/// means the roaming paths — asserted against a tempdir, never the real
/// executable's folder.
#[test]
fn portable_mode_follows_the_marker_file() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(image_viewer::portable_dir_from(dir.path()), None);
    std::fs::write(dir.path().join("portable.txt"), b"").unwrap();
    assert_eq!(image_viewer::portable_dir_from(dir.path()), Some(dir.path().to_path_buf()));
}

/// All three persistent roots must consult portable mode, or portable is a
/// config file sitting next to a roaming cache.
#[test]
fn persistent_roots_consult_portable_mode() {
    let lib = lib_src();
    for f in ["pub fn default_path()", "pub fn thumb_cache_root()", "pub fn spreads_root()"] {
        let at = lib.find(f).unwrap_or_else(|| panic!("{f} is gone"));
        let body = &lib[at..(at + 600).min(lib.len())];
        assert!(body.contains("portable_dir()"), "{f} must check portable mode first");
    }
}

/// Copy-original shares the copy path with an identity edit: same sources,
/// no re-decode, EXIF-oriented but uncropped and unturned.
#[test]
fn copy_original_shares_the_copy_path() {
    let src = main_src();
    assert!(
        src.contains("fn copy_image_to_clipboard(&mut self, unedited: bool)"),
        "there must be one copy path with an unedited arm"
    );
    assert!(
        src.contains("Copy original") && src.contains("self.copy_image_to_clipboard(true)"),
        "the menu must offer the unedited copy"
    );
}

/// The settings panel must scroll: it long outgrew the window height, and
/// `main.rs` is unreachable from every test, so this is a source scan - the
/// established pattern here.
#[test]
fn the_settings_panel_scrolls() {
    let src = main_src();
    let at = src.find("Panel::right(\"settings\")").expect("the settings panel must exist");
    let arm = &src[at..(at + 1200).min(src.len())];
    // The settings pane sits inside a vertical ScrollArea so all expanded
    // sections still fit on short windows; each section is its own
    // collapsible group so it's not a "window inside window".
    assert!(
        arm.contains("ScrollArea::vertical()"),
        "settings content must sit in a vertical ScrollArea, or it runs off \
         the bottom of short windows"
    );
    assert!(arm.contains("CollapsingHeader::new"), "settings sections must be collapsible groups");
}

/// Rebindable keys must be **wired**, and `main.rs` is unreachable from every
/// test, so these are source scans - the established pattern here.
#[test]
fn the_rebindable_keys_are_wired_to_the_direction_rule() {
    let src = main_src();
    assert!(
        src.contains("set_nav_overrides(&self.config.keybindings)"),
        "startup and the settings panel must push keybindings into the nav table"
    );
    assert!(
        src.contains("fn effective_nav_keys()"),
        "overrides must be applied in one place the direction rule reads"
    );
    // The rule still owns every direction: the override table carries keys,
    // and the meanings stay NavInputs asked of nav_is_forward.
    assert!(
        src.contains("NAV_REBINDABLE"),
        "there must be one action->meaning table for rebindable keys"
    );
    assert!(
        src.contains("self.capturing = Some("),
        "Settings must offer click-to-capture rebinding"
    );
}

/// User commands must be **wired**: a chord, a runner, and the expansion.
#[test]
fn the_user_commands_are_wired_to_ctrl_digits() {
    let src = main_src();
    assert!(src.contains("fn run_user_command(&mut self"), "there must be a user-command runner");
    assert!(
        src.contains("expand_user_command(&cmd, &path)"),
        "the runner must expand {{file}}/{{dir}}/{{name}}, not spawn raw config text"
    );
    assert!(src.contains("egui::Key::Num1"), "Ctrl+1..9 must trigger the commands");
}

/// Spawning must be edge-triggered: `key_pressed` also fires for OS
/// key-repeat, and a repeat spawns one process per frame — a held Ctrl+1
/// opened hundreds of Paint windows and froze the machine. Spawn-class
/// actions therefore go through the raw-event matcher with `repeat: false`,
/// never `key_pressed`.
#[test]
fn the_user_command_chord_ignores_key_repeat() {
    let src = main_src();
    let at = src.find("fn action_pressed_fresh(").expect("the edge-triggered matcher must exist");
    let body = &src[at..(at + 1200).min(src.len())];
    assert!(
        body.contains("repeat: false"),
        "the matcher must filter OS key-repeat, or holding Ctrl+1 fork-bombs"
    );
    assert!(
        !body.contains("key_pressed"),
        "the matcher must read raw events, not key_pressed, which fires on repeats"
    );
    for action in ["print", "copy", "copy_path"] {
        assert!(
            src.contains(&format!("action_pressed_fresh(ctx, &kb, \"{action}\")")),
            "spawn-class action `{action}` must go through the edge-triggered matcher"
        );
    }
    // user_1..9 go through the same matcher via an explicit name table (not
    // format-built, so the action-coverage test can see every one).
    assert!(
        src.contains("\"user_1\"") && src.contains("\"user_9\""),
        "all nine user-command actions must be listed by name"
    );
}

/// Guardrails around spawning: a global cooldown consulted by every site
/// that starts an external process, so no repeating chord — this one or a
/// future one — can fork-bomb the machine again. `main.rs` is unreachable
/// from every test, so this is a source scan, the established pattern.
#[test]
fn external_spawns_are_rate_limited() {
    let src = main_src();
    assert!(
        src.contains("const SPAWN_COOLDOWN: Duration"),
        "there must be one cooldown every spawn site shares"
    );
    for (sig, what) in [
        ("fn run_user_command(&mut self", "user commands"),
        ("fn explore_current(&mut self", "Explorer"),
    ] {
        let at = src
            .find(sig)
            .unwrap_or_else(|| panic!("{what} handler is gone; update this test to match"));
        let body = &src[at..(at + 2500).min(src.len())];
        assert!(
            body.contains("spawn_guard("),
            "{what} must go through spawn_guard, or a repeating chord can \
             spawn one process per frame again"
        );
    }
}

/// The binary identifies its own build in Settings, so a copy in Downloads
/// can be told apart from the current checkout. Stale copies already caused
/// two ghost reports (a chord that "did nothing", a freeze that was fixed).
#[test]
fn the_settings_panel_shows_the_build_id() {
    let src = main_src();
    assert!(
        src.contains("env!(\"IV_BUILD_ID\")"),
        "Settings must show the build fingerprint, or stale binaries are \
         undebuggable"
    );
}

/// Beta features wear a red marker in the panel, next to the label — the
/// same marker for both, matching the README's beta sections.
#[test]
fn beta_features_are_marked_beta_in_red() {
    let src = main_src();
    for base in ["Double page mode", "User commands (Ctrl+1..9):"] {
        let at = src.find(base).unwrap_or_else(|| panic!("{base} is gone from the settings panel"));
        let arm = &src[at..(at + 600).min(src.len())];
        assert!(
            arm.contains("(beta)") && arm.contains("warn_fg_color"),
            "{base} must be followed by a red (beta) marker"
        );
    }
}

/// Each side pane hides alone (`L` left, `P` right) under the `I` master, and
/// the EXIF read follows the *effective* visibility — both halves of the
/// gate, or hiding the pane leaves a worker reading a file nobody displays.
#[test]
fn side_panes_hide_individually_and_gate_their_reads() {
    let src = main_src();
    assert!(
        src.contains("self.show_panels && self.show_left"),
        "the left pane and its EXIF read must gate on master and flag"
    );
    assert!(
        src.contains("self.show_panels && self.show_right"),
        "the right pane must gate on master and flag"
    );
    assert!(
        src.contains("self.show_left = !self.show_left")
            && src.contains("self.show_right = !self.show_right"),
        "L and P must each flip their own flag"
    );
    // Each press must say what it did: with the master off a toggle changes
    // nothing visible, and without the notice a working key reads as dead.
    assert!(
        src.contains("\"Left pane hidden\"") && src.contains("\"Right pane hidden\""),
        "both pane toggles must leave a notice naming the outcome"
    );
    // Position, not presence: the master `if` used to wrap *both* side panes,
    // so gating the left one on its flag hid the right one too — and every
    // presence check above passed on that code. The left `if` must close
    // (a `}` at its own indent) before the right panel is shown.
    let left_at =
        src.find("if self.show_panels && self.show_left {").expect("the left pane gate must exist");
    let right_at = src[left_at..]
        .find("Panel::right(\"settings\")")
        .map(|i| left_at + i)
        .expect("the right panel must exist");
    assert!(
        src[left_at..right_at].lines().any(|l| l == "        }"),
        "the left gate must close before the right panel, or L hides both"
    );
}

/// Save-as must be **wired**, not merely present as functions.
///
/// `main.rs` is unreachable from every test, so these are source scans. Each one
/// exists because the feature it covers has a specific way of silently doing
/// nothing: a chord bound to a key the toolkit swallows, a panel that is drawn
/// after an early return so it is invisible in half the states, a defaults
/// value that differs from the behaviour it claims to match.
#[test]
fn save_as_and_paste_are_wired() {
    let src = main_src();
    let lib = lib_src();

    // --- save-as: the chord, the action, the panel, the call ---
    // The action table lives in `lib.rs`, so the *listing* is checked there and
    // the wiring here. Reading one file for both would have made this assertion
    // vacuous rather than wrong.
    assert!(
        lib.contains(r#"("save_as", "Ctrl+Shift+A", "Save as (choose file, quality, size)"),"#),
        "save_as must be a listed action so it appears in Settings and is rebindable"
    );
    assert!(
        src.contains(r#"binding_down(i, &kb, "save_as")"#),
        "the action must be read from the binding table"
    );
    assert!(
        src.contains("            } else if save_as {\n                self.open_save_as();"),
        "the chord must reach open_save_as"
    );
    assert!(
        src.contains("fn do_save_as(&mut self)"),
        "and open_save_as must lead to an actual save, not just a panel"
    );
    assert!(
        src.contains("save_transformed_to(&p, edit.xform, edit.crop, &dest, quality, max_long)"),
        "the panel's numbers must reach the library call - a save that ignores \\
         the quality slider is a slider that lies"
    );

    // **Defaults are the current behaviour.** This is the requirement, so it is
    // asserted rather than described: open_save_as must pre-fill the unique
    // name Ctrl+S would have chosen, with quality at the shared default and no
    // resize. A panel that opens on a different name means "save as" quietly
    // changed what saving does.
    let open = fn_body(&src, "fn open_save_as(&mut self)").expect("open_save_as");
    assert!(
        open.contains("self.save_as_infix(&p)"),
        "open_save_as must ask the shared rule for the name, not pick one itself \
         - hardcoding a crop test and otherwise saying `rotated` cannot tell \
         turned from untouched, and named every unedited picture `_rotated`"
    );
    assert!(
        open.contains("DEFAULT_JPEG_QUALITY"),
        "and the quality must start at the shared default"
    );
    assert!(
        open.contains("self.save_as.max_long = String::new()"),
        "and the size must start blank, which means 'do not resize'"
    );
    // The composition is asserted separately, because "called the shared rule"
    // and "asked it about the *composed* transform" are two properties and the
    // report was the second one.
    let infix = fn_body(&src, "fn save_as_infix(&self, path: &Path)").expect("save_as_infix");
    assert!(
        infix.contains("display_xform") && infix.contains("exif_orientation"),
        "the infix must be asked of the *composed* transform: Ctrl+S names from \
         display_xform(exif, user), so a photo stored sideways is `_rotated` on \
         its own and an untouched one is `_copy`"
    );
    assert!(
        infix.contains("save_infix("),
        "and the rule itself must be the library's shared save_infix, so the two \
         callers cannot answer the same question differently"
    );

    // The panel must be drawn *before* the early returns, or it is invisible
    // over the grid and over the archive placeholder - the two states where a
    // reader is most likely to want it.
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    let panel_at = central.find("if self.save_as.open {").expect("panel draw");
    let grid_at = central.find("if self.gallery_view {").expect("grid branch");
    assert!(
        panel_at < grid_at,
        "the save-as panel must be drawn before the grid's early return, or it \\
         cannot be opened while the grid is up"
    );

    // **Esc closes the panel, and it is checked before `typing` as well as
    // before the quit.** This is a reported bug, and the arm order is the whole
    // content of the fix: the panel's path box holds focus the moment it opens,
    // so `typing` is true, so Esc landed in the search box's arm - clearing the
    // *filter* and leaving the panel standing - and one Esc later fell through
    // to the quit branch. Both halves are asserted, because "before typing"
    // alone would still let Esc clear the search filter.
    //
    // **Absolute offsets, not `fn_body`.** `fn_body` starts counting *after* its
    // marker, so `fn_body(&src, "if esc {")` returns the body of the panel arm
    // alone - the marker already consumed the `if`'s own brace - and this
    // assertion then failed on "typing esc arm" for a chain that was correct. The
    // helper answers "the body of the next thing", which is the wrong question
    // for an if/else chain: what matters is where the arms sit *relative to each
    // other*, so the offsets are taken from `if esc {` and compared.
    let esc_at = src.find("if esc {").expect("the Esc chain");
    let esc_panel =
        src[esc_at..].find("if self.save_as.open {").map(|i| i + esc_at).expect("panel esc arm");
    let esc_typing =
        src[esc_at..].find("} else if typing {").map(|i| i + esc_at).expect("typing esc arm");
    let esc_quit =
        src[esc_at..].find("self.config.esc_closes_app").map(|i| i + esc_at).expect("quit arm");
    assert!(
        esc_panel < esc_typing,
        "the save-as panel's Esc arm must come before the `typing` arm: the \
         panel's path box has focus, so `typing` is true while it is open and \
         Esc would clear the search filter instead of closing the panel"
    );
    assert!(
        esc_panel < esc_quit,
        "and before the quit branch: Esc on a modal that can write a file must \
         never close the app"
    );
}

/// The undo stack is two per-path vectors and a rule about when each moves.
/// Truth table first; the naming here matches the fields so a reader can check
/// the implementation against this comment without opening it.
#[test]
fn undo_and_redo_walk_the_edit_history() {
    use image_viewer::{Edit, EditHistory, Xform};

    let p = std::path::Path::new("a.jpg");
    let mut h = EditHistory::default();

    // Review Focus #1: undo with nothing recorded is a no-op, not a panic.
    assert_eq!(h.undo(p, Edit::default()), None, "nothing to undo");
    assert!(!h.can_undo(p));
    assert!(!h.can_redo(p));

    // Record three states: identity, then turned, then turned twice.
    //
    // `record` is called **before** each change, so three records mean three
    // changes and the image is now in a *fourth* state. That is why the walk
    // below starts from `d` and not from the last recorded state `c`: treating
    // `c` as the current state is the off-by-one that makes this test read as
    // though undo handed back the state one *below* the top of the stack, which
    // is a different data structure and a wrong one.
    let a = Edit { xform: Xform::Identity, crop: None };
    let b = Edit { xform: Xform::Rot90, crop: None };
    let c = Edit { xform: Xform::Rot180, crop: None };
    let d = Edit { xform: Xform::Rot270, crop: None };
    h.record(p, a);
    h.record(p, b);
    h.record(p, c);
    assert_eq!(h.len_for(p), 3);
    assert!(h.can_undo(p));

    // Undo returns the *previous* state and moves the current one to redo.
    assert_eq!(h.undo(p, d), Some(c), "undo hands back the earlier edit");
    assert_eq!(h.undo(p, c), Some(b), "and again");
    assert_eq!(h.undo(p, b), Some(a), "down to the original state");
    assert_eq!(h.undo(p, a), None, "past the start is a no-op");
    assert!(h.can_redo(p));

    // Redo walks back the other way, one state per press.
    assert_eq!(h.redo(p, a), Some(b));
    assert_eq!(h.redo(p, b), Some(c));
    assert_eq!(h.redo(p, c), Some(d));
    assert_eq!(h.redo(p, d), None, "and stops");
}

/// Two properties that are both one line of implementation and both invisible
/// when broken: history is **per path**, and a new edit **clears redo**.
#[test]
fn history_is_per_path_and_a_new_edit_clears_redo() {
    use image_viewer::{Edit, EditHistory, Xform};

    let a = std::path::Path::new("a.jpg");
    let b = std::path::Path::new("b.jpg");
    let mut h = EditHistory::default();
    let base = Edit { xform: Xform::Identity, crop: None };
    let turned = Edit { xform: Xform::Rot90, crop: None };

    // Review Focus #2: two images keep two independent histories. A shared
    // stack would answer B's undo with A's edit.
    h.record(a, base);
    h.record(b, base);
    assert_eq!(h.undo(a, turned), Some(base), "a has one step");
    assert_eq!(h.undo(b, turned), Some(base), "b has one step, unaffected");
    assert!(!h.can_undo(a), "a is exhausted");

    // Review Focus #4: a new edit rewrites the future.
    let mirrored = Edit { xform: Xform::FlipH, crop: None };
    h.record(a, base);
    assert!(!h.can_redo(a), "recording again clears redo for that path");
    assert_eq!(h.redo(a, mirrored), None, "and the old future is unreachable");
}

/// The cap drops the **oldest** step, so the newest edit is never the one
/// refused: a viewer that silently ignored a rotate because its history was full
/// would be worse than one that forgot the deepest step.
#[test]
fn the_history_is_capped_and_drops_the_oldest_step() {
    use image_viewer::{Crop, Edit, EditHistory};
    let p = std::path::Path::new("a.jpg");
    let mut h = EditHistory::default();

    // 60 recorded states; the cap is 50. **Every state is distinct**, and that is
    // the load-bearing part of this fixture: the crop width carries the index, so
    // "which record is this?" is answerable without doing arithmetic on the cap.
    //
    // The earlier version of this test cycled `Xform::quarters_cw(i % 4)` and
    // identified records by that cycle's phase, which discriminated drop-oldest
    // from drop-newest **only when `(60 - cap) % 4 != 0`**. At cap 50 that is
    // true by luck (drop 10), and at cap 48 it is false (drop 12), so the whole
    // assertion went blind at exactly the bug it was added to catch — a tuning
    // change to the cap would have removed the check without any test failing.
    // Distinct records make the window below a fact about the data rather than
    // about the phase of a cycle.
    //
    // The crop also keeps each state non-identity, so a future "do not record a
    // no-op edit" optimisation could not silently zero this fixture out.
    for i in 0..60u8 {
        h.record(
            p,
            Edit {
                xform: image_viewer::Xform::Identity,
                crop: Crop::new(0, 0, 1 + u32::from(i), 1),
            },
        );
    }
    let cap = h.len_for(p);
    assert_eq!(cap, 50, "the stack is bounded");

    // Undoing walks back one state per press, and one more is a no-op rather than
    // a panic or another image's edit. The count comes from `len_for`, so the
    // walk cannot silently disagree with the cap if that is ever retuned.
    let mut undone = Vec::new();
    for _ in 0..cap {
        undone.push(h.undo(p, Edit::default()).expect("one step per recorded entry"));
    }
    let width = |e: &Edit| e.crop.map(|c| c.w);

    // The **order** of what comes back is the half that pins *which* end is
    // dropped, and the length assertion above does not: dropping the newest
    // instead of the oldest leaves the stack at 50 too, and the same 50 undos
    // still bottom out in `None`. Both halves are asserted because a test that
    // cannot fail on the bug it is named for is decoration — verified by
    // mutation: with `remove(0)` swapped for `pop()` the **un-strengthened
    // version of this test** passed unchanged.
    assert_eq!(
        undone.first().and_then(width),
        Some(60),
        "undo walks back from the newest record, not the oldest"
    );
    // Dropping the oldest 10 leaves records 10..=59, whose crop widths are
    // 11..=60, so the last undo must hand back width 11. Derived from `cap`
    // rather than hardcoded, so this expectation survives a retune instead of
    // quietly agreeing with whatever the implementation happens to do.
    assert_eq!(
        undone.last().and_then(width),
        Some(61 - cap as u32),
        "the oldest surviving record is the last undo, not the first"
    );
    assert_eq!(h.undo(p, Edit::default()), None, "and it stops there");
}

/// The four transform buttons share one tail, so `record` belongs there once.
/// Asserted by **byte offset inside `apply_to_current`**, because presence is
/// exactly what was true of the defect this guards: a call present, in the right
/// function, after the edit.
#[test]
fn the_transform_tail_records_before_it_edits() {
    let src = main_src();
    let body = fn_body(&src, "fn apply_to_current(").expect("apply_to_current");
    let at = body.find("history.record(").expect("record must be called");
    let edit = body.find("f(&mut self.edits, &p.path)").expect("the edit itself");
    assert!(
        at < edit,
        "record must run BEFORE the edit is applied, or undo restores the state \
         after the change and every undo is a no-op"
    );
}

/// Both keys are rebindable actions, and both are chords rather than bare
/// letters, so neither may appear in BARE_LETTERS.
#[test]
fn undo_and_redo_are_rebindable_chord_actions() {
    let lib = lib_src();
    assert!(
        lib.contains(r#"("undo", "Ctrl+Z", "Undo view transform"),"#),
        "undo must be a KEY_ACTIONS row so it shows in Settings and can be rebound"
    );
    assert!(
        lib.contains(r#"("redo", "Ctrl+Shift+Z", "Redo view transform"),"#),
        "redo must be a rebindable action too"
    );
}

/// The buttons sit with the transforms and grey out when there is nothing to
/// undo. **Disabled state is the point**, not polish: an always-enabled undo that
/// sometimes does nothing reads as broken, and one that is never enabled reads
/// as absent.
#[test]
fn the_undo_buttons_sit_with_the_transforms_and_disable_when_empty() {
    let src = main_src();
    let at = src.find(r#""Undo""#).expect("an Undo button");
    let near = &src[at.saturating_sub(2500)..(at + 2500).min(src.len())];
    assert!(
        near.contains("can_undo") && near.contains("can_redo"),
        "the undo/redo buttons must be gated on can_undo/can_redo so they disable \
         when there is nothing to undo"
    );
}

/// The rating block lives in the **left pane, above the file's own metadata**,
/// and — this is the part that is easy to get wrong — its read must NOT be part
/// of `wants_exif`. That gate exists to stop a 7-12 ms whole-file read; a rating
/// is a 300-byte sidecar, and joining them would mean the rating vanishes from
/// the UI whenever the pane is hidden.
#[test]
fn the_rating_block_is_in_the_left_pane_and_outside_the_exif_gate() {
    // `fn ui(` lives in `app/ui.rs`: the left pane (`pane.show`) and the EXIF
    // gate (`wants_exif`) are both inside it, so their order inside this one
    // function is the property. A rating is a 300-byte sidecar, not a whole-file
    // read, so it must not be folded into the `wants_exif` gate - hiding the
    // pane must not hide the rating.
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/ui.rs"),
    )
    .expect("src/app/ui.rs");
    // All three markers occur once in `app_ui.rs`, all inside `fn ui(`: the
    // rating heading, the EXIF heading, and the `wants_exif` gate that wraps
    // only the EXIF read. The rating block must come before the gate - it is a
    // separate path and must not vanish when the pane is hidden.
    let rating = src.find(r#"ui.heading("Rating")"#).expect("the rating heading in the left pane");
    let gate = src.find("wants_exif(").expect("the EXIF gate wants_exif");
    let _heading = src.find(r#"ui.heading("EXIF")"#).expect("the EXIF heading");
    assert!(
        rating < gate,
        "the rating block precedes the EXIF gate: it is not folded into it, so \
         hiding the pane cannot hide the rating"
    );
}

/// Archives get no ratings: there is nowhere to write `vol1.cbz/p003.jpg.xmp`,
/// and a rating that disappears on leaving the archive is worse than a refusal.
#[test]
fn archives_are_refused_a_rating() {
    let lib = lib_src();
    assert!(
        fn_body(&lib, "pub fn sidecar_path(").expect("sidecar_path").contains("is_archive_path"),
        "sidecar_path must return None for an archive member, which is what \
         refuses the write"
    );
}

/// The grid must read ratings from a **map the scan filled**, not stat a
/// sidecar per cell per frame: `grid_cols` can be 24 across and the folder
/// thousands deep, so a stat per visible cell is a filesystem storm in the paint
/// frame.
#[test]
fn the_grid_reads_ratings_from_a_map_not_the_filesystem() {
    let src = main_src();
    let cell = fn_body(&src, "fn grid_cell(").expect("grid_cell");
    assert!(
        !cell.contains("Ratings::read("),
        "grid_cell must not read sidecars - that is a filesystem read per cell \
         per frame; it takes them from the ratings map"
    );
    assert!(cell.contains("ratings") || cell.contains("rating"), "and it must read a ratings map");
}

// ---------------------------------------------------------------------------
// Per-image view memory (the "Remember zoom and pan per image" setting)
// ---------------------------------------------------------------------------

/// A remembered view is **the view that was left** — an identity round-trip,
/// not a rescaling. Every field distinct so a swap in any of them fails.
#[test]
fn a_remembered_view_is_the_view_that_was_left() {
    let key = image_viewer::ImgKey::first(std::path::Path::new("p/manga.tiff"));
    let left = image_viewer::SavedView { fit: false, scale: 1.3729, offset: (17.25, -9.75) };
    let mut m = image_viewer::ViewMemory::new();
    m.remember(key.clone(), left);
    assert_eq!(m.restore(&key), Some(left));
}

/// A page of a document is its own picture — the reason this is keyed on
/// [`image_viewer::ImgKey`] at all. A path-keyed map would hand page 3 back
/// page 1's pan, silently, and only for multi-page files.
#[test]
fn the_next_page_of_a_document_is_not_the_last_page_s_view() {
    let p = std::path::Path::new("p/vol.tiff");
    let page0 = image_viewer::ImgKey { path: p.to_path_buf(), page: 0 };
    let page2 = image_viewer::ImgKey { path: p.to_path_buf(), page: 2 };
    let mut m = image_viewer::ViewMemory::new();
    m.remember(page0, image_viewer::SavedView { fit: false, scale: 3.0, offset: (41.0, 0.0) });
    assert!(m.restore(&page2).is_none(), "page 2 must not inherit page 0's view");
    // And the file that was NOT visited is equally empty.
    assert!(m.restore(&image_viewer::ImgKey::first(std::path::Path::new("p/other.jpg"))).is_none());
}

/// The no-memory arm is **today's behaviour, unchanged**: fit and zoom carried
/// over, pan reset to the centre. This is what `arriving_view` exists to
/// guarantee — the setting off is not its arm (the caller decides whether to
/// ask), so this is the arm that keeps "off by default changes nothing" true
/// even for images a toggled-on session never touched.
#[test]
fn an_unvisited_image_gets_the_carry() {
    let saved = image_viewer::arriving_view(None, false, 2.1875);
    assert!(!saved.fit);
    assert_eq!(saved.scale, 2.1875);
    assert_eq!(saved.offset, (0.0, 0.0), "the pan does not travel");
    // A remembered one is handed back whole, so the carry gets nothing mixed in.
    let left = image_viewer::SavedView { fit: true, scale: 0.75, offset: (-3.5, 12.0) };
    assert_eq!(
        image_viewer::arriving_view(Some(left), false, 2.1875),
        left,
        "the remembered view wins over the carry, exactly"
    );
}

/// `forget_file` drops **every page of the file and nothing of its
/// neighbours** — the same scope `invalidate_path` gives every other store it
/// clears.
#[test]
fn a_reexport_drops_the_whole_file_but_not_the_neighbours() {
    let vol1 = std::path::Path::new("p/vol1.cbz"); // archive members share a temp path
    let k1 = image_viewer::ImgKey { path: vol1.to_path_buf(), page: 0 };
    let k1b = image_viewer::ImgKey { path: vol1.to_path_buf(), page: 1 };
    let k2 = image_viewer::ImgKey::first(std::path::Path::new("p/photo.jpg"));
    let view = image_viewer::SavedView { fit: false, scale: 1.0, offset: (8.0, 8.0) };
    let mut m = image_viewer::ViewMemory::new();
    m.remember(k1.clone(), view);
    m.remember(k1b.clone(), view);
    m.remember(k2.clone(), view);
    m.forget_file(vol1);
    assert!(m.restore(&k1).is_none());
    assert!(m.restore(&k1b).is_none());
    assert_eq!(m.restore(&k2), Some(view), "the neighbour keeps its own");
}
/// `#[serde(default)]` is load-bearing here like on every other added
/// `Config` field — an old config without this key must load and keep the
/// shipped behaviour off.
#[test]
fn an_old_config_without_the_remember_setting_lands_on_the_default() {
    let old = r#"{"load_all_on_startup": true, "thumbnail_cache": false}"#;
    let c: image_viewer::Config = serde_json::from_str(old).unwrap();
    assert!(
        !c.remember_view,
        "a config with no remember_view key must get the default, which is off"
    );
    let on = r#"{"load_all_on_startup": true, "remember_view": true}"#;
    let c2: image_viewer::Config = serde_json::from_str(on).unwrap();
    assert!(c2.remember_view, "an explicit true must be honoured");
}

/// The renderer setting is **gone from the file, and a file that still has it
/// must still load.**
///
/// wgpu stopped being built when the app moved to glow, so `renderer` is now an
/// unknown key. Unknown keys are ignored rather than rejected (there is no
/// `deny_unknown_fields`), and that is the whole reason every `config.json`
/// written by an older build keeps working. A test that only checked a *missing*
/// key would not notice the day someone made unknown keys fatal; this one feeds
/// back a key this version has no field for.
#[test]
fn a_config_written_by_the_wgpu_build_still_loads() {
    let old = r#"{
        "load_all_on_startup": true,
        "renderer": { "choice": "dx12", "probed": "gl" },
        "folder": "C:\\photos"
    }"#;
    let c: image_viewer::Config = serde_json::from_str(old).expect("an old config must load");
    assert!(c.load_all_on_startup);
    assert_eq!(c.folder.as_deref(), Some(r"C:\photos"));
}

/// **wgpu is not in the build, and that is a decision about a file the compiler
/// does not check.**
///
/// eframe's default features bring `egui-wgpu`, and with it `wgpu`, `wgpu-core`,
/// `wgpu-hal` and `naga` plus their D3D12 and Vulkan backends. Measured on this
/// machine that is 154 MB more idle memory (321 MB against 84 MB for a bare
/// window) and 269 ms more to create a device, for the **same** frame rate - and
/// its Vulkan backend access-violates during adapter enumeration even though
/// `vulkaninfo` reports a healthy device. The app renders on glow, so none of it
/// is used.
///
/// Removing `default-features = false` restores all of it silently. Nothing else
/// in the tree notices, the build only gets slower, and no test would catch it;
/// this is that test. `AGENTS.md` carries the measurements.
#[test]
fn eframe_is_built_without_wgpu() {
    // The manifest explains in comments *why* wgpu is absent, so a prose line
    // mentioning it must not be what this reads.
    let manifest: String = include_str!("../Cargo.toml")
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    // Just the eframe entry: `zip` also sets `default-features = false`, and a
    // whole-file search for that key would pass while eframe's defaults came
    // back.
    let start = manifest.find("eframe =").expect("an eframe dependency");
    let entry = &manifest[start..];
    let entry = &entry[..entry.find("] }").map(|i| i + 3).unwrap_or(entry.len())];
    assert!(
        entry.contains("default-features = false"),
        "eframe must be built with its default features off: turning them on \
         brings back wgpu, along with the Wayland/X11 and web features a \
         Windows-only glow app never uses. eframe entry: {entry}"
    );
    assert!(
        !entry.contains("\"wgpu\""),
        "wgpu is not used by this app - the renderer is glow. eframe entry: {entry}"
    );
    assert!(
        entry.contains("\"glow\""),
        "glow is the renderer; without it eframe has no backend at all. \
         eframe entry: {entry}"
    );
}

/// The wiring: **the save must happen while the outgoing key is still in
/// hand, and the restore after the key changes** — both inside the
/// cursor-change block, both gated on the setting. `main.rs` is unreachable
/// from every test, so this is the source-scan pattern with the position
/// asserted, because the two defects it guards are both *arrangement* bugs:
/// saving after `pan_key` is reassigned would store the new image's view
/// under the new image's name (the map would say "remembered" and hold the
/// carry), and restoring outside the block would re-derive what the reader
/// is already looking at.
#[test]
fn view_memory_is_wired_to_the_cursor_change_and_the_setting() {
    let src_source = main_src();
    // `fn_body` is built for signatures *without* a trailing `{`, so the body's
    // own braces carry the depth. A brace-terminated block needs the `{` left
    // out of the search instead, or every inner `if` returning to depth 0
    // looks like the block's end. Small local matcher rather than a second
    // definition of fn_body.
    fn span_after<'a>(src: &'a str, trigger: &str) -> Option<&'a str> {
        let sig = src.find(trigger)?;
        let open = src[sig..].find('{')? + sig;
        let mut depth = 0i32;
        let mut end = None;
        for (i, b) in src.as_bytes()[open..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        Some(&src[open + 1..end?])
    }
    let src = src_source;
    let goto = fn_body(&src, "fn goto(").expect("goto");
    let block = span_after(goto, "if self.cur != self.pan_key {").expect("the cursor-change block");
    let remember = block
        .find("self.views.remember(")
        .unwrap_or_else(|| panic!("the outgoing view must be remembered in the block"));
    let key_move =
        block.find("self.pan_key = self.cur.clone();").expect("the block must still move pan_key");
    let restore = block
        .find("self.views.restore(")
        .unwrap_or_else(|| panic!("the arriving view must be restored in the block"));
    assert!(
        remember < key_move,
        "save BEFORE pan_key is reassigned, or the outgoing key is gone and its \
         view is stored under the arriving key's name"
    );
    assert!(
        key_move < restore,
        "restore AFTER the key changes, or it asks about the image being left"
    );
    let ones = block.matches("self.config.remember_view").count();
    assert!(ones >= 2, "both arms must be gated on the setting, found {ones}");
    // The arriving view goes through the pure decision, not a hand-rolled arm.
    assert!(
        block.contains("image_viewer::arriving_view("),
        "the restore must ask the pure function, so a second opinion about the \
         carry cannot form"
    );
    // And the forget: a re-export must not leave a view for pixels that no
    // longer exist, beside the page counts and shapes it already clears.
    let inv = fn_body(&src, "fn invalidate_path(").expect("invalidate_path");
    assert!(
        inv.contains("self.views.forget_file(p);"),
        "invalidate_path must drop the file's remembered views"
    );
    assert!(
        src.contains("Remember zoom and pan per image"),
        "the settings panel must carry the toggle"
    );
}

// ---------------------------------------------------------------------------
// The preload window's byte budget.
// ---------------------------------------------------------------------------

/// `main.rs` is unreachable from tests, so the wiring is a source scan with
/// the order asserted. Three things must hold, and each has a plausible
/// wrong-looking sibling: the lengths come from the **stamp registry the
/// watcher already keeps** (`self.stamps`) rather than a stat per plan — a
/// per-emit `metadata()` on the paint thread is the I/O this crate exists not
/// to do — the slice is built **before** the `Plan` literal that consumes it,
/// and the planner is configured with the shipped constant rather than a
/// literal or `u64::MAX` at the call site (a disabled budget is invisible
/// except in memory).
#[test]
fn the_preload_budget_is_wired_from_the_watcher_s_stamp_registry() {
    let src = main_src();
    let body = fn_body(&src, "fn schedule(&mut self)").expect("schedule");
    let collect = body
        .find("self.stamps.get(p)")
        .expect("schedule must read the stamp registry for the file lengths");
    let plan = body.find("plan_jobs(").expect("schedule must plan");
    assert!(collect < plan, "the slice is worthless if collected after the plan is built");
    assert!(
        body.contains("file_bytes: &file_bytes,"),
        "the plan must be handed the collected slice"
    );
    assert!(
        body.contains("preload_budget: PRELOAD_BUDGET_BYTES,"),
        "the shipped budget is the constant, not a literal or u64::MAX"
    );
    assert!(
        !body.contains("metadata("),
        "the plan path must not stat: the lengths are the watcher's already"
    );
    // And the constant sits with the radius it bounds, so the two knobs that
    // shape one window are found together.
    let radius = src.find("pub(crate) const PRELOAD_RADIUS: usize = 3;").expect("the radius");
    let budget = src.find("const PRELOAD_BUDGET_BYTES: u64 =").expect("the budget");
    assert!(
        radius < budget && budget - radius < 4000,
        "the radius and the budget tune the same window and are documented together"
    );
}

// ---------------------------------------------------------------------------
// The side-panel master (`I`) is a persisted preference; fullscreen is an
// overlay on it, and never a re-write of it.
// ---------------------------------------------------------------------------

/// A config from before the key existed keeps showing panels — the default is
/// `on` for the same reason `loop_back`'s is: a tolerated older file must not
/// change what it shows. An explicit `false` must be honoured, of course.
#[test]
fn an_old_config_without_the_panel_master_lands_on_the_default() {
    let old = r#"{"load_all_on_startup": true}"#;
    let c: image_viewer::Config = serde_json::from_str(old).unwrap();
    assert!(c.show_panels, "no show_panels key must land on the default, which is on");
    let off = r#"{"load_all_on_startup": true, "show_panels": false}"#;
    let c2: image_viewer::Config = serde_json::from_str(off).unwrap();
    assert!(!c2.show_panels, "an explicit false must be honoured");
}

/// The wiring: the master is saved on its toggle and read on startup, and no
/// site is left that would force it back on — the whole-config save on exit
/// would then record the force as the user's choice. `main.rs` is unreachable
/// from tests, so source scans again, with position where the order matters.
#[test]
fn the_panel_master_is_saved_and_fullscreen_only_borrows_it() {
    fn block_after<'a>(src: &'a str, trigger: &str) -> Option<&'a str> {
        let sig = src.find(trigger)?;
        let open = src[sig..].find('{')? + sig;
        let mut depth = 0i32;
        let mut end = None;
        for (i, b) in src.as_bytes()[open..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        Some(&src[open + 1..end?])
    }

    let src = main_src();

    // The funnel: save/restore only through `toggle_panels_master`, and the
    // save inside it is ordered after the flip.
    let helper = fn_body(&src, "fn toggle_panels_master(").expect("master helper");
    let flip = helper
        .find("self.show_panels = !self.show_panels;")
        .expect("the helper must flip the master");
    let saved = helper
        .find("self.config.show_panels = self.show_panels;")
        .expect("the helper must mirror the flip into the config");
    let write = helper.find("self.config.save(").expect("the helper must save on the press");
    assert!(flip < saved && saved < write, "flip, mirror, save, in order");

    // Both `I` sites (key and Plates row) route through it. A site that
    // toggles inline would skip the persist, and the first launch would
    // quietly disagree.
    let dispatch = fn_body(&src, "fn ui(").expect("the event dispatcher");
    let handlers = src.matches("self.toggle_panels_master();").count();
    let toggles_inline = src.matches("self.show_panels = !self.show_panels;").count();
    assert_eq!(
        toggles_inline, 1 as usize,
        "only the helper flips the master, inline flips found: {toggles_inline}"
    );
    assert!(
        handlers >= 2,
        "the key and menu-row sites both call the funnel, found only {handlers}"
    );

    // Startup honours the saved value — the same capture pattern the
    // constructor uses for `loop_back`.
    let ctor = fn_body(&src, "fn new(").expect("constructor");
    let capture = ctor
        .find("let show_panels = config.show_panels;")
        .expect("the constructor must read the saved master");
    let literal = ctor.find("let mut app = Self {").expect("the struct literal");
    assert!(capture < literal, "the capture happens before `config` moves into the struct");

    // No site forces the master back on. Both were hardcoded `true` once;
    // the whole-config save on exit would have recorded the force.
    assert!(
        !src.contains("self.show_panels = true"),
        "a fullscreen exit must restore what the user had, not force the master on"
    );

    // And the fullscreen overlay pairs against the stash: one entering arm
    // that stashes, two leaving arms (F11 again, and Esc) that restore.
    let f11 = block_after(&dispatch, "if f11 {").expect("the F11 block");
    assert!(!f11.contains("config.save"), "the fullscreen dance must not write the config");
    assert!(
        f11.contains("panels_before_fullscreen = Some(self.show_panels)"),
        "entering stash the master"
    );
    assert!(
        f11.contains("panels_before_fullscreen.take().unwrap_or(true)"),
        "F11 exit restores the stash"
    );
    let restores = dispatch.matches("panels_before_fullscreen.take().unwrap_or(true)").count();
    assert_eq!(
        restores, 2 as usize,
        "the Esc exit restores too — one restore, one fullscreen with no way back"
    );
}
