//! The application icon: a rounded "photo" badge with a sky, a sun and two
//! mountains.
//!
//! It is drawn here rather than kept as a committed binary so the artwork is
//! reviewable as code, and so its *properties* can be asserted. The one that
//! matters most is [`coverage`]: the first version of this drew a badge with a
//! half-extent of 0.22 in a 1×1 canvas, so the icon occupied 44% of its own
//! bitmap and read as a small stamp floating in empty space. Nothing about that
//! is visible in a diff and nothing about it is obvious from the code, so it is
//! now a number a test checks.
//!
//! Prose about coordinates: canvas coordinates are 0..1 with y down, and the
//! artwork inside the badge is drawn in *local* coordinates (-1..1, also y
//! down) so the badge can be resized without anything escaping it.

use image::{Rgba, RgbaImage};

/// Sizes Explorer asks for. 16 is the one that matters most — it is what a file
/// list shows — and 256 is the one that has to look deliberate.
pub const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Half-extent of the badge, as a fraction of the canvas. 0.5 would touch the
/// edges; 0.47 leaves a hair of breathing room so the corners do not clip.
pub const HALF: f32 = 0.47;

/// Centre of the sun, in local coordinates.
const SUN_AT: (f32, f32) = (0.45, -0.45);
/// Radius of the sun, in local coordinates.
const SUN_R: f32 = 0.20;

/// Canvas position (0..1) of a point given in local coordinates (-1..1).
///
/// Exposed so a test can ask *where* something should land. Two coordinate
/// bugs here are invisible in a diff and obvious only in a render: a badge drawn
/// too small, and local coordinates that do not actually span -1..1 (which
/// squeezes the artwork inboard and pushes the sun under the rounded corner).
pub fn canvas_of(lx: f32, ly: f32) -> (f32, f32) {
    (0.5 + lx * HALF, 0.5 + ly * HALF)
}

/// Corner radius, in the same units as [`HALF`]. Around 22% of the width is the
/// modern squircle-ish app-icon proportion.
const RADIUS: f32 = 0.21;

/// Width of the darker rim, as a fraction of the canvas. It exists so the badge
/// has an edge against a light Explorer background as well as a dark one.
const RIM: f32 = 0.014;

/// Render at this multiple and box-filter down, which is what gives the curves
/// clean edges. Cheap (the canvas is tiny) and much easier to get right than
/// analytic antialiasing.
const SUPERSAMPLE: u32 = 4;

/// Sky gradient, top then bottom.
const SKY_TOP: [u8; 3] = [0x2D, 0x9C, 0xEB];
const SKY_BOTTOM: [u8; 3] = [0xC9, 0xED, 0xFF];
const SUN: [u8; 3] = [0xFF, 0xC5, 0x31];
const RIDGE_BACK: [u8; 3] = [0x6E, 0x88, 0xA6];
const RIDGE_FRONT: [u8; 3] = [0x2E, 0x42, 0x57];
const RIM_COLOUR: [u8; 3] = [0x16, 0x32, 0x4A];

/// One pixel of the icon, in canvas coordinates (0..1, y down).
fn shade(x: f32, y: f32) -> Option<Rgba<u8>> {
    let d = sd_round_rect(x, y, 0.5, 0.5, HALF, RADIUS);
    if d > 0.0 {
        return None; // outside the badge: transparent
    }
    if d > -RIM {
        return Some(Rgba([RIM_COLOUR[0], RIM_COLOUR[1], RIM_COLOUR[2], 255]));
    }

    // Local coordinates: -1..1 across the badge's interior. Dividing by HALF and
    // not by the badge *width* is the whole point — 2*HALF is 0.94, so dividing
    // by it gives a range of only -0.53..0.53 and quietly squeezes the artwork
    // into the middle, which is what put the sun half outside the corner.
    let lx = (x - 0.5) / HALF;
    let ly = (y - 0.5) / HALF;

    // Sky.
    let t = ((ly + 1.0) * 0.5).clamp(0.0, 1.0);
    let mut px = [
        lerp(SKY_TOP[0], SKY_BOTTOM[0], t),
        lerp(SKY_TOP[1], SKY_BOTTOM[1], t),
        lerp(SKY_TOP[2], SKY_BOTTOM[2], t),
    ];

    // Sun, upper right and clear of the rounded corner.
    let (dx, dy) = (lx - SUN_AT.0, ly - SUN_AT.1);
    if dx * dx + dy * dy < SUN_R * SUN_R {
        px = SUN;
    }
    // Two ridges, back then front. Each is the region under a peak, so the
    // slopes are straight and exact. The back one carries the left of the
    // frame and the front one the right, which is what stops the composition
    // reading as one grey mass with a nub beside it.
    if under_peak(lx, ly, -0.35, 0.05, 0.75) {
        px = RIDGE_BACK;
    }
    if under_peak(lx, ly, 0.30, 0.42, 0.95) {
        px = RIDGE_FRONT;
    }
    Some(Rgba([px[0], px[1], px[2], 255]))
}

