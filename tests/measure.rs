use image_viewer::{set_downscale_filter, DownscaleFilter, load_image_preview_full};

#[test]
fn repro_exact_case() {
    set_downscale_filter(DownscaleFilter::Hermite);
    let tmp = tempfile::tempdir().unwrap();
    // The two failing files were 621x974 and 472x487 (from the epaint assert
    // arithmetic). Try every target around plausible display sizes.
    for (w, h) in [(621u32, 974u32), (472, 487), (974, 621), (487, 472)] {
        let img = image::DynamicImage::new_rgba8(w, h);
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let p = tmp.path().join(format!("{w}x{h}.png"));
        std::fs::write(&p, &png).unwrap();
        for t in [256u32, 512, 1000, 1500, 2000, 2560, 3200, 4096] {
            let r = load_image_preview_full(&p, t).unwrap();
            let expect = r.preview.width as usize * r.preview.height as usize * 4;
            assert_eq!(expect, r.preview.rgba.len(), "{w}x{h} target {t}");
        }
    }
    println!("exact-case repro passed");
}
