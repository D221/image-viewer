//! Saving a crop: the pixels that were framed, and the bands that show what is cut.
//!
//! Split out of `tests/rotate_save.rs`; shared fixtures are in
//! `tests/common/rotate_fixtures.rs`. The clipboard tests live here too - the
//! clipboard is the same edit as pixels, and the save is the same edit as a file.

use image_viewer::{Xform, apply_exif_orientation, exif_orientation, load_image, save_transformed};
use std::path::PathBuf;

#[path = "common/rotate_fixtures.rs"]
mod rotate_fixtures;

use rotate_fixtures::*;

/// The headline: a rectangle drawn on the displayed picture must come out the
/// other side of a lossless save as **that** part of **that** picture.
///
/// For every camera orientation and every user transform. The expected image is
/// built by hand from the display transform — crop what is on screen, then take
/// the pixels — so it shares no code with the path being tested.
///
/// A 16 px grid is used because that is the MCU size for the 4:2:0 subsampling
/// the encoder picks, and the crop has to land on it. A crop that ignores this
/// fails TurboJPEG or silently shifts, which is why the rects here are multiples
/// of 16.
#[test]
fn a_saved_crop_is_exactly_the_region_that_was_framed() {
    let (w, h) = (96u32, 64u32);
    for exif_tag in [0u16, 1, 2, 3, 4, 5, 6, 7, 8] {
        for user in Xform::ALL {
            let tmp = tempfile::tempdir().unwrap();
            let src = quadrants(w, h);
            let p = write_jpeg(tmp.path(), "photo.jpg", &src, exif_tag);

            // Frame a region of the *displayed* picture, in its own pixels.
            let display = image_viewer::display_xform(exif_tag, user);
            let (dw, dh) = display.out_dims(w, h);
            let rect = (
                16.min(dw.saturating_sub(16)),
                16.min(dh.saturating_sub(16)),
                32.min(dw),
                32.min(dh),
            );
            let Some(crop) = image_viewer::Crop::from_display_rect(exif_tag, user, rect, (w, h))
            else {
                continue;
            };

            // What the user should end up with: the displayed picture, cropped.
            let want = {
                let shown = display_xform_pixels(&src, exif_tag, user);
                let rgba = shown.to_rgba8();
                let (cw, ch, px) = crop_display(&rgba, rect);
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(cw, ch, px).unwrap())
            };

            let dest = save_transformed(&p, user, Some(crop))
                .unwrap_or_else(|e| panic!("exif={exif_tag} {user:?}: {e}"));
            let got = load_image(&dest, 4096).unwrap();
            assert_same_pixels(
                &got,
                &want,
                &format!("exif={exif_tag} user={user:?} crop={crop:?}"),
            );
        }
    }
}

/// Crop and turn together, in one save. This is the case that would need a
/// re-encode if the crop were a second pass over the pixels rather than a second
/// field of the same TurboJPEG transform.
#[test]
fn a_crop_and_a_turn_are_saved_together_losslessly() {
    let (w, h) = (96u32, 64u32);
    for (user, crop) in [
        (Xform::Rot90, image_viewer::Crop::new(16, 16, 48, 32).unwrap()),
        (Xform::FlipH, image_viewer::Crop::new(0, 0, 64, 48).unwrap()),
        (Xform::Transverse, image_viewer::Crop::new(32, 16, 32, 32).unwrap()),
        // No turn at all: the crop must not depend on there being one.
        (Xform::Identity, image_viewer::Crop::new(16, 0, 32, 64).unwrap()),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        let src = quadrants(w, h);
        let p = write_jpeg(tmp.path(), "photo.jpg", &src, 0);

        let want = {
            let (cw, ch, px) = crop.apply_rgba(w, h, &src.to_rgba8().into_raw()).unwrap();
            let (ow, oh, px) = user.apply_rgba(cw, ch, &px).unwrap();
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(ow, oh, px).unwrap())
        };

        let dest = save_transformed(&p, user, Some(crop)).unwrap();
        let got = load_image(&dest, 4096).unwrap();
        assert_same_pixels(&got, &want, &format!("{user:?} + {crop:?}"));
    }
}

