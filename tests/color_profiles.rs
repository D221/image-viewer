//! Colour-profile *detection*.
//!
//! Detection only — no pixel is transformed by this stage, so nothing here can
//! change what an image looks like. What is pinned is the classifier's
//! decisions, because they decide which files a future transform must handle.
//!
//! Every fixture is synthesised, per the AGENTS.md rule that suites must not
//! depend on `test-images/`.

#[path = "common/heif_fixtures.rs"]
mod heif_fixtures;
#[path = "common/icc_fixtures.rs"]
mod icc_fixtures;

use icc_fixtures::{
    P3_RED, SRGB_RED, icc_profile, jpeg_with_icc, png_with_iccp, png_with_srgb_chunk,
};
use image_viewer::{ColorSpace, color_info_from_bytes};
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
    assert_eq!(anon_p3.space, ColorSpace::WideGamut, "P3 primaries, no name");

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
    assert_eq!(info.space, ColorSpace::WideGamut, "multi-chunk profile lost; got {:?}", info.space);
    assert_eq!(info.profile_bytes, big.len());
}

#[test]
fn test_no_profile_is_untagged_srgb() {
    // A JPEG with no APP2: sRGB by convention. Same for a PNG with no colour
    // chunks at all, and for a PNG that states sRGB outright.
    assert_eq!(jpeg(&jpeg_with_icc(32, 32, &[])).space, ColorSpace::UntaggedSrgb);
    assert_eq!(png(&png_with_srgb_chunk(32, 32)).space, ColorSpace::UntaggedSrgb);
    // A JPEG built by the encoder has no ICC of its own.
    let mut plain = Vec::new();
    image::DynamicImage::ImageRgba8(image::RgbaImage::new(8, 8))
        .write_to(&mut std::io::Cursor::new(&mut plain), image::ImageFormat::Jpeg)
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
    // Two things still legitimately report NotInspected, and the reason is the
    // same for both: we do not know what colour they are, and saying nothing is
    // the only honest answer. Guessing sRGB is how a wide-gamut photo ends up
    // looking flat and nobody notices, because everything "works".
    //
    // A JXL naked codestream carries its colour inside the codestream, which
    // nothing here parses. The `ftyp` blobs are ISO-BMFF headers with no `meta`
    // box — a container we cannot read, which is a different thing from a format
    // we do not read and is worth keeping in the list: a truncated or hand-edited
    // HEIF must not fall back to a claim. (`test_a_corrupt_heif_claims_nothing_about_its_colour`
    // covers that properly, with real containers cut short.)
    for bytes in [&b"\0\0\0\x18ftypheic"[..], &b"\0\0\0\x18ftypavif"[..], &b"\xff\x0a"[..]] {
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
fn test_gif_has_no_profile_chunk_so_it_is_untagged_srgb() {
    // A GIF carries no colour profile chunk at all — its colours come from a
    // palette. The honest answer is UntaggedSrgb, by the same convention
    // untagged PNG and JPEG already get, not NotInspected.
    for bytes in [&b"GIF87a"[..], &b"GIF89a"[..]] {
        let info = color_info_from_bytes(bytes, false, false);
        assert_eq!(
            info.space,
            ColorSpace::UntaggedSrgb,
            "{:?} has no profile to inspect, so it is untagged sRGB",
            &bytes[..6]
        );
    }
}

#[test]
fn test_webp_has_no_profile_chunk_so_it_is_untagged_srgb() {
    // A WebP has no ICC profile chunk at all — the honest answer is
    // UntaggedSrgb, by the same convention untagged PNG and JPEG already get,
    // not NotInspected.
    for bytes in [&b"RIFF....WEBP"[..], &b"RIFF....WEBPVP8X"[..]] {
        let info = color_info_from_bytes(bytes, false, false);
        assert_eq!(
            info.space,
            ColorSpace::UntaggedSrgb,
            "{:?} has no profile to inspect, so it is untagged sRGB",
            &bytes[..12]
        );
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
    let bytes = jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0));
    assert!(
        !bytes.windows(6).any(|w| w == b"Exif\0\0"),
        "fixture must have no APP1/Exif for this test to mean anything"
    );
    let info = jpeg(&bytes);
    assert_eq!(info.space, ColorSpace::WideGamut);
    assert!(info.panel_line().is_some(), "a P3 file with no EXIF must still produce a panel line");

    // An untagged file stays quiet — that is the overwhelming majority, and
    // "Colour profile: sRGB (untagged)" on every screenshot is noise.
    assert!(jpeg(&jpeg_with_icc(32, 32, &[])).panel_line().is_none());
    // An explicitly sRGB-profiled file *does* earn a line: it tells the user the
    // file was checked and needs no conversion, which is the reassuring answer
    // rather than a useless one.
    let tagged = jpeg(&jpeg_with_icc(32, 32, &icc_profile(Some("sRGB IEC61966-2.1"), SRGB_RED, 0)));
    assert_eq!(tagged.space, ColorSpace::Srgb);
    assert!(tagged.panel_line().is_some(), "an explicitly tagged sRGB file is worth confirming");

    // Anything the user might act on does earn a line.
    for (what, info) in [
        ("wide gamut", jpeg(&jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0)))),
        ("unreadable profile in a real jpeg", jpeg(&jpeg_with_icc(32, 32, &vec![0u8; 300]))),
        ("format we do not read", color_info_from_bytes(b"\0\0\0\x18ftypheic", false, false)),
    ] {
        assert!(
            info.panel_line().is_some(),
            "{what} ({:?}) should be surfaced to the user",
            info.space
        );
    }
}

