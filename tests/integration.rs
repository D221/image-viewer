use image_viewer::{Gallery, load_image};
use std::path::Path;

const TEST_DIR: &str = r"test-images";

#[test]
fn integration_load_large_images() {
    let gallery = Gallery::new(Path::new(TEST_DIR));
    assert!(!gallery.is_empty());

    let mut checked = 0;
    for path in &gallery.files {
        let data = load_image(path, 1600).expect("should load image");
        assert!(data.width > 0);
        assert!(data.height > 0);
        assert!(!data.rgba.is_empty());
        checked += 1;
        if checked >= 2 {
            break;
        }
    }
    assert!(checked >= 1);
}

#[test]
fn integration_gallery_navigates_all() {
    let mut gallery = Gallery::new(Path::new(TEST_DIR));
    let count = gallery.len();
    for _ in 0..count * 2 {
        gallery.next();
    }
    assert_eq!(gallery.index, 0);
}
