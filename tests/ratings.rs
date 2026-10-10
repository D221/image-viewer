//! Ratings and tags, stored as XMP sidecars.
//!
//! The load-bearing property across this whole file: **a round trip preserves
//! every byte it did not write.** A sidecar written by Lightroom holds a Camera
//! Raw block, a histogram and a mask; rewriting it from a parsed model and
//! serialising the model back would drop whatever this code does not model, and
//! dropping another tool's metadata is worse than not having ratings at all.

use image_viewer::ratings::{Rating, Ratings};
use std::path::Path;

fn temp_image(name: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join(name);
    std::fs::write(&p, b"\xff\xd8\xff\xe0not really a jpeg").unwrap();
    (d, p)
}

#[test]
fn an_absent_sidecar_reads_as_all_defaults() {
    let (_d, p) = temp_image("a.jpg");
    let r = Ratings::read(&p).unwrap();
    assert_eq!(r.rating.stars, 0);
    assert!(!r.rating.flag);
    assert!(r.tags.is_empty());
}

#[test]
fn a_rating_round_trips_through_the_sidecar() {
    let (_d, p) = temp_image("a.jpg");
    let mut r = Ratings::read(&p).unwrap();
    r.rating = Rating { stars: 4, flag: true };
    r.tags = vec!["holiday".into(), "portrait".into()];
    r.write(&p).unwrap();

    let back = Ratings::read(&p).unwrap();
    assert_eq!(back.rating.stars, 4);
    assert!(back.rating.flag);
    assert_eq!(back.tags, vec!["holiday".to_string(), "portrait".to_string()]);
}

#[test]
fn stars_are_clamped_and_a_flag_is_a_label() {
    // Lightroom's flag is xmp:Label with text values, not a boolean, so the
    // bytes are Pick/Reject - that is what makes the file readable elsewhere.
    let (_d, p) = temp_image("a.jpg");
    let r = Ratings { rating: Rating { stars: 9, flag: true }, tags: vec![] };
    r.write(&p).unwrap();
    let bytes = std::fs::read(image_viewer::ratings::sidecar_path(&p).unwrap()).unwrap();
    let s = String::from_utf8_lossy(&bytes);
    assert!(s.contains("Pick"), "flag must be xmp:Label Pick, got: {s}");
    assert!(s.contains("5"), "stars clamp to 5, got: {s}");
}

#[test]
fn two_images_sharing_a_stem_get_separate_sidecars() {
    let (d, p1) = temp_image("photo.jpg");
    let p2 = d.path().join("photo.png");
    std::fs::write(&p2, b"\x89PNG\r\n\x1a\n").unwrap();

    Ratings { rating: Rating { stars: 2, flag: false }, tags: vec![] }.write(&p1).unwrap();
    Ratings { rating: Rating { stars: 5, flag: false }, tags: vec![] }.write(&p2).unwrap();

    assert_eq!(Ratings::read(&p1).unwrap().rating.stars, 2, "the .jpg kept its own");
    assert_eq!(Ratings::read(&p2).unwrap().rating.stars, 5, "the .png is untouched by it");
}

#[test]
fn sidecar_path_appends_xmp_and_refuses_archives() {
    let p = Path::new("photo.jpg");
    let side = image_viewer::ratings::sidecar_path(p).unwrap();
    assert_eq!(side.to_string_lossy(), "photo.jpg.xmp");
    assert!(image_viewer::ratings::sidecar_path(Path::new("vol.cbz")).is_none());
}
