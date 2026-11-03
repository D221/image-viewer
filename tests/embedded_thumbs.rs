//! Embedded EXIF thumbnails (grid cells only).
//!
//! Camera JPEGs carry a ~160 px thumbnail in EXIF IFD1. Decoding that instead
//! of the file is 15-40x faster for the gallery grid, but the spec does not say
//! whether the camera stored it already rotated, so the rotation is *decided
//! per file from the aspect ratio* and the caller falls back to a real decode
//! whenever that is ambiguous. See `embedded_thumb_rotation`.

#[path = "common/exif_fixtures.rs"]
mod exif_fixtures;
use exif_fixtures::{Endian, jpeg_with_thumbnail};
use image_viewer::{ImageData, load_image_preview_full};
use std::path::Path;

/// Write bytes to a temp file and load a preview from it at `target`.
fn load(bytes: &[u8], target: u32) -> image_viewer::PreviewAndFull {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("t.jpg");
    std::fs::write(&p, bytes).unwrap();
    load_image_preview_full(Path::new(&p), target).expect("must load")
}

/// Write `bytes` to a temp file, returning the path.
#[allow(dead_code)]
fn stage(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("t.jpg");
    std::fs::write(&p, bytes).unwrap();
    (tmp, p)
}

fn self_consistent(d: &ImageData, what: &str) {
    assert_eq!(
        d.rgba.len(),
        (d.width as usize) * (d.height as usize) * 4,
        "{what}: {}x{} but {} bytes",
        d.width,
        d.height,
        d.rgba.len()
    );
    assert!(d.width > 0 && d.height > 0, "{what}: zero dimension");
}

#[test]
fn test_embedded_thumbnail_is_used_for_small_targets() {
    // A 1200x900 source with a 160x120 thumbnail. Orientation 1, so there is
    // no rotation question at all.
    let bytes =
        jpeg_with_thumbnail(1200, 900, 1, &exif_fixtures::jpeg(160, 120), Endian::Little, None);
    let r = load(&bytes, 256);
    assert_eq!((r.preview.width, r.preview.height), (160, 120));
    // No full decode happened, so there is no full image and it is below target.
    assert!(r.full.is_none());
    assert!(r.below_target);
    self_consistent(&r.preview, "preview");
    // The reported source size is still the real one.
    assert_eq!(r.orig, Some((1200, 900)));
}

#[test]
fn test_embedded_thumbnail_ignored_for_large_targets() {
    // Above the gate the difference in sharpness would be visible, so the file
    // has to be decoded for real.
    let bytes =
        jpeg_with_thumbnail(1200, 900, 1, &exif_fixtures::jpeg(160, 120), Endian::Little, None);
    let r = load(&bytes, 4096);
    assert!(r.full.is_some(), "large target must decode the real image");
    self_consistent(&r.preview, "preview");
    self_consistent(r.full.as_ref().unwrap(), "full");
}

// Deliberately no wall-clock assertion here. The win is large and real in
// release (0.6 ms vs 12 ms for a 1.1 MP source; 16-40x on the 24-38 MP files in
// test-images), but a debug build is dominated by unrelated overhead - notably
// `exif_orientation` re-reading and re-parsing the file - so a timing assertion
// passes or fails depending on the build profile and machine load. The contract
// worth pinning is structural, and the three tests above do that: the embedded
// thumbnail is used, it is a real decode of the right bytes, and no full image
// is produced.

#[test]
fn test_embedded_thumbnail_rotation_from_aspect_ratio() {
    // Orientation 6 = rotate 90 CW to display. The stored main image is
    // 1200x900 (landscape); a correctly-oriented thumbnail is therefore
    // portrait 120x160, and an unrotated one would be landscape 160x120.
    let main =
        jpeg_with_thumbnail(1200, 900, 6, &exif_fixtures::jpeg(120, 160), Endian::Little, None);
    let r = load(&main, 256);
    assert_eq!((r.preview.width, r.preview.height), (120, 160));
    assert_eq!(r.orig, Some((900, 1200)), "orig is reported oriented");
    self_consistent(&r.preview, "preview");

    // Same orientation, but this camera stored the thumbnail unrotated
    // (landscape) — so it does need the tag applied, ending up portrait.
    let unrotated =
        jpeg_with_thumbnail(1200, 900, 6, &exif_fixtures::jpeg(160, 120), Endian::Little, None);
    let r = load(&unrotated, 256);
    assert_eq!((r.preview.width, r.preview.height), (120, 160));
    self_consistent(&r.preview, "preview");
}

