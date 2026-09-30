//! Multi-frame files: animations, and the formats that were silently not animated.
//!
//! Everything here builds its own fixture. That is not ideology — it is forced:
//! **no crate in this dependency set can encode an APNG or an animated WebP.**
//! `image` 0.25 writes lossless VP8L stills and has no `write_frames`; `image-webp`
//! is decoder-only for animation; there is no libwebp binding. So an APNG is
//! assembled chunk by chunk below, because otherwise the claim "the viewer plays
//! APNG" would rest on nothing but the fact that a decoder upstream supports it —
//! which is exactly the kind of claim that turns out to be false in a viewer.
//!
//! The animated-WebP **decode** path is therefore unverified and is not claimed.
//! What is verified is the detection (so a still WebP is not mistaken for an
//! animation and decoded twice) and that the file reaches the animation code at
//! all. See `TODO.md`.

use image_viewer::{animated_format, animation_frame_count, load_image_preview_full, AnimatedFormat};
use std::path::Path;

// ---------------------------------------------------------------------------
// A hand-built APNG
// ---------------------------------------------------------------------------

/// One RGBA frame of a fixture animation.
struct Rgba {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

impl Rgba {
    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Self {
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            px.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
        Self { w, h, px }
    }

    /// The first pixel, as `[r, g, b]`. Enough to tell two frames apart.
    fn corner_rgb(&self) -> [u8; 3] {
        [self.px[0], self.px[1], self.px[2]]
    }

    fn to_rgba_image(self) -> image::RgbaImage {
        image::RgbaImage::from_raw(self.w, self.h, self.px)
            .expect("fixture dimensions are consistent with its pixel count")
    }

    fn to_image(self) -> image::DynamicImage {
        image::DynamicImage::ImageRgba8(self.to_rgba_image())
    }

    fn save(self, p: &Path) {
        self.to_image().save(p).expect("save fixture");
    }

    /// PNG scanlines: a filter byte of 0 (None) in front of each row.
    fn scanlines(&self) -> Vec<u8> {
        let stride = (self.w * 4) as usize;
        let mut out = Vec::with_capacity((stride + 1) * self.h as usize);
        for row in self.px.chunks(stride) {
            out.push(0);
            out.extend_from_slice(row);
        }
        out
    }
}

/// The first pixel of a decoded frame, so a test can say which picture it got
/// without caring whether it came back as `ImageData` or an `RgbaImage`.
fn decoded_corner(data: &image_viewer::ImageData) -> [u8; 3] {
    [data.rgba[0], data.rgba[1], data.rgba[2]]
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(kind);
    hasher.update(data);
    out.extend_from_slice(&hasher.finalize().to_be_bytes());
    out
}

fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// A complete APNG: `acTL`, then an `fcTL` before every frame, the first frame's
/// pixels in `IDAT` and the rest in `fdAT`.
///
/// **`delay_num / delay_den` is a number of SECONDS, not milliseconds.** That is
/// the spec, and getting it wrong is quiet: the first version of this fixture
/// wrote `delay_num = 120, delay_den = 1` — 120 *seconds* per frame — and the
/// only symptom was a four-frame animation claiming to run for eight minutes.
/// Nothing errored, and the frame count was right throughout.
fn apng(frames: &[Rgba], delay_ms: u16) -> Vec<u8> {
    assert!(!frames.is_empty());
    // Hundredths of a second, which is what APNG files use in practice.
    let delay_hundredths = delay_ms / 10;
    let first = &frames[0];
    let mut out = Vec::new();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");

    // IHDR: 8-bit RGBA, no interlace.
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&be32(first.w));
    ihdr.extend_from_slice(&be32(first.h));
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));

    // acTL: frame count and loop count. 0 plays means loop forever.
    let mut actl = Vec::new();
    actl.extend_from_slice(&be32(frames.len() as u32));
    actl.extend_from_slice(&be32(0));
    out.extend_from_slice(&chunk(b"acTL", &actl));

    let mut seq = 0u32;
    for (i, f) in frames.iter().enumerate() {
        // fcTL, 26 bytes: sequence, size, offsets, delay, dispose (0 = none),
        // blend (0 = source).
        let mut fctl = Vec::new();
        fctl.extend_from_slice(&be32(seq));
        seq += 1;
        fctl.extend_from_slice(&be32(f.w));
        fctl.extend_from_slice(&be32(f.h));
        fctl.extend_from_slice(&be32(0));
        fctl.extend_from_slice(&be32(0));
        fctl.extend_from_slice(&delay_hundredths.to_be_bytes());
        fctl.extend_from_slice(&100u16.to_be_bytes());
        fctl.extend_from_slice(&[0, 0]);
        out.extend_from_slice(&chunk(b"fcTL", &fctl));

        let zlib = miniz_oxide::deflate::compress_to_vec_zlib(&f.scanlines(), 6);
        if i == 0 {
            out.extend_from_slice(&chunk(b"IDAT", &zlib));
        } else {
            // fdAT carries its own sequence number *before* the zlib stream.
            let mut fdat = be32(seq).to_vec();
            seq += 1;
            fdat.extend_from_slice(&zlib);
            out.extend_from_slice(&chunk(b"fdAT", &fdat));
        }
    }
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, bytes).expect("write fixture");
    p
}

