//! TIFF, BMP and ICO, and the sniffing that lets them in.
//!
//! Format detection here is content-based, never extension-based, so the tests
//! come in pairs: one that a real file decodes, and one that the *same bytes*
//! under a lying extension still decode. A sniffer that only works on correctly
//! named files is a sniffer that will one day pick the wrong decoder.

use image_viewer::{
    image_dims_bytes, is_bmp_bytes, is_ico_bytes, is_tiff_bytes, load_image, load_image_preview_full,
    scan_folder, SUPPORTED_EXTENSIONS,
};
use std::path::Path;

/// Encode `img` in `format`, with `name` as the file name — deliberately able to
/// disagree with the content.
fn write_as(dir: &Path, name: &str, img: &image::DynamicImage, format: image::ImageFormat) -> std::path::PathBuf {
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
    for i in 0..(w * h) {
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
        let img = if format == image::ImageFormat::Ico {
            sample_rgba()
        } else {
            sample()
        };
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
        let img = if format == image::ImageFormat::Ico {
            sample_rgba()
        } else {
            sample()
        };
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
        &b"II\x00\x00"[..],   // right byte order, wrong version tag
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
        assert!(
            SUPPORTED_EXTENSIONS.contains(&want),
            "{want} missing from SUPPORTED_EXTENSIONS"
        );
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
        let img = if format == image::ImageFormat::Ico {
            sample_rgba()
        } else {
            sample()
        };
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
