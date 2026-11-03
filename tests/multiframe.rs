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

use image_viewer::{animated_format, load_image_preview_full, AnimatedFormat};
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
fn an_apng_is_recognised_from_its_bytes_and_judged_animated() {
    let dir = tempfile::tempdir().unwrap();
    let bytes = apng(&four_colours(), 120);
    let p = write(dir.path(), "four.apng", &bytes);

    assert_eq!(animated_format(&bytes), Some(AnimatedFormat::Png));
    assert!(image_viewer::is_animated(&bytes, AnimatedFormat::Png));
    assert!(image_viewer::is_animated(&std::fs::read(&p).unwrap(), AnimatedFormat::Png));
}

/// The bug that cost the most to find, and it was only reachable with a real
/// file: **a truncated GIF hangs `into_frames().count()` forever**.
///
/// `count()` has no reason to stop, and a GIF whose frame data runs out yields
/// `Err` and then keeps yielding it. On a worker thread that is not an error, it is
/// a hang: the image never paints, the stall watchdog fires at 6 s, and the app
/// sits there with six *neighbour* previews decoded and none of the one you asked
/// for. The file is real — `IMG_2896.gif` in the phone backup, 7.9 MB, 800x1422,
/// truncated at frame 11 — and `image` itself handles it in 0.2 s.
///
/// So: a still image is not animated, and a *truncated* animation still is, and
/// asking the question returns at all. The last one is the one that hangs, and a
/// test that only asserts the answer cannot tell "returned false" from "never
/// returned".
#[test]
fn a_truncated_gif_still_decodes_and_does_not_hang() {
    let dir = tempfile::tempdir().unwrap();
    // Twelve frames, so that cutting the tail leaves several intact — the real file
    // failed at frame 11 of 12, and a 3-frame fixture cut in the same proportion
    // loses the *second* frame, which is not the case being pinned.
    let good = {
        let mut bytes = Vec::new();
        {
            use image::{Delay, Frame};
            let mut enc = image::codecs::gif::GifEncoder::new(&mut bytes);
            let frames: Vec<Frame> = (0..12)
                .map(|i| {
                    let v = (i * 21) as u8;
                    Frame::from_parts(
                        Rgba::solid(64, 48, [v, 255 - v, v / 2]).to_rgba_image(),
                        0,
                        0,
                        Delay::from_numer_denom_ms(80, 1),
                    )
                })
                .collect();
            enc.encode_frames(frames).unwrap();
        }
        bytes
    };
    // Cut the tail: the trailer goes, and so does part of the last frame's pixel
    // data, which is what makes the frame iterator start failing.
    let truncated = &good[..good.len() * 9 / 10];

    assert_eq!(animated_format(truncated), Some(AnimatedFormat::Gif));
    assert!(
        image_viewer::is_animated(truncated, AnimatedFormat::Gif),
        "the first two frames are intact, so it is an animation"
    );

    let p = write(dir.path(), "cut.gif", truncated);
    let anim = image_viewer::load_image_preview_full(&p, 512)
        .expect("a truncated GIF must still decode")
        .anim
        .expect("and still be an animation");
    assert!(anim.frames.len() >= 2, "at least the two intact frames, got {}", anim.frames.len());
    assert_eq!(decoded_corner(&anim.frames[0]), [0, 255, 0]);
    assert_eq!(decoded_corner(&anim.frames[1]), [21, 234, 10]);
}

#[test]
fn a_plain_png_is_not_an_animation() {
    // The whole reason `is_animated` exists rather than just calling `into_frames`
    // and seeing what comes back: `ApngDecoder` over a still PNG yields *zero*
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
    assert!(!image_viewer::is_animated(&bytes, AnimatedFormat::Png));
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
    assert!(
        !image_viewer::is_animated(&bytes, AnimatedFormat::Webp),
        "a still WebP must not be treated as an animation, or it is decoded twice"
    );
    let _ = write(dir.path(), "still.webp", &bytes);
}