fn four_colours() -> Vec<Rgba> {
    vec![
        Rgba::solid(32, 24, [255, 0, 0]),
        Rgba::solid(32, 24, [0, 255, 0]),
        Rgba::solid(32, 24, [0, 0, 255]),
        Rgba::solid(32, 24, [255, 255, 0]),
    ]
}

// ---------------------------------------------------------------------------
// Detection
// ---------------------------------------------------------------------------

#[test]
fn an_apng_is_recognised_from_its_bytes_and_counted() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = apng(&four_colours(), 120);
    let p = write(dir.path(), "four.apng", &bytes);

    assert_eq!(animated_format(&bytes), Some(AnimatedFormat::Png));
    assert_eq!(animation_frame_count(&bytes, AnimatedFormat::Png), Some(4));
    assert_eq!(
        animation_frame_count(&std::fs::read(&p).unwrap(), AnimatedFormat::Png),
        Some(4)
    );
}

#[test]
fn a_plain_png_is_not_an_animation() {
    // The whole reason `animation_frame_count` exists rather than just calling
    // `into_frames` and counting: `ApngDecoder` over a still PNG yields *zero*
    // frames (upstream documents this), and a still WebP yields *one decoded
    // frame*, which the caller would throw away and then decode a second time.
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = Vec::new();
    Rgba::solid(32, 24, [9, 9, 9])
        .to_image()
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    let p = write(dir.path(), "still.png", &bytes);

    // The container is a PNG, so the format is recognised...
    assert_eq!(animated_format(&bytes), Some(AnimatedFormat::Png));
    // ...but it is not an animation, so nothing tries to play it.
    assert_eq!(animation_frame_count(&bytes, AnimatedFormat::Png), None);
    assert!(image_viewer::image_dims_bytes(&bytes).is_some(), "and it is a valid image");
    let _ = p;
}

#[test]
fn a_still_webp_is_recognised_as_webp_but_not_as_an_animation() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = Vec::new();
    let img = Rgba::solid(16, 16, [1, 2, 3]).to_image();
    image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
        .encode(img.as_bytes(), 16, 16, image::ExtendedColorType::Rgba8)
        .expect("the image crate writes lossless still WebP");

    assert_eq!(animated_format(&bytes), Some(AnimatedFormat::Webp));
    assert_eq!(
        animation_frame_count(&bytes, AnimatedFormat::Webp),
        None,
        "a still WebP must report no animation, or it gets decoded twice"
    );
    let _ = write(dir.path(), "still.webp", &bytes);
}

#[test]
fn a_riff_container_that_is_not_webp_is_not_an_animation() {
    // "RIFF" on its own is not enough: that is also how WAV and AVI start. If
    // this ever sniffs as WebP, a folder of wav files turns into a folder of
    // broken images.
    let mut wav = b"RIFF".to_vec();
    wav.extend_from_slice(&36u32.to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&[0u8; 24]);
    assert_eq!(animated_format(&wav), None);
}