// ---------------------------------------------------------------------------
// Holding the crop frame to a shape
// ---------------------------------------------------------------------------

/// The ratio a frame came out at, or `None` if it has no area.
fn aspect_of(r: (f32, f32, f32, f32)) -> Option<f32> {
    let (w, h) = (r.2 - r.0, r.3 - r.1);
    (w > 0.0 && h > 0.0).then_some(w / h)
}

#[test]
fn shift_gives_a_perfect_square_whatever_the_drag_did() {
    for (anchor, corner) in [
        ((0.0, 0.0), (300.0, 100.0)),
        ((0.0, 0.0), (100.0, 300.0)),
        ((50.0, 50.0), (450.0, 60.0)),
        ((0.0, 0.0), (137.0, 421.0)),
    ] {
        let r = image_viewer::constrain_to_aspect(anchor, corner, Some(1.0));
        let ratio = aspect_of(r).unwrap_or_else(|| panic!("{anchor:?}->{corner:?} gave no frame"));
        assert!((ratio - 1.0).abs() < 1e-4, "{anchor:?}->{corner:?} gave {ratio}, not a square");
    }
}

#[test]
fn a_held_shape_keeps_the_press_point_and_the_direction_of_the_drag() {
    // The press point is the anchor, so the frame grows out of where the drag
    // started rather than out of the middle of the drag.
    let anchor = (20.0, 30.0);
    let r = image_viewer::constrain_to_aspect(anchor, (220.0, 130.0), Some(16.0 / 9.0));
    assert_eq!((r.0, r.1), anchor, "the frame moved off the press point");
    // The axis the pointer leads is the one kept: right and down here, so the
    // far corner is still down-right of the anchor.
    assert!(r.2 > anchor.0 && r.3 > anchor.1, "wrong quadrant: {r:?}");

    // The same drag mirrored about the press point frames the same shape. This
    // is the reason the min/max rectangle is rebuilt from the anchor rather than
    // carried through from the raw pointer: a frame you drew bottom-right and one
    // you drew top-left have to be the same size or the same crop needs two
    // different drags depending on the corner you happened to start from.
    let (w, h) = (r.2 - r.0, r.3 - r.1);
    let up_left =
        image_viewer::constrain_to_aspect(anchor, (anchor.0 - w, anchor.1 - h), Some(16.0 / 9.0));
    assert_eq!(
        (up_left.2 - up_left.0, up_left.3 - up_left.1),
        (w, h),
        "the same drag mirrored framed a different shape"
    );
    // The frame lies wholly on the mirrored side and still touches the press
    // point — the anchor is a corner of the rectangle, not its middle.
    assert!(
        up_left.2 <= anchor.0 && up_left.3 <= anchor.1,
        "not up-left of the press point: {up_left:?} vs {anchor:?}"
    );
    assert_eq!(
        (up_left.2, up_left.3),
        anchor,
        "the mirrored frame lost the corner it was dragged from"
    );
}

#[test]
fn the_led_axis_is_the_one_that_gives() {
    // Too wide for the ratio: the width is trimmed, and the height — which the
    // pointer is not leading — is what has to give.
    let wide = image_viewer::constrain_to_aspect((0.0, 0.0), (200.0, 100.0), Some(1.0));
    assert_eq!(wide, (0.0, 0.0, 100.0, 100.0), "width should have been trimmed");
    // Too tall: the mirror of it.
    let tall = image_viewer::constrain_to_aspect((0.0, 0.0), (100.0, 200.0), Some(1.0));
    assert_eq!(tall, (0.0, 0.0, 100.0, 100.0), "height should have been trimmed");
}

