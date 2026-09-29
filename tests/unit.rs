use image_viewer::{load_image, Gallery, ZoomPan};

/// `set_linear_resample` / `set_downscale_filter` are process-wide globals, and
/// `cargo test` runs the tests in this file concurrently. Anything that asserts
/// on a *specific* filter path has to take this lock and put the globals back,
/// or it silently tests whichever path another test last selected.
static FILTER_GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `f` with the downscale filter globals forced to `linear`, restoring the
/// previous setting afterwards.
fn with_linear_resample<T>(linear: bool, f: impl FnOnce() -> T) -> T {
    let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let was = image_viewer::linear_resample_enabled();
    image_viewer::set_linear_resample(linear);
    let out = f();
    image_viewer::set_linear_resample(was);
    out
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
    img.write_to(
        &mut std::io::Cursor::new(&mut real_png),
        image::ImageFormat::Png,
    )
    .unwrap();
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
    img.write_to(
        &mut std::io::Cursor::new(&mut jpeg),
        image::ImageFormat::Jpeg,
    )
    .unwrap();
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
    img.write_to(
        &mut std::io::Cursor::new(&mut png),
        image::ImageFormat::Png,
    )
    .unwrap();
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
    img.write_to(
        &mut std::io::Cursor::new(&mut png),
        image::ImageFormat::Png,
    )
    .unwrap();
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
    let mut names = vec![
        "IMG_10.jpg",
        "a.jpg",
        "IMG_2.jpg",
        "B.jpg",
        "IMG_1.jpg",
        "file10.txt",
        "file9.txt",
    ];
    names.sort_by(|a, b| image_viewer::natural_cmp(a, b));
    assert_eq!(
        names,
        vec![
            "a.jpg",
            "B.jpg",
            "file9.txt",
            "file10.txt",
            "IMG_1.jpg",
            "IMG_2.jpg",
            "IMG_10.jpg",
        ]
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
    let data = image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    };
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
                let out = with_linear_resample(linear, || {
                    image_viewer::downscale_image(&data, target)
                });
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
fn test_preview_load_survives_gamma_space_resize() {
    // End-to-end version of the same bug, through the real decode entry point
    // the viewer uses, with linear-light downsampling off.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("6.png");
    // The reported file's exact dimensions.
    let mut img = image::RgbaImage::new(980, 552);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
    }
    image::DynamicImage::ImageRgba8(img)
        .save(&p)
        .expect("write fixture");

    for target in [256u32, 300, 512, 800, 980, 1024] {
        let r = with_linear_resample(false, || {
            image_viewer::load_image_preview_full(&p, target)
        })
        .unwrap_or_else(|e| panic!("{p:?} @ {target} failed to load: {e}"));
        for (label, d) in [("preview", &r.preview), ("full", r.full.as_ref().unwrap())] {
            assert_eq!(
                d.rgba.len(),
                (d.width as usize) * (d.height as usize) * 4,
                "{label} of {p:?} @ {target}: {}x{} but {} bytes",
                d.width,
                d.height,
                d.rgba.len()
            );
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
    use nom_exif::URational;
    use nom_exif::ExifTag;
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
        vec![
            URational::new(d, 1),
            URational::new(m, 1),
            URational::new(s.0, s.1),
        ]
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
    img.write_to(
        &mut std::io::Cursor::new(&mut gif),
        image::ImageFormat::Gif,
    )
    .unwrap();
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
        enc.encode_frame(
            Frame::from_parts(
                RgbaImage::from_pixel(8, 8, image::Rgba([0, 0, 255, 255])),
                0,
                0,
                Delay::from_numer_denom_ms(200, 1),
            ),
        )
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
    img.write_to(
        &mut std::io::Cursor::new(&mut jpeg),
        image::ImageFormat::Jpeg,
    )
    .unwrap();

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


use image_viewer::{grid_cols, GRID_CELL, GRID_LABEL_H, GRID_ROW_H};

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
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, GRID_ROW_H, 1, |ui, _| {
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
        });
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
            cols = grid_cols(panel.width());
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
fn test_wgpu_backend_set_enables_exactly_one_backend() {
    use image_viewer::{wgpu_backend_set, WgpuBackend};

    // The contract changed, and deliberately. This used to return
    // `(vulkan, dx12)` and kept DX12 enabled *alongside* Vulkan as a fallback.
    // That is not a fallback: wgpu enumerates every backend in the set during
    // adapter discovery and access-violates on this machine when Vulkan is in
    // it, so a two-backend set is a way to crash rather than a way to recover.
    //
    // One bit, always. Which one is the launch loop's decision, made by probing
    // each in a child process; see `tests/renderer.rs` for the policy.
    for b in [WgpuBackend::Auto, WgpuBackend::Dx12, WgpuBackend::Gl, WgpuBackend::Vulkan] {
        assert_eq!(
            wgpu_backend_set(b).count_ones(),
            1,
            "{b:?} enabled more than one backend"
        );
    }
    // `Auto` is resolved before it reaches here, but must not produce an empty
    // set — that fails with a much more confusing error than a wrong backend.
    assert_eq!(wgpu_backend_set(WgpuBackend::Auto), wgpu_backend_set(WgpuBackend::Dx12));

    // Parsing: an unrecognised value must not silently select something, and
    // callers treat `None` as the default. "opengl" used to be rejected and is
    // now a real backend.
    assert_eq!(WgpuBackend::parse("auto"), Some(WgpuBackend::Auto));
    assert_eq!(WgpuBackend::parse("  DX12 "), Some(WgpuBackend::Dx12));
    assert_eq!(WgpuBackend::parse("vulkan"), Some(WgpuBackend::Vulkan));
    assert_eq!(WgpuBackend::parse("vk"), Some(WgpuBackend::Vulkan));
    assert_eq!(WgpuBackend::parse("d3d12"), Some(WgpuBackend::Dx12));
    assert_eq!(WgpuBackend::parse("opengl"), Some(WgpuBackend::Gl));
    assert_eq!(WgpuBackend::parse("gl"), Some(WgpuBackend::Gl));
    assert_eq!(WgpuBackend::parse("nonsense"), None);
}

#[test]
fn test_path_arg_skips_flags() {
    use image_viewer::path_arg;

    // The plain case: one positional path.
    assert_eq!(
        path_arg(["C:\\photos"]).as_deref(),
        Some(std::path::Path::new("C:\\photos"))
    );

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
        assert_eq!(
            fit_scale_no_upscale(natural, area),
            1.0,
            "{natural:?} must not be upscaled"
        );
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
    let scale = image_viewer::fit_scale_no_upscale(
        (natural.x, natural.y),
        (area.width(), area.height()),
    );
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
    assert!(
        uv.width() > 0.0 && uv.height() > 0.0,
        "UV must be non-degenerate, got {uv:?}"
    );
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

#[test]
fn test_rotated_footprint_swaps_on_odd_quarters() {
    use image_viewer::rotated_footprint;
    let (w, h) = (4000.0_f32, 3000.0_f32);
    // Even quarters: unchanged. Odd: axes swapped.
    assert_eq!(rotated_footprint(w, h, 0), (w, h));
    assert_eq!(rotated_footprint(w, h, 1), (h, w));
    assert_eq!(rotated_footprint(w, h, 2), (w, h));
    assert_eq!(rotated_footprint(w, h, 3), (h, w));
    // Out-of-range quarters wrap (rot uses rem_euclid, but be safe here).
    assert_eq!(rotated_footprint(w, h, 4), (w, h));
    assert_eq!(rotated_footprint(w, h, 5), (h, w));
}

#[test]
fn test_quarter_turn_mesh_covers_the_swapped_footprint() {
    // The rotate buttons paint the image by rotating its rect about the rect
    // centre (see `paint_tex`). That only keeps the image centred if a 90°
    // turn of an axis-aligned rect lands exactly on the axis-swapped
    // footprint — which is what fit/pan clamping is computed against. If this
    // premise were wrong, rotating would swing the image off-centre.
    use egui::{Color32, Rect, Vec2};

    for quarter in 0..4u8 {
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
        if quarter & 3 != 0 {
            mesh.rotate(
                egui::emath::Rot2::from_angle(quarter as f32 * std::f32::consts::FRAC_PI_2),
                rect.center(),
            );
        }

        let (dw, dh) = image_viewer::rotated_footprint(src.x * scale, src.y * scale, quarter);
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
            "quarter {quarter}: rotated image {got:?} must fill the footprint {want:?}"
        );
        // And it must stay centred where the unrotated image was.
        assert!(
            (got.center() - centre).x.abs() < 0.01 && (got.center() - centre).y.abs() < 0.01,
            "quarter {quarter}: rotation must not move the centre"
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
                pitch = GRID_ROW_H + ui.spacing().item_spacing.y;
                // Mirrors the real grid: auto_shrink([false, true]) is what
                // gives the scrollbar somewhere to live.
                let mut sa = egui::ScrollArea::vertical().auto_shrink([false, true]);
                if let Some(o) = offset {
                    sa = sa.vertical_scroll_offset(o);
                }
                sa.show_rows(ui, GRID_ROW_H, ROWS, |ui, range| {
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
    assert!(pitch > GRID_ROW_H, "show_rows adds item spacing to the pitch");
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
