//! Shared fixtures for the rotation/save/crop test targets.
//!
//! `tests/rotate_save.rs` was split by subject (see `tests/source_size.rs`);
//! this module holds what more than one of those targets needs. Each target
//! compiles its own copy through `#[path = "common/rotate_fixtures.rs"]`, the
//! pattern `tests/common/` already uses.
#![allow(dead_code)]

use image_viewer::{Xform, apply_exif_orientation};
use std::path::Path;

/// A picture whose every quadrant is a different flat colour, so any wrong
/// turn, flip or transpose is obvious rather than merely plausible.
pub fn quadrants(w: u32, h: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
        let (hx, hy) = (x * 2 >= w, y * 2 >= h);
        match (hx, hy) {
            (false, false) => image::Rgb([255, 0, 0]), // top-left: red
            (true, false) => image::Rgb([0, 255, 0]),  // top-right: green
            (false, true) => image::Rgb([0, 0, 255]),  // bottom-left: blue
            (true, true) => image::Rgb([255, 255, 0]), // bottom-right: yellow
        }
    }))
}

/// Write `img` as a JPEG, with `exif_tag` (0 for none) spliced in as a real
/// APP1 segment so `exif_orientation` can read it back.
pub fn write_jpeg(
    dir: &Path,
    name: &str,
    img: &image::DynamicImage,
    exif_tag: u16,
) -> std::path::PathBuf {
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg).unwrap();

    if exif_tag != 0 {
        // APP1 layout: FFE1, length, "Exif\0\0", TIFF header, IFD0, one entry.
        // Big-endian keeps the hand-built bytes readable.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM\x00\x2a"); // big-endian, version 42
        tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 at offset 8
        tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // orientation
        tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_be_bytes()); // one value
        // A SHORT's value lives in the first 2 bytes of the 4-byte field.
        tiff.extend_from_slice(&exif_tag.to_be_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_be_bytes()); // no IFD1

        let mut app1 = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let mut seg = Vec::new();
        seg.extend_from_slice(&[0xFF, 0xE1]);
        seg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        seg.extend_from_slice(&app1);

        // Insert straight after SOI.
        let mut out = Vec::new();
        out.extend_from_slice(&bytes[0..2]);
        out.extend_from_slice(&seg);
        out.extend_from_slice(&bytes[2..]);
        bytes = out;
    }

    let p = dir.join(name);
    std::fs::write(&p, &bytes).unwrap();
    p
}

/// What the viewer would put on screen: EXIF applied at decode, then the
/// quarter turns, done with the `image` crate exactly as the viewer does.
pub fn what_the_viewer_shows(
    img: &image::DynamicImage,
    exif_tag: u16,
    quarters: u8,
) -> image::DynamicImage {
    let mut data = image_viewer::ImageData {
        width: img.width(),
        height: img.height(),
        rgba: img.to_rgba8().into_raw(),
        load_time: std::time::Duration::ZERO,
    };
    apply_exif_orientation(&mut data, exif_tag);
    for _ in 0..(quarters & 3) {
        data = {
            let img =
                image::RgbaImage::from_raw(data.width, data.height, std::mem::take(&mut data.rgba))
                    .unwrap();
            let r = image::imageops::rotate90(&img);
            let mut d = image_viewer::ImageData {
                width: r.width(),
                height: r.height(),
                rgba: r.into_raw(),
                load_time: std::time::Duration::ZERO,
            };
            apply_exif_orientation(&mut d, 0);
            d
        };
    }
    let rgba = image::RgbaImage::from_raw(data.width, data.height, data.rgba).unwrap();
    image::DynamicImage::ImageRgba8(rgba)
}

pub fn assert_same_pixels(got: &image_viewer::ImageData, want: &image::DynamicImage, what: &str) {
    assert_eq!((got.width, got.height), (want.width(), want.height()), "{what}: wrong dimensions");
    let want_rgba = want.to_rgba8();
    assert_eq!(got.rgba.len(), want_rgba.as_raw().len(), "{what}: wrong buffer length");
    // JPEG is lossy, so compare loosely: a transform that is *correct* still
    // changes the DCT blocks and re-decodes slightly differently. A wrong
    // transform changes whole quadrants, so a mean-error threshold separates
    // them cleanly.
    let mut worst: f64 = 0.0;
    for (a, b) in got.rgba.chunks_exact(4).zip(want_rgba.as_raw().chunks_exact(4)) {
        for (x, y) in a.iter().zip(b.iter()) {
            worst = worst.max(f64::from(x.abs_diff(*y)));
        }
    }
    assert!(worst <= 24.0, "{what}: largest channel error {worst} — a wrong transform?");
}

// --- helpers for the crop tests -------------------------------------------

/// The stored picture turned the way the viewer shows it.
///
/// **One** application of the EXIF tag, and it comes from `display_xform`, which
/// already composes it. Calling `apply_exif_orientation` here as well applies it
/// twice, which for a mirror or a quarter turn cancels to the identity and makes
/// the expectation quietly wrong for every non-trivial tag — which is how this
/// helper was first written.
///
/// The algebra this relies on is not taken on trust:
/// `the_transform_algebra_matches_the_viewers_own_rotation` pins `display_xform`
/// against `apply_exif_orientation` for all nine tags, and
/// `every_transform_has_an_inverse_and_it_cancels_both_ways` pins the inverse on
/// real pixels. So the chain from button to file is checked at every link.
pub fn display_xform_pixels(
    img: &image::DynamicImage,
    exif_tag: u16,
    user: Xform,
) -> image::DynamicImage {
    let rgba = img.to_rgba8();
    let (ow, oh, px) = image_viewer::display_xform(exif_tag, user)
        .apply_rgba(img.width(), img.height(), rgba.as_raw())
        .unwrap();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(ow, oh, px).unwrap())
}

/// Cut a rectangle out of an already-displayed RGBA image, in display pixels.
///
/// Hand-rolled rather than `image::imageops::crop_imm`, which needs `&mut` and so
/// cannot be pointed at a borrow. The point of the crop tests is that this
/// expectation shares no code with the implementation, so a one-line loop is
/// better than a clever one.
pub fn crop_display(rgba: &image::RgbaImage, rect: (u32, u32, u32, u32)) -> (u32, u32, Vec<u8>) {
    let (x, y, w, h) = rect;
    let stride = rgba.width() as usize * 4;
    let src = rgba.as_raw();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let start = (row as usize) * stride + (x as usize) * 4;
        out.extend_from_slice(&src[start..start + w as usize * 4]);
    }
    (w, h, out)
}

pub fn exif_orientation_from(bytes: &[u8]) -> u16 {
    // Only used on in-memory fixtures; reads the tag straight out of the buffer.
    let i = bytes.windows(6).position(|w| w == b"Exif\0\0").expect("no EXIF in fixture");
    let base = i + 6;
    // This fixture is big-endian, which is how write_jpeg builds it.
    let ifd = base + u32::from_be_bytes(bytes[base + 4..base + 8].try_into().unwrap()) as usize;
    let n = u16::from_be_bytes(bytes[ifd..ifd + 2].try_into().unwrap());
    for i in 0..n as usize {
        let e = ifd + 2 + i * 12;
        if u16::from_be_bytes(bytes[e..e + 2].try_into().unwrap()) == 0x0112 {
            return u16::from_be_bytes(bytes[e + 8..e + 10].try_into().unwrap());
        }
    }
    0
}
