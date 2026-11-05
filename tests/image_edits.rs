//! The per-image edit record: what a turn, a crop or a reset did to *this* file.
//!
//! Split out of `tests/rotate_save.rs`; shared fixtures are in
//! `tests/common/rotate_fixtures.rs`. The property is that the record follows the
//! image rather than the app, so navigating away and back finds the same edits and
//! an unedited neighbour is untouched.

use image_viewer::{ImageEdits, Xform, neutralize_exif_orientation};
use std::path::{Path, PathBuf};

#[path = "common/rotate_fixtures.rs"]
mod rotate_fixtures;

use rotate_fixtures::*;

/// The bug this pins: the transform used to be a single `u8` on the app, so
/// turning one photo turned every photo you then navigated to.
#[test]
fn rotating_one_image_does_not_rotate_the_next() {
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");

    r.turn(&a, 1);
    assert_eq!(r.xform(&a), Xform::Rot90, "the turned image should be turned");
    assert_eq!(
        r.xform(&b),
        Xform::Identity,
        "a different image must be untouched - this is the whole point"
    );
}

#[test]
fn going_back_to_a_turned_image_still_finds_it_turned() {
    // The other half. Losing your edit on the way back is as wrong as applying it
    // to everything else, and the single-u8 version did both.
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");

    r.turn(&a, 1);
    r.turn(&b, 2); // while browsing past it
    assert_eq!(r.xform(&a), Xform::Rot90, "coming back must not reset the first image");
    assert_eq!(r.xform(&b), Xform::Rot180);
}

#[test]
fn turns_accumulate_and_wrap_in_both_directions() {
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");

    assert_eq!(r.turn(&a, 1), Xform::Rot90);
    assert_eq!(r.turn(&a, 1), Xform::Rot180);
    assert_eq!(r.turn(&a, 1), Xform::Rot270);
    assert_eq!(r.turn(&a, 1), Xform::Identity, "four quarter turns is a full turn");
    assert_eq!(r.turn(&a, -1), Xform::Rot270, "negative steps go anticlockwise");
    assert_eq!(r.turn(&a, -1), Xform::Rot180);
    // A single step from untransformed, anticlockwise, is a quarter turn back.
    let mut r2 = ImageEdits::default();
    assert_eq!(r2.turn(&a, -1), Xform::Rot270, "-1 from 0 must wrap, not underflow");
    assert_eq!(r2.turn(&a, 5), Xform::Identity, "a whole number of turns is a no-op");
}

#[test]
fn mirroring_twice_is_back_to_the_start() {
    // The one property that makes a mirror button safe to press by accident.
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");

    r.mirror_horizontal(&a);
    assert_eq!(r.xform(&a), Xform::FlipH);
    r.mirror_horizontal(&a);
    assert_eq!(r.xform(&a), Xform::Identity, "two mirrors cancel");
    assert!(r.is_empty(), "and leave nothing stored");

    r.mirror_vertical(&a);
    assert_eq!(r.xform(&a), Xform::FlipV);
    r.mirror_vertical(&a);
    assert_eq!(r.xform(&a), Xform::Identity);
}

/// The two edits do not commute, and this is the whole reason the store holds a
/// transform instead of two counters. Turn-then-mirror is a *diagonal* mirror —
/// if the app kept a turn count and a flip flag, the save would have to discover
/// this at the last moment, where a sign error is invisible in this viewer (it
/// re-applies the same mistake to the file it just wrote) and glaring everywhere
/// else.
#[test]
fn a_quarter_turn_then_a_mirror_is_a_diagonal_mirror() {
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");

    r.turn(&a, 1);
    r.mirror_horizontal(&a);
    assert_eq!(r.xform(&a), Xform::Transverse, "turn-then-mirror is a diagonal mirror");

    // And the other order gives the *other* diagonal, which is a different
    // picture. That they differ is the point; a sign error here would make them
    // the same, which is why this is asserted rather than assumed.
    let mut r2 = ImageEdits::default();
    r2.mirror_horizontal(&a);
    r2.turn(&a, 1);
    assert_eq!(r2.xform(&a), Xform::Transpose);
    assert_ne!(r.xform(&a), r2.xform(&a));
}

/// A mirror must not change the image's shape, or the fit, the panning limits and
/// the tile grid would all be computed for the wrong footprint.
#[test]
fn a_mirror_leaves_the_footprint_alone_but_a_quarter_turn_does_not() {
    let (w, h) = (96.0f32, 64.0f32);
    for xf in Xform::ALL {
        let (dw, dh) = image_viewer::xformed_footprint(w, h, xf);
        let expected = if xf.swaps_axes() { (h, w) } else { (w, h) };
        assert_eq!((dw, dh), expected, "{xf:?}: wrong footprint");
        assert_eq!(
            xf.out_dims(96, 64),
            (dw as u32, dh as u32),
            "{xf:?}: footprint disagrees with out_dims"
        );
    }
}

