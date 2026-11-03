//! TIFF, BMP and ICO, and the sniffing that lets them in.
//!
//! Format detection here is content-based, never extension-based, so the tests
//! come in pairs: one that a real file decodes, and one that the *same bytes*
//! under a lying extension still decode. A sniffer that only works on correctly
//! named files is a sniffer that will one day pick the wrong decoder.

use image_viewer::{
    SUPPORTED_EXTENSIONS, image_dims_bytes, is_bmp_bytes, is_ico_bytes, is_qoi_bytes,
    is_tiff_bytes, load_image, load_image_preview_full, scan_folder,
};
use std::path::Path;

/// Encode `img` in `format`, with `name` as the file name — deliberately able to
/// disagree with the content.
fn write_as(
    dir: &Path,
    name: &str,
    img: &image::DynamicImage,
    format: image::ImageFormat,
) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), format)
        .unwrap_or_else(|e| panic!("encoding {format:?} as {name} failed: {e}"));
    std::fs::write(&path, &bytes).unwrap();
    path
}

fn sample() -> image::DynamicImage {
    // Non-square and non-uniform, so a transposed or mis-decoded buffer shows up
    // as a size mismatch rather than passing on a square test image.
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(96, 48, |x, y| {
        image::Rgb([(x * 2) as u8, (y * 5) as u8, 200])
    }))
}

/// The same picture with an alpha channel.
///
/// ICO needs this: the `image` crate's ICO *encoder* always writes a
/// PNG-compressed entry, and its ICO *decoder* rejects a PNG entry that is not
/// RGBA — so encoding an `Rgb8` image to ICO produces a file the same library
/// then refuses to read. Real icons are unaffected (Windows writes RGBA PNGs
/// for the large sizes and BMP for the small ones), but a test that round-trips
/// through `write_to` has to start from RGBA or it looks like ICO is broken.
fn sample_rgba() -> image::DynamicImage {
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(96, 48, |x, y| {
        image::Rgba([(x * 2) as u8, (y * 5) as u8, 200, 255])
    }))
}

/// Hand-build a single-entry ICO holding a 32bpp BMP-compressed image.
///
/// The encoder cannot produce this form — it always writes PNG — so this is the
/// only way to cover it, and it is the form every pre-Vista icon and every
/// small size in a modern one uses. Worth pinning: it is the common case, and
/// nothing else in the test suite reaches it.
fn bmp_in_ico(w: u32, h: u32) -> Vec<u8> {
    // The payload is a BITMAPINFOHEADER with a doubled height, then bottom-up
    // BGRA pixels, then a 1bpp AND mask with each row padded to 4 bytes.
    let mut bmp = Vec::new();
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&(w as i32).to_le_bytes());
    bmp.extend_from_slice(&((h * 2) as i32).to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&32u16.to_le_bytes()); // bpp
    bmp.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    bmp.extend_from_slice(&(w * h * 4).to_le_bytes());
    bmp.extend_from_slice(&[0u8; 16]); // resolution + palette
    for _ in 0..(w * h) {
        bmp.extend_from_slice(&[0, 0, 255, 255]); // BGRA, opaque blue
    }
    let row_bytes = ((w + 31) / 32) * 4;
    bmp.resize(bmp.len() + (row_bytes * h) as usize, 0u8);

    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type 1 = icon
    ico.extend_from_slice(&1u16.to_le_bytes()); // one image
    ico.extend_from_slice(&[w as u8, h as u8, 0, 0]); // 0 means 256
    ico.extend_from_slice(&1u16.to_le_bytes()); // planes
    ico.extend_from_slice(&32u16.to_le_bytes()); // bpp
    ico.extend_from_slice(&(bmp.len() as u32).to_le_bytes());
    ico.extend_from_slice(&22u32.to_le_bytes()); // offset: 6 + 16
    ico.extend_from_slice(&bmp);
    ico
}

#[test]
fn qoi_decodes_at_its_real_dimensions() {
    // The one format added beyond the obvious set. It has real magic bytes, it
    // round-trips through the crate (so a test can generate a fixture), and it
    // is lossless, so there is nothing to display wrong.
    let tmp = tempfile::tempdir().unwrap();
    let p = write_as(tmp.path(), "a.qoi", &sample(), image::ImageFormat::Qoi);
    let bytes = std::fs::read(&p).unwrap();
    assert!(is_qoi_bytes(&bytes), "qoif header not recognised");
    assert_eq!(image_dims_bytes(&bytes), Some((96, 48)));

    let d = load_image(&p, 4096).unwrap_or_else(|e| panic!("QOI failed: {e:?}"));
    assert_eq!((d.width, d.height), (96, 48));
    assert_eq!(d.rgba.len(), 96 * 48 * 4);

    // Lossless, so the pixels must match the source exactly.
    let want = sample().to_rgba8();
    assert_eq!(d.rgba, want.into_raw(), "QOI is lossless; pixels must be identical");
}