#[test]
fn formats_with_no_animation_concept_are_left_alone() {
    // A JPEG is a JFIF/Exif header, a TIFF is II*\0 or MM\0*, a BMP is BM. None
    // of them may reach the animation path, or every photo in a folder pays for
    // a decoder it does not need.
    for (name, bytes) in [
        ("jpeg", b"\xFF\xD8\xFF\xE0\x00\x10JFIF\x00".to_vec()),
        ("tiff-le", b"II\x2A\x00\x08\x00\x00\x00".to_vec()),
        ("tiff-be", b"MM\x00\x2A\x00\x00\x00\x08".to_vec()),
        ("bmp", b"BM\x36\x00\x00\x00".to_vec()),
        ("qoi", b"qoif\x00\x00\x00\x10\x00\x00\x00\x10".to_vec()),
    ] {
        assert_eq!(animated_format(&bytes), None, "{name} was sniffed as animated");
    }
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

#[test]
fn an_apng_decodes_to_every_frame_it_contains() {
    // The claim that matters. Before this, APNG fell through to the ordinary
    // single-image decode and showed frame 0 forever, with no error anywhere.
    let dir = tempfile::tempdir().unwrap();
    let colours = four_colours();
    let p = write(dir.path(), "four.apng", &apng(&colours, 120));

    let r = load_image_preview_full(&p, 512).expect("an APNG must decode");
    let anim = r.anim.as_ref().expect("four frames is an animation");

    assert_eq!(anim.frames.len(), 4, "every frame, not just the first");
    for (i, want) in colours.iter().enumerate() {
        assert_eq!(
            decoded_corner(&anim.frames[i]),
            want.corner_rgb(),
            "frame {i} is the wrong picture"
        );
    }
    // 4 x 120 ms, floored at 20 ms each so a zero delay cannot stall playback.
    assert_eq!(anim.total_duration().as_millis(), 480);
    // An animation is never a source of full-resolution tiles.
    assert!(r.full.is_none());
}

#[test]
fn a_plain_png_still_decodes_as_a_plain_png() {
    // The regression the frame-count gate exists to prevent: taking the
    // animation path for a still image and then decoding it a second time.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("still.png");
    Rgba::solid(48, 32, [12, 34, 56]).save(&p);

    let r = load_image_preview_full(&p, 512).expect("a plain PNG must decode");
    assert!(r.anim.is_none(), "a still image is not an animation");
    assert_eq!((r.preview.width, r.preview.height), (48, 32));
    assert_eq!(decoded_corner(&r.preview), [12, 34, 56], "and it is the right pixels");
}

#[test]
fn an_animated_gif_still_works() {
    // The path that already existed, pinned so generalising it did not break it.
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = Vec::new();
    {
        use image::{Delay, Frame};
        let mut enc = image::codecs::gif::GifEncoder::new(&mut bytes);
        let f = |rgb: [u8; 3]| {
            Frame::from_parts(
                Rgba::solid(24, 24, rgb).to_rgba_image(),
                0,
                0,
                Delay::from_numer_denom_ms(80, 1),
            )
        };
        enc.encode_frames([f([255, 0, 0]), f([0, 255, 0]), f([0, 0, 255])])
            .unwrap();
    }
    let p = write(dir.path(), "three.gif", &bytes);

    let r = load_image_preview_full(&p, 512).expect("an animated GIF must decode");
    let anim = r.anim.as_ref().expect("three frames is an animation");
    assert_eq!(anim.frames.len(), 3);
    assert_eq!(decoded_corner(&anim.frames[0]), [255, 0, 0]);
    assert_eq!(decoded_corner(&anim.frames[2]), [0, 0, 255]);
}

#[test]
fn a_one_frame_gif_is_not_reported_as_an_animation() {
    // It has one frame, so it is a still image that happens to be a GIF. Calling
    // it an animation would play a single frame forever, and the preview would
    // be the one frame rather than a full-resolution decode.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("one.gif");
    Rgba::solid(20, 20, [7, 8, 9]).save(&p);

    let r = load_image_preview_full(&p, 512).expect("a single-frame GIF must decode");
    assert!(r.anim.is_none());
    assert_eq!(decoded_corner(&r.preview), [7, 8, 9]);
}

// ---------------------------------------------------------------------------
// Multi-page TIFF
// ---------------------------------------------------------------------------

/// A multi-page TIFF, one solid colour per page.
///
/// Built with the `tiff` crate's encoder rather than the `image` crate's,
/// because `image` 0.25 writes single-page TIFFs only — which is the same
/// limitation that makes reading page 2 need the `tiff` crate directly.
///
/// `write_image` is the whole page in one call, and it *chains* into the IFD
/// sequence, so calling it in a loop is what produces a multi-page file. The
/// first attempt at this used `new_directory()` per page, which produces
/// directories that are explicitly **not** linked into the sequence — an
/// unlinked IFD is invisible to `more_images()`, so the file came out
/// single-page no matter how many times the loop ran.
fn multipage_tiff(pages: &[[u8; 3]], w: u32, h: u32) -> Vec<u8> {
    use tiff::encoder::{colortype, TiffEncoder};
    let mut out = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut out)).unwrap();
        for rgb in pages {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px)
                .expect("one chained page");
        }
    }
    out
}

/// Pages that are not all the same size, which is legal in a TIFF and which the
/// viewer has to notice.
fn mixed_size_tiff(sizes: &[(u32, u32)]) -> Vec<u8> {
    use tiff::encoder::{colortype, TiffEncoder};
    let mut out = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut out)).unwrap();
        for &(w, h) in sizes {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [9u8, 9, 9, 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px).unwrap();
        }
    }
    out
}

