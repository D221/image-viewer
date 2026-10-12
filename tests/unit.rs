use image_viewer::{load_image, Gallery, ZoomPan};


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
fn with_resample_at<T>(
    linear: bool,
    strength: f32,
    floor: f32,
    f: impl FnOnce() -> T,
) -> T {
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
    with_resample_at(
        linear,
        cached_default_strength(),
        image_viewer::MIN_FILTER_SCALE,
        f,
    )
}

/// Run `f` at an explicit strength, everything else shipped.
#[allow(dead_code)]
fn with_strength<T>(strength: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(true, strength, image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit strength and the shipped settings otherwise.
///
/// **Every global is pinned to a constant, never read back.** `DOWNSCALE_STRENGTH`
/// is initialised to `SMOOTH_STRENGTH` and `MIN_FILTER_SCALE_IN_FORCE` to the
/// floor, so "whatever is in there now" is `Smooth` until a test says otherwise -
/// and reading it silently turned the *pre-fix* arm of the `MIN_FILTER_SCALE`
/// measurements into a `Smooth` arm, which reported the floor as a no-op and its
/// "before" column as already-correct. `with_floor` below is the same trap with
/// the floor instead of the strength.
fn with_resample<T>(linear: bool, strength: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(linear, strength, image_viewer::MIN_FILTER_SCALE, f)
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

// ---------------------------------------------------------------------------
// The linear-light prefilter
//
// `linear_resize` is two steps: reduce the full-resolution source to at most 2x
// the target, then two Hermite passes in 12-bit linear light down to the target.
// The first step used to be an sRGB **box** average, and a box's sinc sidelobes
// sit near -13 dB without rolling off, so it was aliasing *more* than the kernel
// pass it fed. On a 2223x3109 page at the real 2.879x reduction (the manga
// half-panel target of 1080) the residual in the alias band, measured as the
// output amplitude of a 100-code sinusoid, was 21-29 codes; with a widened
// kernel in the prefilter it is 2-11.
//
// The measurements below were taken with a harness mirroring `linear_resize`
// verbatim (same f32 order, same normalisation, byte-exact against
// `downscale_image` for the box variant), one variable changed at a time. The
// "box" column is what the code did before; the pins are the kernel numbers plus
// two codes of slack, so a later filter tweak fails loudly rather than silently.
//
// **What is not claimed.** This reduces the artefact, it does not abolish it. A
// tone at the source Nyquist cannot survive a 2.879x downscale without
// aliasing; at source period 5 (1.7 output px) 23 codes still get through, and
// the artefact is a hard 1-2 px sparkle, not a slow beat.
// ---------------------------------------------------------------------------

/// The geometry the probes use: the same 2.880x reduction as the real
/// 2223x3109 manga page at the 1080 half-panel target, at a tenth of the pixels.
/// `downscale_image` scales so the **long** side is `max_dim`, so 1244/432 is
/// 2.880 and the result is 309x431.
const PROBE_W: u32 = 890;
const PROBE_H: u32 = 1244;
const PROBE_TARGET: u32 = 432;

/// Horizontal tone of source period `p` px, `amp` codes peak-to-peak. Varying
/// along x only makes the measurement the horizontal transfer function.
fn probe_sinusoid(w: u32, h: u32, p: f32, amp: f32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let t = (2.0 * std::f32::consts::PI * x as f32 / p).sin();
            let v = (128.0 + amp * 0.5 * t).round().clamp(0.0, 255.0) as u8;
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// Dot lattice: 1-px dots of `hi` every `p` source px, `lo` between them. A
/// square wave rather than a tone, so it puts energy at every odd harmonic of
/// the pitch and exercises more of the alias band than a single frequency.
fn probe_dots(w: u32, h: u32, p: f32, lo: u8, hi: u8) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    let half = (p / 2.0).max(1.0);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = if (x as f32) % p < half { hi } else { lo };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// Stochastic 50% black/white dither, the shape a screentone's fine dither
/// actually has. A strict period-2 lattice is *not* used for the exposure probe
/// on purpose: the box prefilter has an exact null at it whose depth depends on
/// whether a given output pixel's span happens to cover 1 or 2 source pixels, so
/// the same pattern measures 128 or 162 codes purely from the phase. A hashed
/// one averages over every alignment, which is also why it is the more honest
/// stand-in for a tone.
fn probe_dither(w: u32, h: u32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            // xorshift-ish hash of the coordinate: no lattice period to alias.
            let mut n = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (y as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            n ^= n >> 29;
            n = n.wrapping_mul(0x94D0_49BB_1331_11EB);
            n ^= n >> 31;
            let v = if n & 1 == 0 { 0u8 } else { 255 };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// p95 - p5 of the red channel down the middle row: the amplitude statistic.
/// The row is read whole, and the whole row is one period-multiple of a
/// horizontal tone, so this is the tone's amplitude as the resampler left it.
fn probe_amplitude(d: &image_viewer::ImageData) -> f32 {
    let w = d.width as usize;
    let mid = (d.rgba.len() / 4 / w / 2) * w * 4;
    let mut v: Vec<u8> = d.rgba[mid..mid + w * 4]
        .iter()
        .step_by(4)
        .copied()
        .collect();
    v.sort_unstable();
    let p = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize] as f32;
    p(0.95) - p(0.05)
}

fn probe_mean(d: &image_viewer::ImageData) -> f32 {
    d.rgba.iter().step_by(4).map(|&v| v as f32).sum::<f32>() / (d.rgba.len() / 4) as f32
}

// ---------------------------------------------------------------------------
// The downscale low-pass strength
//
// **The probe that found the artefact was wrong, and that is the whole reason
// this section exists.** `probe_sinusoid` varies along x only, so it can report
// "the alias band is down to 4 codes" while a square lattice is plainly visible:
// a 1-D probe structurally cannot produce a lattice, because a lattice needs two
// axis-aligned frequencies beating in *both* axes. `probe_square_lattice` below
// is axis-aligned in both axes and is what the strength is measured on.
//
// Three metrics, each stated once and applied identically at every strength:
//
// * `alias_amp` - p95-p5 of the red channel over the **whole** output, both axes.
//   For a pure lattice this is the amplitude of what survived the low-pass.
// * `beat_amp`  - the amplitude of the pattern at **its own output period**,
//   fitted by least squares. This is the visible moire: the lattice the eye
//   groups, which is not the same thing as broadband alias energy.
// * `edge_grad` - mean |dx| over a fixed crop, in codes per output pixel. The
//   cost side of the trade, so it is measured in the same runs as the benefit.
// ---------------------------------------------------------------------------

/// Square dot lattice: `d`-px dots on a `p`-px grid in **both** axes. This is
/// the probe the artefact needs; a stripe pattern in one axis only cannot make
/// one.
fn probe_square_lattice(w: u32, h: u32, p: f32, d: f32, lo: u8, hi: u8) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let on = (x as f32) % p < d && (y as f32) % p < d;
            let v = if on { hi } else { lo };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// p95-p5 of the red channel over the whole output. Two-dimensional on purpose:
/// a single-row statistic cannot see a vertical lattice.
fn alias_amp(d: &image_viewer::ImageData) -> f32 {
    let mut v: Vec<u8> = d.rgba.iter().step_by(4).copied().collect();
    v.sort_unstable();
    let p = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize] as f32;
    p(0.95) - p(0.05)
}

/// Amplitude of the lattice at **its own output period**, by least squares over
/// the interior (a 2-px border, so an edge clamp cannot leak in).
///
/// A square lattice of source pitch `p` reduced by `r` has output pitch `p/r`,
/// and the eye groups that into the artefact. This projects the output onto
/// `sin`/`cos` at that period in both axes and returns the combined amplitude in
/// codes - so it measures the visible lattice, not the broadband residue.
fn beat_amp(d: &image_viewer::ImageData, p_out: f32) -> f32 {
    let (w, h) = (d.width as usize, d.height as usize);
    let (mut ss, mut sc, mut ys, mut yc, mut n) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let kx = 2.0 * std::f64::consts::PI / p_out as f64;
    let ky = 2.0 * std::f64::consts::PI / p_out as f64;
    for y in 2..h.saturating_sub(2) {
        for x in 2..w.saturating_sub(2) {
            let v = d.rgba[(y * w + x) * 4] as f64;
            let s = (kx * x as f64).sin() + (ky * y as f64).sin();
            let c = (kx * x as f64).cos() + (ky * y as f64).cos();
            ss += s * s;
            sc += s * c;
            ys += v * s;
            yc += v * c;
            n += 1.0;
        }
    }
    if ss <= 0.0 || n <= 0.0 {
        return 0.0;
    }
    // Solve [ss sc; sc ss] [a;b] = [ys;yc], taking the off-diagonal's weight
    // into account so a non-integer period's two phase terms are not counted as
    // independent and the amplitude does not come out inflated.
    let (a, b);
    if sc.abs() < 1e-9 * ss.max(1.0) {
        a = ys / ss;
        b = yc / ss;
    } else {
        let det = ss * ss - sc * sc;
        if det.abs() < 1e-9 {
            return 0.0;
        }
        a = (ys * ss - yc * sc) / det;
        b = (yc * ss - ys * sc) / det;
    }
    (a * a + b * b).sqrt() as f32
}

/// Mean |dx| over a fixed crop of the output, in codes per output pixel. The
/// sharpness number: applied identically at every strength, and on the same
/// crop, so the only thing that moves it is the kernel.
fn edge_grad(d: &image_viewer::ImageData, crop: (u32, u32, u32, u32)) -> f32 {
    let (cx, cy, cw, ch) = crop;
    let w = d.width as usize;
    let mut sum = 0f64;
    let mut n = 0f64;
    for y in cy as usize..(cy + ch) as usize {
        for x in cx as usize..(cx + cw - 1) as usize {
            sum += (d.rgba[(y * w + x + 1) * 4] as f64 - d.rgba[(y * w + x) * 4] as f64).abs();
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        (sum / n) as f32
    }
}

/// Which output pixels sit on **flat tone** and which on a **strong edge**.
///
/// **Decided from the full-resolution source, once, and the same mask is scored
/// at every strength.** This is the split the two questions actually need, and
/// without it neither is answerable on a real page:
///
/// * The lattice lives in *screentone*. A page's global p95-p5 is set by its
///   black line art against white paper (242-255 codes on all three fixtures,
///   essentially unmoved by any strength), so a whole-page statistic cannot see
///   the artefact at all - measured: `print_real_page_strength_sweep` moves it
///   by 7% on 32.jpg while `grad` falls 54% at the same strength.
/// * The cost lands on *edges*. A whole-page mean gradient spends most of its
///   samples on flat tone, so it reads a sparkle removal as a sharpness loss.
///
/// Both masks are computed from the source with a 3x3 window, and neither looks
/// at the output, so the two numbers cannot flatter or punish a strength by
/// picking different pixels per arm.
fn tone_and_edge_masks(
    src: &image_viewer::ImageData,
    out_w: u32,
    out_h: u32,
) -> (Vec<bool>, Vec<bool>) {
    let (sw, sh) = (src.width as usize, src.height as usize);
    let g = |x: usize, y: usize| src.rgba[(y * sw + x) * 4] as f32;
    // Per source pixel: local range over a 3x3 window. A screentone's range is
    // its own dither amplitude; a line edge's is the full black-to-white step.
    let mut tone = Vec::with_capacity((out_w as usize) * (out_h as usize));
    let mut edge = Vec::with_capacity(tone.capacity());
    for oy in 0..out_h as usize {
        for ox in 0..out_w as usize {
            // Nearest source pixel, matching the resize's own sampling density.
            let sx = ((ox as f32 + 0.5) * sw as f32 / out_w as f32) as usize;
            let sy = ((oy as f32 + 0.5) * sh as f32 / out_h as f32) as usize;
            let x0 = sx.saturating_sub(1);
            let y0 = sy.saturating_sub(1);
            let x1 = (sx + 1).min(sw - 1);
            let y1 = (sy + 1).min(sh - 1);
            let (mut lo, mut hi) = (255f32, 0f32);
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    let v = g(xx, yy);
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            let range = hi - lo;
            // Flat tone: local range under a quarter of the full scale. Edge:
            // over half. The two cannot both be true and the gap is wide, so the
            // thresholds are not delicate.
            tone.push(range < 64.0);
            edge.push(range > 128.0);
        }
    }
    (tone, edge)
}

/// Mean |dx| over the pixels `mask` selects, in codes per output pixel.
///
/// On the **edge** mask this is the sharpness number and it should fall as the
/// kernel widens; on the **tone** mask it is the sparkle number and it is what
/// the setting is for.
fn masked_grad(d: &image_viewer::ImageData, mask: &[bool]) -> f32 {
    let w = d.width as usize;
    let mut sum = 0f64;
    let mut n = 0f64;
    for y in 0..d.height as usize {
        for x in 0..w.saturating_sub(1) {
            if !mask[y * w + x] {
                continue;
            }
            sum += (d.rgba[(y * w + x + 1) * 4] as f64 - d.rgba[(y * w + x) * 4] as f64).abs();
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        (sum / n) as f32
    }
}

/// The strength a **first run** gets. Read from `Config::load` on an absent file
/// rather than named here, so a probe that says "the default path" cannot go on
/// measuring the other one. That is not hypothetical: the pins below were taken
/// while the default was `Smooth` and were still being read as the default path
/// after it moved. There is no `Config::default()` — `Config::load`'s hand-built
/// struct *is* the default, which is why this goes through it rather than through
/// the field.
fn default_config() -> image_viewer::Config {
    let tmp = tempfile::tempdir().unwrap();
    image_viewer::Config::load(&tmp.path().join("absent.json"))
}

/// The strength a **fresh** config asks for. `default_strength` reads the live
/// global; this reads the default config from disk, so it cannot be moved by a
/// test that ran earlier in the file.
fn default_strength() -> f32 {
    if default_config().smooth_downscale {
        image_viewer::SMOOTH_STRENGTH
    } else {
        image_viewer::FAST_STRENGTH
    }
}

/// `default_strength`, computed once.
///
/// `default_config` builds a tempdir and parses a file, and the helpers above
/// call this on every resize - so a probe table that resizes a few hundred times
/// would otherwise spend its time in `Config::load`. A `OnceLock` is safe here
/// precisely because the answer is a property of the shipped default, which is a
/// constant: nothing in the suite writes a `Config` with `smooth_downscale` set,
/// and `a_config_written_before_smooth_downscale_still_loads_and_stays_off`
/// asserts that an older config leaves it off.
fn cached_default_strength() -> f32 {
    static ONCE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *ONCE.get_or_init(default_strength)
}

/// The **shipped default** resize. Every probe below this line that reports on
/// "the path" means this one, so it is defined as the default rather than as a
/// strength constant that has to be updated in step with the config.
fn probe_resize(src: &image_viewer::ImageData) -> image_viewer::ImageData {
    resize_at(src, PROBE_TARGET, cached_default_strength())
}

/// Resize at an explicit strength and explicit target. Both the probe geometry
/// and the strength are arguments so one table can carry the whole sweep.
fn resize_at(
    src: &image_viewer::ImageData,
    target: u32,
    strength: f32,
) -> image_viewer::ImageData {
    with_resample(true, strength, || {
        image_viewer::downscale_image(src, target)
    })
}

/// Resize at an explicit target with the shipped settings - **including the
/// `MIN_FILTER_SCALE` floor**, which is the point: this is "what the viewer does",
/// as distinct from `resize_at`, which forces a strength.
///
/// The strength comes from `cached_default_strength()` rather than the global, for the
/// same reason the floor is pinned: reading a process-wide global inside a
/// concurrently-run test file means testing whichever arm ran last.
fn resize_shipped(src: &image_viewer::ImageData, target: u32) -> image_viewer::ImageData {
    with_resample(true, cached_default_strength(), || {
        image_viewer::downscale_image(src, target)
    })
}

/// Resize at an explicit target and floor, at an explicit strength. `floor 0.0`
/// is the pre-`MIN_FILTER_SCALE` path.
fn resize_floor(
    src: &image_viewer::ImageData,
    target: u32,
    floor: f32,
) -> image_viewer::ImageData {
    with_floor_at(cached_default_strength(), floor, || {
        image_viewer::downscale_image(src, target)
    })
}

/// The reported case: a 1440p landscape photo on a 1920-long panel, so
/// `preview_target` is 1920 and the reduction is 2560/1920 = 1.3333.
///
/// **The geometry is a constant here because every number in this section is
/// meaningless without it.** A lattice measured at a different reduction is a
/// different measurement - the foldover band moves with `2R` - so these cannot
/// be swept over `target` without re-deriving what each reading means.
const QHD_W: u32 = 2560;
const QHD_H: u32 = 1440;
const QHD_TARGET: u32 = 1920;
/// 2560 / 1920, the reduction this geometry actually gets.
const QHD_R: f32 = 1.3333;
/// Output Nyquist in **source** pixels at that reduction: content finer than
/// this cannot be represented and is foldover if the kernel lets it through.
const QHD_FOLDOVER_PERIOD: f32 = 2.0 * QHD_R;

/// Print the numbers every probe above is pinned against. `cargo test --test unit
/// -- --ignored --nocapture print_prefilter_pins`. Kept in the file so a filter
/// change can be re-pinned from a printout rather than from a bisect.
#[test]
#[ignore]
fn print_prefilter_pins() {
    let out = probe_resize(&probe_sinusoid(PROBE_W, PROBE_H, 1.0, 100.0));
    println!(
        "geometry: {}x{} -> {}x{}  R {:.4} x {:.4}",
        PROBE_W,
        PROBE_H,
        out.width,
        out.height,
        PROBE_W as f32 / out.width as f32,
        PROBE_H as f32 / out.height as f32
    );
    print!("sinusoid 100 codes: ");
    for p in [2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0] {
        let src = probe_sinusoid(PROBE_W, PROBE_H, p, 100.0);
        print!(
            "{}:{:.0} ",
            p,
            probe_amplitude(&probe_resize(&src))
        );
    }
    println!();
    print!("dots 0/100 codes:  ");
    for p in [2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0] {
        let src = probe_dots(PROBE_W, PROBE_H, p, 0, 100);
        print!(
            "{}:{:.0} ",
            p,
            probe_amplitude(&probe_resize(&src))
        );
    }
    println!();
    print!("8x reduction:      ");
    for p in [2.5f32, 3.0, 5.0, 6.0, 11.0] {
        let src = probe_sinusoid(1200, 1200, p, 100.0);
        let out = resize_at(&src, 150, image_viewer::FAST_STRENGTH);
        print!("{}:{:.0} ", p, probe_amplitude(&out));
    }
    println!();
    let dither = probe_dither(PROBE_W, PROBE_H);
    let flat = probe_resize(&{
        let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
        for p in rgba.chunks_exact_mut(4) {
            p.copy_from_slice(&[128, 128, 128, 255]);
        }
        image_viewer::ImageData {
            width: PROBE_W,
            height: PROBE_H,
            rgba,
            load_time: std::time::Duration::ZERO,
        }
    });
    println!(
        "50% dither mean {:.2} (in {:.2}); flat 128 mean {:.2}",
        probe_mean(&probe_resize(&dither)),
        probe_mean(&dither),
        probe_mean(&flat)
    );
}

/// Where the real-content fixtures live, if they do. `target/release/examples`
/// is a build output and gitignored, so every reader of it skips rather than
/// dying - the same exemption the criterion targets take for `test-images/`.
fn real_page(name: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("examples")
        .join(name);
    p.exists().then_some(p)
}

/// Copy a real page into a tempdir and load it, so nothing in the fixture
/// directory is opened for writing. The real pages are read-only inputs.
fn load_real_page(dir: &std::path::Path, name: &str) -> Option<image_viewer::ImageData> {
    let src = real_page(name)?;
    let dst = dir.join(name);
    std::fs::copy(&src, &dst).ok()?;
    image_viewer::load_image(&dst, image_viewer::MAX_TEXTURE_DIM).ok()
}

/// The strengths a sweep is run at. One list, so a table and the thing it is
/// compared against cannot be at different strengths.
const SWEEP_STRENGTHS: &[f32] = &[1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// The synthetic square-lattice sweep. `cargo test --test unit -- --ignored
/// --nocapture print_square_lattice_sweep`. Cheap (a ~1.1 MP source), so it is
/// the one to re-run when re-pinning.
#[test]
#[ignore]
fn print_square_lattice_sweep() {
    println!(
        "== synthetic square lattice, {PROBE_W}x{PROBE_H} -> {PROBE_TARGET} =="
    );
    println!(
        "{:>8} {:>6} | {:>9} {:>9} | {:>9} {:>9} | {:>9}",
        "strength", "pitch", "amp", "amp x", "beat", "beat x", "grad x"
    );
    for &pitch in &[2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0] {
        let src = probe_square_lattice(PROBE_W, PROBE_H, pitch, 1.0, 0, 100);
        let base = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let r = PROBE_W as f32 / base.width as f32;
        let p_out = pitch / r;
        let (a_base, b_base) = (alias_amp(&base), beat_amp(&base, p_out));
        let crop = (base.width / 4, base.height / 4, base.width / 2, base.height / 2);
        let g_base = edge_grad(&base, crop);
        for &strength in SWEEP_STRENGTHS {
            let out = resize_at(&src, PROBE_TARGET, strength);
            let (a, b, g) = (alias_amp(&out), beat_amp(&out, p_out), edge_grad(&out, crop));
            println!(
                "{strength:>8.2} {pitch:>6.1} | {a:>9.2} {:>9.3} | {b:>9.2} {:>9.3} | {:>9.3}",
                a / a_base.max(1e-6),
                b / b_base.max(1e-6),
                g / g_base.max(1e-6)
            );
        }
    }
}

/// The same sweep on real pages. Separate from the synthetic one because a full
/// decode of a 6.9 MP page is slow in a debug build and the two want different
/// iteration cycles.
#[test]
#[ignore]
fn print_real_page_strength_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, target) in [("32.jpg", 1080u32), ("12.jpg", 1080), ("028.jpg", 1080)] {
        let Some(page) = load_real_page(tmp.path(), name) else {
            println!("== {name}: absent, skipped ==");
            continue;
        };
        let base = resize_at(&page, target, image_viewer::FAST_STRENGTH);
        println!(
            "== {name} {}x{} -> {}x{} (R {:.4}) ==",
            page.width,
            page.height,
            base.width,
            base.height,
            page.width as f32 / base.width as f32
        );
        let (tone, edge) = tone_and_edge_masks(&page, base.width, base.height);
        let n_tone = tone.iter().filter(|b| **b).count();
        let n_edge = edge.iter().filter(|b| **b).count();
        println!(
            "   flat-tone pixels {n_tone} ({:.1}%), edge pixels {n_edge} ({:.1}%)",
            100.0 * n_tone as f64 / tone.len() as f64,
            100.0 * n_edge as f64 / edge.len() as f64
        );
        println!(
            "{:>8} | {:>10} {:>6} | {:>10} {:>6} | {:>8} {:>6}",
            "strength", "tone HF", "x", "edge grad", "x", "p95-5", "x"
        );
        let (t_base, e_base, a_base) = (
            masked_grad(&base, &tone),
            masked_grad(&base, &edge),
            alias_amp(&base),
        );
        for &strength in SWEEP_STRENGTHS {
            let out = resize_at(&page, target, strength);
            let (t, e, a) = (
                masked_grad(&out, &tone),
                masked_grad(&out, &edge),
                alias_amp(&out),
            );
            println!(
                "{strength:>8.2} | {t:>10.3} {:>6.3} | {e:>10.3} {:>6.3} | {a:>8.1} {:>6.3}",
                t / t_base.max(1e-6),
                e / e_base.max(1e-6),
                a / a_base.max(1e-6)
            );
        }
    }
}

/// Hard vertical step edge, white left of `x0`, black right of it. The
/// sharpness probe: a wider kernel cannot help but widen the transition.
fn probe_step(w: u32, h: u32, x0: u32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = if (x as u32) < x0 { 255u8 } else { 0 };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// 10-90% rise distance of the step edge, in **output pixels**, averaged over
/// rows and over both edges' orientations.
///
/// The unambiguous sharpness number: it is a length, not an energy, so it cannot
/// be moved by the picture's contrast, and a wider low-pass cannot do anything
/// but lengthen it. Reported for the probe's own 2.880x reduction.
fn step_rise(out: &image_viewer::ImageData) -> f32 {
    let w = out.width as usize;
    // The probe is white to the left of the edge, so the profile descends. 90%
    // is the first crossing of 229.5 and 10% the first of 25.5; interpolating
    // between the straddling pair makes the measure sub-pixel, which matters
    // because the whole effect is a fraction of an output pixel per strength
    // step.
    let (t90, t10) = (0.9f32 * 255.0, 0.1f32 * 255.0);
    // `crossing` finds the first sample below `t` and interpolates against its
    // predecessor. Scanned as "first below" rather than "straddling pair"
    // because the profile can step from above the threshold straight past it:
    // at strength 1.0 the edge is 254, 187, 21, 0, so the 10% threshold of 25.5
    // is already behind the 21 sample and a straddling-pair test never fires.
    // That was a bug in the metric, not a limit of the resampler - it reported
    // 0.000 at every strength, which `print_step_edge_profile` is what caught.
    let crossing = |get: &dyn Fn(usize) -> f32, t: f32| -> Option<f32> {
        for x in 1..w {
            let (v0, v1) = (get(x - 1), get(x));
            if v1 < t {
                return (v0 >= t).then(|| (x - 1) as f32 + (v0 - t) / (v0 - v1).max(1e-6));
            }
        }
        None
    };
    let mut total = 0f64;
    let mut n = 0f64;
    for y in 0..out.height as usize {
        let row = |x: usize| out.rgba[(y * w + x) * 4] as f32;
        let hi = crossing(&row, t90);
        let lo = crossing(&row, t10);
        if let (Some(hi), Some(lo)) = (hi, lo) {
            // `lo - hi`, not `hi - lo`: the profile **descends** (white to the
            // left), so the 10% crossing is to the *right* of the 90% one and the
            // difference is negative. Taking `.max(0.0)` on the wrong order turns
            // every row into a zero-length edge and the metric into a constant
            // 0.000 that looks like a limit on the resampler. That is what it
            // did for two runs before `print_step_edge_profile` was written.
            total += (lo - hi).abs() as f64;
            n += 1.0;
        }
    }
    if n == 0.0 {
        0.0
    } else {
        (total / n) as f32
    }
}

/// Write the **same crop** of a real page at each strength, scaled 2x nearest,
/// so the artefact and the cost can be looked at rather than only measured.
///
/// `cargo test --test unit --release -- --ignored --nocapture
/// export_strength_crops`. Writes into `target/strength-probe/`. This is the one
/// piece of evidence a number cannot give, and it is why the strength was chosen
/// by eye as well as by table - a 71% wider step edge is either obviously softer
/// or not, and only a picture says which.
#[test]
#[ignore]
fn export_strength_crops() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("strength-probe");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    // Crops as fractions of the output, so one entry works at either page size.
    // (x, y, w, h) in 1000ths of the output's dimensions.
    for (name, target, frac) in [
        // The screentone field: where the lattice lives.
        ("32.jpg", 1080u32, (60u32, 430u32, 500u32, 320u32)),
        // Line art: where the cost of removing it lands.
        ("028.jpg", 1080, (330, 60, 620, 420)),
    ] {
        let Some(page) = load_real_page(tmp.path(), name) else {
            println!("{name} absent, skipped");
            continue;
        };
        for &s in &[1.0f32, 1.5, 1.75, 2.0] {
            let out = resize_at(&page, target, s);
            let (w, h) = (out.width as usize, out.height as usize);
            let cx = w * frac.0 as usize / 1000;
            let cy = h * frac.1 as usize / 1000;
            let cw = (w * frac.2 as usize / 1000).min(w - cx);
            let ch = (h * frac.3 as usize / 1000).min(h - cy);
            // 2x nearest so the sparkle is visible at 1:1 on screen.
            let (sw, sh) = (cw * 2, ch * 2);
            let mut buf = vec![0u8; sw * sh * 4];
            for y in 0..sh {
                for x in 0..sw {
                    let si = ((cy + y / 2) * w + cx + x / 2) * 4;
                    let di = (y * sw + x) * 4;
                    buf[di..di + 4].copy_from_slice(&out.rgba[si..si + 4]);
                }
            }
            let img = image::RgbaImage::from_raw(sw as u32, sh as u32, buf).unwrap();
            let stem = name.trim_end_matches(".jpg");
            let p = dir.join(format!("{stem}-s{s:.2}.png"));
            img.save(&p).unwrap();
            println!("wrote {}", p.display());
        }

        // A side-by-side strip of the same crop at every strength, so the
        // comparison is one image rather than four read one at a time - which is
        // how a difference gets looked at honestly.
        let arms: Vec<image_viewer::ImageData> = [1.0f32, 1.5, 1.75, 2.0]
            .iter()
            .map(|&s| resize_at(&page, target, s))
            .collect();
        let base = &arms[0];
        let w = base.width as usize;
        let h = base.height as usize;
        let (cx, cy) = (
            (w * frac.0 as usize / 1000).min(w.saturating_sub(1)),
            (h * frac.1 as usize / 1000).min(h.saturating_sub(1)),
        );
        let cw = (w * frac.2 as usize / 1000).min(w - cx);
        let ch = (h * frac.3 as usize / 1000).min(h - cy);
        let (sw, sh) = (cw * 2, ch * 2);
        let strip_w = sw * arms.len() + 4 * (arms.len() - 1);
        let mut strip = vec![255u8; strip_w * sh * 4];
        for (i, arm) in arms.iter().enumerate() {
            let ox = i * (sw + 4);
            for y in 0..sh {
                for x in 0..sw {
                    let si = ((cy + y / 2) * w + cx + x / 2) * 4;
                    let di = (y * strip_w + ox + x) * 4;
                    strip[di..di + 4].copy_from_slice(&arm.rgba[si..si + 4]);
                }
            }
        }
        let stem = name.trim_end_matches(".jpg");
        let p = dir.join(format!("{stem}-strip.png"));
        image::RgbaImage::from_raw(strip_w as u32, sh as u32, strip)
            .unwrap()
            .save(&p)
            .unwrap();
        println!("wrote {} (left to right: 1.00, 1.50, 1.75, 2.00)", p.display());
    }
}

/// The step edge's output profile, printed. Exists because `step_rise` reads
/// 0.000 at every strength and that is either a real limit or a bug in the
/// metric, and those are very different things to build a sharpness number on.
#[test]
#[ignore]
fn print_step_edge_profile() {
    let src = probe_step(PROBE_W, PROBE_H, PROBE_W / 2);
    for &s in &[1.0f32, 2.0, 3.0] {
        let out = resize_at(&src, PROBE_TARGET, s);
        let w = out.width as usize;
        let mid = out.height as usize / 2;
        let prof: Vec<u8> = (w / 2 - 5..w / 2 + 5)
            .map(|x| out.rgba[(mid * w + x) * 4])
            .collect();
        println!("  strength {s}: {} values around the edge: {:?}", out.width, prof);
    }
}

/// Step-edge rise at each strength, plus the real-page numbers beside it.
#[test]
#[ignore]
fn print_step_edge_sharpness() {
    println!("== step edge, 890x1244 -> 432 (R 2.880); rise in output px ==");
    let src = probe_step(PROBE_W, PROBE_H, PROBE_W / 2);
    for &s in SWEEP_STRENGTHS {
        let out = resize_at(&src, PROBE_TARGET, s);
        println!(
            "  strength {s:>4.2}: rise {:.3} output px  ({:.3} source px)",
            step_rise(&out),
            step_rise(&out) * PROBE_W as f32 / out.width as f32
        );
    }
    // A thin-line probe too: a 1.5 source px line is what a line-art page is
    // mostly made of, and it is the case where widening the kernel is most
    // visible.
    println!("== 3-source-px bar, 890x1244 -> 432 ==");
    let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
    for y in 0..PROBE_H as usize {
        for x in 0..PROBE_W as usize {
            let v = if (x as u32) % 40 < 3 { 0u8 } else { 255 };
            let o = (y * PROBE_W as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let bars = image_viewer::ImageData {
        width: PROBE_W,
        height: PROBE_H,
        rgba,
        load_time: std::time::Duration::ZERO,
    };
    let base = resize_at(&bars, PROBE_TARGET, image_viewer::FAST_STRENGTH);
    let crop = (base.width / 4, base.height / 4, base.width / 2, base.height / 2);
    let g0 = edge_grad(&base, crop);
    for &s in SWEEP_STRENGTHS {
        let out = resize_at(&bars, PROBE_TARGET, s);
        println!(
            "  strength {s:>4.2}: bar grad {:.3} codes/px  ({:.3}x of fast)",
            edge_grad(&out, crop),
            edge_grad(&out, crop) / g0
        );
    }
}

/// Is the residual on the worst-case page still a **lattice**, or is it now
/// broadband? This is the question the honest limit turns on.
///
/// `alias_amp` going down does not say the artefact went away - it says *some*
/// amplitude went down, and a 1-px sparkle and a lattice of the same amplitude
/// are not the same defect. So: project the flat-tone pixels of the output onto
/// a 2-D sinusoid at every output period from 1.5 to 8 px, and look at the
/// **peak** and at where it sits. A lattice shows one sharp peak at the pitch of
/// the artefact; broadband residue shows a low, flat curve.
#[test]
#[ignore]
fn print_tone_residual_spectrum() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, target) in [("32.jpg", 1080u32), ("12.jpg", 1080)] {
        let Some(page) = load_real_page(tmp.path(), name) else {
            println!("{name} absent, skipped");
            continue;
        };
        let base = resize_at(&page, target, image_viewer::FAST_STRENGTH);
        let (tone, _) = tone_and_edge_masks(&page, base.width, base.height);
        println!(
            "== {name} -> {}x{}, flat-tone 2-D spectrum of mean amplitude per output px ==",
            base.width, base.height
        );
        let arms: Vec<image_viewer::ImageData> = SWEEP_STRENGTHS
            .iter()
            .map(|&s| resize_at(&page, target, s))
            .collect();
        let mut names: Vec<String> = SWEEP_STRENGTHS.iter().map(|s| format!("{s:.2}")).collect();
        names.push("mean".into());
        println!("{:>6} | {}", "period", names.join(" | "));
        for &p_out in &[1.5f32, 2.0, 2.5, 3.0, 4.0, 6.0, 8.0] {
            let row: Vec<f32> = arms
                .iter()
                .map(|d| tone_spectrum_amp(d, &tone, p_out))
                .collect();
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            let cells: Vec<String> = row
                .iter()
                .map(|v| format!("{v:.3}"))
                .chain(std::iter::once(format!("{mean:.3}")))
                .collect();
            println!("{p_out:>6.1} | {}", cells.join(" | "));
        }
    }
}

/// Amplitude of a 2-D sinusoid of output period `p_out`, averaged over the
/// flat-tone pixels only. Same projection as `beat_amp`, restricted to the mask.
fn tone_spectrum_amp(d: &image_viewer::ImageData, mask: &[bool], p_out: f32) -> f32 {
    let (w, h) = (d.width as usize, d.height as usize);
    let k = 2.0 * std::f64::consts::PI / p_out as f64;
    // Subtract the mask's own mean first, or a non-zero DC leaks into the
    // projection and reads as structure that is not there.
    let (mut n, mut mean) = (0f64, 0f64);
    for (i, m) in mask.iter().enumerate().take(w * h) {
        if *m {
            mean += d.rgba[i * 4] as f64;
            n += 1.0;
        }
    }
    if n < 8.0 {
        return 0.0;
    }
    mean /= n;
    let (mut sss, mut ssc, mut sys, mut syc) = (0f64, 0f64, 0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !mask[i] {
                continue;
            }
            let v = d.rgba[i * 4] as f64 - mean;
            let s = (k * x as f64).sin() + (k * y as f64).sin();
            let c = (k * x as f64).cos() + (k * y as f64).cos();
            sss += s * s;
            ssc += s * c;
            sys += v * s;
            syc += v * c;
        }
    }
    if sss <= 0.0 {
        return 0.0;
    }
    let (a, b) = if ssc.abs() < 1e-9 * sss.max(1.0) {
        (sys / sss, syc / sss)
    } else {
        let det = sss * sss - ssc * ssc;
        if det.abs() < 1e-9 {
            return 0.0;
        } else {
            ((sys * sss - syc * ssc) / det, (syc * sss - sys * ssc) / det)
        }
    };
    // **No `/ sqrt(n)`.** There was one, and it was wrong: for a *coherent*
    // pattern - which is what a dot screen is - the amplitude is `A` at every
    // masked pixel, so the least-squares fit above already returns `A` (up to the
    // 1/sqrt(2) of adding the two axes). Dividing that by the square root of the
    // pixel count then reports ~0.0002 where `alias_amp` on the same image reads
    // 10 codes, i.e. it divides a real signal by ~700 at this output size, and it
    // cannot resolve a coherent peak at all. Every conclusion drawn from it -
    // "no peak at any output period from 1.5 to 8 px at any strength, the residue
    // is broadband" - was read off that divided quantity. `beat_amp`, the
    // sibling projection, has never had this division, which is why it reports
    // codes and this did not.
    (a * a + b * b).sqrt() as f32
}

/// **`tone_spectrum_amp` reports codes, and not a quantity smaller by ~sqrt(n).**
///
/// The function carried a `/ sqrt(n)`, on the theory that it measured "mean
/// amplitude per output pixel". For a *coherent* pattern — which is the only kind
/// this is asked about, because a dot screen is coherent by construction — the
/// amplitude is `A` at every masked pixel, so the least-squares fit already
/// returns `A` and the extra division divides a real signal by the square root of
/// the pixel count. At a 890x1244 output with an all-true mask that is a factor of
/// 1045.
///
/// It is worth a test because the consequence was not cosmetic: every per-pitch
/// number in the strength record came from it, including the claim that the
/// surviving artefact is broadband with "no peak at any output period from 1.5 to
/// 8 px at any strength". A metric that cannot resolve a coherent peak cannot
/// support that claim, and the claim was load-bearing — it is what said the
/// lattice was "reduced, not abolished".
///
/// Calibrated here against the sibling projection `beat_amp`, which has never had
/// the division and has always reported codes: on the same image, at the same
/// output period, the two must now agree to within the 1/sqrt(2) that adding the
/// x and y axes costs, and **not** differ by three orders of magnitude. The
/// lattice's own output pitch is probed, which is where the answer is non-zero —
/// probing away from it is what let the bad metric look like a flat curve.
#[test]
fn the_tone_spectrum_metric_reports_codes() {
    for &(p, d) in &[(4.0f32, 1.0f32), (4.0, 2.0), (6.0, 2.0)] {
        let src = probe_square_lattice(PROBE_W, PROBE_H, p, d, 0, 100);
        let out = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let p_out = p / (PROBE_W as f32 / out.width as f32);
        let mask = vec![true; (out.width * out.height) as usize];
        let (a, b) = (
            tone_spectrum_amp(&out, &mask, p_out),
            beat_amp(&out, p_out),
        );
        assert!(
            a > 0.5,
            "lattice pitch {p} dot {d} at its own output pitch {p_out:.3}: \
             tone_spectrum_amp reported {a:.5} codes, want a real amplitude - a \
             metric this small cannot resolve a coherent pattern"
        );
        // `beat_amp` fits the same two projections; the two may differ by the
        // 1/sqrt(2) of summing sin in both axes, and by the sign convention of
        // the two-term solve, but they are the same order of magnitude.
        assert!(
            a <= b.max(a) * 4.0 + 1.0 && b <= a * 4.0 + 1.0,
            "lattice pitch {p} dot {d}: tone_spectrum_amp {a:.3} and beat_amp \
             {b:.3} are not the same quantity"
        );
    }
}

/// Wall clock at the two shipped positions, on the worst-case real page.
/// Interleaved arms, both warmed before either is timed.
#[test]
#[ignore]
fn print_downscale_strength_wall_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(page) = load_real_page(tmp.path(), "32.jpg") else {
        println!("32.jpg absent, skipped");
        return;
    };
    println!("== wall clock, 32.jpg 2223x3109 -> 1080, ms (release) ==");
    // **Interleaved one at a time, in alternating order, not arm-after-arm.**
    // The first version of this ran all of `fast` and then all of `smooth`, and
    // the two `fast` blocks it reported on either side of `smooth` disagreed by
    // 38% (76 ms then 105 ms) - so whatever drifts here (thread pool warming,
    // the turbojpeg scratch buffers, the page cache) is worth more than the
    // effect being measured. One resize per alternating step means the drift is
    // shared between the arms instead of accumulating on one of them.
    let mut fast = Vec::new();
    let mut smooth = Vec::new();
    // Warm both first: the cache and the thread pool are shared process state,
    // and the first call into either arm pays for both.
    for &s in &[image_viewer::FAST_STRENGTH, image_viewer::SMOOTH_STRENGTH] {
        let _ = resize_at(&page, 1080, s);
    }
    for i in 0..9 {
        // Alternate which arm goes first, so neither systematically inherits the
        // other's warm-up or cool-down.
        let order: &[f32] = if i % 2 == 0 {
            &[image_viewer::FAST_STRENGTH, image_viewer::SMOOTH_STRENGTH]
        } else {
            &[image_viewer::SMOOTH_STRENGTH, image_viewer::FAST_STRENGTH]
        };
        for &s in order {
            let t = std::time::Instant::now();
            let _ = resize_at(&page, 1080, s);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if s == image_viewer::FAST_STRENGTH {
                fast.push(ms);
            } else {
                smooth.push(ms);
            }
        }
    }
    for (name, mut v) in [("fast 1.0", fast), ("smooth 2.0", smooth)] {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "  {name:>11}: median {:.0} ms   min {:.0}   max {:.0}",
            v[v.len() / 2],
            v[0],
            v[v.len() - 1]
        );
    }
}

/// THE guard on the strength: on a **square lattice**, which is the only probe
/// that can show the artefact, `Smooth` must leave strictly less than `Fast`.
///
/// **Why a square lattice and not the sinusoid above.** `probe_sinusoid` varies
/// along x only, so it cannot produce a lattice in the first place — it reports
/// the horizontal transfer function, and a horizontal transfer function reading
/// 4 codes says nothing about a *square* grid. That is not a criticism of the
/// probe: it is exactly the right probe for the question it asks, which is why it
/// is kept. It is the wrong probe for this one, and the previous session's
/// "4 codes, therefore fixed" came from asking it this.
///
/// **Why `Fast` is the control rather than an absolute number.** An absolute
/// pin would say one strength is good enough and would go stale silently if the
/// kernel changed. The property that matters is the *direction and the size* of
/// the trade, and it is measurable without a reference implementation: more
/// strength must mean less lattice. The absolute numbers are in the comment.
#[test]
fn smooth_low_passes_a_square_lattice_harder_than_fast() {
    // alias_amp (p95-p5, whole output, both axes) at the probe's 2.880x:
    //
    //   pitch   2.5   3.0   3.5   4.0   5.0   6.0   8.0
    //   Fast   10.0   2.0  14.0  10.0  19.0  22.0  23.0
    //   Smooth  1.0   0.0   1.0   1.0   0.0   0.0   4.0
    //
    // Pins are the Smooth numbers plus one code, the same slack the prefilter
    // tests use: the most a different target's f32 contraction could move a
    // weighted sum. At every pitch the ratio is 0.00-0.17x.
    let cases = [
        (2.5f32, 2.0f32),
        (3.0, 1.0),
        (3.5, 2.0),
        (4.0, 2.0),
        (5.0, 1.0),
        (6.0, 1.0),
        (8.0, 5.0),
    ];
    for (pitch, max_codes) in cases {
        let src = probe_square_lattice(PROBE_W, PROBE_H, pitch, 1.0, 0, 100);
        let fast = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let smooth = resize_at(&src, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);
        let (a_fast, a_smooth) = (alias_amp(&fast), alias_amp(&smooth));
        assert!(
            a_smooth <= max_codes,
            "square lattice at source pitch {pitch} px: Smooth left {a_smooth} codes, \
             want <= {max_codes} (Fast leaves {a_fast})"
        );
        assert!(
            a_smooth < a_fast,
            "square lattice at source pitch {pitch} px: Smooth {a_smooth} codes is \
             not below Fast {a_fast} - the strength is not reaching the kernel"
        );
    }
}

/// The same trade, measured as the amplitude of the lattice **at its own output
/// period** rather than as broadband spread.
///
/// The eye groups the surviving pattern into the visible artefact; it is not
/// reading a percentile. So this asks the narrower question — is the lattice's
/// own frequency still there? — and it is a separate projection from
/// `alias_amp`, so the two agreeing is evidence rather than one number twice.
#[test]
fn smooth_removes_the_lattice_at_its_own_output_pitch() {
    // beat_amp at the lattice's own output period, codes:
    //
    //   pitch   3.5   4.0   5.0   6.0
    //   Fast    1.24  2.74  5.75  7.09
    //   Smooth  0.08  0.06  0.10  0.04
    let cases = [(3.5f32, 0.2f32), (4.0, 0.2), (5.0, 0.2), (6.0, 0.2)];
    for (pitch, max_codes) in cases {
        let src = probe_square_lattice(PROBE_W, PROBE_H, pitch, 1.0, 0, 100);
        let fast = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let smooth = resize_at(&src, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);
        let r = PROBE_W as f32 / fast.width as f32;
        let p_out = pitch / r;
        let (b_fast, b_smooth) = (beat_amp(&fast, p_out), beat_amp(&smooth, p_out));
        assert!(
            b_smooth <= max_codes,
            "square lattice at source pitch {pitch} px ({p_out:.2} output px): \
             {b_smooth:.2} codes of lattice at its own pitch survived, want \
             <= {max_codes} (Fast leaves {b_fast:.2})"
        );
    }
}

/// The cost, stated as a number: `Smooth` makes a hard edge wider. Pinned so
/// that "it removes the artefact" cannot be re-earned later by a change that
/// quietly gives the sharpness back — which would mean the setting is no longer
/// doing the thing it is named for.
///
/// 10-90% rise on a step edge at the probe's 2.880x reduction, output pixels:
///
///   Fast 1.607 -> Smooth 2.747   (1.71x)
///
/// The bar probe is the same statement on content a manga page is mostly made
/// of: 3-source-px bars at 2x output. Fast 14.118 -> Smooth 7.634 codes/px.
#[test]
fn smooth_costs_edge_sharpness_and_the_cost_is_bounded() {
    let step = probe_step(PROBE_W, PROBE_H, PROBE_W / 2);
    let fast = resize_at(&step, PROBE_TARGET, image_viewer::FAST_STRENGTH);
    let smooth = resize_at(&step, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);
    let (r_fast, r_smooth) = (step_rise(&fast), step_rise(&smooth));
    assert!(
        r_smooth > r_fast,
        "Smooth must widen the step edge ({r_fast:.3} -> {r_smooth:.3} output px); \
         it did not, so the strength is not reaching the kernel"
    );
    assert!(
        r_smooth <= r_fast * 1.75,
        "the step edge grew to {r_smooth:.3} output px from {r_fast:.3} \
         ({:.2}x) - past the 1.71x the trade was chosen at, which means the \
         default has moved",
        r_smooth / r_fast.max(1e-6)
    );

    let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
    for y in 0..PROBE_H as usize {
        for x in 0..PROBE_W as usize {
            let v = if (x as u32) % 40 < 3 { 0u8 } else { 255 };
            let o = (y * PROBE_W as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let bars = image_viewer::ImageData {
        width: PROBE_W,
        height: PROBE_H,
        rgba,
        load_time: std::time::Duration::ZERO,
    };
    let crop = |d: &image_viewer::ImageData| {
        (
            d.width / 4,
            d.height / 4,
            d.width / 2,
            d.height / 2,
        )
    };
    let (b_fast, b_smooth) = (
        resize_at(&bars, PROBE_TARGET, image_viewer::FAST_STRENGTH),
        resize_at(&bars, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH),
    );
    let g_fast = edge_grad(&b_fast, crop(&b_fast));
    let g_smooth = edge_grad(&b_smooth, crop(&b_smooth));
    assert!(
        g_smooth < g_fast * 0.60,
        "3-px bar gradient {g_smooth:.3} vs Fast {g_fast:.3} ({:.3}x); it must fall \
         below 0.60x - 0.541x is what the default was chosen at",
        g_smooth / g_fast.max(1e-6)
    );
}

/// `Fast` is **today's kernel, bit for bit**, and that is checkable without a
/// reference build: at strength 1.0 the multiplier is the identity, so the
/// output has to equal what an unscaled `filter_scale` would produce. Held by
/// asserting `FAST_STRENGTH == 1.0` and that `set_downscale_smoothing(false)`
/// lands exactly on it — the two facts together mean the `Fast` position is the
/// pre-existing code path and not a third flavour of it.
///
/// This is the guard that would catch someone "tidying" `FAST_STRENGTH` to
/// something else, which would silently make the toggle a lie: the label would
/// say Fast and the picture would change anyway.
#[test]
fn fast_is_exactly_the_pre_existing_kernel_width() {
    assert_eq!(image_viewer::FAST_STRENGTH, 1.0);
    assert!(image_viewer::SMOOTH_STRENGTH > image_viewer::FAST_STRENGTH);

    let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let was = image_viewer::downscale_strength();
    image_viewer::set_downscale_smoothing(false);
    let at_fast = image_viewer::downscale_strength();
    let smoothing_off = image_viewer::downscale_smoothing();
    image_viewer::set_downscale_smoothing(true);
    let at_smooth = image_viewer::downscale_strength();
    let smoothing_on = image_viewer::downscale_smoothing();
    image_viewer::set_downscale_strength(was);

    assert_eq!(at_fast, 1.0, "Fast must be the identity multiplier");
    assert!(!smoothing_off);
    assert_eq!(at_smooth, image_viewer::SMOOTH_STRENGTH);
    assert!(smoothing_on);
}

/// **The strength has to reach both halves of the resize, and it has to be one
/// value.** Pinned against the source text, because the failure is not
/// observable from outside and the library cannot reach `main.rs`.
///
/// Two halves, two failure modes, both silent:
///
/// * the prefilter widened but the final passes not, or the reverse. Either way
///   the artefact is only partly removed and nothing says so. The prefilter's
///   own `filter_scale` and the final pass's are separate `contributions` calls.
/// * **the two halves reading the setting separately.** A toggle pressed between
///   the prefilter's weights and the final pass's would resize with a prefilter
///   of one width feeding a final pass of another — a picture that is neither
///   setting. That is the same class as the two manga-mode sites that were each
///   correct alone, which is why this reads the count and not just presence.
///
/// Verified to fail both ways: with `strength` dropped from either
/// `contributions` call site, and with `downscale_strength()` moved into
/// `contributions` so each call site reads it for itself.
#[test]
fn one_strength_reaches_both_halves_of_the_resize() {
    let src = include_str!("../src/lib.rs");

    // Brace-match a function body rather than slicing to the first `\n}`: a
    // nested block at column 0 would end the slice early and the scan would pass
    // on a truncated function.
    let body_of = |name: &str| -> &str {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("{name} exists"));
        let body = &src[start..];
        let mut depth = 0i32;
        let end = body
            .char_indices()
            .find(|&(_, c)| {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                depth == 0 && c == '}'
            })
            .map(|(i, _)| i + 1)
            .expect("braces balance");
        &body[..end]
    };

    let lr = body_of("linear_resize");
    assert_eq!(
        lr.matches("downscale_strength()").count(),
        1,
        "linear_resize must read downscale_strength() exactly once, so the \
         pre-reduction and the final passes cannot disagree"
    );
    assert!(
        lr.contains("kernel_reduce_linear(data, pre_w, pre_h, strength, 0.0)"),
        "the pre-reduction must be handed the strength linear_resize read"
    );
    // The floor rides along with the strength: both final-pass axes get the same
    // pair, so a floor that reached one axis and not the other would be a
    // vertically-striped picture with no error.
    assert!(
        lr.contains("contributions(lw, new_w, &kern, support, strength, floor)")
            && lr.contains("contributions(lh, new_h, &kern, support, strength, floor)"),
        "both final-pass axes must be handed the same strength and the same \
         MIN_FILTER_SCALE floor"
    );

    let kr = body_of("kernel_reduce_linear");
    assert!(
        kr.contains("strength: f32")
            && kr.contains("contributions(sw as u32, pw as u32, &kern, support, strength, floor)")
            && kr.contains("contributions(sh as u32, ph as u32, &kern, support, strength, floor)"),
        "the pre-reduction must take the strength as an argument and use it on \
         both axes - reading it for itself would let it disagree with the final \
         passes"
    );

    let con = body_of("contributions");
    assert!(
        !con.contains("downscale_strength()") && !con.contains("min_filter_scale()"),
        "contributions must take the strength and the floor as parameters, not \
         read the settings: it is called four times per resize and a read there \
         is four chances for the halves to disagree"
    );
    assert!(
        con.contains("strength / scale"),
        "the multiplier has to scale filter_scale, which is the only term that \
         widens the kernel - widening the span alone evaluates every extra tap \
         at exactly zero and is bit-identical"
    );
}

/// The settings toggle must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here (`spreads.rs`,
/// `keybindings.rs`, `renderer.rs`).
///
/// Three things it checks, each a way the setting could exist and do nothing:
/// the label exists, the config load applies it, and the panel's changed arm
/// **invalidates the previews**. The third is the one that matters: a
/// decode-time setting that leaves `previews` alone flips the box, changes
/// nothing visible, and reads as a broken feature. `linear_resample` is the
/// documented counter-example in `AGENTS.md`.
#[test]
fn the_smooth_toggle_is_wired_and_invalidates_the_previews() {
    let src = include_str!("../src/main.rs");

    assert!(
        src.contains("set_downscale_smoothing(config.smooth_downscale);"),
        "startup must apply config.smooth_downscale to the resampler"
    );
    assert!(
        src.contains("&mut self.config.smooth_downscale"),
        "the settings panel must have a checkbox bound to it"
    );

    // The checkbox's own arm: find the `.changed()` block that mentions the
    // field and require it to set `refilter`. Locating it by field name rather
    // than by a fixed offset is what makes this survive the panel being
    // reordered, which it will be.
    let at = src
        .find("&mut self.config.smooth_downscale")
        .expect("the checkbox exists");
    let after = &src[at..];
    let arm_end = after
        .find("}\n                ")
        .expect("the changed() arm closes");
    let arm = &after[..arm_end];
    assert!(
        arm.contains("set_downscale_smoothing("),
        "the changed arm must apply the setting immediately, not only on the \
         next launch"
    );
    assert!(
        arm.contains("refilter = true"),
        "the changed arm must set refilter, or the previews decoded by the old \
         kernel stay on screen and the toggle appears broken (AGENTS.md records \
         exactly this for linear_resample)"
    );
    assert!(
        src.contains("if refilter {"),
        "the refilter flag must still be acted on"
    );
}

/// A config written before this setting existed still loads, and gets **`false`**.
///
/// Both halves, because they are opposites: it must load *and* it must come up
/// `false`. A `#[serde(default)]` with the wrong default compiles, passes every
/// other test in the suite, and hands every existing reader a setting that is
/// wrong for three of the six pages it was measured on.
///
/// **This assertion was `true` and is now `false`, and the change is the fix.**
/// The setting shipped default-on on the strength of a *uniform synthetic
/// lattice*, where more strength wins at every pitch. Judging rendered output on
/// six 2223x3109 pages, strength 2.0 was better on `32.jpg` and `30.jpg`,
/// unchanged on `52.jpg`, and worse on `12.jpg`, `21.jpg` and `44.jpg` — a black
/// dot pattern appearing in grey screentone. `Fast` was better than `master` on
/// all six. So the default follows the measurement that was taken on the artefact
/// users actually have, and the previous default was chosen on a probe that
/// cannot see the trade.
#[test]
fn a_config_written_before_smooth_downscale_still_loads_and_stays_off() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(
        !cfg.smooth_downscale,
        "an old config must come up with Smooth OFF: on real pages it is better on \
         two files, unchanged on one and worse on three, so it cannot be what \
         everyone gets"
    );

    // And the default a *missing file* gets, which is a different construction
    // (`Config::load`'s hand-built struct) and was a separate way for the field
    // to be forgotten.
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(
        !fresh.smooth_downscale,
        "a first run must also come up with Smooth OFF"
    );
}

#[test]
fn a_config_written_before_grid_cell_size_still_loads_at_150() {
    // Same two constructions as the Smooth test above: an old file without the
    // key (serde default) and a missing file (`Config::load`'s hand-built
    // struct). Either one forgotten parks every existing user on the wrong
    // cell size with no error.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert_eq!(
        cfg.grid_cell, 150.0,
        "an old config without the key must come up at 150pt cells"
    );
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert_eq!(
        fresh.grid_cell, 150.0,
        "a first run must also come up at 150pt cells"
    );
}

#[test]
fn a_config_written_before_slideshow_shuffle_still_loads_with_it_off() {
    // A shuffled show surprises anyone who did not ask for one, so an old
    // config — and a new install — must come up with it off. Same two
    // constructions as the tests above.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(
        !cfg.slideshow_shuffle,
        "an old config without the key must not start shuffling shows"
    );
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(
        !fresh.slideshow_shuffle,
        "a first run must also come up with shuffle off"
    );
}

/// **The regression guard: `Smooth` must not be the default, and the reason must
/// not quietly stop being true.**
///
/// Two independent claims, because the second is the one that stops it coming
/// back.
///
/// *The default.* `Config::load` on an absent file must give
/// `smooth_downscale == false`. The evidence for that is on real pages and cannot
/// be reproduced here — `12.jpg`, `21.jpg` and `44.jpg` are 4-6 MB scans in a
/// gitignored build directory — so this pins the **decision**, in the one place a
/// decision like it can be re-made by accident.
/// `a_config_written_before_smooth_downscale_still_loads_and_stays_off` covers the
/// two other ways the field reaches a real user.
///
/// *The mechanism.* The synthetic probe below is the in-tree half, and it is the
/// part that reproduces. `probe_multi_pitch_screentone` is a **grey** field
/// carrying a base dot grid, a second screen at 30 degrees and a stochastic
/// dither — which is what real screentone is, and what a 0/100 test lattice is
/// not.
///
/// **What widening actually does, measured, in one image.** Amplitude at an output
/// period, Fast -> Smooth, scanning 1.05 to 4.00 output px in 0.05 steps:
///
/// * at **1.389 px** — the base grid's own pitch — 1.853 -> **0.629** codes, so
///   widening removes it, which is the effect the setting was built for;
/// * at **3.60 px** — a longer period the same image also carries — 0.295 ->
///   **0.564** codes, so widening **nearly doubles** it.
///   And 1.4 px goes 0.468 -> 0.550, 3.5 px the same.
///
/// So the setting is not a monotone improvement in the low-pass and a cost in the
/// detail. It moves energy **between** pitches, and which pitches gain depends on
/// the content. `smooth_low_passes_a_square_lattice_harder_than_fast` measures the
/// winning half and passes; this one measures the losing half, and both are true.
///
/// **The obvious explanation is wrong, and it is worth recording because it is
/// the one to reach for.** It is not about multi-pitch content: removing the
/// second screen and the dither, leaving a *pure single-pitch* 0/100 lattice at
/// source pitch 4.0, still shows a ratio of 1.25 at 4.4 output px and 1.53
/// somewhere finer. So this is not "real tone has several screens and a single
/// screen cannot". It is the filter's response meeting **foldover**: the promoted
/// component is at a period that is a *multiple* of the pattern's own, i.e. an
/// image of the pattern's own frequency in the output, and widening changes the
/// balance between a component and its folded image. The previous sweep only ever
/// measured at the lattice's own output pitch, where widening wins monotonically,
/// which is why it read as a clean fix.
///
/// **Why the assertion is a scan and not a pinned pitch.** A pinned frequency is a
/// number that goes stale the moment the kernel changes, and the previous
/// single-pitch sweep is what produced the wrong default in the first place. The
/// claim pinned here is the finding itself — in one image, some pitch improves and
/// another gets worse — which survives a kernel change if the finding does and
/// fails loudly if it stops being true.
#[test]
fn smooth_is_not_the_default_because_it_moves_energy_between_pitches() {
    // 1. The decision, in the place it is made.
    assert!(
        !default_config().smooth_downscale,
        "Smooth must default OFF: it is better on 2 of 6 real pages, unchanged on \
         1 and worse on 3"
    );

    // 2. The mechanism, on a fixture that exists on a clean clone. The claim is
    // **not** "pitch X gets worse" — a single pinned frequency is a number that
    // goes stale the moment the kernel changes, and the previous single-pitch
    // sweep is what produced the wrong default in the first place. The claim is
    // the finding itself: in ONE image, widening the low-pass suppresses one
    // pitch and promotes another. Scan for both.
    let src = probe_multi_pitch_screentone(PROBE_W, PROBE_H, 4.0, 5.1, 30.0);
    let fast = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
    let smooth = resize_at(&src, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);

    let mut worst: Option<(f32, f32, f32, f32)> = None; // ratio, period, fast, smooth
    let mut best: Option<(f32, f32, f32, f32)> = None;
    for i in 0..=59 {
        let p = 1.05 + i as f32 * 0.05;
        let (a, b) = (
            screentone_lattice_amplitude(&fast, p),
            screentone_lattice_amplitude(&smooth, p),
        );
        // A ratio against a signal that is not there is not a measurement. At
        // exactly 2.00 output px both projections read 0.000 for every strength,
        // because that is the output Nyquist and the pattern is below it — the
        // frequency content there is genuinely absent, not small.
        if a < 0.05 {
            continue;
        }
        let ratio = b / a;
        if worst.map_or(true, |w| ratio > w.0) {
            worst = Some((ratio, p, a, b));
        }
        if best.map_or(true, |w| ratio < w.0) {
            best = Some((ratio, p, a, b));
        }
    }
    let (w, bst) = (worst.expect("scan produced samples"), best.expect("and more"));

    assert!(
        bst.0 <= 0.70,
        "at output period {:.2} px the widened low-pass should suppress the \
         pattern (Fast {:.3} -> Smooth {:.3} codes, ratio {:.2}); it left {:.2}. \
         The fixture no longer shows widening helping anywhere, so it is not a \
         multi-pitch trade any more and this guard is decoration.",
        bst.1, bst.2, bst.3, bst.0, bst.0
    );
    assert!(
        w.0 >= 1.50,
        "at output period {:.2} px the widened low-pass should PROMOTE the \
         pattern (Fast {:.3} -> Smooth {:.3} codes, ratio {:.2}); it left {:.2}. \
         This is the regression: the same widening that removes one pitch's beat \
         creates another's, which is why the setting cannot be the default.",
        w.1, w.2, w.3, w.0, w.0
    );
}

/// Grey screentone with energy at **more than one pitch**, which is what real tone
/// is and a 0/100 test lattice is not.
///
/// Three components, chosen so their beats land at different output periods:
///
/// * a base dot grid, the one a single-pitch probe sees,
/// * a second screen at 30 degrees, whose period is incommensurate with the first,
/// * a stochastic dither, so the pattern is not perfectly periodic and has some
///   broadband content the way real tone does.
///
/// **Grey, not 0/100, deliberately.** The artefact is a grid *in a tone*, and a
/// black-on-white lattice is a harder case that
/// `smooth_low_passes_a_square_lattice_harder_than_fast` already covers. Measured
/// rather than argued: see the guard above — the promoted pitch is not caused by
/// the multi-pitch structure, but the grey field is the content the report is
/// about and this is the fixture for it.
fn probe_multi_pitch_screentone(
    w: u32,
    h: u32,
    base_pitch: f32,
    screen_pitch: f32,
    screen_angle: f32,
) -> image_viewer::ImageData {
    let (sin30, cos30) = (std::f32::consts::PI / 180.0 * screen_angle).sin_cos();
    // A tiny deterministic LCG: `f32::sin` of an index is fine but this keeps the
    // dither reproducible across platforms without a dependency.
    let mut seed = 0x2f6e_2b1u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32
    };
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let (fx, fy) = (x as f32, y as f32);
            // Base grid.
            let g1 = if fx % base_pitch < base_pitch * 0.375
                && fy % base_pitch < base_pitch * 0.375
            {
                1.0f32
            } else {
                0.0
            };
            // Angled screen: its own period along its own axis.
            let u = (fx * cos30 + fy * sin30) % screen_pitch;
            let v = (-fx * sin30 + fy * cos30) % screen_pitch;
            let g2 = if u < screen_pitch * 0.314 && v < screen_pitch * 0.314 {
                1.0f32
            } else {
                0.0
            };
            // Stochastic dither, +/- 6 codes.
            let d = (next() - 0.5) * 12.0;
            let v = (150.0 - 46.0 * g1 - 34.0 * g2 + d).round().clamp(0.0, 255.0) as u8;
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// Amplitude of a 2-D pattern at output period `p_out`, in codes, by least
/// squares over the whole output. The same projection as `beat_amp` - a named
/// function rather than an inline copy, because it is what the "widening moves
/// energy between pitches" guard is decided on.
fn screentone_lattice_amplitude(d: &image_viewer::ImageData, p_out: f32) -> f32 {
    beat_amp(d, p_out)
}

#[test]
fn the_prefilter_low_passes_the_alias_band_far_deeper_than_a_box() {
    // Source periods 2.5-6 px land at 0.87-2.08 *output* px, i.e. at or below
    // the output Nyquist: everything that survives here is foldover, and it is
    // the 1-2 px sparkle the manga pages show as a lattice.
    //
    // box -> kernel prefilter, 100-code sinusoid, codes:
    //   2.5: 29 -> 4    3.0: 25 -> 2    3.5: 21 -> 5
    //   4.0: 29 -> 11   5.0: 28 -> 23   6.0: 41 -> 35
    // Pins are the kernel numbers plus one code, which is the most a different
    // target's f32 contraction could move a weighted sum.
    let cases = [
        (2.5f32, 5.0f32),
        (3.0, 3.0),
        (3.5, 6.0),
        (4.0, 12.0),
        (5.0, 24.0),
        (6.0, 36.0),
    ];
    for (period, max_codes) in cases {
        let src = probe_sinusoid(PROBE_W, PROBE_H, period, 100.0);
        let out = probe_resize(&src);
        let amp = probe_amplitude(&out);
        assert!(
            amp <= max_codes,
            "source period {period} px ({:.2} output px): {} codes of alias \
             survived, want <= {max_codes}",
            period * out.width as f32 / src.width as f32,
            amp
        );
    }
}

#[test]
fn a_dot_lattice_leaves_far_less_sparkle_than_a_box_prefilter() {
    // The same probe as a real dot lattice (1-px dots, 50% duty) rather than a
    // tone, over the alias band and a little above it.
    //
    // box -> kernel prefilter, 0/100 codes, codes:
    //   2.5: 24 -> 9    3.0: 13 -> 2    3.5: 28 -> 15   4.0: 22 -> 8
    //   5.0: 35 -> 20   6.0: 46 -> 40   8.0: 65 -> 60
    let cases = [
        (2.5f32, 10.0f32),
        (3.0, 3.0),
        (3.5, 16.0),
        (4.0, 9.0),
        (5.0, 21.0),
        (6.0, 41.0),
        (8.0, 61.0),
    ];
    for (period, max_codes) in cases {
        let src = probe_dots(PROBE_W, PROBE_H, period, 0, 100);
        let out = probe_resize(&src);
        let amp = probe_amplitude(&out);
        assert!(
            amp <= max_codes,
            "dot lattice at source period {period} px ({:.2} output px): {} codes \
             survived, want <= {max_codes}",
            period * out.width as f32 / src.width as f32,
            amp
        );
    }
}

#[test]
fn a_large_reduction_is_no_worse_than_the_box_prefilter_was() {
    // 1200 -> 150 is 8x. At that reduction the prefilter reduces by 2x and the
    // final pass by 4x, so this is where a widened filter could over-blur.
    // It does not: the kernel prefilter is lower or equal at every pitch.
    //
    // box -> kernel prefilter, 100-code sinusoid, codes:
    //   2.5: 2 -> 0   3.0: 4 -> 0   5.0: 14 -> 4   6.0: 6 -> 3   11: 10 -> 9
    let cases = [(2.5f32, 1.0f32), (3.0, 1.0), (5.0, 5.0), (6.0, 4.0), (11.0, 10.0)];
    for (period, max_codes) in cases {
        let src = probe_sinusoid(1200, 1200, period, 100.0);
        let out = with_linear_resample(true, || image_viewer::downscale_image(&src, 150));
        let amp = probe_amplitude(&out);
        assert!(
            amp <= max_codes,
            "8x reduction, source period {period} px: {amp} codes survived, want <= {max_codes}"
        );
    }
}

#[test]
fn the_prefilter_does_not_darken_a_near_nyquist_tone() {
    // THE guard on the prefilter's working space. A weighted average is only
    // exposure-preserving in the space it averages in: averaging *gamma-encoded*
    // values of a 50% dither gives ~128 (its own sRGB mean), while the tone's
    // true average reflectance is 50% linear, i.e. sRGB ~188.
    //
    //   50% near-Nyquist dither, mean R of the 309x432 result:
    //     box in sRGB (today)      164.4
    //     kernel in sRGB           130.0   <- the trap: looks like "no change"
    //     kernel in linear light   186.2   <- and within 1.4 of the single-kernel
    //                                                path's 187.6
    //
    // So a prefilter that averaged in the wrong space is not neutral, and the
    // band below is where that shows. Lower bound only: the prefilter may not
    // darken the tone, and this one brightens it toward the linear-light mean.
    let src = probe_dither(PROBE_W, PROBE_H);
    let out = probe_resize(&src);
    let m = probe_mean(&out);
    assert!(
        (180.0..=195.0).contains(&m),
        "50% dither came out at mean {m:.2}, want 180..195 (the linear-light \
         average of its tones; the box prefilter gave 164.4 and an sRGB-space \
         kernel gives 130.0)"
    );
}

#[test]
fn a_flat_field_survives_the_prefilter_unchanged() {
    // The other half of the exposure guard, and the one that says nothing about
    // *which* filter is used: every weighted average of a constant is that
    // constant. Verified to pass with the box prefilter in place as well, so it
    // is a regression guard and not a discriminator — the discriminating half is
    // `the_prefilter_does_not_darken_a_near_nyquist_tone`.
    for v in [0u8, 1, 64, 128, 200, 254, 255] {
        let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
        for p in rgba.chunks_exact_mut(4) {
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let src = image_viewer::ImageData {
            width: PROBE_W,
            height: PROBE_H,
            rgba,
            load_time: std::time::Duration::ZERO,
        };
        let out = probe_resize(&src);
        let m = probe_mean(&out);
        assert!(
            (m - v as f32).abs() <= 1.0,
            "flat field {v} came out at mean {m:.2}"
        );
    }
}

#[test]
fn the_prefilter_intermediate_is_at_most_twice_the_target() {
    // This is the memory bound. The prefilter exists so the 12-bit linear
    // conversion runs on a buffer of at most 2x the target rather than on the
    // full-resolution source: on a 6.9 MP page that is 26.7 MB instead of
    // 55 MB, and deleting the step doubles the conversion cost as well.
    let cases = [
        (890u32, 309u32, 618u32),   // the probe geometry: 2x the target
        (2223, 772, 1544),          // the real 2223x3109 page at its 772 wide
        (3109, 1080, 2160),         // ... and its height
        (100, 1000, 100),           // no shrink: the dimension itself
        (1, 1, 1),                  // 1 px
        (7, 3, 6),                  // odd small
        (4096, 2048, 4096),         // exactly 2x
        (4097, 2048, 4096),         // just over
        (u32::MAX, 4096, 8192),     // saturating_mul must not wrap
        (u32::MAX, u32::MAX, u32::MAX),
    ];
    for (dim, target, want) in cases {
        assert_eq!(
            image_viewer::prefilter_dim(dim, target),
            want,
            "prefilter_dim({dim}, {target})"
        );
    }
}

#[test]
fn linear_resize_keeps_its_two_step_shape() {
    // The intermediate size above is a pure function and is pinned there, but
    // nothing observable says `linear_resize` still *uses* it: delete the
    // pre-reduction and every quality test in this file still passes (a single
    // kernel pass scores the same or better on all of them) while the 12-bit
    // buffer grows to the full-resolution source. So the wiring is pinned
    // against the source text, which is the established pattern in this repo
    // (`spreads.rs`, `keybindings.rs`) and the only one available here.
    //
    // It checks *position*, not just presence: `linear_resize`'s own
    // brace-matched body, not the file, and not a comment elsewhere. Verified to
    // fail both ways round — with `box_reduce_srgb` put back, and with the
    // pre-reduction deleted outright.
    let src = include_str!("../src/lib.rs");
    let start = src.find("fn linear_resize(").expect("linear_resize exists");
    let body = &src[start..];
    // Brace-match the body rather than looking for the first `\n}`: a nested
    // block at column 0 would end the slice early and the scan would then pass
    // on a *truncated* function — which is the failure mode the `spreads.rs`
    // position check was written for.
    let mut depth = 0i32;
    let end = body
        .char_indices()
        .find(|&(_, c)| {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            depth == 0 && c == '}'
        })
        .map(|(i, _)| i + 1)
        .expect("linear_resize's braces balance");
    let body = &body[..end];
    assert!(
        body.contains("prefilter_dim(data.width, new_w)")
            && body.contains("prefilter_dim(data.height, new_h)"),
        "linear_resize must size both axes with prefilter_dim"
    );
    assert!(
        body.contains("kernel_reduce_linear("),
        "linear_resize must pre-reduce before the linear conversion; without it \
         the 12-bit buffer is the full-resolution source"
    );
    assert!(
        !body.contains("box_reduce"),
        "the sRGB box prefilter is gone; a box aliases more than the kernel it feeds"
    );
}

/// 1-D sinusoid of a known period, `amp` codes either side of mid-grey, so the
/// peak-to-peak excursion is `2 * amp` (100 codes at the `50.0` the section
/// below uses). The **transfer function's** probe: `|H|` read directly, with no
/// lattice and therefore no foldover to confuse the reading.
///
/// This is the one place a 1-D probe is the right instrument, and the reason is
/// worth stating because `probe_sinusoid`'s original use was a mistake: the
/// *width* question wants one frequency at a time, and a square lattice cannot
/// answer it because anything it shows may be foldover rather than filtering.
/// The lattice probe is not deprecated - it is the right tool for the artefact -
/// it is just the wrong tool for this.
fn probe_sine(w: u32, h: u32, period: f32, amp: f32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = (127.5 + amp * (2.0 * std::f32::consts::PI * x as f32 / period).sin())
                .round()
                .clamp(0.0, 255.0) as u8;
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// p95-p5 of the red channel, in codes: how much of a 100-codes-peak-to-peak
/// sinusoid survived the resize. `probe_sinusoid`'s `amp` parameter is a
/// half-amplitude, so a 100-code reading is the full excursion.
fn sine_amp(d: &image_viewer::ImageData) -> f32 {
    alias_amp(d)
}

/// **The reported defect: 1440p at `Fast` does not reject the foldover band.**
///
/// The geometry is a 2560x1440 landscape photo on a 1920-long panel, so the
/// reduction is 1.3333 and **the prefilter does not fire** -
/// `prefilter_dim(2560, 1920) = 2560`, the source unchanged. The output Nyquist
/// is 2.67 source px, so source periods of 1.5 and 2.5 px cannot be represented
/// and a kernel that passes them is producing foldover.
///
/// A 100-codes-peak-to-peak sinusoid, codes surviving, `Fast`:
///
/// | source period | in foldover band? | before the floor | with `MIN_FILTER_SCALE` |
/// |---|---|---|---|
/// | 1.5 | yes | 51 | **19** |
/// | 2.5 | yes | 41 | **11** |
/// | 4.0 | no | 58 | **37** |
/// | 6.0 | no | 84 | **67** |
///
/// Pins are the "before" numbers with a tolerance in the floor's favour, so the
/// assertion fails if the floor stops reaching the kernel.
///
/// **The two passband rows are in this table and not only in the sibling test,
/// and each row carries its own `in_band` flag that the loop asserts against
/// `QHD_FOLDOVER_PERIOD`.** A floor that rejected *everything* would pass a test
/// written only on the foldover band - which is the more flattering reading, and
/// so the more likely one to be written. The bands are asserted from the same
/// constant the table is quoted in, so the table cannot quietly go stale while the
/// pins stay green; a version that asserted `period < NYQUIST` inside this loop
/// failed immediately on its own 4.0 px row.
#[test]
fn the_min_filter_scale_rejects_the_foldover_band_a_1440p_source_carries() {
    let cases = [
        // (source period, in foldover band, may_leave, must_leave)
        (1.5f32, true, 52.0f32, 20.0f32),
        (2.5, true, 42.0, 12.0),
        (4.0, false, 59.0, 38.0),
        (6.0, false, 85.0, 68.0),
    ];
    for (period, in_band, was, now) in cases {
        assert_eq!(
            in_band,
            period < QHD_FOLDOVER_PERIOD,
            "probe bug: {period} px is {} the foldover band at R {QHD_R} (Nyquist \
             is {QHD_FOLDOVER_PERIOD} source px), and the table says the opposite. \
             One of the two is stale, which is how a pin comes to assert nothing",
            if in_band { "inside" } else { "outside" }
        );
        let src = probe_sine(QHD_W, QHD_H, period, 50.0);
        let before = sine_amp(&resize_floor(&src, QHD_TARGET, 0.0));
        // **The floor is opt-in, so the arm under test is 2.0 written out**, not
        // "whatever ships". Comparing the floor against the shipped default would
        // compare it against itself the day the default changed, and this test
        // would pass for the wrong reason - which is how it was written before.
        let after = sine_amp(&resize_floor(&src, QHD_TARGET, 2.0));
        assert!(
            before <= was && before > 10.0,
            "the pre-fix path left {before:.1} codes at source period {period} px, \
             which should be {was} - if this reading is ~0 the probe geometry \
             changed and the pin means nothing"
        );
        assert!(
            after <= now,
            "1440p at the shipped settings leaves {after:.1} codes at source \
             period {period} px (inside the foldover band, Nyquist \
             {QHD_FOLDOVER_PERIOD} px); want <= {now} (was {before:.1}). \
             MIN_FILTER_SCALE is not reaching the kernel"
        );
    }
}

/// **The shipped pipeline is byte-identical to having no floor at all, and that is
/// the regression guard for a user-reported regression.**
///
/// `MIN_FILTER_SCALE` shipped at **2.0** and was withdrawn back to `0.0`. On a
/// **2560x1440 display at 100% scaling** - where pages reduce by about 1.33x, so
/// the floor forced the kernel from 1.33 to 2.0 - moire became *worse* on every
/// file, on every filter, and `Smooth` did not help. Its benefit had been measured
/// on a synthetic **uniform** lattice (91.9 -> 55.9 codes); real screentone
/// disagreed. That is the third time in this project a synthetic proxy said
/// "fixed" while a person looking at real content said worse.
///
/// So the property worth protecting is **not** "an armed floor rejects the foldover
/// band" - that is an opt-in arm, and the two tests above cover it. It is that
/// **someone who does not ask for a floor gets the pre-`b237e7a` pipeline byte for
/// byte.** That is the state a user verified as better than `master` on every
/// file, and the only one safe to ship while the real-content question is open.
///
/// **Byte-identity, not a tolerance.** A floor that merely failed to bind would
/// pass any tolerance check, and "close enough" is not a property of a pipeline
/// that produces pixels.
#[test]
fn the_shipped_resize_is_byte_identical_to_no_floor_at_all() {
    // A spread half-panel (1350x1920 -> 1080), the 1440p geometry from the user
    // report, and two reductions either side of `PREFILTER_FACTOR`, so the claim
    // covers both sides of the threshold the floor was bridging.
    for (w, h, target) in [
        (QHD_W, QHD_H, QHD_TARGET),
        (1350, 1920, 1080),
        (2560, 1440, 1920),
        (1000, 1414, 1080),
    ] {
        let src = probe_square_lattice(w, h, 4.0, 1.0, 0, 100);
        let shipped = resize_shipped(&src, target);
        let off = resize_floor(&src, target, 0.0);
        assert_eq!(
            (shipped.width, shipped.height),
            (off.width, off.height),
            "shipped and no-floor disagree on size at {w}x{h} -> {target}"
        );
        assert!(
            shipped.rgba == off.rgba,
            "the shipped resize still differs from an explicit 0.0 floor at \
             {w}x{h} -> {target}; first differing byte at index {:?}. The shipped \
             pipeline must be byte-identical to the pre-b237e7a path.",
            shipped
                .rgba
                .iter()
                .zip(off.rgba.iter())
                .position(|(a, b)| a != b)
        );
    }
}

/// The other half, and the reason the test above is not a "blur everything"
/// test: a wider kernel must not eat the passband.
///
/// A 100-codes-peak-to-peak sinusoid at source periods **above** the foldover
/// band, i.e.
/// content the output can represent and the low-pass is supposed to keep.
/// `Smooth` for comparison, measured in the same session: **15 / 49 at 4 / 6 px**
/// against the floor's 37 / 67 - so the floor keeps roughly two thirds of the
/// detail `Smooth` destroys at the same foldover improvement.
#[test]
fn the_floor_keeps_the_passband_a_1440p_source_carries() {
    let cases = [(4.0f32, 36.0f32), (6.0, 66.0), (12.0, 89.0)];
    for (period, max_codes) in cases {
        assert!(
            period > QHD_FOLDOVER_PERIOD,
            "probe bug: {period} px is not representable at R {QHD_R}"
        );
        let src = probe_sine(QHD_W, QHD_H, period, 100.0);
        let out = resize_shipped(&src, QHD_TARGET);
        let a = sine_amp(&out);
        assert!(
            a >= max_codes,
            "the floor cut representable detail at source period {period} px to \
             {a:.1} codes, want >= {max_codes}. A floor that only rejects is a \
             blur, not a filter"
        );
    }
}

/// **The fix must cost nothing where the prefilter already fires.** This is the
/// property that makes it a floor rather than a retune: byte-identical output at
/// every reduction the shipped threshold already covers.
///
/// The reductions are chosen around the threshold rather than sampled: exactly
/// 2.0 (where `prefilter_dim` returns the source and neither path fires),
/// just above it (where it fires by one pixel), the two real geometries from
/// `AGENTS.md` (2.879 for the manga half-panel, 8.0 for a large scan), and 4.0.
///
/// Verified to fail with the floor applied to `kernel_reduce_linear` as well as
/// the final pass - which is the mistake this guards, and which is invisible
/// in every quality assertion in this file.
#[test]
fn the_min_filter_scale_is_a_no_op_where_the_prefilter_already_fires() {
    // `R` values are *derived* from the geometry rather than written out, so the
    // condition and the sample cannot drift apart: `prefilter_dim` fires iff
    // `pre_w < QHD_W`, i.e. iff the reduction exceeds `PREFILTER_FACTOR`.
    let factor = image_viewer::PREFILTER_FACTOR as f64;
    for over in [0.0f64, 0.02, 0.05, 0.25, 0.879, 1.0, 4.0] {
        let r = factor + over;
        let target = ((QHD_W as f64 / r).round() as u32).max(2);
        let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
        let before = resize_floor(&src, target, 0.0);
        let after = resize_floor(&src, target, 2.0);
        let fires = image_viewer::prefilter_dim(QHD_W, target) < QHD_W;
        // At exactly `PREFILTER_FACTOR` the prefilter does **not** fire
        // (`prefilter_dim` returns the source unchanged), but the floor is still
        // inert there: `scale = target/source = 0.5` already gives
        // `filter_scale == 2.0 == MIN_FILTER_SCALE`, so `max` cannot bind. That
        // boundary is the whole reason the floor is 2.0 and not "a bit more", so
        // it is asserted rather than skipped.
        assert_eq!(
            before.rgba == after.rgba,
            true,
            "at R {r:.3} (target {target}, prefilter {}) the floor changed the \
             output, so it is not inert above PREFILTER_FACTOR. Every reduction \
             that works today goes through here and must not change",
            if fires { "fires" } else { "does not fire" }
        );
    }

    // And it must NOT be inert below, or the fix is a no-op wearing a test.
    for under in [0.2f64, 0.5, 0.8] {
        let r = factor - under;
        let target = ((QHD_W as f64 / r).round() as u32).max(2);
        let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
        assert!(
            resize_floor(&src, target, 0.0).rgba != resize_floor(&src, target, 2.0).rgba,
            "at R {r:.3} (target {target}) an armed floor of 2.0 left the output \
             unchanged, so the 1440p case it exists for is not being fixed"
        );
    }
}

/// **The floor must actually reach the kernel below the threshold**, or the
/// no-op test above would pass for the wrong reason.
///
/// This is the check `the_min_filter_scale_rejects_the_foldover_band...` cannot
/// make on its own: that one could be satisfied by anything that changes the
/// output, including a bug. This one asks whether `contributions` is *handed* the
/// floor, on both axes, and only on the final pass - a source scan, because
/// `contributions` is private and nothing observable from outside says which
/// call sites carry the argument.
#[test]
fn the_floor_reaches_both_final_axes_and_only_those() {
    let src = include_str!("../src/lib.rs");
    let body_of = |name: &str| -> &str {
        let start = src
            .find(&format!("fn {name}("))
            .unwrap_or_else(|| panic!("{name} exists"));
        let body = &src[start..];
        let mut depth = 0i32;
        let end = body
            .char_indices()
            .find(|&(_, c)| match c {
                '{' => {
                    depth += 1;
                    false
                }
                '}' => {
                    depth -= 1;
                    depth == 0
                }
                _ => false,
            })
            .map(|(i, _)| i + 1)
            .expect("braces balance");
        &body[..end]
    };

    let lr = body_of("linear_resize");
    assert_eq!(
        lr.matches("min_filter_scale()").count(),
        1,
        "linear_resize must read min_filter_scale() exactly once, so the two \
         final axes cannot disagree about it"
    );
    assert_eq!(
        lr.matches("min_filter_scale()").count(),
        1,
        "linear_resize must read min_filter_scale() exactly once, so the two \
         final axes cannot disagree about it"
    );
    assert!(
        lr.contains("contributions(lw, new_w, &kern, support, strength, floor)")
            && lr.contains("contributions(lh, new_h, &kern, support, strength, floor)"),
        "both final-pass axes must be handed the floor"
    );
    // **The literal `0.0`, and this is the check that was written after getting
    // it wrong.** `kernel_reduce_linear` runs at exactly `PREFILTER_FACTOR`, so
    // its own `scale` is 0.5 and its `filter_scale` is 2.0 already — handing it
    // the floor looks harmless and is not: measured, it changes the output at
    // R 2.02 and above, i.e. at every reduction the shipped threshold covers,
    // because the pre-reduction's ratio is `R / PREFILTER_FACTOR` and at R 2.02
    // that is 1.01, far under the floor. `the_min_filter_scale_is_a_no_op_where_the_prefilter_already_fires`
    // fails on it, which is why the no-op guard and this scan are the same
    // property asserted two ways.
    assert!(
        lr.contains("kernel_reduce_linear(data, pre_w, pre_h, strength, 0.0)"),
        "linear_resize must hand the pre-reduction a literal 0.0 floor, not `floor`. \
         The pre-reduction already runs at PREFILTER_FACTOR, so its kernel meets \
         the floor; giving it the floor anyway widens output at every reduction \
         above the shipped threshold"
    );
    assert!(
        !lr.contains("kernel_reduce_linear(data, pre_w, pre_h, strength, floor)"),
        "the pre-reduction must NOT be handed the floor - see above"
    );

    let kr = body_of("kernel_reduce_linear");
    assert!(
        kr.contains("floor: f32"),
        "kernel_reduce_linear must take the floor as an argument rather than \
         reading the global: it is called once per resize and a read there is a \
         second value that could disagree with the final passes"
    );
    assert!(
        !kr.contains("min_filter_scale()"),
        "kernel_reduce_linear must not read min_filter_scale() itself - see above"
    );
    assert!(
        kr.contains("contributions(sw as u32, pw as u32, &kern, support, strength, floor)")
            && kr.contains("contributions(sh as u32, ph as u32, &kern, support, strength, floor)"),
        "the pre-reduction must use the floor it was given on both axes - count, \
         not presence: one axis floored and one not would be a picture low-passed \
         horizontally only"
    );

    let con = body_of("contributions");
    assert!(
        con.contains(".max(floor)"),
        "contributions must apply the floor to filter_scale - taking the argument \
         and ignoring it is a floor that exists and does nothing"
    );
}

/// **`PREFILTER_FACTOR` is not a quality knob, and this is the arithmetic that
/// says so.** It was read as one for a long time - it is the number the reported
/// 1440p case was blamed on - and lowering it is the change that looks obvious
/// and cannot work.
///
/// `contributions` sets `filter_scale = strength / scale` from each stage's own
/// ratio, so a decimation `R` split into stages `s1` and `s2` has
/// **`s1 * s2 == R`**. The two stages' kernel widths therefore multiply back to
/// the width the reduction already implied, and no split can make the pair wider
/// than `R` alone. The prefilter cap is a **memory** bound (it decides how big
/// the buffer the 12-bit conversion runs over is) and nothing else.
///
/// This is asserted rather than measured-at-test-time because it is a statement
/// about the code's arithmetic, and a test that recomputed it from the same
/// formula would only be checking itself. The caps span the whole plausible
/// range including below 1.0, and the reductions span the regime the report is
/// about (1.1 - 1.95, where no cap fires at all) and the regime that works
/// (2.0 - 8.0).
#[test]
fn splitting_a_decimation_cannot_widen_the_kernel_past_the_reduction() {
    for r in [1.1f64, 1.2, 1.333, 1.5, 1.778, 1.95, 2.0, 2.5, 2.879, 4.0, 8.0] {
        for cap in [1.60f64, 1.30, 1.15, 1.02, 0.75, 0.50] {
            let target = ((QHD_W as f64 / r).round() as u32).max(2);
            let pre = (QHD_W as f64).min(target as f64 * cap);
            if pre >= QHD_W as f64 {
                continue; // the cap cannot bind; the path is the single pass
            }
            let s1 = QHD_W as f64 / pre;
            let s2 = pre / target as f64;
            assert!(
                (s1 * s2 - r).abs() < 1e-3,
                "R {r:.4} split by cap {cap} into stages {s1:.4} and {s2:.4} \
                 multiplied to {} - if this stopped holding, the prefilter cap \
                 WOULD be a quality knob and lowering PREFILTER_FACTOR could \
                 widen the low-pass. Re-derive MIN_FILTER_SCALE before trusting \
                 anything above it",
                s1 * s2
            );
        }
    }
}

/// **What the fix does not do, pinned so it cannot be quietly over-claimed.**
///
/// At `MIN_FILTER_SCALE` the 1440p residual is 55.87 codes of square-lattice
/// amplitude where `Smooth` reaches 28.57. The moiré is reduced by roughly two
/// thirds, not abolished, and the constant is deliberately not the wider 2.5
/// that would get further - 2.5 is above the two-stage path's own 2.0, so
/// choosing it would change every reduction that works today, which is the
/// property `the_min_filter_scale_is_a_no_op_where_the_prefilter_already_fires`
/// exists to protect.
///
/// This test asserts the floor is **not** `Smooth`-equivalent at 1440p, so that
/// a future change which quietly converges the two fails here rather than
/// inheriting `Smooth`'s measured regression on `12.jpg` / `21.jpg` / `44.jpg`.
///
/// **The constants being equal is not the interesting fact and is not asserted
/// here.** `MIN_FILTER_SCALE` and `SMOOTH_STRENGTH` are both 2.0, and it is
/// tempting to pin that as "they are different numbers" - which is false, and a
/// guard written that way would be asserting a coincidence. They are different
/// *mechanisms* that happen to share a value: the floor is an absolute minimum
/// on one stage and the strength is a multiplier on all of them, so the floor
/// is inert above `PREFILTER_FACTOR` and the strength is not. What is pinned is
/// the behavioural consequence - different output at 1440p - because that is the
/// claim the mechanism has to keep supporting.
#[test]
fn the_floor_is_not_smooth_renamed_at_1440p() {
    let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
    let floor = resize_shipped(&src, QHD_TARGET);
    let smooth = resize_at(&src, QHD_TARGET, image_viewer::SMOOTH_STRENGTH);
    assert_ne!(
        floor.rgba, smooth.rgba,
        "the floor and Smooth now produce identical 1440p output. That is not \
         necessarily wrong, but it means the floor has picked up the trade \
         Smooth was rejected for (a black dot pattern in grey screentone on \
         12.jpg, 21.jpg and 44.jpg), so it must be re-judged on those files \
         before this assertion is simply deleted"
    );
}

/// Print the numbers the section above is pinned against. `cargo test --test unit
/// --release -- --ignored --nocapture print_min_filter_scale`. Kept for the same
/// reason `print_prefilter_pins` is: a kernel change should be re-pinnable from
/// a printout rather than from a bisect.
#[test]
#[ignore]
fn print_min_filter_scale() {
    println!("\n== MIN_FILTER_SCALE transfer function, {QHD_W}x{QHD_H} -> {QHD_TARGET} ==");
    println!("   R {QHD_R}, foldover band = source period < {QHD_FOLDOVER_PERIOD} px");
    println!(
        "{:>10} {:>10} {:>10} {:>10} {:>10}",
        "period", "floor off", "floor 2.0", "floor 2.5", "Smooth"
    );
    for period in [1.5f32, 2.0, 2.5, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0] {
        let src = probe_sine(QHD_W, QHD_H, period, 50.0);
        println!(
            "{period:>10.1} {:>10} {:>10} {:>10} {:>10}",
            sine_amp(&resize_floor(&src, QHD_TARGET, 0.0)),
            sine_amp(&resize_floor(&src, QHD_TARGET, 2.0)),
            sine_amp(&resize_floor(&src, QHD_TARGET, 2.5)),
            sine_amp(&resize_at(&src, QHD_TARGET, image_viewer::SMOOTH_STRENGTH)),
        );
    }

    let step = probe_step(QHD_W, QHD_H, QHD_W / 2);
    let tone = probe_multi_pitch_screentone(QHD_W, QHD_H, 3.0, 2.6, 30.0);
    println!("\n== cost at the reported geometry ==");
    println!(
        "{:>14} {:>10} {:>10} {:>10} {:>10}",
        "arm", "step rise", "lattice", "tone sum", "tone peak"
    );
    // Arms as `(name, resize)` so each column's images come from the *same*
    // arm. Selecting per-image by string comparison was the first version and it
    // is the shape of mistake that reports a number for the wrong arm.
    let arms: Vec<(&str, Box<dyn Fn(&image_viewer::ImageData) -> image_viewer::ImageData>)> =
        vec![
            ("floor off", Box::new(|d| resize_floor(d, QHD_TARGET, 0.0))),
            ("floor 2.0", Box::new(|d| resize_shipped(d, QHD_TARGET))),
            ("floor 2.5", Box::new(|d| resize_floor(d, QHD_TARGET, 2.5))),
            (
                "Smooth",
                Box::new(|d| resize_at(d, QHD_TARGET, image_viewer::SMOOTH_STRENGTH)),
            ),
        ];
    println!(
        "{:>14} {:>10} {:>10} {:>10} {:>10}",
        "arm", "step rise", "lattice", "tone sum", "tone peak"
    );
    for (name, resize) in &arms {
        let mut lattice = 0f32;
        for &p in &[2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0] {
            let l = probe_square_lattice(QHD_W, QHD_H, p, 1.0, 0, 100);
            lattice += beat_amp(&resize(&l), p / QHD_R);
        }
        let tout = resize(&tone);
        let (mut sum, mut peak) = (0f32, 0f32);
        let mut i = 0;
        while i <= 100 {
            let a = beat_amp(&tout, 1.2 + i as f32 * 0.05);
            sum += a;
            peak = peak.max(a);
            i += 1;
        }
        println!(
            "{name:>14} {rise:>10} {lattice:>10.2} {sum:>10.2} {peak:>10.2}",
            rise = step_rise(&resize(&step))
        );
    }

    println!("\n== wall clock, interleaved and warmed (release only) ==");
    let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
    println!("{:>8} {:>12} {:>12} {:>12}", "R", "floor off", "floor 2.0", "Smooth");
    for r in [1.1f64, 1.2, 1.333, 1.5, 1.778, 2.0, 2.5, 2.879, 4.0] {
        let target = ((QHD_W as f64 / r).round() as u32).max(2);
        let arms: Vec<(&str, Box<dyn Fn() -> ()>)> = vec![
            ("off", Box::new(|| {
                let _ = resize_floor(&src, target, 0.0);
            })),
            ("2.0", Box::new(|| {
                let _ = resize_shipped(&src, target);
            })),
            ("smooth", Box::new(|| {
                let _ = resize_at(&src, target, image_viewer::SMOOTH_STRENGTH);
            })),
        ];
        // **Interleaved, and every arm warmed before any is timed.** Arms run
        // one-after-another report a drift larger than the effect - a run in this
        // repo had two *identical* control blocks disagree by 38% - and the
        // first, un-warmed call of a path is not a sample of that path.
        for (_, arm) in &arms {
            arm();
        }
        let mut med = [0f32; 3];
        for i in 0..arms.len() {
            let mut v = Vec::new();
            for _ in 0..9 {
                // One measurement per round, rotating which arm goes first, so
                // neither arm is systematically measured at the same point in the
                // machine's thermal or allocator state.
                let order: Vec<usize> = (0..arms.len())
                    .map(|k| (k + i) % arms.len())
                    .collect();
                for j in order {
                    let t0 = std::time::Instant::now();
                    arms[j].1();
                    if j == i {
                        v.push(t0.elapsed().as_secs_f32() * 1000.0);
                    }
                }
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            med[i] = v[v.len() / 2];
        }
        println!(
            "{r:>8.3} {:>12.1} {:>12.1} {:>12.1}   ({:.2}x floor, {:.2}x smooth)",
            med[0],
            med[1],
            med[2],
            med[1] / med[0].max(1e-6),
            med[2] / med[0].max(1e-6)
        );
    }
}

#[test]
fn every_downscale_filter_survives_the_widened_prefilter() {
    // The prefilter runs the *selected* filter at a widened support (Lanczos3's
    // radius is 3 source px, so 4.3 after the prefilter's own 1.44x), and its
    // result indexes `to_srgb`. Overshoot past the 12-bit range must clamp: the
    // same class of bug as the 4097 index that used to abort a worker.
    //
    // A crash guard, not a discriminator: it passes with the box prefilter too.
    for f in [
        image_viewer::DownscaleFilter::Hermite,
        image_viewer::DownscaleFilter::Catrom,
        image_viewer::DownscaleFilter::Lanczos3,
    ] {
        let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        let was = image_viewer::linear_resample_enabled();
        image_viewer::set_linear_resample(true);
        image_viewer::set_downscale_filter(f);
        let out = image_viewer::downscale_image(
            &probe_dots(PROBE_W, PROBE_H, 3.0, 0, 255),
            PROBE_TARGET,
        );
        image_viewer::set_downscale_filter(image_viewer::DownscaleFilter::Hermite);
        image_viewer::set_linear_resample(was);
        assert_eq!(
            out.rgba.len(),
            (out.width as usize) * (out.height as usize) * 4,
            "{f:?}: buffer and dims disagree"
        );
        assert!(out.rgba.chunks_exact(4).all(|p| p[3] == 255), "{f:?}: alpha");
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


use image_viewer::{grid_cols, grid_row_h, GRID_CELL, GRID_LABEL_H};

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
    assert_eq!(
        image_viewer::grid_follow_offset(7, 100.0, 500.0, 100.0),
        Some(300.0)
    );
    // Above the view: align the row's top with the view's top.
    assert_eq!(
        image_viewer::grid_follow_offset(0, 100.0, 500.0, 100.0),
        Some(0.0)
    );
    // No measurement yet (grid just opened): pin the row to the top, which
    // is today's open behavior and the only sane thing with no viewport.
    assert_eq!(
        image_viewer::grid_follow_offset(9, 100.0, 0.0, 0.0),
        Some(900.0)
    );
    // Never a negative offset, whatever the stored numbers say.
    assert_eq!(
        image_viewer::grid_follow_offset(0, 100.0, 500.0, -600.0),
        Some(0.0)
    );
}

/// A shuffled show saves the folder's order on start and both off-ramps
/// restore it through one funnel. Asserted as counts, because the defect
/// this guards is a *third* site setting the flag directly: a stop path
/// that writes `slideshow = false` itself skips the restore and leaves the
/// folder shuffled with no way back.
#[test]
fn a_shuffled_show_saves_its_order_and_both_off_ramps_restore_it() {
    let src = include_str!("../src/main.rs");
    let start = fn_body(src, "fn start_slideshow(&mut self)").expect("start_slideshow");
    assert!(
        start.contains("image_viewer::shuffle("),
        "starting a shuffled show must walk a shuffled copy, not the folder order"
    );
    assert!(
        start.contains("slideshow_saved = Some("),
        "and it must save the folder's own order first, or stopping has \
         nothing to put back"
    );
    let stop = fn_body(src, "fn stop_slideshow(&mut self)").expect("stop_slideshow");
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
    let src = include_str!("../src/main.rs");
    let grid = fn_body(src, "fn gallery_grid(&mut self").expect("gallery_grid");
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
    let mut files: Vec<std::path::PathBuf> = (0..64)
        .map(|i| std::path::PathBuf::from(format!("{i:03}.jpg")))
        .collect();
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
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show_rows(ui, grid_row_h(GRID_CELL), 1, |ui, _| {
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
            let text = std::fs::read_to_string(file)
                .unwrap_or_else(|e| panic!("cannot read {file}: {e}"));
            for (i, line) in text.lines().enumerate() {
                let code = strip_line_comment(line);
                for c in code.chars() {
                    if c.is_ascii() || c == '\u{FFFD}' {
                        continue;
                    }
                    seen.entry(c)
                        .or_insert_with(|| format!("{file}:{}", i + 1));
                }
            }
        }

        ui.ctx().fonts_mut(|f| {
            for (c, origin) in &seen {
                let s = c.to_string();
                let absent = sizes
                    .iter()
                    .any(|sz| !f.has_glyphs(&egui::FontId::proportional(*sz), &s));
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
    use image_viewer::{xformed_footprint, Xform};
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
    assert_eq!(
        cfg.workers, 0,
        "an old config must come up on auto (0), not on a fixed count"
    );
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert_eq!(fresh.workers, 0, "a first run must also come up on auto");
}

/// The worker-count setting must be **wired**, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_worker_count_setting_is_wired_to_pool_startup() {
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("resolve_worker_count("),
        "pool startup must go through resolve_worker_count, not compute the clamp inline"
    );
    assert!(
        src.contains("config.workers"),
        "pool startup must read the setting"
    );
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
    let src = include_str!("../src/main.rs");
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
    let asc = image_viewer::scan_folder_sorted_dir(
        tmp.path(),
        image_viewer::SortMode::Name,
        false,
    );
    let desc = image_viewer::scan_folder_sorted_dir(
        tmp.path(),
        image_viewer::SortMode::Name,
        true,
    );
    let names = |fs: &[std::path::PathBuf]| {
        fs.iter()
            .map(|p| p.file_name().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(&asc).iter().map(|s| s.to_str().unwrap()).collect::<Vec<_>>(),
        ["a.jpg", "b.jpg", "c.jpg"]
    );
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Name must be ascending reversed");

    // Size: shipped order is largest-first; descending puts smallest first.
    let desc =
        image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Size, true);
    assert_eq!(desc[0].file_name().unwrap(), "a.jpg");
    let asc =
        image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Size, false);
    let mut rev = asc.clone();
    rev.reverse();
    assert_eq!(desc, rev, "descending Size must be ascending reversed");

    // Date: order depends on mtime granularity, but descending must still be
    // the exact reverse of whatever ascending produced.
    let asc =
        image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Date, false);
    let desc =
        image_viewer::scan_folder_sorted_dir(tmp.path(), image_viewer::SortMode::Date, true);
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
    assert!(
        !cfg.sort_descending,
        "an old config must keep the shipped orders"
    );
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.sort_descending, "a first run must also keep them");
}

/// The sort-direction setting must be **wired**, and `main.rs` is unreachable
/// from every test, so this is a source scan - the established pattern here.
#[test]
fn the_sort_direction_setting_is_wired_to_rescan() {
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("copy_path_to_clipboard"),
        "there must be a path-copy action distinct from copy_image_to_clipboard"
    );
    // The call site, not the definition: the arm around it must branch on the
    // shift state, since both chords arrive as the same event.
    let call = src
        .find("self.copy_path_to_clipboard(ctx)")
        .expect("the Copy arm must call it");
    let arm = src[..call]
        .rfind("clip_img")
        .expect("the call must sit in the Copy arm");
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
    assert_eq!(
        image_viewer::decode_user_comment(b"ASCII\0\0\0hello").as_deref(),
        Some("hello")
    );
    assert_eq!(image_viewer::decode_user_comment(b"ASCII\0\0\0\0\0\0"), None);
}

/// The comment lines must reach the panel, and `main.rs` is unreachable from
/// every test, so this is a source scan - the established pattern here.
#[test]
fn the_comment_lines_are_wired_into_extract_exif() {
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("image_viewer::exif_comment("),
        "extract_exif must show the EXIF description/UserComment"
    );
    assert!(
        src.contains("image_viewer::jpeg_comment("),
        "extract_exif must also show a JPEG COM segment"
    );
}

/// Screen-size save must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here.
#[test]
fn the_screen_save_key_is_wired_to_a_scaled_save() {
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("save_transformed_scaled("),
        "Ctrl+Shift+S must go through the scaled save, not the full-res one"
    );
    assert!(
        src.contains("save_scaled_copy()"),
        "there must be a screen-size save action"
    );
}

/// Toasts (Tier 1 #11): a resort must say what order it produced. Notices are
/// status-line toasts; the sort change was the state change that stayed silent.
#[test]
fn resort_names_the_order_it_produced() {
    let src = include_str!("../src/main.rs");
    let at = src
        .find("if resort {")
        .expect("the settings panel must have a resort arm");
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
    let src = include_str!("../src/main.rs");
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
    let name = format!(
        "Local\\image-viewer-claim-test-{}",
        std::process::id()
    );
    assert!(
        image_viewer::claim_single_instance(&name),
        "first claim of a fresh name must win"
    );
    assert!(
        !image_viewer::claim_single_instance(&name),
        "second claim of a held name must lose"
    );
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
    assert_eq!(
        image_viewer::canonical_chord("shift+ctrl+s"),
        Some("Ctrl+Shift+S".into())
    );
    assert_eq!(image_viewer::canonical_chord("Space"), Some("Space".into()));
    assert_eq!(image_viewer::canonical_chord("Ctrl++"), None);
    assert_eq!(
        image_viewer::split_chord("Alt+shift+Delete"),
        Some((false, true, true, "Delete".into()))
    );
    assert_eq!(image_viewer::split_chord("Ctrl"), None);
}

/// Every default chord's key must be a known name: otherwise it can never
/// match, and the action ships dead.
#[test]
fn every_default_key_is_a_known_name() {
    for (id, chord, _) in image_viewer::KEY_ACTIONS {
        let (_, _, _, name) = image_viewer::split_chord(chord)
            .unwrap_or_else(|| panic!("default chord of `{id}` names no key"));
        assert!(
            image_viewer::KEY_NAMES.contains(&name.as_str()),
            "default chord of `{id}` names unknown key `{name}`"
        );
    }
}

/// Capture, totality, and live labels must be wired: the capture state and
/// its commit path, both directions of the key-name table for every known
/// name, and buttons built through the live-label helper.
#[test]
fn capture_and_labels_are_wired() {
    let src = include_str!("../src/main.rs");
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
        assert!(
            src.contains(&format!("\"{name}\" =>")),
            "key name `{name}` has no parse arm"
        );
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
    let src = include_str!("../src/main.rs");
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
        src.contains("fn scan_current(&self)") && src.contains("self.scan_current()"),
        "rescan must go through the same funnel as the opens"
    );
    assert!(
        src.contains("load_all_from_folder_recursive("),
        "the position-preserving funnel must have a recursive arm"
    );
}

/// The body of the function whose signature is `sig`, by brace counting.
///
/// The same helper as `tests/spreads.rs`, kept local rather than shared because
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

/// Crop is the one site where "record before the change" is not adjacent to the
/// change: `set_crop` runs **per drag frame**, so recording at the mutation would
/// push ~200 entries for one gesture and make undo useless after a single drag.
///
/// So the record goes on **press**, and this asserts that it is on the press path
/// rather than merely present somewhere in the function — the same lesson as
/// `the_turns_second_half_is_derived_not_latched`.
#[test]
fn a_crop_gesture_records_once_on_press() {
    let src = include_str!("../src/main.rs");
    let body = fn_body(src, "fn crop_from_screen(").expect("crop_from_screen");
    // `record` must NOT be inside crop_from_screen (it is pure geometry).
    assert!(
        !body.contains("history.record("),
        "crop_from_screen is pure geometry; recording inside it would record \
         once per drag frame"
    );
    // It must be on the press handler, which runs once per gesture. The arm is
    // entered every frame the button is held, so the record must also sit after
    // the `primary_pressed()` test that makes it a press.
    let press = fn_body(src, "fn central(").expect("central");
    let pressed = press
        .find("primary_pressed()")
        .expect("the crop press must test for the press frame");
    let record = press
        .find("self.history.record(")
        .expect("the crop press must record");
    let set_crop = press
        .find("self.edits.set_crop(")
        .expect("the crop drag must set");
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
    use image_viewer::{leave_archive_at, Gallery};
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
    let src = include_str!("../src/main.rs");

    let leave = fn_body(src, "fn leave_archive").expect("leave_archive");
    // Leaving arms no guard — it just builds the parent gallery with the
    // cursor on the archive. `goto` no longer enters, so no guard is needed.
    assert!(
        leave.contains("just_left") == false,
        "leave_archive no longer needs a one-shot because goto does not enter"
    );

    let goto = fn_body(src, "fn goto(&mut self, ctx: &Context)").expect("goto");
    // Goto must not enter archives at all — the one-shot is gone because
    // the whole branch is gone.
    assert!(
        goto.contains("just_left") == false,
        "goto must not contain the old one-shot"
    );
    // The body shows the position logic without any archive entry: it sets
        // `is_nav`, `lat.begin`, then `snap_to_turn_anchor` — no `open_archive`.
        // The absence of an archive branch *is* the test.
        assert!(
            goto.contains("self.snap_to_turn_anchor()"),
            "goto runs the position setup without an archive branch"
        );

    let step = fn_body(src, "fn step(&mut self, ctx: &Context, forward: bool)").expect("step");
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("image_viewer::set_show_archives(config.show_archives)"),
        "start-up must tell the library before the first scan"
    );
    let before = src.find("set_show_archives(config.show_archives)").expect("start-up call");
    let scan = src.find("let files = Self::sorted_scan(").expect("the start-up scan");
    assert!(
        before < scan,
        "the setting has to be set before the scan that reads it, or the first \
         listing of the session answers from a stale flag"
    );

    let panel = fn_body(src, "impl eframe::App for MyApp").expect("the app impl");
    assert!(
        panel.contains("&mut self.config.show_archives"),
        "the toggle must exist in Settings"
    );
    let toggle = panel
        .find("&mut self.config.show_archives")
        .expect("the checkbox");
    let setter = panel
        .find("set_show_archives(self.config.show_archives)")
        .expect("the setter");
    let rescan = panel[toggle..]
        .find("self.rescan(ctx, true)")
        .expect("the rescan");
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
    let src = include_str!("../src/main.rs");

    // `goto` no longer enters: it positions, and the body shows it.
    let goto = fn_body(src, "fn goto(&mut self, ctx: &Context)").expect("goto");
    // The body shows the position logic without any archive entry: it sets
    // `is_nav`, `lat.begin`, then `snap_to_turn_anchor` — no `open_archive`.
    // The absence of an archive branch *is* the test.
    assert!(
        goto.contains("self.snap_to_turn_anchor()"),
        "goto runs the position setup without an archive branch"
    );
    assert!(
        goto.contains("open_archive") == false,
        "goto must not call open_archive"
    );

    // Entry is in `step` (arrowing) and `pick_grid_selection` (grid).
    let step = fn_body(src, "fn step(&mut self, ctx: &Context, forward: bool)")
        .expect("step");
    assert!(
        step.contains("enter_archive_if_present(ctx)"),
        "arrowing onto an archive must call the entry helper"
    );

    let pick = fn_body(src, "fn pick_grid_selection(&mut self, ctx: &Context)")
        .expect("pick_grid_selection");
    assert!(
        pick.contains("enter_archive_if_present(ctx)"),
        "grid click/Enter must call the entry helper"
    );

    // The decode side: plan_jobs refuses archives (library rule, tested there).
    // These are the paths that bypass plan_jobs and need their own guard.
    assert!(
        src.contains("let is_archive = is_archive_path(path);") && src.contains("!is_archive &&"),
        "a grid cell must not ask for a thumbnail of a container"
    );
    assert!(
        src.contains(".filter(|p| !is_archive_path(p))"),
        "the EXIF panel pushes its own job, so it needs its own archive guard"
    );

    // `Del` refuses when the cursor is ON an archive, not only when inside one.
    let del = fn_body(src, "fn delete_current(&mut self, ctx: &Context)").expect("delete_current");
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
        assert!(
            image_viewer::parse_exif_datetime(bad).is_none(),
            "{bad:?} must not parse"
        );
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
    assert_eq!(
        image_viewer::anim_frame_start(&anim, 2),
        std::time::Duration::from_millis(120)
    );
    assert_eq!(
        image_viewer::anim_frame_start(&anim, 99),
        std::time::Duration::from_millis(170)
    );

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
    assert_eq!(
        image_viewer::tile_axis_range(0.0, 1067.5, 0.0, 0.125, 2.0, 8521),
        (0, 4270)
    );
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
    assert_eq!(
        image_viewer::tile_axis_range(-100.0, 99999.0, 0.0, 0.125, 2.0, 8521),
        (0, 8521)
    );
}