#[test]
fn test_ambiguous_rotation_falls_back_to_a_real_decode() {
    // Orientation 3 (180) preserves the aspect ratio, so the evidence cannot
    // separate the hypotheses. We must not guess: decode properly instead.
    let bytes =
        jpeg_with_thumbnail(1200, 900, 3, &exif_fixtures::jpeg(120, 160), Endian::Little, None);
    let r = load(&bytes, 256);
    assert!(r.full.is_some(), "ambiguous orientation must fall back, not guess");
    self_consistent(&r.preview, "preview");
}

#[test]
fn test_near_square_source_falls_back_rather_than_guess() {
    // A square source makes both rotation hypotheses look the same, so there is
    // nothing to decide from. Fall back.
    let bytes =
        jpeg_with_thumbnail(600, 595, 6, &exif_fixtures::jpeg(158, 160), Endian::Little, None);
    let r = load(&bytes, 256);
    assert!(r.full.is_some(), "near-square source must not be guessed at");
    self_consistent(&r.preview, "preview");
}

#[test]
fn test_big_endian_exif_is_handled() {
    let bytes =
        jpeg_with_thumbnail(1200, 900, 1, &exif_fixtures::jpeg(160, 120), Endian::Big, None);
    let r = load(&bytes, 256);
    assert_eq!((r.preview.width, r.preview.height), (160, 120));
    self_consistent(&r.preview, "preview");
}

#[test]
fn test_malformed_thumbnail_offsets_do_not_panic() {
    // Untrusted input: the offset/length in IFD1 are attacker-controlled, so
    // every one of these must degrade to "no thumbnail" and still load.
    let thumb = exif_fixtures::jpeg(160, 120);
    let cases: Vec<(&str, Option<u32>)> = vec![
        ("offset past EOF", Some(0x7FFF_FFFF)),
        ("offset inside the image but past the EXIF block", Some(900_000)),
        ("offset 0", Some(0)),
    ];
    for (what, at) in cases {
        let bytes = jpeg_with_thumbnail(1200, 900, 1, &thumb, Endian::Little, at);
        // Must not panic, must still load.
        let r = load(&bytes, 256);
        assert!(r.full.is_some(), "{what}: should have fallen back");
        self_consistent(&r.preview, what);
    }
}

#[test]
fn test_truncated_thumbnail_does_not_panic() {
    // Length that stops mid-JPEG: the SOI/EOI check has to catch it.
    let thumb = exif_fixtures::jpeg(160, 120);
    let cut = &thumb[..thumb.len() / 2];
    let bytes = jpeg_with_thumbnail(1200, 900, 1, cut, Endian::Little, None);
    let r = load(&bytes, 256);
    self_consistent(&r.preview, "truncated");
    assert!(r.full.is_some(), "a truncated thumbnail must be rejected");
}

#[test]
fn test_thumbnail_as_large_as_the_image_is_rejected() {
    // A "thumbnail" nearly the size of the source is not a thumbnail; using it
    // would be a pointless upscale.
    let bytes =
        jpeg_with_thumbnail(400, 300, 1, &exif_fixtures::jpeg(390, 290), Endian::Little, None);
    let r = load(&bytes, 256);
    assert!(r.full.is_some());
    self_consistent(&r.preview, "preview");
}

#[test]
fn test_jpeg_without_exif_still_works() {
    // The common case: no APP1 at all.
    let mut img = image::RgbaImage::new(600, 400);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 255) as u8, (y % 255) as u8, 10, 255]);
    }
    let mut bytes = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .unwrap();
    let r = load(&bytes, 256);
    self_consistent(&r.preview, "preview");
    self_consistent(r.full.as_ref().unwrap(), "full");
}