#[test]
fn a_free_drag_is_left_exactly_as_it_was() {
    // `None` is not "approximately free" — the rectangle has to be bit-identical
    // to the drag, or every uncropped drag in the app shifts by a rounding step.
    for (anchor, corner) in
        [((0.0, 0.0), (300.0, 100.0)), ((10.0, 20.0), (5.0, 300.0)), ((-4.0, -9.0), (11.0, 2.0))]
    {
        let (ax, ay) = anchor;
        let (cx, cy) = corner;
        assert_eq!(
            image_viewer::constrain_to_aspect(anchor, corner, None),
            (ax.min(cx), ay.min(cy), ax.max(cx), ay.max(cy)),
            "{anchor:?}->{corner:?} moved with no shape asked for"
        );
    }
}

#[test]
fn a_drag_with_no_area_is_not_given_a_shape() {
    // Pressing and holding Shift without moving must not pop a frame into being.
    // This needs no special case in the implementation to hold — a zero on one
    // axis zeroes the other, so every branch collapses the frame — but it is the
    // user-visible property, and it is what a "minimum frame size" or a
    // "snap-to-a-shape-on-press" change would break.
    for corner in [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)] {
        for aspect in [None, Some(1.0), Some(16.0 / 9.0), Some(1.0 / 100.0)] {
            let r = image_viewer::constrain_to_aspect((0.0, 0.0), corner, aspect);
            assert_eq!(aspect_of(r), None, "a flat drag grew into {r:?}");
        }
    }
}

#[test]
fn every_shape_in_the_list_resolves_to_the_ratio_it_is_labelled_with() {
    for a in image_viewer::CropAspect::ALL {
        let want = match a {
            image_viewer::CropAspect::Free => None,
            image_viewer::CropAspect::Square => Some(1.0),
            image_viewer::CropAspect::Widescreen => Some(16.0 / 9.0),
            image_viewer::CropAspect::Portrait => Some(9.0 / 16.0),
            image_viewer::CropAspect::Classic => Some(4.0 / 3.0),
            image_viewer::CropAspect::TallClassic => Some(3.0 / 4.0),
            image_viewer::CropAspect::Photo => Some(3.0 / 2.0),
            image_viewer::CropAspect::TallPhoto => Some(2.0 / 3.0),
            // Resolved against the image, not the label.
            image_viewer::CropAspect::Source => Some(3.0 / 2.0),
        };
        assert_eq!(
            a.ratio((3000, 2000)),
            want,
            "{:?} ({}) does not resolve to its own label",
            a,
            a.label()
        );
    }
}

#[test]
fn same_as_the_image_follows_the_picture_as_it_is_shown() {
    // A 3:2 photo turned on its side is 2:3 on screen, and a frame that insisted
    // on 3:2 would not fit it — the caller passes the *displayed* size for
    // exactly this reason, so this is the assertion that the source is the
    // displayed picture and not the file.
    assert_eq!(image_viewer::CropAspect::Source.ratio((3000, 2000)), Some(1.5));
    assert_eq!(image_viewer::CropAspect::Source.ratio((2000, 3000)), Some(2.0 / 3.0));
    // A degenerate size cannot be divided by, and must not become NaN — a NaN
    // ratio would sail through `constrain_to_aspect`'s filter and produce a
    // rectangle of nonsense.
    assert_eq!(image_viewer::CropAspect::Source.ratio((0, 0)), None);
}

#[test]
fn a_config_written_before_this_existed_still_loads() {
    // The whole reason the field is `#[serde(default)]`: an old config that
    // knows nothing about crop shapes must start, and must start on Free, or the
    // feature arrives as a behaviour change nobody asked for on upgrade.
    let old = r#"{
        "load_all_on_startup": true,
        "folder": null,
        "sort": "name"
    }"#;
    let c: image_viewer::Config = serde_json::from_str(old).expect("an old config must parse");
    assert_eq!(c.crop_aspect, image_viewer::CropAspect::Free);
    assert_eq!(c.crop_aspect, image_viewer::CropAspect::default());
}

#[test]
fn a_crop_shape_survives_a_config_round_trip() {
    for a in image_viewer::CropAspect::ALL {
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, format!("\"{}\"", json.trim_matches('"')), "not a bare word");
        let back: image_viewer::CropAspect = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a, "{a:?} did not survive {json}");
    }
    // Lowercase in the file, whatever the variant is called in Rust: an enum
    // that writes its variant name cannot be read back by anyone who edited the
    // file to the documented spelling.
    let json = serde_json::to_string(&image_viewer::CropAspect::TallPhoto).unwrap();
    assert_eq!(json, "\"tallphoto\"");
}