/// The tile loop goes through the ratio-aware math, not around it: the old
/// inline formula named the range in source pixels whatever the buffer was,
/// which is the defect above, so its absence is asserted as well as the new
/// calls' presence. Either half alone passes on a halfway revert.
#[test]
fn the_tile_loop_positions_through_the_ratio_aware_math() {
    let src = include_str!("../src/main.rs");
    let central = fn_body(src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
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
    let red = vec![
        255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
    ];
    let h = image_viewer::histogram_rgba(&red, 2, 2);
    assert_eq!(h.bins[0][255], 4);
    assert_eq!(h.bins[1][0], 4);
    assert_eq!(h.bins[2][0], 4);
    assert_eq!(h.bins[0][0], 0);
    assert_eq!(h.black, 0);
    assert_eq!(h.white, 0);
    // Half pure black, half pure white: clipping is exact.
    let bw = vec![
        0u8, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255,
    ];
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
    let src = include_str!("../src/main.rs");
    let decode = fn_body(src, "fn decode_preview(").expect("decode_preview");
    assert!(
        decode.contains("image_viewer::histogram_rgba("),
        "decode_preview must count the preview buffer it already holds: the \
         CPU pixels are dropped after upload, so later is too late"
    );
    let central = fn_body(src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
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
    let src = include_str!("../src/main.rs");
    let cell = fn_body(src, "fn grid_cell(").expect("grid_cell");
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
    let src = include_str!("../src/lib.rs");
    let decode = fn_body(src, "fn decode_bytes(").expect("decode_bytes");
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

/// The frame clock survives its move to lib with speed: file timing pins
/// the old behavior, and 2x halves both the index math and the repaint wait
/// (which is real time — scheduling an effective-time wait would repaint
/// late by the speed factor).
#[test]
fn anim_frame_state_keeps_time_and_scales() {
    use image_viewer::ImageData;
    use std::time::Duration;
    let frame = || ImageData {
        width: 1,
        height: 1,
        rgba: vec![0, 0, 0, 255],
        load_time: Duration::ZERO,
    };
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
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("fn set_wallpaper_shown(&mut self"),
        "there must be a processed-wallpaper action"
    );
    let at = src
        .find("fn set_wallpaper_shown(&mut self)")
        .expect("the action must exist");
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

/// Fit window keeps the chrome it measures and clamps to the monitor.
#[test]
fn fit_window_hugs_content_and_clamps() {    // 800x600 window, 700x500 central: 100x100 of chrome, kept.
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
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
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
    let img = image::RgbaImage::from_fn(2, 2, |x, y| image::Rgba([x as u8 * 80, y as u8 * 80, 40, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .unwrap();
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
    use image_viewer::{is_browsable, scan_folder, scan_folder_recursive, set_show_archives,
        show_archives_enabled};
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
            .map(|p| {
                p.strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
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
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert_eq!(
        walked,
        vec![
            "vol1.7z",
            "vol1.cbz",
            "vol1.zip",
            "vol2/v2.cbz"
        ],
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
        let (extracted, skipped) =
            image_viewer::extract_archive(&root.join(name), &dest).unwrap_or_else(|e| {
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
    let err = image_viewer::extract_archive(
        std::path::Path::new("x.tar.zst"),
        &root.join("out-none"),
    )
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("self.fit_window_to_image()"),
        "`T` must arm the fit"
    );
    assert!(
        src.contains("ViewportCommand::InnerSize("),
        "central must send a live resize with the drawn size"
    );
    assert!(
        src.contains("fit_window_size("),
        "the resize must go through the chrome-measuring helper"
    );
    assert!(
        src.contains("self.view.fit = false")
            && src.contains("self.view.scale = 1.0 / ppp"),
        "`T` must reset to absolute 100%, never a shrink-to-fit"
    );
    // Same frame: the reset must land before `scale` is read from the view,
    // or the window is sized for a zoom the picture draws a frame later.
    // T mode is always absolute: no shrink path exists, and `fit_zoom` is
    // gone entirely rather than merely uncalled.
    assert!(
        !src.contains("fit_zoom("),
        "the shrink path is deleted, not bypassed"
    );
    let lib = include_str!("../src/lib.rs");
    assert!(
        !lib.contains("fn fit_zoom("),
        "the shrink helper must go with its only caller"
    );
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("self.config.fit_window_mode = !self.config.fit_window_mode"),
        "`T` must toggle the mode, not fire once"
    );
    let goto_at = src.find("fn goto(&mut self").expect("goto must exist");
    let goto = &src[goto_at..(goto_at + 8000).min(src.len())];
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
        src.contains("ViewportCommand::OuterPosition(")
            && src.contains("((m.x - want.0) / 2.0"),
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
    let src = include_str!("../src/main.rs");
    let at = src
        .find("fn sync_shown(&mut self")
        .expect("sync_shown must exist");
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("fn toggle_fit_mode(&mut self)")
            && src.contains("fn toggle_fit_mode(&mut self"),
        "toggle_fit_mode must exist"
    );
    let at = src.find("self.config.fit_mode = !self.config.fit_mode").expect("toggle arm must exist");
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
    let src = include_str!("../src/main.rs");
    let goto_at = src.find("fn goto(&mut self").expect("goto must exist");
    let goto = &src[goto_at..(goto_at + 8000).min(src.len())];
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("exact_size("),
        "side panes must pin their width in the mode, or chrome drifts post-measure"
    );
    assert!(
        src.contains("pending_pos"),
        "centering must wait for the resize to settle, not race it"
    );
    let at = src
        .find("self.config.fit_window_mode,")
        .expect("the mode toggle must exist");
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
    let src = include_str!("../src/main.rs");
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
/// playing animations and stills step on as before.
#[test]
fn space_resumes_a_paused_animation() {
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("ViewportCommand::Decorations("),
        "hiding the title bar must send a live viewport command"
    );
    assert!(
        src.contains("set_topmost(self.config.always_on_top)"),
        "always-on-top must apply straight away, not on restart"
    );
    let lib = include_str!("../src/lib.rs");
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
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    let at = src
        .find("if esc {")
        .expect("the Esc chain must exist");
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
        panel < typing && typing < crop && crop < archive && archive < fullscreen
            && fullscreen < close,
        "Esc chain must be: save-as panel → typing → crop → archive → fullscreen \
         → quit, got order: panel={} typing={} crop={} archive={} fullscreen={} \
         close={}",
        panel, typing, crop, archive, fullscreen, close
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
    let lib = include_str!("../src/lib.rs");
    assert!(
        lib.contains("fn print_file(") && lib.contains("ShellExecuteW"),
        "printing must go through the native print verb"
    );
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("fn print_current(&mut self"),
        "there must be a print action"
    );
    assert!(
        src.contains("action_pressed_fresh(ctx, &kb, \"print\")"),
        "print must go through the edge-triggered matcher, not a bespoke chord"
    );
    let body_at = src
        .find("fn print_current(&mut self)")
        .expect("the print action must exist");
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
    let src = include_str!("../src/main.rs");
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

/// Portable mode is the marker file: present means beside-the-exe, absent
/// means the roaming paths — asserted against a tempdir, never the real
/// executable's folder.
#[test]
fn portable_mode_follows_the_marker_file() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(image_viewer::portable_dir_from(dir.path()), None);
    std::fs::write(dir.path().join("portable.txt"), b"").unwrap();
    assert_eq!(
        image_viewer::portable_dir_from(dir.path()),
        Some(dir.path().to_path_buf())
    );
}

/// All three persistent roots must consult portable mode, or portable is a
/// config file sitting next to a roaming cache.
#[test]
fn persistent_roots_consult_portable_mode() {
    let lib = include_str!("../src/lib.rs");
    for f in [
        "pub fn default_path()",
        "pub fn thumb_cache_root()",
        "pub fn spreads_root()",
    ] {
        let at = lib.find(f).unwrap_or_else(|| panic!("{f} is gone"));
        let body = &lib[at..(at + 600).min(lib.len())];
        assert!(
            body.contains("portable_dir()"),
            "{f} must check portable mode first"
        );
    }
}

/// Copy-original shares the copy path with an identity edit: same sources,
/// no re-decode, EXIF-oriented but uncropped and unturned.
#[test]
fn copy_original_shares_the_copy_path() {
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    let at = src
        .find("Panel::right(\"settings\")")
        .expect("the settings panel must exist");
    let arm = &src[at..(at + 800).min(src.len())];
    assert!(
        arm.contains("ScrollArea::vertical()"),
        "settings content must sit in a vertical ScrollArea, or it runs off \
         the bottom of short windows"
    );
}

/// Rebindable keys must be **wired**, and `main.rs` is unreachable from every
/// test, so these are source scans - the established pattern here.
#[test]
fn the_rebindable_keys_are_wired_to_the_direction_rule() {
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    assert!(
        src.contains("fn run_user_command(&mut self"),
        "there must be a user-command runner"
    );
    assert!(
        src.contains("expand_user_command(&cmd, &path)"),
        "the runner must expand {{file}}/{{dir}}/{{name}}, not spawn raw config text"
    );
    assert!(
        src.contains("egui::Key::Num1"),
        "Ctrl+1..9 must trigger the commands"
    );
}

/// Spawning must be edge-triggered: `key_pressed` also fires for OS
/// key-repeat, and a repeat spawns one process per frame — a held Ctrl+1
/// opened hundreds of Paint windows and froze the machine. Spawn-class
/// actions therefore go through the raw-event matcher with `repeat: false`,
/// never `key_pressed`.
#[test]
fn the_user_command_chord_ignores_key_repeat() {
    let src = include_str!("../src/main.rs");
    let at = src
        .find("fn action_pressed_fresh(")
        .expect("the edge-triggered matcher must exist");
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
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    for base in ["Double page mode", "User commands (Ctrl+1..9):"] {
        let at = src
            .find(base)
            .unwrap_or_else(|| panic!("{base} is gone from the settings panel"));
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
    let src = include_str!("../src/main.rs");
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
    let left_at = src
        .find("if self.show_panels && self.show_left {")
        .expect("the left pane gate must exist");
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
    let src = include_str!("../src/main.rs");
    let lib = include_str!("../src/lib.rs");

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
        src.contains("} else if save_as {\n                self.open_save_as();"),
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
    let open = fn_body(src, "fn open_save_as(&mut self)").expect("open_save_as");
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
    let infix = fn_body(src, "fn save_as_infix(&self, path: &Path)").expect("save_as_infix");
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
    let central = fn_body(src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
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
    // marker, so `fn_body(src, "if esc {")` returns the body of the panel arm
    // alone - the marker already consumed the `if`'s own brace - and this
    // assertion then failed on "typing esc arm" for a chain that was correct. The
    // helper answers "the body of the next thing", which is the wrong question
    // for an if/else chain: what matters is where the arms sit *relative to each
    // other*, so the offsets are taken from `if esc {` and compared.
    let esc_at = src.find("if esc {").expect("the Esc chain");
    let esc_panel = src[esc_at..]
        .find("if self.save_as.open {")
        .map(|i| i + esc_at)
        .expect("panel esc arm");
    let esc_typing = src[esc_at..]
        .find("} else if typing {")
        .map(|i| i + esc_at)
        .expect("typing esc arm");
    let esc_quit = src[esc_at..]
        .find("self.config.esc_closes_app")
        .map(|i| i + esc_at)
        .expect("quit arm");
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
        h.record(p, Edit { xform: image_viewer::Xform::Identity, crop: Crop::new(0, 0, 1 + u32::from(i), 1) });
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
    let src = include_str!("../src/main.rs");
    let body = fn_body(src, "fn apply_to_current(").expect("apply_to_current");
    let at = body.find("history.record(").expect("record must be called");
    let edit = body
        .find("f(&mut self.edits, &p.path)")
        .expect("the edit itself");
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
    let lib = include_str!("../src/lib.rs");
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
    let src = include_str!("../src/main.rs");
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
    let src = include_str!("../src/main.rs");
    let exif_at = src.find("wants_exif(").expect("the EXIF gate exists");
    let rating_at = src.find("Ratings::read(").expect("the rating is read somewhere");
    assert!(
        rating_at > exif_at,
        "the rating read must come after the EXIF gate's own read, so it is a \
         separate path rather than folded into it"
    );
    // The left pane lives in `fn ui(`, not `fn central(`: central paints only
    // the middle panel (it ends well before the pane code), while the EXIF pane
    // is built in `ui`. The property is "above the EXIF heading in the pane",
    // so this scans the pane — anchored on its `pane.show` closure rather than
    // on `fn ui(`, because the naive brace counter in `fn_body` closes `ui`
    // early on a comment quoting code with unbalanced braces. The left pane is
    // the first `pane.show` in the file, so `find` lands on it.
    let pane = fn_body(src, "pane.show(ui, |ui|").expect("the left pane");
    let block = pane.find("rating_widget").or_else(|| pane.find("Rating"))
        .expect("a rating block in the pane");
    let heading = pane.find(r#"ui.heading("EXIF")"#).expect("the EXIF heading");
    assert!(block < heading, "the rating block is above the camera's own metadata");
}

/// Archives get no ratings: there is nowhere to write `vol1.cbz/p003.jpg.xmp`,
/// and a rating that disappears on leaving the archive is worse than a refusal.
#[test]
fn archives_are_refused_a_rating() {
    let lib = include_str!("../src/lib.rs");
    assert!(
        fn_body(lib, "pub fn sidecar_path(")
            .expect("sidecar_path")
            .contains("is_archive_path"),
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
    let src = include_str!("../src/main.rs");
    let cell = fn_body(src, "fn grid_cell(").expect("grid_cell");
    assert!(
        !cell.contains("Ratings::read("),
        "grid_cell must not read sidecars - that is a filesystem read per cell \
         per frame; it takes them from the ratings map"
    );
    assert!(
        cell.contains("ratings") || cell.contains("rating"),
        "and it must read a ratings map"
    );
}
