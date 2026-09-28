//! Colour-profile *detection*.
//!
//! Detection only — no pixel is transformed by this stage, so nothing here can
//! change what an image looks like. What is pinned is the classifier's
//! decisions, because they decide which files a future transform must handle.
//!
//! Every fixture is synthesised, per the AGENTS.md rule that suites must not
//! depend on `test-images/`.

#[path = "common/icc_fixtures.rs"]
mod icc_fixtures;

use icc_fixtures::{
    icc_profile, jpeg_with_icc, png_with_iccp, png_with_srgb_chunk, P3_RED, SRGB_RED,
};
use image_viewer::{color_info_from_bytes, ColorSpace};
use std::path::Path;

/// Route bytes through the same entry point the app uses.
fn jpeg(bytes: &[u8]) -> image_viewer::ColorInfo {
    color_info_from_bytes(bytes, true, false)
}
fn png(bytes: &[u8]) -> image_viewer::ColorInfo {
    color_info_from_bytes(bytes, false, true)
}

#[test]
fn test_named_srgb_profile_is_reported_as_srgb() {
    let p = icc_profile(Some("sRGB IEC61966-2.1"), SRGB_RED, 0);
    let info = jpeg(&jpeg_with_icc(64, 48, &p));
    assert_eq!(info.space, ColorSpace::Srgb);
    assert!(info.description.contains("sRGB"), "got {:?}", info.description);
    assert_eq!(info.data_space, "RGB");
    assert_eq!(info.red_primaries, Some([SRGB_RED.0, SRGB_RED.1, SRGB_RED.2]));
}

#[test]
fn test_named_p3_profile_is_reported_wide_gamut() {
    let p = icc_profile(Some("Display P3"), P3_RED, 0);
    let info = jpeg(&jpeg_with_icc(64, 48, &p));
    assert_eq!(info.space, ColorSpace::WideGamut);
    assert_eq!(info.description, "Display P3");
    assert_eq!(info.red_primaries, Some([P3_RED.0, P3_RED.1, P3_RED.2]));
}

#[test]
fn test_primaries_classify_when_there_is_no_description() {
    // The case that actually dominates real files: 170 of the 200 wide-gamut
    // files in a phone backup carry a profile with **no** `desc` tag at all, so
    // the name path cannot fire and the primaries are the only signal.
    let anon_srgb = jpeg(&jpeg_with_icc(64, 48, &icc_profile(None, SRGB_RED, 0)));
    assert_eq!(anon_srgb.space, ColorSpace::Srgb, "sRGB primaries, no name");
    assert_eq!(anon_srgb.description, "");

    let anon_p3 = jpeg(&jpeg_with_icc(64, 48, &icc_profile(None, P3_RED, 0)));
    assert_eq!(
        anon_p3.space,
        ColorSpace::WideGamut,
        "P3 primaries, no name"
    );

    // ...and the two must not be confused with each other.
    assert_ne!(anon_srgb.red_primaries, anon_p3.red_primaries);
}

#[test]
fn test_png_iccp_profile_is_inflated() {
    // PNG stores iCCP zlib-compressed. Handing the raw chunk to the ICC parser
    // finds no 'acsp' at offset 36 and silently reports "unknown" — which is
    // exactly the bug this test exists to pin. It only showed up once real P3
    // PNGs were available to check against.
    let p = icc_profile(Some("Display P3"), P3_RED, 0);
    let info = png(&png_with_iccp(64, 48, &p, "p3"));
    assert_eq!(
        info.space,
        ColorSpace::WideGamut,
        "compressed iCCP must still classify; got {:?} ({})",
        info.space,
        info.description
    );
    assert_eq!(info.red_primaries, Some([P3_RED.0, P3_RED.1, P3_RED.2]));

    let s = icc_profile(Some("sRGB IEC61966-2.1"), SRGB_RED, 0);
    assert_eq!(png(&png_with_iccp(64, 48, &s, "srgb")).space, ColorSpace::Srgb);
}

#[test]
fn test_multi_chunk_icc_profile_is_reassembled() {
    // A profile over ~64 KB is split across numbered APP2 chunks. Getting the
    // reassembly wrong yields a truncated profile and "unknown".
    let big = icc_profile(Some("Display P3"), P3_RED, 200_000);
    assert!(big.len() > 120_000, "fixture must span several chunks");
    let info = jpeg(&jpeg_with_icc(32, 32, &big));
    assert_eq!(
        info.space,
        ColorSpace::WideGamut,
        "multi-chunk profile lost; got {:?}",
        info.space
    );
    assert_eq!(info.profile_bytes, big.len());
}

#[test]
fn test_no_profile_is_untagged_srgb() {
    // A JPEG with no APP2: sRGB by convention. Same for a PNG with no colour
    // chunks at all, and for a PNG that states sRGB outright.
    assert_eq!(
        jpeg(&jpeg_with_icc(32, 32, &[])).space,
        ColorSpace::UntaggedSrgb
    );
    assert_eq!(png(&png_with_srgb_chunk(32, 32)).space, ColorSpace::UntaggedSrgb);
    // A JPEG built by the encoder has no ICC of its own.
    let mut plain = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(8, 8)).write_to(
        &mut std::io::Cursor::new(&mut plain),
        image::ImageFormat::Jpeg,
    )
    .unwrap();
    assert_eq!(jpeg(&plain).space, ColorSpace::UntaggedSrgb);
    assert_eq!(jpeg(&plain).profile_bytes, 0);
}

#[test]
fn test_non_rgb_profile_is_not_called_srgb() {
    // A CMYK profile must not be mistaken for an RGB one; the transform for it
    // is a completely different problem.
    let mut p = icc_profile(Some("Some CMYK press profile"), SRGB_RED, 0);
    p[16..20].copy_from_slice(b"CMYK");
    let info = jpeg(&jpeg_with_icc(32, 32, &p));
    assert_eq!(info.space, ColorSpace::NonRgb);
    assert_eq!(info.data_space, "CMYK");
}