// ---------------------------------------------------------------------------
// The clipboard: the same edit, as pixels
// ---------------------------------------------------------------------------
//
// `Ctrl+S` writes the framed picture to a file and `Ctrl+C` puts it on the
// clipboard, so they are one feature with two outputs. The clipboard takes
// pixels, so it cannot use the lossless DCT path and does not want to: nothing is
// re-compressed at all there.
//
// That leaves the interesting question, which is whether two different routes to
// the same picture agree. `save_transformed` cuts in **stored** space and then
// applies the whole display transform. `region_for_save` is handed a buffer that
// is already EXIF-oriented, so it has to move the rectangle into that buffer's
// space and then apply the user's turn alone. Two compositions, one answer — and
// the ways to get it wrong are silent, because a wrongly-placed crop is still a
// perfectly good picture of the wrong part of the photo.

/// The buffer a decode hands the app: EXIF orientation applied, user turn not.
fn oriented_decode(img: &image::DynamicImage, exif_tag: u16) -> image_viewer::ImageData {
    let mut d = image_viewer::ImageData {
        width: img.width(),
        height: img.height(),
        rgba: img.to_rgba8().into_raw(),
        load_time: std::time::Duration::ZERO,
    };
    apply_exif_orientation(&mut d, exif_tag);
    d
}

/// The clipboard route, checked against the save's own formulation of the edit.
///
/// `want` is built the way `save_transformed` builds it — crop the stored pixels,
/// then apply `display_xform(exif, user)` — so the two are independent statements
/// of the same edit and cannot both be wrong in the same way. Raster algebra
/// only, so the comparison is exact: a JPEG in the loop would make a loose
/// threshold the only option and would hide a crop of the wrong corner inside it.
#[test]
fn the_clipboard_region_is_the_picture_the_save_would_write() {
    let (w, h) = (96u32, 64u32);
    let src = quadrants(w, h);
    let stored_rgba = src.to_rgba8().into_raw();
    for exif_tag in [0u16, 1, 2, 3, 4, 5, 6, 7, 8] {
        let buffer = oriented_decode(&src, exif_tag);
        for user in Xform::ALL {
            // A grid of frames, including ones flush to an edge and ones big
            // enough to be the whole picture: the corners are where a transposed
            // rectangle shows up.
            let display = image_viewer::display_xform(exif_tag, user);
            let (dw, dh) = display.out_dims(w, h);
            for rect in [
                (0u32, 0u32, dw, dh),
                (0, 0, dw.min(32), dh.min(32)),
                (dw / 4, dh / 4, dw / 2, dh / 2),
                (dw.saturating_sub(32), dh.saturating_sub(16), 32.min(dw), 16.min(dh)),
            ] {
                let Some(crop) =
                    image_viewer::Crop::from_display_rect(exif_tag, user, rect, (w, h))
                else {
                    continue;
                };
                let what = format!("exif={exif_tag} user={user:?} crop={crop:?}");

                let got = image_viewer::region_for_save(&buffer, exif_tag, user, Some(crop))
                    .unwrap_or_else(|| panic!("{what}: refused a crop it was given"));

                let (cw, ch, cut) = crop.apply_rgba(w, h, &stored_rgba).unwrap();
                let want = display
                    .apply_rgba(cw, ch, &cut)
                    .unwrap_or_else(|| panic!("{what}: the save's own route failed too"));

                assert_eq!(
                    (got.width, got.height),
                    (want.0, want.1),
                    "{what}: wrong size for the framed region"
                );
                assert_eq!(got.rgba, want.2, "{what}: wrong pixels for the framed region");
            }
        }
    }
}