// ---------------------------------------------------------------------------
// Wide-gamut to sRGB: the matrix
// ---------------------------------------------------------------------------
//
// The conversion is behind a setting and defaults off. What is pinned here is the
// maths, because that is the part that can be wrong in a way nobody sees: the
// pixels still look like an image, just a slightly wrong one.
//
// The matrix is **derived from the two spaces' chromaticities** rather than copied
// out of a spec, and then checked against a fact about files. `P3_RED` is the
// D50-adapted red primary measured out of 192 real Display P3 profiles, and ICC
// requires primaries to be stored adapted to D50 — so the derived matrix, pushed
// through the same adaptation, has to land on that number. If the derivation, the
// adaptation or the published primaries were wrong, they would disagree.

/// Bradford chromatic adaptation, D65 → D50, as published to seven decimals.
///
/// Taken as a constant rather than derived from the cone-response matrix on
/// purpose. The cone-response form is only published to four decimals, and that
/// is good enough to about 0.3% — which would have made this test a comparison
/// of rounding errors. The two forms also failed in opposite directions on the
/// way here: the first attempt had the product order the wrong way round
/// (`M_A · scale · M_A⁻¹` instead of `M_A⁻¹ · scale · M_A`), which type-checks and
/// returns a perfectly plausible matrix that maps the white point somewhere else.
///
/// Test-only. The conversion itself needs no adaptation at all — both RGB spaces
/// are D65-referred — which is why this lives in the test and not in `lib.rs`.
const BRADFORD_D65_TO_D50: [[f64; 3]; 3] = [
    [1.0478112, 0.0228866, -0.0501270],
    [0.0295424, 0.9904844, -0.0170491],
    [-0.0092345, 0.0150436, 0.7521316],
];

fn mul_v(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    ]
}

#[test]
fn test_the_derived_matrix_reproduces_the_icc_primaries_we_measured() {
    // D65, the white point both RGB spaces are referred to, and D50, the ICC
    // profile connection space's illuminant.
    let to_d50 = BRADFORD_D65_TO_D50;
    // The adaptation has to map the source white exactly onto the destination
    // white, or nothing derived from it means anything. Asserting that first
    // turns the next assertion from "close enough" into a real check — and it is
    // the assertion that caught the transposed product.
    let mapped = mul_v(&to_d50, [0.95047, 1.0, 1.08883]);
    for (got, want) in mapped.iter().zip([0.96422, 1.0, 0.82521]) {
        assert!((got - want).abs() < 1e-5, "the adaptation moved the white point to {mapped:?}");
    }

    for (what, primaries) in [("Display P3", image_viewer::P3_XY), ("sRGB", image_viewer::SRGB_XY)]
    {
        let rgb_to_xyz = image_viewer::rgb_to_xyz(primaries, image_viewer::D65_XY);
        let red_65 = [rgb_to_xyz[0][0], rgb_to_xyz[1][0], rgb_to_xyz[2][0]];
        let adapted = mul_v(&to_d50, red_65);

        let expected: [i32; 3] = if what == "Display P3" {
            [P3_RED.0, P3_RED.1, P3_RED.2]
        } else {
            [SRGB_RED.0, SRGB_RED.1, SRGB_RED.2]
        };
        // Agreement is currently 5 units on Display P3 and 9 on sRGB, out of a
        // 16-bit fixed-point field. 100 is generous and still unambiguous: the
        // transposed derivation this replaced was out by ~3900, and a matrix
        // built on the wrong white point is out by more.
        for (channel, (got, want)) in adapted.iter().zip(expected).enumerate() {
            let want = want as f64 / 65536.0;
            assert!(
                (got - want).abs() < 100.0 / 65536.0,
                "{what} channel {channel}: derived D50 primary {got:.6}, \
                 measured in real profiles {want:.6}"
            );
        }
    }
}

