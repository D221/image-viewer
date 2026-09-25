use image_viewer::{load_image, Gallery, ZoomPan};

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
    let out = image_viewer::downscale_image(&data, 8);
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
    let out = image_viewer::downscale_image(&data, 7);
    assert_eq!((out.width, out.height), (7, 7));
    assert_eq!(out.rgba.len(), 7 * 7 * 4);
    // Values stay in gamut.
    for p in out.rgba.chunks_exact(4) {
        assert!(p[3] == 255);
 cosy_check(&p[0..3]);
    }
}

fn cosy_check(c: &[u8]) {
    for v in c {
        assert!(*v <= 255, "channel out of range: {v}");
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