/// No edit, no change. This is the path every image takes until somebody turns
/// something, and it must stay a copy of the decode rather than a resample, a
/// re-orientation or a no-op that quietly drops the alpha.
#[test]
fn an_unedited_image_goes_to_the_clipboard_exactly_as_decoded() {
    let src = quadrants(96, 64);
    for exif_tag in [0u16, 1, 3, 6, 8] {
        let buffer = oriented_decode(&src, exif_tag);
        for (user, crop) in [
            (Xform::Identity, None),
            (Xform::Identity, Some(image_viewer::Crop::new(0, 0, 96, 64).unwrap())),
        ] {
            let got = image_viewer::region_for_save(&buffer, exif_tag, user, crop).unwrap();
            assert_eq!(
                (got.width, got.height),
                (buffer.width, buffer.height),
                "exif={exif_tag} {user:?}: an edit that is a no-op changed the size"
            );
            assert_eq!(
                got.rgba, buffer.rgba,
                "exif={exif_tag} {user:?} {crop:?}: an edit that is a no-op changed the pixels"
            );
        }
    }
}

/// The clipboard against the real thing: decode a file, cut it, and compare with
/// the file `Ctrl+S` would have written and read back.
///
/// The looser comparison is `assert_same_pixels`, because the save goes through a
/// JPEG round trip and the clipboard does not. The point is not bit-equality —
/// it is that the two routes name the same *region*, and a region that is off by
/// a transpose or a quarter turn is off by whole quadrants, which no amount of
/// JPEG noise can disguise.
#[test]
fn the_clipboard_matches_a_real_saved_file() {
    let (w, h) = (96u32, 64u32);
    for (exif_tag, user, crop) in [
        (0u16, Xform::Identity, image_viewer::Crop::new(16, 16, 32, 32)),
        (6, Xform::Identity, image_viewer::Crop::new(16, 16, 32, 32)),
        (6, Xform::Rot90, image_viewer::Crop::new(8, 0, 48, 32)),
        (8, Xform::Transverse, image_viewer::Crop::new(0, 16, 32, 32)),
        (3, Xform::FlipV, image_viewer::Crop::new(16, 8, 32, 32)),
    ]
    .map(|(t, u, c)| (t, u, c.unwrap()))
    {
        let tmp = tempfile::tempdir().unwrap();
        let src = quadrants(w, h);
        let p = write_jpeg(tmp.path(), "photo.jpg", &src, exif_tag);
        let what = format!("exif={exif_tag} user={user:?} crop={crop:?}");

        // The save's answer, through a real file.
        let dest = save_transformed(&p, user, Some(crop)).unwrap();
        let saved = load_image(&dest, 4096).unwrap();

        // The clipboard's answer, from the same decode the app already holds.
        let buffer = image_viewer::load_image_full(&p, 0).unwrap();
        assert_eq!(
            exif_orientation(&p),
            exif_tag,
            "{what}: the fixture's EXIF tag did not round-trip, so this proves nothing"
        );
        let copied = image_viewer::region_for_save(&buffer, exif_tag, user, Some(crop)).unwrap();

        let want = image::DynamicImage::ImageRgba8(
            image::RgbaImage::from_raw(saved.width, saved.height, saved.rgba).unwrap(),
        );
        assert_same_pixels(&copied, &want, &what);
    }
}

/// A crop that keeps every pixel must behave exactly like no crop at all —
/// including the file name, which would otherwise say `_cropped` for a file that
/// was not.
#[test]
fn a_crop_that_keeps_everything_is_not_a_crop() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "whole.jpg", &quadrants(64, 48), 0);
    let full = image_viewer::Crop::new(0, 0, 64, 48).unwrap();
    assert!(full.is_full(64, 48));
    let dest = save_transformed(&p, Xform::Identity, Some(full)).unwrap();
    assert_eq!(
        dest.file_name().unwrap().to_string_lossy(),
        "whole_copy.jpg",
        "a no-op crop must not change the name"
    );
    let got = load_image(&dest, 4096).unwrap();
    assert_eq!((got.width, got.height), (64, 48));
}