#[test]
fn test_the_conversion_matrix_maps_white_to_white_and_inverts() {
    let m = image_viewer::p3_to_srgb_matrix();

    // White is white in both spaces, so a correct matrix cannot move it.
    let white =
        [m[0][0] + m[0][1] + m[0][2], m[1][0] + m[1][1] + m[1][2], m[2][0] + m[2][1] + m[2][2]];
    for (c, v) in white.iter().enumerate() {
        assert!((v - 1.0).abs() < 1e-9, "white channel {c} moved to {v}");
    }

    // And it must be invertible, or "convert" is not a round trip.
    let round = image_viewer::mul_mat_ref(m, &image_viewer::srgb_to_p3_matrix());
    for (r, row) in round.iter().enumerate() {
        for (c, got) in row.iter().enumerate() {
            let want = if r == c { 1.0 } else { 0.0 };
            assert!((*got - want).abs() < 1e-9, "round trip at [{r}][{c}] is {got}, want {want}");
        }
    }
}

#[test]
fn test_the_conversion_leaves_neutrals_alone_and_moves_mid_tones() {
    // What has to hold whatever the matrix says, and that a plausible-but-wrong
    // conversion would break.
    //
    // The interesting pixels are **mid-tones**, not primaries. Every pure primary
    // converts to itself once clipping is applied — P3 red is outside sRGB, clips
    // to sRGB red, and the byte comes back 255 — so a test built from primaries
    // passes against a completely broken matrix, which is how the first version of
    // this one managed to assert nothing at all.
    let mut px: Vec<u8> = Vec::new();
    for (r, g, b, a) in [
        (0u8, 0u8, 0u8, 0u8), // black, fully transparent
        (255, 255, 255, 255), // white
        (128, 128, 128, 128), // mid grey, half alpha
        (128, 64, 32, 255),   // a mid-tone: P3 and sRGB disagree about this one
        (200, 150, 100, 255), // and another
        (0, 0, 255, 255),     // P3's blue primary, which is sRGB's blue primary
    ] {
        px.extend_from_slice(&[r, g, b, a]);
    }
    let before = px.clone();
    image_viewer::convert_wide_gamut_rgba(&mut px);

    // Alpha is never touched. A colour transform that moved alpha would make
    // every converted PNG with transparency wrong in a way nobody would read as
    // a colour bug.
    for (i, (b, a)) in before.chunks(4).zip(px.chunks(4)).enumerate() {
        assert_eq!(b[3], a[3], "pixel {i}: alpha changed");
    }
    // Black, white and grey are the colours every RGB space agrees on.
    assert_eq!(&px[0..3], &[0, 0, 0], "black moved");
    assert_eq!(&px[4..7], &[255, 255, 255], "white moved");
    assert_eq!(&px[8..11], &[128, 128, 128], "grey is white scaled down, so it cannot move");
    // A mid-tone moves, and it moves *in a direction*: P3 is the wider gamut, so
    // the same numbers in P3 describe a more saturated colour, and converting to
    // sRGB has to pull the channels toward the neutral — red up, green and blue
    // down.
    let (r0, g0, b0) = (before[12], before[13], before[14]);
    let (r1, g1, b1) = (px[12], px[13], px[14]);
    assert_ne!((r1, g1, b1), (r0, g0, b0), "a mid-tone must actually convert");
    assert!(r1 > r0, "red rises: {r0} -> {r1}");
    assert!(g1 < g0, "green falls: {g0} -> {g1}");
    assert!(b1 < b0, "blue falls: {b0} -> {b1}");

    // And the shared primary must come out byte-identical, which is the clearest
    // evidence the matrix is anchored in the right place.
    assert_eq!(&px[20..24], &before[20..24], "the shared blue primary must be untouched");
}