#[test]
fn test_other_containers_are_not_inspected_not_assumed() {
    // HEIC/AVIF/JXL keep colour info in an ISO-BMFF `colr` box or the codestream.
    // Reporting them as NotInspected rather than assuming sRGB is deliberate:
    // assuming is how a wide-gamut HEIC photo ends up looking flat and nobody
    // notices, because everything "works".
    for bytes in [
        &b"\0\0\0\x18ftypheic"[..],
        &b"\0\0\0\x18ftypavif"[..],
        &b"\xff\x0a"[..],
    ] {
        let info = color_info_from_bytes(bytes, false, false);
        assert_eq!(
            info.space,
            ColorSpace::NotInspected,
            "{:02X?} must not be assumed sRGB",
            &bytes[..4.min(bytes.len())]
        );
        assert!(!info.summary().contains("sRGB"));
    }
}

#[test]
fn test_malformed_profiles_never_panic_and_never_claim_srgb() {
    // Offsets, lengths and tag counts come straight out of the file. Every one
    // of these must degrade to Unknown, and must not be mistaken for sRGB.
    let good = icc_profile(Some("Display P3"), P3_RED, 0);
    let mut cases: Vec<Vec<u8>> = Vec::new();

    // Not an ICC profile at all.
    cases.push(vec![0u8; 400]);
    // Right size, wrong signature.
    let mut no_acsp = good.clone();
    no_acsp[36..40].copy_from_slice(b"XXXX");
    cases.push(no_acsp);
    // Absurd tag count.
    let mut many = good.clone();
    many[128..132].copy_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
    cases.push(many);
    // Tag offsets pointing outside the profile.
    let mut wild = good.clone();
    wild[136..140].copy_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
    cases.push(wild);
    // Truncated mid-header and mid-table.
    cases.push(good[..64].to_vec());
    cases.push(good[..140].to_vec());
    // A 'desc' tag whose string offset is past the end.
    let mut bad_desc = good.clone();
    bad_desc[136..140].copy_from_slice(&0x0000_FFFFu32.to_be_bytes());
    cases.push(bad_desc);

    for (i, c) in cases.iter().enumerate() {
        let info = jpeg(&jpeg_with_icc(16, 16, c));
        assert!(
            matches!(
                info.space,
                ColorSpace::Unknown | ColorSpace::NonRgb | ColorSpace::UntaggedSrgb
            ),
            "case {i}: malformed profile reported {:?} ({}), which is a claim we cannot support",
            info.space,
            info.description
        );
        assert_ne!(
            info.space,
            ColorSpace::Srgb,
            "case {i}: malformed profile must never be reported as sRGB"
        );
    }
}

#[test]
fn test_a_profile_is_reported_even_with_no_exif_block() {
    // The regression. `extract_exif` used to open with `read_exif(path).ok()?`,
    // so a file with an ICC profile and **no** EXIF — which is what a
    // screenshot or a forwarded/downloaded phone photo is — returned `None` and
    // the panel rendered empty, hiding the only interesting fact about the file.
    // Real file hit: `color-p3-named.jpg` from a phone backup.
    let mut bytes = jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0));
    assert!(
        !bytes.windows(6).any(|w| w == b"Exif\0\0"),
        "fixture must have no APP1/Exif for this test to mean anything"
    );
    let info = jpeg(&bytes);
    assert_eq!(info.space, ColorSpace::WideGamut);
    assert!(
        info.panel_line().is_some(),
        "a P3 file with no EXIF must still produce a panel line"
    );

    // An untagged file stays quiet — that is the overwhelming majority, and
    // "Colour profile: sRGB (untagged)" on every screenshot is noise.
    assert!(jpeg(&jpeg_with_icc(32, 32, &[])).panel_line().is_none());
    // An explicitly sRGB-profiled file *does* earn a line: it tells the user the
    // file was checked and needs no conversion, which is the reassuring answer
    // rather than a useless one.
    let tagged = jpeg(&jpeg_with_icc(32, 32, &icc_profile(Some("sRGB IEC61966-2.1"), SRGB_RED, 0)));
    assert_eq!(tagged.space, ColorSpace::Srgb);
    assert!(
        tagged.panel_line().is_some(),
        "an explicitly tagged sRGB file is worth confirming"
    );

    // Anything the user might act on does earn a line.
    for (what, info) in [
        (
            "wide gamut",
            jpeg(&jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0))),
        ),
        (
            "unreadable profile in a real jpeg",
            jpeg(&jpeg_with_icc(32, 32, &vec![0u8; 300])),
        ),
        (
            "format we do not read",
            color_info_from_bytes(b"\0\0\0\x18ftypheic", false, false),
        ),
    ] {
        assert!(
            info.panel_line().is_some(),
            "{what} ({:?}) should be surfaced to the user",
            info.space
        );
    }
}

#[test]
fn test_color_info_on_disk_matches_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("p3.jpg");
    std::fs::write(
        &p,
        jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0)),
    )
    .unwrap();
    let from_disk = image_viewer::color_info(Path::new(&p));
    let from_bytes = jpeg(&std::fs::read(&p).unwrap());
    assert_eq!(from_disk.space, from_bytes.space);
    assert_eq!(from_disk.description, from_bytes.description);
    assert_eq!(from_disk.red_primaries, from_bytes.red_primaries);

    // An unreadable path is Unknown, not a panic and not a guess.
    assert_eq!(
        image_viewer::color_info(Path::new("/definitely/not/here.jpg")).space,
        ColorSpace::Unknown
    );
}
