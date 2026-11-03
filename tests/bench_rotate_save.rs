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

use image_viewer::{Crop, Xform, save_transformed};
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
        image::Rgb([
            r.clamp(0.0, 255.0) as u8,
            g.clamp(0.0, 255.0) as u8,
            b.clamp(0.0, 255.0) as u8,
        ])
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

/// A crop taken together with a turn, which is **two** `tjTransform` calls.
///
/// Worth its own number because the one-call version does not work: TurboJPEG
/// reads the crop region in the transform's destination space, and refuses it
/// outright for the four axis-swapping operations. So this measures what the
/// workaround actually costs, and — more importantly — whether the size ratio
/// survives a second pass over the coefficients. It does: both calls rearrange
/// DCT blocks and neither decompresses to pixels, so there is still no second
/// generation, and the ratio is the only evidence for that.
#[test]
#[ignore = "builds a large image and prints a report"]
fn cropped_save_is_still_lossless() {
    let tmp = tempfile::tempdir().unwrap();
    eprintln!("\n  crop + turn as two lossless passes (vs one for a bare turn)\n");
    println!(
        "  {:<28} {:>10} {:>9} {:>9} {:>10} {:>8} {:>7}",
        "image", "crop", "crop+turn", "turn only", "size in", "size out", "ratio"
    );

    for (w, h) in [(1920u32, 1080u32), (4000, 3000), (6000, 4000)] {
        let p = tmp.path().join(format!("src_{w}x{h}.jpg"));
        write_jpeg(&p, &photo(w, h), 90);
        let in_bytes = std::fs::metadata(&p).unwrap().len();

        // Block-aligned, so MCU trimming is a no-op and the size ratio is
        // measuring losslessness rather than a few trimmed pixels.
        let crop = Crop::new(w / 4, h / 4, w / 2, h / 2).unwrap();

        let t = Instant::now();
        let crop_only = save_transformed(&p, Xform::Identity, Some(crop)).unwrap();
        let crop_ms = t.elapsed().as_secs_f64() * 1000.0;
        let crop_bytes = std::fs::metadata(&crop_only).unwrap().len();

        let t = Instant::now();
        let both = save_transformed(&p, Xform::Rot90, Some(crop)).unwrap();
        let both_ms = t.elapsed().as_secs_f64() * 1000.0;
        let both_bytes = std::fs::metadata(&both).unwrap().len();

        let t = Instant::now();
        // Only its running time is reported here; the bytes are the crop+turn
        // pair's, which is what the ratio column uses.
        let _turn_only = save_transformed(&p, Xform::Rot90, None).unwrap();
        let turn_ms = t.elapsed().as_secs_f64() * 1000.0;

        eprintln!(
            "  {:<28} {:>8.0}ms {:>8.0}ms {:>8.0}ms {:>8.1}MB {:>8.1}MB {:>6.2}x",
            format!("{w}x{h} {}x{}", crop.w, crop.h),
            crop_ms,
            both_ms,
            turn_ms,
            mb(in_bytes),
            mb(both_bytes),
            both_bytes as f64 / crop_bytes as f64,
        );
    }
    eprintln!(
        "\n  The ratio column is crop+turn output over crop-only output, so it answers\n  \
         whether the second pass cost anything in size. ~1.00 means the turn moved\n  \
         coefficients and left the entropy-coded data alone, which is what \"lossless\"\n  \
         means here. A re-encode would land well under 1.00 at the same quality.\n"
    );
}

#[test]
#[ignore = "builds a large image and prints a report"]
fn lossless_save_vs_the_obvious_alternative() {
    let tmp = tempfile::tempdir().unwrap();
    eprintln!("\n  lossless save (tjTransform) vs decode + turn pixels + re-encode\n");

    println!(
        "  {:<20} {:>10} {:>9} {:>10} {:>10} {:>8} {:>7}",
        "image", "in", "lossless", "re-encode", "size in", "size out", "ratio"
    );

    for (w, h) in [(1920u32, 1080u32), (4000, 3000), (6000, 4000)] {
        let img = photo(w, h);
        let p = tmp.path().join(format!("src_{w}x{h}.jpg"));
        write_jpeg(&p, &img, 90);
        let in_bytes = std::fs::metadata(&p).unwrap().len();

        // The obvious alternative, for comparison: what a second generation
        // costs. This is what the JPEG path avoids entirely.
        let t = Instant::now();
        let dec = image::open(&p).unwrap();
        let turned = image::imageops::rotate90(&dec.to_rgb8());
        let mut re = Vec::new();
        image::DynamicImage::ImageRgb8(turned)
            .write_to(&mut std::io::Cursor::new(&mut re), image::ImageFormat::Jpeg)
            .unwrap();
        let reencode_ms = t.elapsed().as_secs_f64() * 1000.0;

        // The real thing, for every transform that is not the identity. Mirrors
        // are in here because the question "did adding them cost anything?" has a
        // number, and because the size ratio is the lossless claim for them too —
        // nothing about a reflection makes it cheaper or dearer than a turn, and
        // that is worth seeing rather than assuming.
        for xform in Xform::ALL {
            if xform == Xform::Identity {
                continue;
            }
            let t = Instant::now();
            let dest = save_transformed(&p, xform, None).unwrap();
            let lossless_ms = t.elapsed().as_secs_f64() * 1000.0;
            let out_bytes = std::fs::metadata(&dest).unwrap().len();

            eprintln!(
                "  {:<20} {:>8.1}MB {:>8.0}ms {:>8.0}ms {:>8.1}MB {:>8.1}MB {:>6.2}x",
                format!("{w}x{h} {}", xform.label()),
                mb(in_bytes),
                lossless_ms,
                reencode_ms,
                mb(in_bytes),
                mb(out_bytes),
                out_bytes as f64 / in_bytes as f64,
            );
        }
    }
    eprintln!("\n  size ratio ~1.00 is the lossless claim: no re-compression happened.");
    eprintln!("  a re-encode would land well under 1.00 (or move a lot) at the same quality.\n");
}
