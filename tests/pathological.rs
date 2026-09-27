//! Pathological-input performance measurements.
//!
//! Run explicitly, since generating and decoding 40 MP images is slow:
//!
//! ```text
//! cargo test --release --test pathological -- --ignored --nocapture
//! ```
//!
//! These are measurements, not assertions: they print a table and only fail on
//! gross regressions. The point is to find out where the time actually goes
//! before optimising anything.

use image::{ImageFormat, RgbImage};
use std::time::Instant;

/// Smooth gradient + high-frequency detail: compresses like a photo, so the
/// decoder cannot cheat, and it exercises the resampler's edge handling.
fn synth(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let fx = x as f32 / w as f32;
            let fy = y as f32 / h as f32;
            let checker = ((x / 7) + (y / 7)) % 2 == 0;
            let base = (fx * 255.0) as u8;
            let g = (fy * 255.0) as u8;
            let b = if checker { 255 - base } else { base / 2 };
            img.put_pixel(x, y, image::Rgb([base, g, b]));
        }
    }
    img
}

fn write(dir: &std::path::Path, name: &str, img: &RgbImage, fmt: ImageFormat) -> std::path::PathBuf {
    let p = dir.join(name);
    let mut bytes = Vec::new();
    match fmt {
        ImageFormat::Jpeg => {
            // 90: big enough to be a real file, small enough to keep in RAM here.
            let rgb = image::DynamicImage::ImageRgb8(img.clone());
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90);
            enc.encode_image(&rgb).unwrap();
        }
        f => img.write_to(&mut std::io::Cursor::new(&mut bytes), f).unwrap(),
    }
    std::fs::write(&p, &bytes).unwrap();
    p
}

fn report(label: &str, mp: f64, total: Duration_, extra: &str) {
    println!(
        "  {:<28} {:>5.0}MP  total {:>8.1}ms  {}",
        label,
        mp,
        total.as_secs_f64() * 1000.0,
        extra
    );
}

type Duration_ = std::time::Duration;

#[test]
#[ignore = "slow: generates and decodes multi-megapixel images"]
fn measure_large_image_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();

    // ~40 MP: 8200x4900.
    let (w, h) = (8200u32, 4900u32);
    let mp = (w * h) as f64 / 1e6;
    println!("\n== preview path, {mp:.0} MP source ==");

    let img = synth(w, h);

    for (label, name, fmt) in [
        ("JPEG q90", "big.jpg", ImageFormat::Jpeg),
        ("PNG (lossless)", "big.png", ImageFormat::Png),
    ] {
        let t = Instant::now();
        let path = write(dir, name, &img, fmt);
        let write_ms = t.elapsed().as_secs_f64() * 1000.0;
        let mb = std::fs::metadata(&path).unwrap().len() as f64 / 1e6;

        // What the viewer actually asks for: a display-sized preview.
        let t = Instant::now();
        let r = image_viewer::load_image_preview_full(&path, 1920)
            .unwrap_or_else(|e| panic!("{label}: {e}"));
        let total = t.elapsed();
        let pv = &r.preview;
        let below = r.below_target;
        let full = r.full.is_some();
        report(
            label,
            mp,
            total,
            &format!(
                "file {mb:.1}MB (encode {write_ms:.0}ms) -> preview {}x{} ({}ms internal) below_target={below} full_kept={full}",
                pv.width,
                pv.height,
                pv.load_time.as_millis()
            ),
        );
    }

    // The downscale in isolation, on the full-resolution buffer: this is the
    // part the viewer runs on the worker while the user waits.
    println!("\n== downscale in isolation (linear-light path) ==");
    let data = image_viewer::load_image(&dir.join("big.png"), 16384).unwrap();
    for target in [1920u32, 4096] {
        let t = Instant::now();
        let small = image_viewer::downscale_image(&data, target);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  {}x{} -> {}x{}  {:>8.1}ms   ({:.1} MP/s)",
            data.width,
            data.height,
            small.width,
            small.height,
            ms,
            (data.width as f64 * data.height as f64 / 1e6) / (ms / 1000.0)
        );
    }
}

#[test]
#[ignore = "slow: generates and decodes 60 MP images"]
fn measure_sixty_megapixel() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let (w, h) = (9500u32, 6400u32); // ~60.8 MP
    let mp = (w * h) as f64 / 1e6;
    println!("\n== {mp:.0} MP JPEG ==");
    let img = synth(w, h);
    let path = write(dir, "huge.jpg", &img, ImageFormat::Jpeg);
    let mb = std::fs::metadata(&path).unwrap().len() as f64 / 1e6;
    let t = Instant::now();
    let r = image_viewer::load_image_preview_full(&path, 1920).unwrap();
    report(
        "JPEG q90",
        mp,
        t.elapsed(),
        &format!(
            "file {mb:.1}MB -> preview {}x{} ({}ms) below_target={}",
            r.preview.width,
            r.preview.height,
            r.preview.load_time.as_millis(),
            r.below_target
        ),
    );
}
