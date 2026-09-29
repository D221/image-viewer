//! How fast is a lossless save, and what does it cost on disk?
//!
//! Two numbers matter and neither is obvious beforehand:
//!
//!  * **Time.** `tjTransform` works on DCT coefficients and never fully
//!    decompresses, so it should be markedly cheaper than the obvious
//!    alternative of decoding the image, turning the pixels and re-encoding —
//!    which is also the only way to do it for a non-JPEG.
//!  * **Size.** If this were a re-encode, the file would come out a different
//!    size — usually smaller at the same nominal quality, because libjpeg
//!    spends a fresh rate-distortion decision on every block. A lossless
//!    transform moves the blocks around and leaves the entropy-coded data
//!    alone, so the size should land very close to the original. That is the
//!    load-bearing number: it is what "lossless" actually means here, and it is
//!    not something the code can assert about itself.
//!
//! `#[ignore]`d, like the other benches: it builds a large image and prints a
//! report. Correctness lives in `rotate_save.rs`.

use image_viewer::save_rotated;
use std::time::Instant;

/// A photographic-ish image: smooth gradients plus some structure, because a
/// flat block compresses to nothing and would make every timing meaningless.
fn photo(w: u32, h: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
        let fx = x as f32 / w as f32;
        let fy = y as f32 / h as f32;
        let r = 128.0 + 100.0 * (fx * 9.0).sin() * (fy * 4.0).cos();
        let g = 90.0 + 80.0 * (fx * 3.0).cos() + 40.0 * (fy * 11.0).sin();
        let b = 140.0 + 70.0 * (fx * 7.0).sin() * (fy * 7.0).sin();
        image::Rgb([r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8])
    }))
}

fn write_jpeg(path: &std::path::Path, img: &image::DynamicImage, q: u8) {
    let mut bytes = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, q);
    enc.encode_image(&img.to_rgb8()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn mb(n: u64) -> f64 {
    n as f64 / 1048576.0
}

#[test]
#[ignore = "builds a large image and prints a report"]
fn lossless_save_vs_the_obvious_alternative() {
    let tmp = tempfile::tempdir().unwrap();
    eprintln!("\n  lossless save (tjTransform) vs decode + turn pixels + re-encode\n");

    println!(
        "  {:<14} {:>10} {:>9} {:>10} {:>10} {:>8} {:>7}",
        "image", "in", "lossless", "re-encode", "size in", "size out", "ratio"
    );

    for (w, h) in [(1920u32, 1080u32), (4000, 3000), (6000, 4000)] {
        let img = photo(w, h);
        let p = tmp.path().join(format!("src_{w}x{h}.jpg"));
        write_jpeg(&p, &img, 90);
        let in_bytes = std::fs::metadata(&p).unwrap().len();

        // The real thing. Rotate to a non-identity transform so it does work.
        let t = Instant::now();
        let dest = save_rotated(&p, 1).unwrap();
        let lossless_ms = t.elapsed().as_secs_f64() * 1000.0;
        let out_bytes = std::fs::metadata(&dest).unwrap().len();

        // The obvious alternative, for comparison: what a second generation
        // costs. This is what the JPEG path avoids entirely.
        let t = Instant::now();
        let dec = image::open(&p).unwrap();
        let turned = image::imageops::rotate90(&dec.to_rgb8());
        let mut re = Vec::new();
        image::DynamicImage::ImageRgb8(turned)
            .write_to(
                &mut std::io::Cursor::new(&mut re),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        let reencode_ms = t.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "  {:<14} {:>8.1}MB {:>8.0}ms {:>8.0}ms {:>8.1}MB {:>8.1}MB {:>6.2}x",
            format!("{w}x{h}"),
            mb(in_bytes),
            lossless_ms,
            reencode_ms,
            mb(in_bytes),
            mb(out_bytes),
            out_bytes as f64 / in_bytes as f64,
        );
    }
    eprintln!("\n  size ratio ~1.00 is the lossless claim: no re-compression happened.");
    eprintln!("  a re-encode would land well under 1.00 (or move a lot) at the same quality.\n");
}