#[test]
fn a_real_crop_is_named_for_what_it_did() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "shot.jpg", &quadrants(64, 48), 0);
    let dest =
        save_transformed(&p, Xform::Rot90, Some(image_viewer::Crop::new(0, 0, 32, 32).unwrap()))
            .unwrap();
    assert_eq!(
        dest.file_name().unwrap().to_string_lossy(),
        "shot_cropped.jpg",
        "a crop that also turns should not be called _rotated"
    );
}

/// MCU snapping: the origin has to land on the block grid, and it has to *grow*
/// rather than shrink, so nothing the user framed is ever cut away.
#[test]
fn a_crop_snaps_onto_the_mcu_grid_without_losing_anything() {
    let (iw, ih) = (100u32, 70u32);
    // 4:2:0 is a 16 px block, the common case; 4:4:4 is 8.
    for (mw, mh) in [(16u32, 16u32), (8, 8)] {
        let asked = image_viewer::Crop::new(20, 13, 30, 25).unwrap();
        let got = asked.snapped_to_mcu(mw, mh, iw, ih).unwrap();
        assert_eq!(got.x % mw, 0, "{mw}px grid: origin not aligned ({got:?})");
        assert_eq!(got.y % mh, 0, "{mw}px grid: origin not aligned ({got:?})");
        assert!(
            got.x <= asked.x && got.y <= asked.y,
            "{mw}px grid: snapping moved the origin the wrong way ({got:?})"
        );
        assert!(
            got.right() >= asked.right() && got.bottom() >= asked.bottom(),
            "{mw}px grid: snapping cut into the selection ({got:?} vs {asked:?})"
        );
        assert!(got.right() <= iw && got.bottom() <= ih, "{mw}px grid: escaped the image");
    }
    // A crop already on the grid must come back untouched: no drift, no growth.
    let aligned = image_viewer::Crop::new(32, 32, 32, 16).unwrap();
    assert_eq!(
        aligned.snapped_to_mcu(16, 16, 100, 70).unwrap(),
        aligned,
        "an aligned crop must be a no-op"
    );
}

#[test]
fn an_empty_or_outside_crop_is_refused_rather_than_panicking() {
    assert!(image_viewer::Crop::new(0, 0, 0, 10).is_none(), "zero width");
    assert!(image_viewer::Crop::new(0, 0, 10, 0).is_none(), "zero height");
    let outside = image_viewer::Crop::new(500, 500, 10, 10).unwrap();
    assert!(outside.clamped_to(64, 48).is_none(), "entirely outside");
    // Partly outside is trimmed rather than refused.
    let partly = image_viewer::Crop::new(60, 40, 20, 20).unwrap();
    assert_eq!(partly.clamped_to(64, 48).unwrap(), image_viewer::Crop::new(60, 40, 4, 8).unwrap());
}

/// `from_display_rect` and `map` have to agree, or the frame drawn on screen is
/// not the frame that gets saved. Checked on every transform over a grid of
/// rectangles, in both directions: the stored rect must map forward to exactly
/// the display rect that produced it.
#[test]
fn a_display_rect_survives_the_round_trip_through_stored_space() {
    let (w, h) = (97u32, 61u32);
    for exif_tag in [0u16, 1, 2, 3, 4, 5, 6, 7, 8] {
        for user in Xform::ALL {
            let display = image_viewer::display_xform(exif_tag, user);
            let (dw, dh) = display.out_dims(w, h);
            for rect in [
                (0u32, 0u32, 16u32, 16u32),
                (16, 16, 32, 32),
                (dw / 2, dh / 2, 16, 16),
                (dw.saturating_sub(16), 0, 16, 16),
                (0, dh.saturating_sub(16), 16, 16),
            ] {
                if rect.2 == 0 || rect.3 == 0 {
                    continue;
                }
                let Some(stored) =
                    image_viewer::Crop::from_display_rect(exif_tag, user, rect, (w, h))
                else {
                    continue;
                };
                // Forward: stored -> display must land back on the same rect.
                let back = stored.to_display_rect(display, (w, h));
                assert_eq!(
                    (back.0, back.1, back.2, back.3),
                    rect,
                    "exif={exif_tag} {user:?}: {rect:?} -> stored {stored:?} -> {back:?}"
                );
            }
        }
    }
}

