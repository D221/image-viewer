use image::{DynamicImage, Rgb, RgbImage};
use image_viewer::{load_image, Gallery};
use std::path::Path;
use std::sync::OnceLock;

/// A folder of generated images, built once per test binary.
///
/// This suite used to point at `test-images/`, which is gitignored: it passed on
/// the machine that wrote it and could not pass anywhere else, because
/// `Gallery::new` on a folder with nothing in it returns an empty gallery and the
/// first assertion stopped the test dead. `tests/standard.rs` had the same defect
/// for the whole history of this repo. Generate the files instead — that is the
/// rule the rest of the suite follows.
fn folder() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path();

        // A gradient rather than a flat colour, so a decode that came back the
        // wrong size or blank is visible in the bytes and not only in the
        // dimensions.
        let gradient = |w: u32, h: u32| {
            let mut img = RgbImage::new(w, h);
            for (x, y, px) in img.enumerate_pixels_mut() {
                *px = Rgb([(x % 256) as u8, (y % 256) as u8, ((x / 4 + y / 4) % 256) as u8]);
            }
            DynamicImage::ImageRgb8(img)
        };

        // Big enough to take the DCT-scaled path, which is the branch this suite
        // existed to cover.
        gradient(2400, 1600).save(dir.join("a.jpg")).expect("write a.jpg");

        // A PNG, and a PNG wearing a .jpg extension. Format detection is by
        // content and must stay that way: routing this one through a JPEG
        // decoder is the regression this file guards.
        gradient(800, 600).save(dir.join("b.png")).expect("write b.png");
        std::fs::copy(dir.join("b.png"), dir.join("c.jpg")).expect("write c.jpg");

        tmp
    })
    .path()
}

/// Every decode has to satisfy the invariant the whole texture pipeline depends
/// on: a buffer of exactly `w * h * 4` bytes. `ColorImage::from_rgba_unmultiplied`
/// asserts it, so a violation kills the worker rather than showing up as a
/// stretched picture — which is why it is worth checking across a folder rather
/// than on the one path that happened to break last time.
#[test]
fn integration_every_file_in_a_folder_loads_with_a_consistent_buffer() {
    let gallery = Gallery::new(folder());
    assert_eq!(gallery.len(), 3, "the scan missed a file: {:?}", gallery.files);

    for path in &gallery.files {
        let d = load_image(path, 1600).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(d.width > 0 && d.height > 0, "{}: empty", path.display());
        assert_eq!(
            d.rgba.len(),
            (d.width as usize) * (d.height as usize) * 4,
            "{}: {}x{} but {} bytes",
            path.display(),
            d.width,
            d.height,
            d.rgba.len()
        );
    }
}

#[test]
fn integration_a_png_named_jpg_decodes_as_a_png() {
    let misnamed = folder().join("c.jpg");
    let by_content = load_image(&misnamed, 1600).expect("a PNG named .jpg still loads");
    let real_png = load_image(&folder().join("b.png"), 1600).unwrap();

    assert_eq!(
        (by_content.width, by_content.height),
        (real_png.width, real_png.height),
        "the extension changed the decode path"
    );
    assert_eq!(
        by_content.rgba, real_png.rgba,
        "same picture, different pixels: one of the two went down another decoder"
    );
}

#[test]
fn integration_gallery_navigates_all() {
    let mut gallery = Gallery::new(folder());
    let count = gallery.len();
    assert!(count > 0);

    // Two full laps from wherever we start, so this holds wherever the scan put
    // the index and for any count. Loop-back is on by default; the wrap policy
    // itself is pinned in `filter_and_paging.rs`.
    for _ in 0..count * 2 {
        assert!(gallery.index < count, "the index left the gallery");
        gallery.next();
    }
    assert_eq!(gallery.index, 0);
}

#[test]
fn integration_an_empty_folder_is_a_gallery_not_a_panic() {
    let empty = tempfile::tempdir().expect("temp dir");
    let gallery = Gallery::new(empty.path());
    assert_eq!(gallery.len(), 0);
    assert!(gallery.current().is_none());

    // `next` on nothing used to be the way this suite found out it had no files.
    let mut gallery = gallery;
    assert!(!gallery.next(), "next() on an empty gallery must report no move");
    assert!(!gallery.prev());
    assert!(load_image(&empty.path().join("nope.jpg"), 1600).is_err());
}