/// `set_wide_gamut_conversion` is a process-wide global and `cargo nextest` runs
/// these concurrently, so anything asserting on a specific state takes this lock
/// and puts the old value back. Same trap, same fix as `FILTER_GLOBALS` in
/// `tests/unit.rs`.
static CONVERSION_GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn with_conversion<T>(on: bool, f: impl FnOnce() -> T) -> T {
    let _guard = CONVERSION_GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
    let was = image_viewer::wide_gamut_conversion();
    image_viewer::set_wide_gamut_conversion(on);
    let out = f();
    image_viewer::set_wide_gamut_conversion(was);
    out
}

#[test]
fn test_the_conversion_is_off_by_default_and_only_touches_wide_gamut_files() {
    // The default matters more than the maths: this changes how photos look, and
    // nobody asked for that. If this ever asserts false, an existing user's
    // pictures changed without them doing anything.
    assert!(!image_viewer::wide_gamut_conversion(), "wide-gamut conversion must ship off");

    let dir = tempfile::tempdir().unwrap();
    let p3 = dir.path().join("p3.jpg");
    std::fs::write(&p3, jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0)))
        .unwrap();
    let plain = dir.path().join("plain.jpg");
    std::fs::write(&plain, jpeg_with_icc(32, 32, &[])).unwrap();

    let decode = |p: &std::path::Path, on: bool| {
        with_conversion(on, || {
            image_viewer::load_page_preview_full(p, 512, 0).expect("decode").preview.rgba
        })
    };

    // Off is off, and it is the same off the setting shipped as.
    assert_eq!(decode(&p3, false), decode(&p3, false));
    // On, a wide-gamut file is converted.
    let unconverted = decode(&p3, false);
    let converted = decode(&p3, true);
    assert_ne!(unconverted, converted, "a Display P3 file must convert when the setting is on");
    // And off again is off again, so the flag is not sticky in the other
    // direction either.
    assert_eq!(unconverted, decode(&p3, false));
    // An untagged file is already sRGB, and pushing it through the matrix would
    // be the exact error the classifier exists to prevent.
    assert_eq!(
        decode(&plain, false),
        decode(&plain, true),
        "an untagged file must not be converted"
    );
}

#[test]
fn test_the_decode_carries_its_own_colour_classification() {
    // The rule this change encodes: the colour answer is computed from the bytes
    // the decode already read, and travels with the result. `color_info(path)`
    // — a second whole-file `fs::read` of the file, per decode — is what this
    // replaces, which is why the pin is agreement: whatever the standalone read
    // would say, the decode's own answer already is.
    let dir = tempfile::tempdir().unwrap();
    let p3 = dir.path().join("p3.jpg");
    std::fs::write(&p3, jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0)))
        .unwrap();
    let plain = dir.path().join("plain.jpg");
    std::fs::write(&plain, jpeg_with_icc(32, 32, &[])).unwrap();

    let r = image_viewer::load_page_preview_full(&p3, 512, 0).unwrap();
    assert_eq!(
        r.colour,
        image_viewer::color_info(&p3),
        "the decode's colour answer must be the standalone read's answer"
    );
    assert_eq!(r.colour.space, image_viewer::ColorSpace::WideGamut);

    // And the same for a file with no profile at all: the answer rides on the
    // decode every time, not only when something interesting is there.
    let r = image_viewer::load_page_preview_full(&plain, 512, 0).unwrap();
    assert_eq!(r.colour, image_viewer::color_info(&plain));
}