#[test]
fn an_untransformed_image_costs_nothing() {
    // Returning to identity removes the entry rather than storing it, so browsing
    // a folder and undoing your edits leaves no residue.
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");
    assert!(r.is_empty());
    r.turn(&a, 1);
    assert_eq!(r.len(), 1);
    r.turn(&a, 3); // back to identity
    assert_eq!(r.xform(&a), Xform::Identity);
    assert_eq!(r.len(), 0, "an identity transform should not be stored");
    assert!(r.is_empty());
}

#[test]
fn reset_puts_an_image_back_where_it_started() {
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");
    r.turn(&a, 1);
    r.mirror_vertical(&a);
    r.turn(&b, 1);

    r.reset(&a);
    assert_eq!(r.xform(&a), Xform::Identity, "reset clears the whole transform");
    assert_eq!(r.len(), 1, "and only that image's entry");
    assert_eq!(r.xform(&b), Xform::Rot90, "another image is untouched");
}

#[test]
fn the_record_is_bounded() {
    // A viewer left open on a 100k-image folder must not grow a map for as long
    // as it runs. Losing edits is much the smaller problem.
    let mut r = ImageEdits::default();
    for i in 0..5000 {
        r.set(
            &PathBuf::from(format!("img{i}.jpg")),
            image_viewer::Edit { xform: Xform::ALL[1 + (i % 3)], crop: None },
        );
    }
    assert!(r.len() <= 4096, "grew to {} entries with no bound", r.len());
}

/// The save composes the EXIF tag with the *current image's* transform. If these
/// two ever came from different images the saved file would be wrong in a way
/// nothing else would catch, because the viewer applies the tag itself.
#[test]
fn a_saved_file_uses_the_transform_of_the_image_being_saved() {
    let mut r = ImageEdits::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");
    r.turn(&a, 1);
    r.turn(&b, 2);

    // The shape the app uses: look the transform up by the path being saved.
    let save_b = |p: &Path| r.xform(p);
    assert_eq!(save_b(&a), Xform::Rot90);
    assert_eq!(save_b(&b), Xform::Rot180);
    // And the composed transform differs, so the two files are genuinely not
    // being saved with each other's transform.
    assert_ne!(
        image_viewer::display_xform(0, save_b(&a)),
        image_viewer::display_xform(0, save_b(&b))
    );
}

#[test]
fn a_transform_cancels_itself_inverse() {
    assert_eq!(Xform::Rot90.then(Xform::Rot270), Xform::Identity);
    assert_eq!(Xform::Rot180.then(Xform::Rot180), Xform::Identity);
    assert_eq!(Xform::FlipH.then(Xform::FlipH), Xform::Identity);
    assert_eq!(Xform::Transpose.then(Xform::Transpose), Xform::Identity);
    assert_eq!(Xform::Transverse.then(Xform::Transverse), Xform::Identity);
    assert_eq!(Xform::Identity.then(Xform::Rot90), Xform::Rot90);
}

#[test]
fn the_orientation_rewrite_survives_a_hostile_exif_block() {
    // Every offset in here comes from the file, and the output goes to other
    // programs. A panic or an out-of-bounds write here is memory corruption, not
    // a caught error, so every malformed shape has to be a clean `false`.
    let mut good = {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::read(write_jpeg(tmp.path(), "g.jpg", &quadrants(16, 16), 6)).unwrap()
    };
    assert!(image_viewer::is_jpeg_bytes(&good));
    assert_eq!(exif_orientation_from(&good), 6, "fixture is wrong");
    assert!(neutralize_exif_orientation(&mut good), "no change reported");
    assert_eq!(exif_orientation_from(&good), 1, "orientation was not set to 1");

    // Truncations at every length: nothing may panic.
    for cut in 0..good.len() {
        let mut b = good[..cut].to_vec();
        let _ = neutralize_exif_orientation(&mut b);
    }
    // Corrupt every byte in turn around the EXIF header.
    for i in 0..good.len().min(80) {
        for v in [0x00u8, 0xFF, 0x7F, 0x80] {
            let mut b = good.clone();
            b[i] = v;
            let _ = neutralize_exif_orientation(&mut b);
        }
    }
    // Absurd segment lengths and IFD offsets.
    for i in 0..good.len().min(40) {
        let mut b = good.clone();
        let n2 = (i + 2).min(b.len());
        b[i..n2].copy_from_slice(&0xFFFFu16.to_be_bytes());
        let _ = neutralize_exif_orientation(&mut b);
    }
    // No EXIF at all: a clean no-op, not a failure.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("plain.jpg");
    let mut b = Vec::new();
    quadrants(8, 8).write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Jpeg).unwrap();
    std::fs::write(&p, &b).unwrap();
    let mut copy = b.clone();
    assert!(!neutralize_exif_orientation(&mut copy), "a JPEG with no EXIF claimed a change");
    assert_eq!(copy, b, "the bytes were modified anyway");
}