fn lerp(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round().clamp(0.0, 255.0) as u8
}

/// Is `(lx, ly)` inside the triangle formed by a peak and the bottom edge?
/// `slope` is the horizontal run per unit of rise.
fn under_peak(lx: f32, ly: f32, peak_x: f32, peak_y: f32, slope: f32) -> bool {
    if ly < peak_y {
        return false;
    }
    (lx - peak_x).abs() <= (ly - peak_y) * slope
}

/// Signed distance to a rounded rectangle, negative inside.
fn sd_round_rect(px: f32, py: f32, cx: f32, cy: f32, half: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (half - r);
    let qy = (py - cy).abs() - (half - r);
    let (ax, ay) = (qx.max(0.0), qy.max(0.0));
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
}

/// The icon at `size`×`size`, antialiased.
pub fn render(size: u32) -> RgbaImage {
    let big = size * SUPERSAMPLE;
    let mut hi = RgbaImage::new(big, big);
    for py in 0..big {
        for px in 0..big {
            let x = (px as f32 + 0.5) / big as f32;
            let y = (py as f32 + 0.5) / big as f32;
            hi.put_pixel(px, py, shade(x, y).unwrap_or(Rgba([0, 0, 0, 0])));
        }
    }
    image::imageops::resize(&hi, size, size, image::imageops::FilterType::Triangle)
}

/// Fraction of the bitmap that is actually drawn, 0.0..=1.0.
///
/// This is the number that was wrong. A badge smaller than the canvas is not a
/// subtle aesthetic problem: Windows scales the icon to fit, so the visible
/// result is an icon that looks like it was shrunk, with transparent padding
/// eating most of the space in a file list.
pub fn coverage(size: u32) -> f32 {
    let img = render(size);
    let opaque = img.pixels().filter(|p| p.0[3] > 8).count();
    opaque as f32 / (size * size) as f32
}

/// The whole `.ico`, with every size in [`SIZES`].
///
/// Each entry is PNG-compressed. That is a Vista+ extension to the format, but
/// Explorer, the taskbar and the Alt-Tab switcher on any Windows this runs on
/// handle it, and it is both smaller and sharper than a hand-built BMP (no
/// 32-bit BGRA rows with a 1-bit AND mask to get right).
pub fn build_ico() -> Vec<u8> {
    let entries: Vec<(u32, RgbaImage)> = SIZES.iter().map(|&s| (s, render(s))).collect();

    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type 1 = icon
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    let mut payloads = Vec::with_capacity(entries.len());
    let mut offset = 6 + entries.len() * 16;
    for (size, img) in &entries {
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encoding an icon entry");

        // 0 means 256 in this one-byte field.
        let dim = if *size >= 256 { 0u8 } else { *size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]); // dims, palette, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
        payloads.push(png);
    }
    for p in payloads {
        out.extend_from_slice(&p);
    }
    out
}

/// Write `assets/app.ico`, plus a contact sheet so the result can be looked at
/// rather than trusted.
pub fn write_assets(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("app.ico"), build_ico())?;

    // A strip of every size on a neutral background, largest last.
    let pad = 16u32;
    let h = 256 + pad * 2;
    let w = SIZES.iter().map(|s| s + pad).sum::<u32>() + pad;
    let mut sheet = RgbaImage::new(w, h);
    for b in sheet.pixels_mut() {
        *b = Rgba([40, 44, 52, 255]);
    }
    let mut x = pad;
    for &s in &SIZES {
        image::imageops::overlay(&mut sheet, &render(s), x as i64, (pad + 256 - s) as i64);
        x += s + pad;
    }
    let _ = sheet.save(dir.join("app-icon-preview.png"));
    Ok(())
}
