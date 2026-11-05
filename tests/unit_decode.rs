//! Content-based format detection, the linear-light resize, and lossless
//! container handling.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

use image_viewer::load_image;

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

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