/// The dimming bands must cover the picture outside the frame **exactly**.
///
/// Found by a screenshot: the top and bottom of the image stayed at full
/// brightness while the left and right dimmed correctly. The bands were built as
/// corner rectangles — the top one ran from the picture's corner to the frame's
/// top-*left* corner — so only the corners of those edges were ever dimmed, and it
/// read as a rendering bug rather than an arithmetic one.
///
/// Area is the assertion, because it catches all three ways this can be wrong at
/// once: a band that is too small leaves a gap, one that is too large covers the
/// frame, and one that is misplaced shifts the total somewhere else. Checking that
/// the four sum to `picture - frame` cannot pass for any of them.
#[test]
fn the_dimming_bands_cover_exactly_what_is_being_cut_away() {
    let area = |r: [f32; 4]| ((r[2] - r[0]).max(0.0)) * ((r[3] - r[1]).max(0.0));

    for picture in [
        [0.0f32, 0.0, 800.0, 600.0],
        // Not starting at the origin, and a non-zero offset: a band written in
        // terms of the picture's corner is only right when the corner is (0, 0).
        [37.0, 12.0, 837.0, 612.0],
    ] {
        for frame in [
            [100.0f32, 80.0, 400.0, 300.0],
            [picture[0], picture[1], picture[2], picture[3]], // whole picture
            // Flush against each edge in turn, and hanging off it: a drag that
            // ends outside the image must not dim anything beyond the edge.
            [picture[0], picture[1], picture[0] + 50.0, picture[3]],
            [picture[2] - 50.0, picture[1], picture[2], picture[3]],
            [picture[0], picture[1], picture[2], picture[1] + 50.0],
            [picture[0], picture[3] - 50.0, picture[2], picture[3]],
            [-40.0f32, -40.0, picture[2] + 40.0, picture[3] + 40.0],
        ] {
            let bands = image_viewer::dim_bands(picture, frame);
            let covered: f32 = bands.iter().map(|b| area(*b)).sum();
            let frame_clipped = {
                let f = [
                    frame[0].max(picture[0]),
                    frame[1].max(picture[1]),
                    frame[2].min(picture[2]),
                    frame[3].min(picture[3]),
                ];
                area(f)
            };
            let want = area(picture) - frame_clipped;
            assert!(
                (covered - want).abs() < 0.5,
                "bands cover {covered} but the picture minus the frame is {want}\n  picture {picture:?}\n  frame {frame:?}\n  bands {bands:?}"
            );

            // And no band may reach into the frame, or the frame is not "saved".
            let [fx0, fy0, fx1, fy1] = [
                frame[0].max(picture[0]),
                frame[1].max(picture[1]),
                frame[2].min(picture[2]),
                frame[3].min(picture[3]),
            ];
            for b in bands {
                let overlap_x = (b[2].min(fx1) - b[0].max(fx0)).max(0.0);
                let overlap_y = (b[3].min(fy1) - b[1].max(fy0)).max(0.0);
                assert!(
                    overlap_x * overlap_y <= 0.0,
                    "band {b:?} covers part of the frame {frame:?} in picture {picture:?}"
                );
                // Nor may a band stick out of the picture.
                assert!(
                    b[0] >= picture[0] - 0.01
                        && b[1] >= picture[1] - 0.01
                        && b[2] <= picture[2] + 0.01
                        && b[3] <= picture[3] + 0.01,
                    "band {b:?} escapes the picture {picture:?}"
                );
            }
        }
    }
}

