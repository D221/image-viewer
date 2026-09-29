//! Generate `assets/app.ico`, the icon embedded in the exe.
//!
//! Run with `cargo run --release --example make_icon`.
//!
//! It is a generator rather than a committed binary blob so the artwork is
//! reviewable and adjustable: the icon is a small piece of procedural drawing,
//! not an opaque asset, and anyone can change one number and re-run it.
//!
//! Windows needs the icon *inside the exe* for Explorer to have anything to
//! show — an `.ico` sitting next to the binary is not consulted. So this writes
//! the file, and `build.rs` embeds it via `winresource`.
//!
//! The artwork is deliberately a plain "a picture" glyph: it has to stay legible
//! at 16 px in a file list, where detail is not available.

use image::{Rgba, RgbaImage};

/// Sizes Explorer asks for. 16 is the one that matters most — it is what a file
/// list shows — and 256 is the one that has to look deliberate.
const SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Render at 4x and box-filter down, which is what gives the curves clean edges.
/// Cheap here (the canvas is tiny) and much easier to get right than analytic
/// antialiasing.
const SS: u32 = 4;

fn main() {
    let out_dir = std::path::Path::new("assets");
    std::fs::create_dir_all(out_dir).expect("creating assets/");

    let mut entries = Vec::new();
    for &s in SIZES.iter() {
        entries.push((s, render(s).into_raw()));
    }
    std::fs::write(out_dir.join("app.ico"), build_ico(&entries)).expect("writing app.ico");

    // A contact sheet, so the result can be looked at rather than trusted.
    let mut sheet = RgbaImage::new(SIZES.iter().map(|s| s + 16).sum::<u32>() + 16, 272);
    for b in sheet.pixels_mut() {
        *b = Rgba([40, 44, 52, 255]);
    }
    let mut x = 16u32;
    for (s, _) in &entries {
        let img = render(*s);
        image::imageops::overlay(&mut sheet, &img, x as i64, (272 - s - 16) as i64);
        x += s + 16;
    }
    sheet.save(out_dir.join("app-icon-preview.png")).ok();
    println!("wrote assets/app.ico ({} sizes) and a preview PNG", entries.len());
}

/// Normalised coordinates: 0..1 across the canvas, so every size is the same
/// drawing.
fn shade(x: f32, y: f32) -> Option<Rgba<u8>> {
    // Outside the rounded square: transparent.
    let d = sd_round_rect(x, y, 0.5, 0.5, 0.22, 0.045);
    if d > 0.0 {
        return None;
    }

    // A thin darker rim, so the badge has an edge against a light Explorer
    // background as well as a dark one.
    let rim = (-0.022..0.0).contains(&d);
    if rim {
        return Some(Rgba([30, 41, 59, 255]));
    }

    // Sky gradient.
    let t = y.clamp(0.0, 1.0);
    let sky = [
        (125.0 + 60.0 * t) as u8,
        (211.0 + 20.0 * t) as u8,
        (252.0) as u8,
    ];

    // Sun.
    let sun = (x - 0.705).powi(2) + (y - 0.315).powi(2);
    if sun < 0.0085 * 0.0085 * 25.0 {
        return Some(Rgba([251, 191, 36, 255]));
    }

    // Two mountains, back then front. Each is the intersection of two
    // half-planes under its peak, so the slopes are exact.
    if under_peak(x, y, 0.375, 0.44, 0.62) {
        return Some(Rgba([100, 116, 139, 255]));
    }
    if under_peak(x, y, 0.645, 0.60, 0.50) {
        return Some(Rgba([51, 65, 85, 255]));
    }
    Some(Rgba([sky[0], sky[1], sky[2], 255]))
}

/// Is `(x, y)` inside the triangle formed by a peak and the bottom edge?
/// `peak` is the apex; `slope` is the horizontal run per unit of rise.
fn under_peak(x: f32, y: f32, peak_x: f32, peak_y: f32, slope: f32) -> bool {
    if y < peak_y {
        return false;
    }
    let half = (y - peak_y) * slope;
    (x - peak_x).abs() <= half
}

/// Signed distance to a rounded rectangle, negative inside.
fn sd_round_rect(px: f32, py: f32, cx: f32, cy: f32, half: f32, r: f32) -> f32 {
    let qx = (px - cx).abs() - (half - r);
    let qy = (py - cy).abs() - (half - r);
    let (ax, ay) = (qx.max(0.0), qy.max(0.0));
    (ax * ax + ay * ay).sqrt() + qx.max(qy).min(0.0) - r
}

fn render(size: u32) -> RgbaImage {
    let big = size * SS;
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

/// Pack RGBA images into a single-icon `.ico`.
///
/// Every entry is PNG-compressed. That is a Vista+ extension to the format, but
/// Explorer, the taskbar and the Alt-Tab switcher on any Windows this will run on
/// handle it, and it is both smaller and sharper than a hand-built BMP
/// (no 32-bit BGRA rows with a 1-bit AND mask to get right).
fn build_ico(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type 1 = icon
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());

    // Directory entries first, so the offsets are known before the payloads.
    let mut dir_end = 6 + entries.len() * 16;
    let mut payloads = Vec::new();
    for &(size, ref rgba) in entries {
        let mut png = Vec::new();
        let img = RgbaImage::from_raw(size, size, rgba.clone()).unwrap();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .expect("encoding an icon entry");

        // 0 means 256 in this field, which is a size field of one byte.
        let dim = if size >= 256 { 0u8 } else { size as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]); // dims, palette, reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(dir_end as u32).to_le_bytes());
        dir_end += png.len();
        payloads.push(png);
    }
    for p in payloads {
        out.extend_from_slice(&p);
    }
    out
}