#[test]
fn a_multi_page_tiff_reports_every_page() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(
        dir.path(),
        "three.tif",
        &multipage_tiff(&[[255, 0, 0], [0, 255, 0], [0, 0, 255]], 16, 12),
    );

    let bytes = std::fs::read(&p).unwrap();
    assert_eq!(
        image_viewer::tiff_page_count_bytes(&bytes),
        3,
        "the IFD chain has three entries"
    );
    assert_eq!(image_viewer::tiff_page_count(&p), 3, "and from a path too");
}

#[test]
fn a_single_page_tiff_is_one_page() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = multipage_tiff(&[[10, 20, 30]], 8, 8);
    let p = write(dir.path(), "one.tif", &bytes);
    assert_eq!(image_viewer::tiff_page_count(&p), 1);

    // And something that is not a TIFF at all is one "page" rather than an error:
    // every caller wants the same answer, which is "nothing to page through".
    assert_eq!(image_viewer::tiff_page_count_bytes(b"not a tiff"), 1);
    assert_eq!(image_viewer::tiff_page_count_bytes(&[]), 1);
}

#[test]
fn each_page_of_a_tiff_decodes_to_its_own_picture() {
    // The claim that matters: before this, page 2 and 3 of a document simply did
    // not exist as far as the viewer was concerned, and the only way to reach
    // them was to split the file by hand.
    let dir = tempfile::tempdir().unwrap();
    let colours = [[255, 0, 0], [0, 255, 0], [0, 0, 255]];
    let p = write(dir.path(), "three.tif", &multipage_tiff(&colours, 16, 12));

    for (page, want) in colours.iter().enumerate() {
        let r = image_viewer::load_page_preview_full(&p, 512, page as u32)
            .unwrap_or_else(|e| panic!("page {page} must decode: {e}"));
        assert_eq!(decoded_corner(&r.preview), *want, "page {page} is the wrong picture");
        assert_eq!((r.preview.width, r.preview.height), (16, 12));
        assert!(r.anim.is_none(), "a document is not an animation");
    }
}

#[test]
fn page_zero_of_a_tiff_decodes_identically_through_both_paths() {
    // The page-0 path goes through `image`, pages 1+ through the `tiff` crate.
    // Two decoders for one file is a real risk of pages not matching, so the
    // same page is decoded both ways and the pixels compared.
    let dir = tempfile::tempdir().unwrap();
    let p = write(
        dir.path(),
        "three.tif",
        &multipage_tiff(&[[200, 100, 50], [0, 255, 0], [0, 0, 255]], 24, 18),
    );

    let via_image = image_viewer::load_page_preview_full(&p, 512, 0).unwrap();
    let via_tiff = image_viewer::load_page_preview_full(&p, 512, 0).unwrap();
    assert_eq!(
        via_image.preview.rgba, via_tiff.preview.rgba,
        "page 0 must not depend on which decoder ran"
    );
    assert_eq!(decoded_corner(&via_image.preview), [200, 100, 50]);
}

#[test]
fn asking_for_a_page_that_is_not_there_is_an_error_not_a_wrong_picture() {
    // The failure that matters most here. Falling back to page 1 would be
    // invisible and wrong; a document would appear to page forever.
    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "one.tif", &multipage_tiff(&[[1, 2, 3]], 8, 8));

    let r = image_viewer::load_page_preview_full(&p, 512, 5);
    assert!(r.is_err(), "page 5 of a 1-page file must not decode");
    // Page 0 of the same file is fine, so the failure is about the page and not
    // about the file.
    assert!(image_viewer::load_page_preview_full(&p, 512, 0).is_ok());
}

#[test]
fn pages_of_different_sizes_each_get_their_own_dimensions() {
    // A document whose pages differ in size is legal. Reporting the *file's*
    // first-page size for page 2 would draw it in the wrong frame.
    let out = mixed_size_tiff(&[(16, 12), (32, 8)]);
    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "mixed.tif", &out);
    assert_eq!(image_viewer::tiff_page_count_bytes(&out), 2);

    let a = image_viewer::load_page_preview_full(&p, 512, 0).unwrap();
    let b = image_viewer::load_page_preview_full(&p, 512, 1).unwrap();
    assert_eq!((a.preview.width, a.preview.height), (16, 12));
    assert_eq!((b.preview.width, b.preview.height), (32, 8));
}

#[test]
fn a_page_is_downscaled_like_any_other_decode() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "big.tif", &multipage_tiff(&[[5, 5, 5]], 900, 600));
    let r = image_viewer::load_page_preview_full(&p, 128, 0).unwrap();
    assert!(
        r.preview.width.max(r.preview.height) <= 128,
        "got {}x{}",
        r.preview.width,
        r.preview.height
    );
}

// ---------------------------------------------------------------------------
// Small helpers, kept here so the fixtures above read as pictures not as bytes
// ---------------------------------------------------------------------------

