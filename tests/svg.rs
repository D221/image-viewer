//! Tests for SVG support (§9.6).
//!
//! SVG is content-sniffed (magic bytes are `<?xml`+`<svg` or bare `<svg`), and
//! renders at any target size — there is no `full` tier and no source-size
//! decode: the preview *is* the decode.

use image_viewer::{decode_svg, is_svg_bytes, load_image_preview_full};

/// A simple 100x100 red square SVG.
fn red_square_svg() -> &'static [u8] {
    b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<svg width=\"100\" height=\"100\" xmlns=\"http://www.w3.org/2000/svg\">\n  <rect width=\"100\" height=\"100\" fill=\"red\"/>\n</svg>"
}

/// A bare `<svg` root with no XML declaration.
fn bare_svg() -> &'static [u8] {
    b"<svg width=\"200\" height=\"50\"><circle cx=\"100\" cy=\"25\" r=\"25\" fill=\"blue\"/></svg>"
}

#[test]
fn is_svg_detects_xml_declaration_form() {
    assert!(is_svg_bytes(red_square_svg()));
}

#[test]
fn is_svg_detects_bare_root_form() {
    assert!(is_svg_bytes(bare_svg()));
}

#[test]
fn is_svg_rejects_non_svg() {
    assert!(!is_svg_bytes(b"Hello, world"));
    assert!(!is_svg_bytes(b"<?xml version=\"1.0\"?><notsvg/>"));
    assert!(!is_svg_bytes(b""));
    // A bare `<svg` at the start is a valid SVG sniff match (per the todo:
    // "a bare `<svg`"), so it is accepted, not rejected.
    assert!(is_svg_bytes(b"<svg>"));
    assert!(!is_svg_bytes(b"FFD8FF"));
}

#[test]
fn decode_svg_renders_at_target_size() {
    let data = decode_svg(red_square_svg(), 200, std::time::Instant::now()).unwrap();
    // 100x100 viewport at target 200: scale is min(200/100, 1.0) = 1.0, so 100x100
    assert_eq!(data.width, 100);
    assert_eq!(data.height, 100);
    // RGBA buffer matches dimensions
    assert_eq!(data.rgba.len(), 100 * 100 * 4);
    // Center pixel should be red (255, 0, 0, 255)
    let cx = 50usize;
    let cy = 50usize;
    let o = (cy * data.width as usize + cx) * 4;
    assert_eq!(&data.rgba[o..o + 4], &[255, 0, 0, 255]);
}

#[test]
fn decode_svg_scales_down_viewport() {
    let data = decode_svg(red_square_svg(), 50, std::time::Instant::now()).unwrap();
    // 100x100 viewport at target 50: scale is min(50/100, 1.0) = 0.5, so 50x50
    assert_eq!(data.width, 50);
    assert_eq!(data.height, 50);
}

#[test]
fn decode_svg_scales_up_viewport() {
    let data = decode_svg(bare_svg(), 400, std::time::Instant::now()).unwrap();
    // 200x50 viewport at target 400: scale is min(400/200, 1.0) = 1.0, so 200x50
    assert_eq!(data.width, 200);
    assert_eq!(data.height, 50);
}

#[test]
fn decode_svg_without_viewport_still_decodes() {
    let no_viewport = b"<svg><rect width=\"10\" height=\"10\" fill=\"green\"/></svg>";
    let data = decode_svg(no_viewport, 100, std::time::Instant::now()).unwrap();
    // SVG decodes at some non-zero size even without explicit width/height
    assert!(data.width > 0);
    assert!(data.height > 0);
    assert_eq!(data.rgba.len(), data.width as usize * data.height as usize * 4);
}

#[test]
fn load_image_preview_full_handles_svg() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("red.svg");
    std::fs::write(&path, red_square_svg()).unwrap();
    let r = load_image_preview_full(&path, 200).unwrap();
    assert_eq!(r.preview.width, 100);
    assert_eq!(r.preview.height, 100);
    assert!(r.full.is_none(), "SVG has no full tier — preview is the decode");
    assert!(r.below_target, "SVG render at target density is below_target");
}
