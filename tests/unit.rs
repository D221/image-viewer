use image_viewer::{Gallery, ZoomPan};

#[test]
fn test_scan_folder_returns_files() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("test.jpg"), b"fake_jpg_data").ok();
    std::fs::write(tmp.path().join("other.jpeg"), b"fake_jpeg_data").ok();
    std::fs::write(tmp.path().join("sample.png"), b"fake_png_data").ok();

    let gallery = Gallery::new(tmp.path());
    assert_eq!(gallery.len(), 3);
}

#[test]
fn test_gallery_next_and_prev() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.jpg"), b"fake").ok();
    std::fs::write(tmp.path().join("b.jpg"), b"fake").ok();

    let mut gallery = Gallery::new(tmp.path());
    assert!(!gallery.is_empty());
    assert_eq!(gallery.len(), 2);

    // Navigate
    gallery.next();
    assert_eq!(gallery.index, 1);
    gallery.prev();
    assert_eq!(gallery.index, 0);
    gallery.prev();
    // Should wrap
    assert_eq!(gallery.index, 1);
}

#[test]
fn test_load_all_from_folder_remembers_current() {
    // Create folder with multiple images
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("a.jpg"), b"fake").ok();
    std::fs::write(tmp.path().join("b.jpg"), b"fake").ok();
    std::fs::write(tmp.path().join("c.jpg"), b"fake").ok();

    let mut gallery = Gallery::new(tmp.path());
    assert_eq!(gallery.current().unwrap(), tmp.path().join("a.jpg"));

    // Navigate to second image
    gallery.next();
    assert_eq!(gallery.current().unwrap(), tmp.path().join("b.jpg"));

    // Call load_all_from_folder with the same folder
    gallery.load_all_from_folder(tmp.path());

    // Should remember current image (b.jpg) instead of jumping to first
    assert_eq!(gallery.current().unwrap(), tmp.path().join("b.jpg"));
}

#[test]
fn test_zoom_pan_reset_and_clamp() {
    let mut zoom_pan = ZoomPan::new();
    assert_eq!(zoom_pan.zoom, 1.0);

    // zoom_by takes scroll delta; each unit is multiplied by 0.05
    zoom_pan.zoom_by(-20.0); // -20 * 0.05 = -1.0 => zoom = 0.0 -> clamped to 0.1
    assert_eq!(zoom_pan.zoom, 0.1);

    zoom_pan.zoom_by(200.0); // 200 * 0.05 = 10 => zoom = 10.1 -> clamped to 8.0
    assert_eq!(zoom_pan.zoom, 8.0);

    zoom_pan.reset();
    assert_eq!(zoom_pan.zoom, 1.0);
}
