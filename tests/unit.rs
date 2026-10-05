use image_viewer::{load_image, Gallery, ZoomPan};

/// `set_linear_resample` / `set_downscale_filter` are process-wide globals, and
/// `cargo test` runs the tests in this file concurrently. Anything that asserts
/// on a *specific* filter path has to take this lock and put the globals back,
/// or it silently tests whichever path another test last selected.
static FILTER_GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Run `f` with the downscale filter globals forced to `linear`, restoring the
/// previous setting afterwards.
fn with_linear_resample<T>(linear: bool, f: impl FnOnce() -> T) -> T {
    with_resample(linear, image_viewer::downscale_strength(), f)
}

/// Set both filter globals at once, restore both after.
///
/// **One lock acquisition, never two.** These used to be separate helpers and
/// nesting them - `with_strength(s, || with_linear_resample(true, ..))` -
/// deadlocked on the second `lock()` of a non-reentrant `Mutex`, which is why the
/// strengths are swept through here rather than through a wrapper of its own.
/// A guard that only worked when the two settings were never changed together
/// would have been a guard that hung instead of testing anything.
fn with_resample<T>(linear: bool, strength: f32, f: impl FnOnce() -> T) -> T {
    let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let (was_linear, was_strength) = (
        image_viewer::linear_resample_enabled(),
        image_viewer::downscale_strength(),
    );
    image_viewer::set_linear_resample(linear);
    image_viewer::set_downscale_strength(strength);
    let out = f();
    image_viewer::set_linear_resample(was_linear);
    image_viewer::set_downscale_strength(was_strength);
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

/// The geometry the probes below use: the same 2.880x reduction as the real
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
/// the single-row statistic in `probe_amplitude` cannot see a vertical lattice.
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

fn default_strength() -> f32 {
    if default_config().smooth_downscale {
        image_viewer::SMOOTH_STRENGTH
    } else {
        image_viewer::FAST_STRENGTH
    }
}

/// The **shipped default** resize. Every probe below this line that reports on
/// "the path" means this one, so it is defined as the default rather than as a
/// strength constant that has to be updated in step with the config.
fn probe_resize(src: &image_viewer::ImageData) -> image_viewer::ImageData {
    resize_at(src, PROBE_TARGET, default_strength())
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
    // pattern — which is what a dot screen is — the amplitude is `A` at every
    // masked pixel, so the least-squares fit above already returns `A` (up to the
    // 1/sqrt(2) of adding the two axes). Dividing that by the square root of the
    // pixel count then reports ~0.0002 where `alias_amp` on the same image reads
    // 10 codes, i.e. it divides a real signal by ~700 at this output size, and it
    // cannot resolve a coherent peak at all. Every conclusion drawn from it —
    // "no peak at any output period from 1.5 to 8 px at any strength, the residue
    // is broadband" — was read off that divided quantity. `beat_amp`, the
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
        lr.contains("kernel_reduce_linear(data, pre_w, pre_h, strength)"),
        "the pre-reduction must be handed the strength linear_resize read"
    );
    assert!(
        lr.contains("contributions(lw, new_w, &kern, support, strength)")
            && lr.contains("contributions(lh, new_h, &kern, support, strength)"),
        "both final-pass axes must be handed the same strength"
    );

    let kr = body_of("kernel_reduce_linear");
    assert!(
        kr.contains("strength: f32")
            && kr.contains("contributions(sw as u32, pw as u32, &kern, support, strength)")
            && kr.contains("contributions(sh as u32, ph as u32, &kern, support, strength)"),
        "the pre-reduction must take the strength as an argument and use it on \
         both axes - reading it for itself would let it disagree with the final \
         passes"
    );

    let con = body_of("contributions");
    assert!(
        !con.contains("downscale_strength()"),
        "contributions must take the strength as a parameter, not read the \
         setting: it is called four times per resize and a read there is four \
         chances for the halves to disagree"
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

/// Amplitude of a 2-D pattern at output period `p_out`, in codes, by least squares
/// over the whole output. The same projection as `beat_amp` — this is the number
/// the guard above is decided on, so it is a named function rather than an
/// inline copy.
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