#[test]
fn a_qoi_survives_a_lying_extension() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_as(tmp.path(), "actually_qoi.jpg", &sample(), image::ImageFormat::Qoi);
    let d = load_image(&p, 4096).unwrap_or_else(|e| panic!("failed: {e:?}"));
    assert_eq!((d.width, d.height), (96, 48));
}

/// A renamed video names itself instead of parroting the guesser: an MP4
/// under a `.gif` name must fail with the container named, not with "the
/// image format could not be determined" — which answers neither "what is
/// it" nor "can I do anything about it".
#[test]
fn a_renamed_video_names_its_container_instead_of_the_guessers_shrug() {
    fn mp4_named(dir: &Path, name: &str, brand: &[u8; 4]) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut v = Vec::new();
        v.extend_from_slice(&[0, 0, 0, 0x20]);
        v.extend_from_slice(b"ftyp");
        v.extend_from_slice(brand);
        v.extend_from_slice(&[0; 20]);
        std::fs::write(&p, &v).unwrap();
        p
    }
    let tmp = tempfile::tempdir().unwrap();
    for (name, brand) in [("clip.gif", b"mp42"), ("movie.jpg", b"isom")] {
        let p = mp4_named(tmp.path(), name, brand);
        let err = image_viewer::load_image(&p, 4096).unwrap_err().to_string();
        assert!(
            err.contains("MP4") || err.contains("video"),
            "{name} must name the video container, got: {err}"
        );
        assert!(
            !err.contains("could not be determined"),
            "the guesser's shrug answers nothing: {err}"
        );
    }
    // Empty is empty, not undetermined either.
    let p = tmp.path().join("nothing.png");
    std::fs::write(&p, b"").unwrap();
    let err = image_viewer::load_image(&p, 4096).unwrap_err().to_string();
    assert!(err.contains("empty"), "a 0-byte file must say so, got: {err}");
}

/// The other direction, and the reason the remap is gated on the magic: a
/// GIF that *is* a GIF but decodes to nothing is corrupt, not unsupported,
/// and must keep the decode error rather than gain a wrong explanation.
#[test]
fn a_corrupt_gif_keeps_its_decode_error() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("broken.gif");
    let mut v = b"GIF89a".to_vec();
    v.extend(std::iter::repeat(0xFF).take(256));
    std::fs::write(&p, &v).unwrap();
    let err = image_viewer::load_image(&p, 4096).unwrap_err().to_string();
    assert!(
        !err.contains("unsupported"),
        "a corrupt GIF is a failed decode, not an unsupported file: {err}"
    );
}

#[test]
fn the_qoi_sniffer_does_not_fire_on_other_formats() {
    // A sniffer that fires on the wrong file picks the wrong decoder and
    // produces a plausible wrong-sized image rather than an error.
    let tmp = tempfile::tempdir().unwrap();
    for (name, format) in [
        ("a.qoi", image::ImageFormat::Qoi),
        ("a.png", image::ImageFormat::Png),
        ("a.jpg", image::ImageFormat::Jpeg),
        ("a.bmp", image::ImageFormat::Bmp),
        ("a.tiff", image::ImageFormat::Tiff),
        ("a.gif", image::ImageFormat::Gif),
    ] {
        let p = write_as(tmp.path(), name, &sample(), format);
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(is_qoi_bytes(&bytes), name.ends_with(".qoi"), "is_qoi_bytes wrong for {name}");
    }
    // A too-short buffer must not read past the end.
    for n in 0..20usize {
        let b = vec![b'q'; n];
        assert!(!is_qoi_bytes(&b) || n >= 14, "is_qoi_bytes over-read at len {n}");
    }
}