#[test]
fn test_out_of_gamut_colours_clip_rather_than_wrap() {
    // The honest cost of the conversion, pinned: P3's red and green primaries are
    // outside sRGB, and clipping is the only thing that can be done with them.
    // They come back as the *same* bytes, because clipping maps a P3 primary onto
    // the sRGB primary of the same hue — which is why a fixture built from
    // primaries cannot test this feature at all.
    let mut px = Vec::new();
    for c in [(255u8, 0u8, 0u8), (0, 255, 0), (0, 0, 255), (255, 255, 0)] {
        px.extend_from_slice(&[c.0, c.1, c.2, 255]);
    }
    image_viewer::convert_wide_gamut_rgba(&mut px);
    let want = [(255u8, 0u8, 0u8), (0, 255, 0), (0, 0, 255), (255, 255, 0)];
    for (i, (got, want)) in px.chunks(4).zip(want).enumerate() {
        assert_eq!(
            (got[0], got[1], got[2]),
            want,
            "saturated colour {i} did not clip to its sRGB primary"
        );
    }
}

#[test]
fn test_the_transfer_luts_are_within_one_code_of_the_exact_curves() {
    // The LUTs are what make this affordable on the decode path, and they are an
    // approximation. Pin how good: the exact curves are recomputed here from the
    // ICC definitions rather than taken from the same place the LUT is.
    let m = image_viewer::p3_to_srgb_matrix();
    let mut worst = 0.0f64;
    for v in 0..=255u32 {
        let c = v as f64 / 255.0;
        let lin = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        let (r, g, b) = (lin, lin, lin);
        let lr = m[0][0] * r + m[0][1] * g + m[0][2] * b;
        let exact = if lr <= 0.003_130_8 { lr * 12.92 } else { 1.055 * lr.powf(1.0 / 2.4) - 0.055 };
        worst = worst.max((exact * 255.0 - v as f64).abs());
    }
    assert!(worst <= 1.0, "the 12-bit LUT is {worst:.2} codes from the exact curve at worst");
}

#[test]
fn test_color_info_on_disk_matches_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("p3.jpg");
    std::fs::write(&p, jpeg_with_icc(32, 32, &icc_profile(Some("Display P3"), P3_RED, 0))).unwrap();
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

// ---------------------------------------------------------------------------
// HEIC / AVIF: the `colr` box
// ---------------------------------------------------------------------------
//
// HEIF keeps its colour in an ISO-BMFF `colr` box, which is why these files used
// to report "not inspected" while carrying a perfectly ordinary ICC profile —
// and iPhone HEIC is frequently Display P3, so that was a wide-gamut photo being
// displayed as if it were sRGB with nothing in the metadata panel to say so.
//
// `heic::DecoderConfig::extract_icc` returns the profile bytes, so this is glue
// rather than a container walk. What the fixtures below cannot be built by any
// crate in this tree, hence `tests/common/heif_fixtures.rs`.
//
// Verified against real files as well: of four HEIF/AVIF images in
// `test-images`, two carry a 536-byte profile that classifies as Display P3 from
// its primaries **with no `desc` tag**, and two have no profile at all.

/// Route bytes through the same entry point the app uses, as a caller that has
/// already sniffed "not a JPEG, not a PNG" would.
fn heif(bytes: &[u8]) -> image_viewer::ColorInfo {
    color_info_from_bytes(bytes, false, false)
}

#[test]
fn test_a_heif_profile_is_classified_like_any_other() {
    // The real shape of the problem: no description, primaries only. This is 170
    // of the 200 wide-gamut files in a phone backup, so a path that needs `desc`
    // would be a path that does not work on real files.
    let info = heif(&heif_fixtures::heif_with_icc(&icc_profile(None, P3_RED, 0)));
    assert_eq!(info.space, ColorSpace::WideGamut);
    assert_eq!(info.description, "", "the fixture has no desc tag");
    assert_eq!(info.red_primaries, Some([P3_RED.0, P3_RED.1, P3_RED.2]));
    assert_eq!(info.data_space, "RGB");
    assert!(
        info.panel_line().is_some(),
        "a wide-gamut HEIF is exactly what the panel line exists for"
    );

    // A named sRGB profile behaves as it does everywhere else, including being
    // worth a confirming line: "checked, needs nothing" beats silence.
    let tagged =
        heif(&heif_fixtures::heif_with_icc(&icc_profile(Some("sRGB IEC61966-2.1"), SRGB_RED, 0)));
    assert_eq!(tagged.space, ColorSpace::Srgb);
    assert!(tagged.panel_line().is_some());
}