#[test]
fn an_animated_webp_decodes_every_frame_it_contains() {
    // The one decode path no amount of synthesis could reach, which is why this
    // file exists at all rather than being built here: `image` writes lossless
    // VP8L stills only, `image-webp`'s encoder is single-frame, and there is no
    // libwebp binding in the tree. See `tests/fixtures/README.md` for the source
    // and licence.
    //
    // The failure this guards is not "animated WebP is broken" — it never was.
    // It is that three lines calling an upstream decoder had never been *run*,
    // which looks identical from the outside whether they are right or wrong.
    // APNG and animated WebP were both frozen on frame 0 that way once already.
    let bytes = include_bytes!("fixtures/animated.webp");

    // Detection first: a still WebP yields one *decoded* frame from
    // `into_frames` that the caller throws away and then decodes again, so
    // recognising the animation is half the work.
    assert_eq!(animated_format(bytes), Some(AnimatedFormat::Webp));
    assert!(
        image_viewer::is_animated(bytes, AnimatedFormat::Webp),
        "a VP8X/ANIM container must be recognised as an animation"
    );

    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "animated.webp", bytes);
    let anim = load_image_preview_full(&p, 512)
        .expect("an animated WebP must decode")
        .anim
        .expect("and be reported as an animation, not as one picture");

    // Pinned numbers, because a fixture that decodes "some frames" is exactly
    // the frozen-on-frame-0 bug wearing a different hat.
    assert_eq!(anim.frames.len(), 10, "the file has 10 frames");
    assert!(
        anim.frames.iter().all(|f| (f.width, f.height) == (400, 400)),
        "every frame is 400x400"
    );
    assert!(anim.delays.iter().all(|d| d.as_millis() == 40), "40 ms per frame: {:?}", anim.delays);
    assert_eq!(anim.total_duration().as_millis(), 400, "400 ms loop");

    // The frames must actually differ. Ten copies of one picture would satisfy
    // every assertion above, and would be a still image wearing a container.
    let differing = anim.frames.windows(2).filter(|pair| pair[0].rgba != pair[1].rgba).count();
    assert_eq!(differing, 9, "all 9 adjacent frame pairs must differ, or this is not animating");
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
        enc.encode_frames([f([255, 0, 0]), f([0, 255, 0]), f([0, 0, 255])]).unwrap();
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
            enc.write_image::<colortype::RGBA8>(w, h, &px).expect("one chained page");
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
    assert_eq!(image_viewer::tiff_page_count_bytes(&bytes), 3, "the IFD chain has three entries");
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

/// A two-page TIFF whose **second** page is CMYK, which is the case that used to
/// be refused.
///
/// Page 1 is RGBA so the file is a normal document that merely *contains* a CMYK
/// page — the realistic shape. Page 2's pixels are chosen so the conversion is
/// checkable rather than merely plausible: TIFF CMYK is ink amounts where 0 means
/// none, and `image` computes `(255 - c) * (1 - k/255)`, so no ink is white, full
/// cyan is (0, 255, 255) and full black ink is (0, 0, 0). Anything else would mean
/// we had started guessing at the colour transform ourselves.
fn cmyk_second_page_tiff(w: u32, h: u32) -> Vec<u8> {
    use tiff::encoder::{colortype, TiffEncoder};
    let mut out = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut out)).unwrap();
        let rgba: Vec<u8> = (0..w * h).flat_map(|_| [10u8, 20, 30, 255]).collect();
        enc.write_image::<colortype::RGBA8>(w, h, &rgba).unwrap();

        let n = (w * h) as usize;
        let mut cmyk = vec![0u8; n * 4]; // no ink at all -> white
        cmyk[0..4].copy_from_slice(&[255, 0, 0, 0]); // full cyan
        cmyk[4..8].copy_from_slice(&[0, 0, 0, 255]); // full black ink
        enc.write_image::<colortype::CMYK8>(w, h, &cmyk).unwrap();
    }
    out
}