/// A valid uncompressed 32-bit Targa, hand-built.
///
/// The `image` crate can *decode* Targa but not *encode* it, so this is the only
/// way to have a real one — and having a real one is what turns "Targa has no
/// magic bytes" from a claim into a demonstration.
fn build_tga(w: u32, h: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(18 + (w * h * 4) as usize);
    v.push(0); // id length
    v.push(0); // no colour map
    v.push(2); // image type 2 = uncompressed true-colour
    v.extend_from_slice(&0u16.to_le_bytes()); // colour map origin
    v.extend_from_slice(&0u16.to_le_bytes()); // colour map length
    v.push(0); // colour map depth
    v.extend_from_slice(&0u16.to_le_bytes()); // x origin
    v.extend_from_slice(&0u16.to_le_bytes()); // y origin
    v.extend_from_slice(&(w as u16).to_le_bytes());
    v.extend_from_slice(&(h as u16).to_le_bytes());
    v.push(32); // bits per pixel
    v.push(8); // 8 alpha bits, top-to-bottom row order
    for y in 0..h {
        for x in 0..w {
            // BGRA
            v.extend_from_slice(&[(x * 3) as u8, (y * 5) as u8, 200, 255]);
        }
    }
    v
}

/// The `image` crate decoders this app skips, each with a stated reason.
#[test]
fn the_skipped_crate_decoders_all_have_a_stated_reason() {
    for (ext, why) in image_viewer::IMAGE_CRATE_DECODERS_WE_SKIP {
        assert!(!why.trim().is_empty(), "{ext} has no reason recorded");
    }
    // The one that matters structurally: a format we cannot detect from content
    // must not be advertised in a list that is built on content detection.
    assert!(
        !SUPPORTED_EXTENSIONS.contains(&"tga"),
        "tga is listed as supported but has no signature to detect it by"
    );
}

#[test]
fn a_targa_cannot_be_identified_from_its_content() {
    // The reason `tga` is skipped, proved rather than asserted. A real,
    // hand-built, structurally valid Targa — and content sniffing cannot see it,
    // because Targa has no signature.
    //
    // (The `tga` crate feature is deliberately not enabled, so this does not
    // claim the crate can decode one. The claim is narrower and is what actually
    // blocks us: our path never gets far enough to ask.)
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("pic.tga");
    let bytes = build_tga(40, 24);
    std::fs::write(&p, &bytes).unwrap();

    // The header we wrote is the Targa 2.0 header, so this is a real file.
    assert_eq!(bytes[2], 2, "image type 2 = uncompressed true-colour");
    assert_eq!(u16::from_le_bytes([bytes[12], bytes[13]]), 40, "declared width");
    assert_eq!(u16::from_le_bytes([bytes[14], bytes[15]]), 24, "declared height");
    assert_eq!(bytes.len(), 18 + 40 * 24 * 4);

    // And content sniffing genuinely cannot identify it.
    let sniffed = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.format());
    assert!(
        sniffed.is_none(),
        "Targa is now identifiable from content ({sniffed:?}) - revisit IMAGE_CRATE_DECODERS_WE_SKIP"
    );
    assert_eq!(image_dims_bytes(&bytes), None, "our header probe must not guess either");

    // Which is exactly why our own path fails, and must keep failing loudly
    // rather than guessing from the extension. The message names it
    // unsupported rather than unknown: nothing identifiable is in the bytes
    // at all, and "unknown" would suggest something to discover.
    let err = load_image(&p, 4096).expect_err("a Targa should not decode here");
    let msg = format!("{err:?}");
    assert!(
        msg.to_lowercase().contains("unsupported"),
        "expected an unsupported-file error, got {msg}"
    );
}

#[test]
fn bmp_decodes_at_its_real_dimensions() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_as(tmp.path(), "pic.bmp", &sample(), image::ImageFormat::Bmp);
    let bytes = std::fs::read(&p).unwrap();
    assert!(is_bmp_bytes(&bytes), "BM header not recognised");
    assert_eq!(image_dims_bytes(&bytes), Some((96, 48)));

    let d = load_image(&p, 4096).expect("BMP should decode");
    assert_eq!((d.width, d.height), (96, 48));
    assert_eq!(d.rgba.len(), 96 * 48 * 4);
}

#[test]
fn tiff_decodes_at_its_real_dimensions() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_as(tmp.path(), "scan.tiff", &sample(), image::ImageFormat::Tiff);
    let bytes = std::fs::read(&p).unwrap();
    assert!(is_tiff_bytes(&bytes), "TIFF header not recognised");
    assert_eq!(image_dims_bytes(&bytes), Some((96, 48)));

    let d = load_image(&p, 4096).expect("TIFF should decode");
    assert_eq!((d.width, d.height), (96, 48));
    assert_eq!(d.rgba.len(), 96 * 48 * 4);
}

