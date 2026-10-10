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

/// **The whole point of this feature.** A sidecar written by another tool holds
/// packets this code does not model. After we change a rating, every byte we did
/// not intend to change must be identical — byte-for-byte, not "semantically
/// equivalent".
#[test]
fn changing_a_rating_preserves_every_other_packet() {
    let (_d, p) = temp_image("a.jpg");
    let foreign = r#"<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
 <rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
  <rdf:Description rdf:about=""
    xmlns:xmp="http://ns.adobe.com/xap/1.0/"
    xmlns:crs="http://ns.adobe.com/camera-raw-settings/1.0/"
    xmp:Rating="1"
    crs:ToneCurveName2012="Medium High Contrast"
    crs:Exposure2012="+0.35">
  </rdf:Description>
 </rdf:RDF>
</x:xmpmeta>
<?xpacket end="w"?>"#;
    let side = image_viewer::ratings::sidecar_path(&p).unwrap();
    std::fs::write(&side, foreign).unwrap();

    let mut r = Ratings::read(&p).unwrap();
    assert_eq!(r.rating.stars, 1, "the foreign rating was read");
    r.rating.stars = 5;
    r.write(&p).unwrap();

    let after = std::fs::read_to_string(&side).unwrap();
    assert!(after.contains("crs:ToneCurveName2012=\"Medium High Contrast\""),
        "the Camera Raw packet must survive:\n{after}");
    assert!(after.contains("crs:Exposure2012=\"+0.35\""),
        "and so must every other attribute:\n{after}");
    assert!(after.contains("xmp:Rating=\"5\""));
    assert!(!after.contains("xmp:Rating=\"1\""));
}

/// A sidecar with no rating element at all: the attribute must be **inserted**,
/// and the file must still parse. A "just write our own block" merge loses the
/// rating the moment anything else touches it.
#[test]
fn a_sidecar_without_a_rating_gets_one_inserted() {
    let (_d, p) = temp_image("a.jpg");
    let side = image_viewer::ratings::sidecar_path(&p).unwrap();
    std::fs::write(&side, r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" crs:Version="15.0"/></rdf:RDF></x:xmpmeta>"#).unwrap();

    let mut r = Ratings::read(&p).unwrap();
    assert_eq!(r.rating.stars, 0, "no element means no rating, not an error");
    r.rating.stars = 3;
    r.write(&p).unwrap();

    let after = std::fs::read_to_string(&side).unwrap();
    assert!(after.contains("crs:Version=\"15.0\""), "preserved: {after}");
    assert_eq!(Ratings::read(&p).unwrap().rating.stars, 3);
}

/// A damaged sidecar is an **error naming the file**, and the image still opens.
/// A corrupt sidecar must never make a photo unopenable — it is a file we made,
/// not a property of the picture.
#[test]
fn a_damaged_sidecar_is_an_error_and_leaves_the_image_alone() {
    let (_d, p) = temp_image("a.jpg");
    let side = image_viewer::ratings::sidecar_path(&p).unwrap();
    std::fs::write(&side, "this is not xml at all").unwrap();

    let err = Ratings::read(&p).unwrap_err();
    assert!(err.contains("xmp") || err.contains("sidecar"), "names the file: {err}");

    // The image is untouched: the viewer's own decode still works.
    assert!(image_viewer::looks_like_supported_image(
        &std::fs::read(&p).unwrap()
    ));
}

/// A `Second` (yellow) label is a third state this app does not use. Changing
/// the stars must leave it byte-for-byte alone — rewriting it to `Reject`
/// would destroy a judgement another tool made.
#[test]
fn a_second_label_survives_a_rating_change() {
    let (_d, p) = temp_image("a.jpg");
    let side = image_viewer::ratings::sidecar_path(&p).unwrap();
    std::fs::write(&side, r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="2" xmp:Label="Second"/></rdf:RDF></x:xmpmeta>"#).unwrap();

    let mut r = Ratings::read(&p).unwrap();
    assert_eq!(r.rating.stars, 2);
    assert!(!r.rating.flag, "Second is not our flag");
    r.rating.stars = 4;
    r.tags = vec!["kept".into()];
    r.write(&p).unwrap();

    let after = std::fs::read_to_string(&side).unwrap();
    assert!(after.contains("xmp:Label=\"Second\""), "Second preserved: {after}");
    assert!(after.contains("xmp:Rating=\"4\""));
    let back = Ratings::read(&p).unwrap();
    assert_eq!(back.rating.stars, 4);
    assert!(!back.rating.flag);
    assert_eq!(back.tags, vec!["kept".to_string()]);
}

/// The Task 1 review's deferred minor: a literal `xmp:Rating="…"` sitting in
/// element text (not in a tag) must not be read as the rating, nor rewritten.
#[test]
fn a_literal_rating_in_text_does_not_fool_the_parser() {
    let (_d, p) = temp_image("a.jpg");
    let side = image_viewer::ratings::sidecar_path(&p).unwrap();
    std::fs::write(&side, r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmp:Rating="1"><dc:note xmlns:dc="http://purl.org/dc/elements/1.1/">a note about xmp:Rating="5" here</dc:note></rdf:Description></rdf:RDF></x:xmpmeta>"#).unwrap();

    let mut r = Ratings::read(&p).unwrap();
    assert_eq!(r.rating.stars, 1, "the text literal is not the rating");
    r.rating.stars = 3;
    r.write(&p).unwrap();

    let after = std::fs::read_to_string(&side).unwrap();
    assert!(after.contains("xmp:Rating=\"3\""), "real attribute updated: {after}");
    assert!(after.contains("xmp:Rating=\"5\""), "text literal untouched: {after}");
    assert_eq!(after.matches("xmp:Rating=\"3\"").count(), 1);
}

/// Writing is atomic, so a crash mid-write cannot leave a half file that reads
/// as a valid rating.
#[test]
fn a_write_leaves_no_temporary_file_behind() {
    let (_d, p) = temp_image("a.jpg");
    Ratings { rating: Rating { stars: 3, flag: false }, tags: vec![] }.write(&p).unwrap();
    let side = image_viewer::ratings::sidecar_path(&p).unwrap();
    let leftovers: Vec<_> = std::fs::read_dir(side.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("tmp"))
        .collect();
    assert!(leftovers.is_empty(), "no .tmp left beside the sidecar: {leftovers:?}");
}