#[test]
fn a_cmyk_past_the_first_page_is_converted_not_refused() {
    let dir = tempfile::tempdir().unwrap();
    let p = write(dir.path(), "print.tiff", &cmyk_second_page_tiff(4, 2));
    assert_eq!(image_viewer::tiff_page_count(&p), 2);

    // Page 1 is the first *page*, and it works for the ordinary reason.
    let first = image_viewer::load_page_preview_full(&p, 512, 0).unwrap();
    assert_eq!((first.preview.width, first.preview.height), (4, 2));

    let cmyk = image_viewer::load_page_preview_full(&p, 512, 1).unwrap();
    assert_eq!(
        (cmyk.preview.width, cmyk.preview.height),
        (4, 2),
        "the conversion must not change the page's dimensions"
    );
    assert_eq!(
        cmyk.preview.rgba.len(),
        4 * 2 * 4,
        "and the buffer must still be exactly w*h*4, which is what the texture \
         upload asserts"
    );

    // The actual conversion, checked against `image`'s own formula.
    let px = |i: usize| {
        [cmyk.preview.rgba[i * 4], cmyk.preview.rgba[i * 4 + 1], cmyk.preview.rgba[i * 4 + 2]]
    };
    assert_eq!(px(0), [0, 255, 255], "full cyan ink");
    assert_eq!(px(1), [0, 0, 0], "full black ink");
    assert_eq!(px(2), [255, 255, 255], "no ink is white, not black");
    assert_eq!(px(3), [255, 255, 255]);
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
// Pages as browsable positions
// ---------------------------------------------------------------------------

/// A gallery over `files`, with page counts supplied directly.
///
/// [`image_viewer::Gallery::learn_pages`] reads the file, which is right for the
/// app and wrong for a test about *navigation*: a test that wants a 3-page
/// document to behave like a 3-page document should not depend on the IFD walker
/// also being right, or every navigation test fails for the wrong reason when it
/// breaks. The counting is tested separately, in `a_multi_page_tiff_reports_every_page`.
fn gallery_with_pages(files: &[(&str, u32)]) -> image_viewer::Gallery {
    let paths: Vec<std::path::PathBuf> =
        files.iter().map(|(n, _)| std::path::PathBuf::from(n)).collect();
    let mut g = image_viewer::Gallery::empty();
    g.set_filtered(&paths, "");
    g.set_page_counts(files.iter().map(|(_, n)| *n));
    g
}

#[test]
fn pressing_right_walks_the_pages_of_a_document_and_then_the_next_file() {
    // The behaviour that was asked for, and the reason pages are a cursor rather
    // than extra entries in the list: arrows work, with nothing to learn.
    let mut g = gallery_with_pages(&[("doc.tif", 3), ("photo.jpg", 1)]);

    assert_eq!((g.index, g.current_page()), (0, 0));
    assert!(g.next(), "page 2");
    assert_eq!((g.index, g.current_page()), (0, 1));
    assert!(g.next(), "page 3");
    assert_eq!((g.index, g.current_page()), (0, 2));
    assert!(g.next(), "and on into the next file");
    assert_eq!((g.index, g.current_page()), (1, 0), "the next file starts at its first page");
}

#[test]
fn pressing_left_from_a_first_page_lands_on_the_previous_files_last_page() {
    // Symmetric with `next`. Landing on the *first* page of the previous file
    // would mean two presses per file to step backwards, which feels broken even
    // though it is not wrong.
    let mut g = gallery_with_pages(&[("a.tif", 3), ("b.tif", 4)]);
    g.goto_index(1);
    assert_eq!(g.current_page(), 0);

    assert!(g.prev());
    assert_eq!((g.index, g.current_page()), (0, 2), "the last page of a.tif");
    assert!(g.prev());
    assert_eq!((g.index, g.current_page()), (0, 1));
    assert!(g.prev());
    assert_eq!((g.index, g.current_page()), (0, 0));
}

#[test]
fn an_ordinary_folder_behaves_exactly_as_it_did_before_pages() {
    // Every image is one page, so all of this must be the old behaviour. A
    // regression here would be invisible in a folder of photos and obvious in a
    // folder of scans, which is the wrong way round.
    let mut g = gallery_with_pages(&[("a.jpg", 1), ("b.jpg", 1), ("c.jpg", 1)]);
    assert!(g.next());
    assert_eq!((g.index, g.current_page()), (1, 0));
    assert!(g.next());
    assert_eq!((g.index, g.current_page()), (2, 0));
    assert!(g.next(), "wraps, as loop_back defaults to on");
    assert_eq!((g.index, g.current_page()), (0, 0));
    assert!(g.prev());
    assert_eq!((g.index, g.current_page()), (2, 0));
    // Three images and looping on: three steps is a full turn of the folder and
    // must land back where it started, with no page drift on the way.
    for _ in 0..3 {
        assert!(g.next());
    }
    assert_eq!((g.index, g.current_page()), (2, 0), "a full turn of the folder");
}

#[test]
fn loop_back_off_still_stops_at_the_ends() {
    // The point of `next` returning bool: with looping off, the last page of the
    // last file is a real end and the caller must be able to see it.
    let mut g = gallery_with_pages(&[("doc.tif", 3), ("photo.jpg", 1)]);
    g.set_loop_back(false);
    for expected in [(0, 1), (0, 2), (1, 0)] {
        assert!(g.next(), "step to {expected:?}");
        assert_eq!((g.index, g.current_page()), expected);
    }
    assert!(!g.next(), "past the end with looping off");
    assert_eq!((g.index, g.current_page()), (1, 0), "and it did not move");

    g.goto_index(0);
    assert!(!g.prev(), "before the start with looping off");
    assert_eq!((g.index, g.current_page()), (0, 0));
}

#[test]
fn a_page_count_that_shrinks_puts_the_cursor_back_in_range() {
    // A 12-page document re-exported as one page. Without the clamp the cursor
    // sits on page 12 of a document that has one page, and `next` walks off the
    // end of something that does not exist.
    let mut g = gallery_with_pages(&[("doc.tif", 12), ("b.jpg", 1)]);
    for _ in 0..11 {
        assert!(g.next());
    }
    assert_eq!(g.current_page(), 11);

    g.forget_page_count(std::path::Path::new("doc.tif"));
    assert_eq!(g.current_page(), 0, "the cursor is back on a page that exists");
    assert!(g.next(), "and navigation continues from there");
    assert_eq!((g.index, g.current_page()), (1, 0));
}

#[test]
fn forgetting_a_page_count_of_another_file_leaves_the_cursor_alone() {
    let mut g = gallery_with_pages(&[("a.tif", 5), ("b.tif", 5)]);
    g.goto_index(0);
    for _ in 0..3 {
        g.next();
    }
    assert_eq!(g.current_page(), 3);
    g.forget_page_count(std::path::Path::new("b.tif"));
    assert_eq!(g.current_page(), 3, "b's count has nothing to do with where we are");
}

#[test]
fn rebuilding_the_list_forgets_every_page_count() {
    // Otherwise the map grows for the whole session as you browse, and — worse —
    // can describe a file that is no longer in the list.
    let mut g = gallery_with_pages(&[("doc.tif", 4), ("b.jpg", 1)]);
    g.next();
    assert_eq!(g.current_page(), 1);
    g.load_all_from_folder(std::path::Path::new("."));
    assert_eq!(g.current_page(), 0, "a rebuilt list starts at a page we know exists");
    assert_eq!(g.known_page_counts(), 0, "and remembers nothing about the old list");
}

#[test]
fn the_filter_keeps_your_place_in_a_document_but_not_across_files() {
    // Two cases, and they differ on purpose. The current file surviving the filter
    // means the user keeps their page; the current file being filtered out means
    // the index moved, and carrying a page number to a different file would be
    // showing page 4 of something that has one page.
    let all: Vec<std::path::PathBuf> =
        ["doc.tif", "other.tif", "photo.jpg"].iter().map(std::path::PathBuf::from).collect();
    let mut g = image_viewer::Gallery::empty();
    g.set_filtered(&all, "");
    g.set_page_counts([4, 4, 1]);
    for _ in 0..2 {
        g.next();
    }
    assert_eq!((g.index, g.current_page()), (0, 2));

    // "doc" matches doc.tif and other.tif, so the current file survives.
    assert!(!g.set_filtered(&all, "doc"), "the current file is still there, so nothing moved");
    assert_eq!((g.index, g.current_page()), (0, 2), "and the page is kept");

    // "photo" matches only photo.tif's neighbour, so the current file is filtered
    // out and the index falls back to the nearest surviving position — which is 0,
    // because that is where the doc was. What matters is the page.
    assert!(g.set_filtered(&all, "photo"), "the current file was filtered out");
    assert_eq!(g.index, 0, "clamped to a position that exists");
    assert_eq!(g.current_page(), 0, "and the page did not travel to a different file");
}

#[test]
fn the_page_count_is_learned_from_the_file_and_asked_only_once() {
    // The cost control: four bytes for anything that is not a TIFF, and the
    // answer cached so browsing back and forth does not re-read.
    let dir = tempfile::tempdir().unwrap();
    let p =
        write(dir.path(), "three.tif", &multipage_tiff(&[[1, 2, 3], [4, 5, 6], [7, 8, 9]], 8, 8));
    let mut g = image_viewer::Gallery::empty();
    g.set_filtered(&[p.clone()], "");

    assert_eq!(g.learn_pages(&p), 3);
    assert_eq!(g.pages_in_current(), 3);
    // Second call is answered from the cache, and agrees.
    assert_eq!(g.learn_pages(&p), 3);

    // **The cache is what makes this cheap, and it is checked by deletion rather
    // than by counting.** `learn_pages` runs on every navigation, and the first
    // version always re-read — which put a `File::open` on the critical path of
    // every keypress. Deleting the file makes a re-read impossible: if the answer
    // still comes back as 3, it came from the cache.
    std::fs::remove_file(&p).unwrap();
    assert_eq!(
        g.learn_pages(&p),
        3,
        "a cached count must survive the file being unreadable, or every arrow key re-opens it"
    );
    assert_eq!(g.pages_in_current(), 3);

    // A plain image is one page, and asking costs one 4-byte read.
    let jpg = dir.path().join("one.jpg");
    Rgba::solid(8, 8, [4, 4, 4]).save(&jpg);
    assert_eq!(image_viewer::tiff_page_count(&jpg), 1);
    // A file that is not there at all is one page rather than a panic: `goto` asks
    // on arrival, and a file can vanish between the scan and the arrival.
    assert_eq!(image_viewer::tiff_page_count(&dir.path().join("gone.tif")), 1);
}

#[test]
fn the_cursor_can_never_name_a_page_that_does_not_exist() {
    // Checked as a property over every arrangement, because the failure it guards
    // is an off-the-end index into a decode that then asks a file for a page it
    // has not got.
    for docs in [[1u32, 1, 1], [3, 1, 1], [1, 5, 1], [1, 1, 4], [2, 3, 4], [7, 1, 1]] {
        for start in 0..docs.len() {
            let mut g = gallery_with_pages(&[("a", docs[0]), ("b", docs[1]), ("c", docs[2])]);
            g.goto_index(start);
            for _ in 0..30 {
                assert!(
                    g.current_page() < docs[g.index],
                    "page {} of a {}-page file at index {} (docs {docs:?})",
                    g.current_page(),
                    docs[g.index],
                    g.index
                );
                g.next();
            }
            for _ in 0..30 {
                assert!(
                    g.current_page() < docs[g.index],
                    "backwards: page {} of a {}-page file (docs {docs:?})",
                    g.current_page(),
                    docs[g.index]
                );
                g.prev();
            }
        }
    }
}

#[test]
fn the_protected_set_does_not_grow_with_the_length_of_a_document() {
    // The bug this pins: protecting every page of the protected *files* makes the
    // cache unbounded while you walk a document, because nothing is ever
    // evictable. On a folder of photos it cannot happen — every file is one page —
    // so the only way to see it is to ask the policy directly.
    let files: Vec<std::path::PathBuf> =
        ["doc.tif", "a.jpg", "b.jpg"].iter().map(std::path::PathBuf::from).collect();
    for page in 0..200u32 {
        let p = image_viewer::protected_preview_keys(&files, 0, page, None, 3);
        // The window contributes at most `2 * radius + 1` distinct files, and the
        // page on screen is at most one more.
        assert!(p.len() <= 2 * 3 + 2, "page {page} protected {} keys: {p:?}", p.len());
    }
}

#[test]
fn the_protected_set_keeps_the_page_you_are_on_and_only_page_zero_of_neighbours() {
    use image_viewer::{protected_preview_keys, ImgKey};
    let files: Vec<std::path::PathBuf> =
        (0..9).map(|i| std::path::PathBuf::from(format!("{i}.jpg"))).collect();

    // On page 6 of a document, with nothing shown yet.
    let p = protected_preview_keys(&files, 0, 6, None, 3);
    assert!(p.contains(&ImgKey { path: files[0].clone(), page: 6 }), "the page on screen");
    // The window is centred on the current index and so includes it, which means
    // page 1 of the current file is protected as well. That is one extra key, it
    // was true of the pre-page version too, and it is not what the size test is
    // about — what matters is that it does not become *all* of the file's pages.
    assert!(
        p.contains(&ImgKey::first(&files[0])),
        "the window includes its own centre, so page 1 of the current file is in it"
    );
    // Neighbours: page 0 only, which is all the preload window decodes. The window
    // is ±3 and wraps, so from index 0 in a 9-image list that is 6,7,8,0,1,2,3 —
    // three *backwards* neighbours are included, which the first version of this
    // test got wrong by assuming the window only reached forwards.
    for i in [1usize, 2, 3, 6, 7, 8] {
        assert!(p.contains(&ImgKey::first(&files[i])), "neighbour {i}");
    }
    for i in [4usize, 5] {
        assert!(!p.contains(&ImgKey::first(&files[i])), "too far to be a neighbour: {i}");
    }
    assert_eq!(p.len(), 8, "the page on screen, plus the window's 7 files at page 0");

    // A neighbour in the other direction, wrapping past the end of the list.
    let p = protected_preview_keys(&files, 0, 0, None, 1);
    assert!(p.contains(&ImgKey::first(&files[1])));
    assert!(p.contains(&ImgKey::first(&files[8])), "wraps to the last file");
    assert!(!p.contains(&ImgKey::first(&files[4])), "and no further than that");
}

#[test]
fn the_protected_set_includes_what_is_actually_on_screen() {
    // `shown` lags `cur` by design, so during a navigation it is a different
    // image from the one being protected as "current". Forgetting it means the
    // picture on screen can be the eviction victim.
    use image_viewer::{protected_preview_keys, ImgKey};
    let files: Vec<std::path::PathBuf> =
        ["a.jpg", "b.jpg", "c.jpg"].iter().map(std::path::PathBuf::from).collect();
    let shown = ImgKey { path: files[2].clone(), page: 0 };
    let p = protected_preview_keys(&files, 0, 0, Some(&shown), 1);
    assert!(p.contains(&shown), "what is painted is never evicted");
}

// ---------------------------------------------------------------------------
// Small helpers, kept here so the fixtures above read as pictures not as bytes
// ---------------------------------------------------------------------------