#[test]
fn ico_decodes_at_its_real_dimensions() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_as(tmp.path(), "icon.ico", &sample_rgba(), image::ImageFormat::Ico);
    let bytes = std::fs::read(&p).unwrap();
    assert!(is_ico_bytes(&bytes), "ICO header not recognised");
    assert_eq!(image_dims_bytes(&bytes), Some((96, 48)));

    let d = load_image(&p, 4096).expect("ICO should decode");
    assert_eq!((d.width, d.height), (96, 48));
}

#[test]
fn an_ico_holding_a_bmp_compressed_entry_decodes() {
    // The form the encoder cannot write, and the one real icons use at the small
    // sizes. Built by hand, so it also proves the decoder is not simply routing
    // everything through the PNG path.
    let tmp = tempfile::tempdir().unwrap();
    let bytes = bmp_in_ico(16, 16);
    let p = tmp.path().join("legacy.ico");
    std::fs::write(&p, &bytes).unwrap();

    assert!(is_ico_bytes(&bytes));
    assert_eq!(image_dims_bytes(&bytes), Some((16, 16)));
    let d = load_image(&p, 4096).expect("BMP-compressed ICO should decode");
    assert_eq!((d.width, d.height), (16, 16));
}

#[test]
fn every_new_format_survives_a_lying_extension() {
    // The regression that matters for this feature: detection is by content, so
    // a TIFF named .jpg and a BMP named .png must both still open. This is the
    // same property the PNG-named-.jpg test covers for the older formats, and it
    // is the one a new sniffer is most likely to break.
    let tmp = tempfile::tempdir().unwrap();
    for (name, format) in [
        ("actually_a_tiff.jpg", image::ImageFormat::Tiff),
        ("actually_a_bmp.png", image::ImageFormat::Bmp),
        ("actually_an_ico.webp", image::ImageFormat::Ico),
    ] {
        // ICO needs an RGBA source; see `sample_rgba`.
        let img = if format == image::ImageFormat::Ico { sample_rgba() } else { sample() };
        let p = write_as(tmp.path(), name, &img, format);
        let d = load_image(&p, 4096).unwrap_or_else(|e| panic!("{name} failed: {e:?}"));
        assert_eq!((d.width, d.height), (96, 48), "{name} decoded at the wrong size");
    }
}

#[test]
fn the_sniffers_do_not_fire_on_each_other() {
    // A loose sniffer picks the wrong decoder, and the symptom is a plausible
    // wrong-sized image rather than an error, so it can sit here unnoticed.
    let tmp = tempfile::tempdir().unwrap();
    let cases: [(&str, image::ImageFormat); 6] = [
        ("a.png", image::ImageFormat::Png),
        ("a.jpg", image::ImageFormat::Jpeg),
        ("a.bmp", image::ImageFormat::Bmp),
        ("a.tiff", image::ImageFormat::Tiff),
        ("a.ico", image::ImageFormat::Ico),
        ("a.gif", image::ImageFormat::Gif),
    ];
    let mut written = Vec::new();
    for (name, format) in cases {
        let img = if format == image::ImageFormat::Ico { sample_rgba() } else { sample() };
        let p = write_as(tmp.path(), name, &img, format);
        written.push((name, std::fs::read(&p).unwrap()));
    }
    for (name, bytes) in &written {
        let want_bmp = name.ends_with(".bmp");
        let want_tiff = name.ends_with(".tiff");
        let want_ico = name.ends_with(".ico");
        assert_eq!(is_bmp_bytes(bytes), want_bmp, "is_bmp_bytes wrong for {name}");
        assert_eq!(is_tiff_bytes(bytes), want_tiff, "is_tiff_bytes wrong for {name}");
        assert_eq!(is_ico_bytes(bytes), want_ico, "is_ico_bytes wrong for {name}");
    }
}