#[test]
fn test_heif_nclx_parameters_are_never_called_srgb() {
    // **The trap this fixture exists for.** `extract_icc` returns `None` when a
    // file states its colour with `nclx` parameters instead of a profile, which
    // is how a phone HEIC often says "Display P3". Reading that `None` as "no
    // profile, therefore sRGB" would report a wide-gamut photo as sRGB — the
    // exact failure the detector is for, arrived at from the opposite direction.
    let info = heif(&heif_fixtures::heif_with_nclx_display_p3());
    assert_eq!(
        info.space,
        ColorSpace::NotInspected,
        "nclx parameters are not a profile, and are certainly not sRGB"
    );
    assert_ne!(info.space, ColorSpace::Srgb);
    assert_ne!(info.space, ColorSpace::UntaggedSrgb);
    assert_eq!(info.profile_bytes, 0);
}

#[test]
fn test_a_corrupt_heif_claims_nothing_about_its_colour() {
    // `extract_icc` returns `Err` for a container it cannot parse. Every entry
    // point into a file has to survive that: the whole EXIF panel is built from
    // untrusted bytes, and a decode error here must not become a claim.
    let good = heif_fixtures::heif_with_icc(&icc_profile(Some("Display P3"), P3_RED, 0));

    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        // Deliberately no recognised magic: GIF magic would make this a GIF
        // by the detector's magic-based contract (see
        // `test_gif_has_no_profile_chunk_so_it_is_untagged_srgb`), which is a
        // claim about a format, not about garbage.
        ("not a container at all", b"XXXX and then some".to_vec()),
        ("truncated inside ftyp", good[..12].to_vec()),
        ("truncated inside meta", good[..good.len() - 40].to_vec()),
    ];

    for (what, bytes) in cases {
        let info = heif(&bytes);
        assert_eq!(info.space, ColorSpace::NotInspected, "{what}: reported {:?}", info.space);
        assert!(
            matches!(info.space, ColorSpace::NotInspected | ColorSpace::Unknown),
            "{what}: a container we cannot parse is not evidence of any space"
        );
    }
}

#[test]
fn test_scan_colour_audits_a_whole_folder() {
    // What `--icc-scan` runs. Two properties matter and are easy to lose: it
    // covers every format the viewer opens (not just the one being worked on),
    // and it does not audit things that are not images.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // Natural order is the contract, and these names are chosen so a lexical
    // sort would produce a different answer (`IMG_10` before `IMG_2`).
    for (name, bytes) in [
        ("IMG_10.jpg", jpeg_with_icc(16, 16, &icc_profile(None, P3_RED, 0))),
        ("IMG_2.jpg", jpeg_with_icc(16, 16, &icc_profile(Some("sRGB IEC61966-2.1"), SRGB_RED, 0))),
        ("b.heic", heif_fixtures::heif_with_nclx_display_p3()),
    ] {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    // Not an image, and not an extension the viewer claims: it must not appear.
    std::fs::write(dir.join("notes.txt"), b"not an image").unwrap();
    std::fs::write(dir.join("raw.cbz"), b"PK\x03\x04").unwrap();

    let found = image_viewer::scan_colour(dir);
    let names: Vec<String> =
        found.iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(
        names,
        vec!["b.heic", "IMG_2.jpg", "IMG_10.jpg"],
        "natural order (`IMG_2` before `IMG_10`, case-insensitively), and nothing \
         that is not a supported image"
    );

    let space_of = |name: &str| {
        found.iter().find(|(p, _)| p.file_name().unwrap() == name).map(|(_, i)| i.space).unwrap()
    };
    assert_eq!(space_of("IMG_2.jpg"), ColorSpace::Srgb);
    assert_eq!(space_of("IMG_10.jpg"), ColorSpace::WideGamut);
    assert_eq!(space_of("b.heic"), ColorSpace::NotInspected);

    // An empty or missing folder is empty, not an error: the command prints
    // "no images" and exits rather than failing.
    assert!(image_viewer::scan_colour(&dir.join("does-not-exist")).is_empty());
}
