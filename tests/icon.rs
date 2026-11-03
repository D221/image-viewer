//! The application icon's *properties*.
//!
//! All three of these exist because the icon was wrong in a way nothing else
//! would have caught. The artwork is generated code, so it compiles, it embeds,
//! `ExtractIconExW` finds it, and the registration test passes — while looking
//! like a stamp floating in an empty square. Neither bug is visible in a diff
//! and neither is obvious from reading the code, which is exactly the kind of
//! thing that has to become a number.

use image_viewer::icon::{self, SIZES};

#[test]
fn the_badge_fills_the_bitmap() {
    // The original drew the rounded rect with a half-extent of 0.22, so the icon
    // occupied 44% of its own bitmap. Windows scales an icon to fit its box, so
    // the visible result was an icon that looked shrunk rather than an icon that
    // was clearly the wrong size.
    let c = icon::coverage(256);
    assert!(
        c > 0.80,
        "the icon fills only {c:.0}% of a 256px bitmap - it will look like a small \
         stamp in a large empty box. HALF is {}.",
        icon::HALF
    );
    // The same check at the size that actually matters, a file list.
    assert!(icon::coverage(16) > 0.85, "16px icon fills only {:.0}%", icon::coverage(16));
}

#[test]
fn every_size_fills_most_of_its_bitmap() {
    // A smaller bitmap is filled *more*, because the corner radius is a fixed
    // fraction and so takes proportionally less off the corners. A size that
    // breaks that pattern is a rendering bug, not a rounding artefact.
    for &s in &SIZES {
        let c = icon::coverage(s);
        assert!(c > 0.80, "{s}px fills only {c:.0}% of its bitmap");
    }
}

#[test]
fn local_coordinates_really_span_minus_one_to_one() {
    // The second bug: the artwork is positioned in local coordinates, and the
    // scaling divided by the badge *width* (2*HALF = 0.94) instead of its
    // half-extent, so `lx` only reached ±0.53. Everything was squeezed into the
    // middle and the sun landed exactly where the rounded corner ate it.
    //
    // The sun is the sharpest probe: it is a fixed disc at a known local
    // position, so if the mapping is wrong it ends up somewhere else and this
    // pixel is not yellow.
    let img = icon::render(256);
    let (cx, cy) = icon::canvas_of(0.45, -0.45);
    let px = img.get_pixel((cx * 256.0).round() as u32, (cy * 256.0).round() as u32).0;
    assert_eq!(
        &px[..3],
        &[0xFF, 0xC5, 0x31],
        "the sun's centre is not the sun's colour, so the local-to-canvas mapping is wrong"
    );

    // And the badge edge really is near the bitmap edge, which is the first bug
    // restated in the same units.
    let edge = 0.5 - icon::HALF;
    assert!(
        edge < 0.05,
        "the badge starts {edge:.3} in from the edge, leaving a visible transparent margin"
    );
}

#[test]
fn both_ridges_reach_the_bottom_of_the_frame() {
    // The composition, not just the geometry. A version with the local scaling
    // wrong produced one huge grey mass and a dark nub beside it, because the
    // back ridge swallowed everything. Each ridge has to touch the bottom edge
    // somewhere, or the picture has a hole in it.
    let img = icon::render(256);
    // Near the bottom, but *inside* the badge. The badge spans local -1..1, so
    // the outermost rows and columns are transparent margin; sampling the very
    // last row finds nothing at all and looks like a missing ridge.
    let bottom = (256.0 * 0.95) as u32;
    assert!(img.get_pixel(128, bottom).0[3] > 8, "test setup: row {bottom} is outside the badge");
    let is = |x: u32, want: [u8; 3]| {
        let p = img.get_pixel(x, bottom).0;
        &p[..3] == &want
    };
    let back = (0..256).any(|x| is(x, [0x6E, 0x88, 0xA6]));
    let front = (0..256).any(|x| is(x, [0x2E, 0x42, 0x57]));
    assert!(back, "the back ridge never reaches the bottom edge");
    assert!(front, "the front ridge never reaches the bottom edge");
    // They must overlap horizontally, or the composition reads as two separate
    // shapes rather than one landscape.
    let back_x = (0..256).find(|&x| is(x, [0x6E, 0x88, 0xA6]));
    let front_x = (0..256).find(|&x| is(x, [0x2E, 0x42, 0x57]));
    assert!(
        (back_x.unwrap() as i32 - front_x.unwrap() as i32).abs() > 40,
        "the two ridges start at almost the same x, which makes one look like a \
         hairline on the other (back at {back_x:?}, front at {front_x:?})"
    );
}

#[test]
fn the_ico_holds_every_size_with_a_valid_directory() {
    let ico = icon::build_ico();
    assert_eq!(&ico[0..2], &[0, 0], "reserved");
    assert_eq!(&ico[2..4], &[1, 0], "type 1 = icon");
    assert_eq!(u16::from_le_bytes([ico[4], ico[5]]) as usize, SIZES.len(), "entry count");
    for (i, &size) in SIZES.iter().enumerate() {
        let e = 6 + i * 16;
        let dim = ico[e];
        // 0 means 256 in this one-byte field.
        assert_eq!(dim as u32, if size >= 256 { 0 } else { size }, "entry {i} width");
        assert_eq!(ico[e + 1] as u32, dim as u32, "entry {i} height");
        let len = u32::from_le_bytes(ico[e + 8..e + 12].try_into().unwrap()) as usize;
        let off = u32::from_le_bytes(ico[e + 12..e + 16].try_into().unwrap()) as usize;
        assert!(len > 0, "entry {i} is empty");
        assert!(off + len <= ico.len(), "entry {i} runs past the end of the file");
        // PNG-compressed entries start with the PNG signature.
        assert_eq!(&ico[off..off + 4], b"\x89PNG", "entry {i} is not a PNG payload");
    }
}

#[test]
fn the_icon_is_fully_opaque_in_the_middle() {
    // A sanity floor on the renderer itself: the centre of the badge must not
    // be transparent, whatever the artwork says.
    let img = icon::render(64);
    let c = img.get_pixel(32, 32).0;
    assert_eq!(c[3], 255, "the centre of the icon is not opaque");
}