#[test]
fn truncated_and_empty_input_is_rejected_rather_than_panicking() {
    // These run on a worker against whatever bytes are on disk, including a file
    // that is still being written. Every one of them has to be a clean `false`,
    // because a panic here is a lost image, not a caught error.
    for bytes in [
        &[][..],
        &b"B"[..],
        &b"BM"[..],
        &b"BM\x00\x00"[..],
        &b"II"[..],
        &b"II\x2a"[..],
        &b"MM"[..],
        &b"MM\x00"[..],
        &b"II\x00\x00"[..], // right byte order, wrong version tag
        &b"MM\x00\x00"[..],
        &b"\x00\x00\x00\x00"[..],
        &b"\x00\x00\x01\x00"[..],
        &b"\x00\x00\x02\x00\x00\x00"[..],
    ] {
        let label = format!("{:?}", &bytes[..bytes.len().min(8)]);
        assert!(!is_bmp_bytes(bytes), "is_bmp_bytes accepted {label}");
        assert!(!is_tiff_bytes(bytes), "is_tiff_bytes accepted {label}");
        assert!(!is_ico_bytes(bytes), "is_ico_bytes accepted {label}");
    }
}

#[test]
fn a_bare_tiff_signature_is_accepted() {
    // The other side of the case above, and the thing that made the list above
    // wrong when first written: `II*\0` and `MM\0*` are complete, valid TIFF
    // headers. A sniffer must not demand a whole file to recognise one — these
    // are four bytes on disk and they are exactly what a real file starts with.
    for bytes in [
        &b"II\x2a\x00"[..],
        &b"MM\x00\x2a"[..],
        &b"II\x2b\x00"[..], // BigTIFF
        &b"MM\x00\x2b"[..], // BigTIFF
    ] {
        assert!(is_tiff_bytes(bytes), "rejected a valid TIFF signature {bytes:?}");
        assert!(!is_bmp_bytes(bytes), "{bytes:?} also matched BMP");
        assert!(!is_ico_bytes(bytes), "{bytes:?} also matched ICO");
    }
}

#[test]
fn a_folder_scan_finds_the_new_formats() {
    // Detection being content-based does not make the *listing* content-based:
    // `scan_folder` still filters on extension, so a format missing from
    // SUPPORTED_EXTENSIONS is invisible in a folder even though it would decode.
    let tmp = tempfile::tempdir().unwrap();
    for (name, format) in [
        ("a.tiff", image::ImageFormat::Tiff),
        ("a.tif", image::ImageFormat::Tiff),
        ("a.bmp", image::ImageFormat::Bmp),
        ("a.ico", image::ImageFormat::Ico),
        ("a.jpg", image::ImageFormat::Jpeg),
    ] {
        write_as(tmp.path(), name, &sample(), format);
    }
    std::fs::write(tmp.path().join("notes.txt"), b"not an image").unwrap();

    let found: Vec<String> = scan_folder(tmp.path())
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    for want in ["a.tiff", "a.tif", "a.bmp", "a.ico", "a.jpg"] {
        assert!(found.iter().any(|f| f == want), "{want} missing from {found:?}");
    }
    assert!(!found.iter().any(|f| f == "notes.txt"), "a text file was listed");
}

#[test]
fn the_extension_list_covers_both_spellings_and_both_ico_kinds() {
    // `tif`/`tiff` and `ico`/`cur` are the same formats; a scan that only knows
    // one spelling silently hides files the decoder could open.
    for want in ["tif", "tiff", "bmp", "dib", "ico", "cur"] {
        assert!(SUPPORTED_EXTENSIONS.contains(&want), "{want} missing from SUPPORTED_EXTENSIONS");
    }
}

#[test]
fn a_preview_of_a_new_format_reports_its_source_dimensions() {
    // The single-pass preview path is what the viewer actually uses, and it
    // reports `orig` from the sniffed header. For a quarter-turn check that has
    // to agree with the decode, so a mismatch would show up as a wrong fit.
    let tmp = tempfile::tempdir().unwrap();
    for (name, format) in [
        ("p.bmp", image::ImageFormat::Bmp),
        ("p.tiff", image::ImageFormat::Tiff),
        ("p.ico", image::ImageFormat::Ico),
    ] {
        let img = if format == image::ImageFormat::Ico { sample_rgba() } else { sample() };
        let p = write_as(tmp.path(), name, &img, format);
        let r = load_image_preview_full(&p, 4096).unwrap_or_else(|e| panic!("{name}: {e:?}"));
        assert_eq!(r.orig, Some((96, 48)), "{name} sniffed the wrong size");
        assert_eq!(
            (r.preview.width, r.preview.height),
            (96, 48),
            "{name} previewed at the wrong size"
        );
        // Small files skip the two-stage path, so the full buffer rides along.
        assert!(r.full.is_some(), "{name} should keep its full buffer in one pass");
    }
}
