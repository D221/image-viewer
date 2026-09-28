use image_viewer::load_image;
use std::path::Path;

/// Standardized comparison of load methods.
/// Verifies all strategies return correct dimensions and produces consistent results.
///
/// Builds its own image in a temp dir. It used to hardcode
/// `test-images\booklet_front.jpg`, but `test-images/` is gitignored, so this
/// test could never pass in a fresh clone - it failed with NotFound before any
/// of the decode work in this session, and "just supply the file locally" is a
/// poor thing to leave in a suite you are trying to use as a safety net.
#[test]
fn standardized_compare_load_methods() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("source.jpg");

    // A gradient, so a downscale that silently returned the wrong size or a
    // blank buffer is at least visible in the bytes, not just the dimensions.
    let (w, h) = (2400u32, 1600u32);
    let mut img = image::RgbImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([
            (x % 256) as u8,
            (y % 256) as u8,
            ((x + y) % 256) as u8,
        ]);
    }
    image::DynamicImage::ImageRgb8(img)
        .save(&path)
        .expect("write fixture jpeg");

    // Strategy 1: downscale to 1600
    let d1 = load_image(&path, 1600).unwrap();

    // Strategy 2: downscale to 800
    let d2 = load_image(&path, 800).unwrap();

    // Strategy 3: full resolution (very high max_dim)
    let d3 = load_image(&path, 50000).unwrap();

    // All should produce same original dimensions (before scaling applies, but load_image
    // returns scaled dimensions). Actually load_image returns scaled width/height.
    // We just verify they are consistent and positive.
    assert!(d1.width > 0 && d1.height > 0);
    assert!(d2.width > 0 && d2.height > 0);
    assert!(d3.width > 0 && d3.height > 0);

    // Downscale 800 should have smaller pixel count than full
    assert!(d2.width <= d3.width);
    assert!(d2.height <= d3.height);

    // All should have non-empty rgba buffers
    assert!(!d1.rgba.is_empty());
    assert!(!d2.rgba.is_empty());
    assert!(!d3.rgba.is_empty());

    // Every decode must satisfy the invariant the whole texture pipeline
    // depends on: a buffer exactly w*h*4. `ColorImage::from_rgba_unmultiplied`
    // asserts this, and a violation there kills the worker, so it is worth
    // checking on every strategy rather than only on the one path that
    // happened to break.
    for (label, d) in [("1600", &d1), ("800", &d2), ("full", &d3)] {
        assert_eq!(
            d.rgba.len(),
            (d.width as usize) * (d.height as usize) * 4,
            "{label}: {}x{} but {} bytes",
            d.width,
            d.height,
            d.rgba.len()
        );
    }

    // The full decode is the source dimensions, unchanged.
    assert_eq!((d3.width, d3.height), (w, h));
    eprintln!("DBG d1={}x{} d2={}x{} d3={}x{}", d1.width, d1.height, d2.width, d2.height, d3.width, d3.height);
    // The full decode is the source dimensions, unchanged.
    assert_eq!((d3.width, d3.height), (w, h));

    // `max_dim` is a *minimum* long-side target, not a hard cap. `decode_jpeg_scaled`
    // picks a power-of-two DCT scaling factor, and deliberately chooses the
    // smallest reduction that still leaves >= max_dim - so a source that sits
    // between two factors comes back larger than asked for rather than smaller
    // and blurry. Here 2400/2 = 1200 < 1600, so the 1600 request is served at
    // full size. Assert the real contract, not the intuitive one:
    // every result is at least the requested target on its long side.
    for (label, d, target) in [("1600", &d1, 1600u32), ("800", &d2, 800), ("full", &d3, 50000)] {
        assert!(
            d.width.max(d.height) >= target.min(w.max(h)),
            "{label}: long side {} is below the requested {target}",
            d.width.max(d.height)
        );
    }
    // A smaller target must still never come back larger than a bigger one.
    assert!(d2.width <= d1.width && d2.height <= d1.height);
    // And a request well under the source really does downscale.
    assert!(d2.width < d3.width, "800 target should have downscaled");
}