/// Specifically: the top and bottom bands must span the full width of the picture.
///
/// The area test above catches this, but not *which* band is wrong, and the bug
/// was only obvious once the bands were named. Two assertions, one per axis, so a
/// failure says which edge was left bright.
#[test]
fn the_top_and_bottom_bands_span_the_whole_picture() {
    let picture = [0.0f32, 0.0, 800.0, 600.0];
    let frame = [100.0f32, 80.0, 400.0, 300.0];
    let [top, bottom, left, right] = image_viewer::dim_bands(picture, frame);

    assert_eq!(
        (top[0], top[2]),
        (picture[0], picture[2]),
        "the top band must reach both edges of the picture, not just the frame's"
    );
    assert_eq!(
        (bottom[0], bottom[2]),
        (picture[0], picture[2]),
        "the bottom band must reach both edges of the picture, not just the frame's"
    );
    // The left and right bands reach the full height *only* where the top and
    // bottom bands are not, so the four never double-cover a corner.
    assert_eq!((left[1], left[3]), (frame[1], frame[3]));
    assert_eq!((right[1], right[3]), (frame[1], frame[3]));
}

// --- the per-image edit store ---------------------------------------------

/// A crop and a turn are one edit, not two. Two maps would make "the crop is
/// gone but the turn is not" a reachable state, and neither map's bound would be
/// the other's.
#[test]
fn a_crop_and_a_turn_are_one_edit_and_reset_clears_both() {
    let mut e = image_viewer::ImageEdits::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");

    e.turn(&a, 1);
    e.set_crop(&a, Some(image_viewer::Crop::new(16, 16, 32, 32).unwrap()));
    assert_eq!(e.xform(&a), Xform::Rot90);
    assert_eq!(e.crop(&a), Some(image_viewer::Crop::new(16, 16, 32, 32).unwrap()));
    assert_eq!(e.len(), 1, "one image, one entry");

    // Clearing the crop must leave the turn alone.
    e.clear_crop(&a);
    assert_eq!(e.crop(&a), None);
    assert_eq!(e.xform(&a), Xform::Rot90, "clearing a crop must not clear the turn");
    assert_eq!(e.len(), 1, "the turn alone is still an edit");

    // And reset is the other direction: everything, at once.
    e.set_crop(&a, Some(image_viewer::Crop::new(0, 0, 16, 16).unwrap()));
    e.reset(&a);
    assert!(e.is_empty(), "reset clears crop and transform together");
    assert!(e.get(&b).is_identity(), "and only that image");
}

/// Turning after cropping must not move the frame. The crop is in stored
/// coordinates, so it names the same *pixels* whatever is done to the picture
/// afterwards, and the drawn frame follows the image rather than staying put.
#[test]
fn turning_after_cropping_keeps_the_same_pixels_framed() {
    let mut e = image_viewer::ImageEdits::default();
    let a = PathBuf::from("a.jpg");
    let crop = image_viewer::Crop::new(48, 16, 32, 32).unwrap();
    e.set_crop(&a, Some(crop));

    // The frame is stored once and is not rewritten by a turn.
    e.turn(&a, 1);
    e.mirror_vertical(&a);
    assert_eq!(e.crop(&a), Some(crop), "the framed pixels must not change");

    // What the user sees does follow the image: the same stored rectangle maps to
    // a different place on a turned, mirrored picture.
    let stored = (96u32, 64u32);
    let before = crop.to_display_rect(image_viewer::display_xform(0, Xform::Identity), stored);
    let after = crop.to_display_rect(image_viewer::display_xform(0, Xform::Transpose), stored);
    assert_ne!(before, after, "the frame must move with the picture");
}

#[test]
fn an_edit_says_what_it_did() {
    let none = image_viewer::Edit::default();
    assert!(none.is_identity());
    assert_eq!(none.label(), "no transform");
    let turn = image_viewer::Edit { xform: Xform::Rot90, crop: None };
    assert!(!turn.is_identity());
    assert_eq!(turn.label(), "rotated 90° CW");
    let both = image_viewer::Edit {
        xform: Xform::FlipH,
        crop: Some(image_viewer::Crop::new(16, 16, 32, 48).unwrap()),
    };
    assert_eq!(both.label(), "cropped to 32×48, mirrored left-right");
    // Crop alone, and a rotation-only edit, are both non-identity.
    assert!(
        !image_viewer::Edit {
            xform: Xform::Identity,
            crop: Some(image_viewer::Crop::new(0, 0, 8, 8).unwrap()),
        }
        .is_identity()
    );
}
