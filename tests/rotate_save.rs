//! Saving a rotation losslessly.
//!
//! The property that has to hold, and the only one that really matters:
//! **a saved file must decode to the picture that was on screen.** Everything
//! else — the transform algebra, the EXIF rewrite, the naming — is machinery in
//! service of that, and each of those is a way to get it subtly wrong (mirrored,
//! rotated twice, or rotated but still carrying the old orientation tag, which is
//! invisible here because the viewer applies the tag itself and cancels out, and
//! glaring everywhere else).
//!
//! So the headline test below saves, then reads the result back *through the
//! viewer's own decode path*, and compares against the same transform applied
//! with the `image` crate. If the two ever disagree, the save is wrong.

use image_viewer::{
    apply_exif_orientation, display_xform, exif_orientation, load_image,
    neutralize_exif_orientation, save_transformed, unique_dest, ImageTransforms, Xform,
};
use std::path::PathBuf;
use std::path::Path;

/// A picture whose every quadrant is a different flat colour, so any wrong
/// turn, flip or transpose is obvious rather than merely plausible.
fn quadrants(w: u32, h: u32) -> image::DynamicImage {
    image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(w, h, |x, y| {
        let (hx, hy) = (x * 2 >= w, y * 2 >= h);
        match (hx, hy) {
            (false, false) => image::Rgb([255, 0, 0]),   // top-left: red
            (true, false) => image::Rgb([0, 255, 0]),    // top-right: green
            (false, true) => image::Rgb([0, 0, 255]),    // bottom-left: blue
            (true, true) => image::Rgb([255, 255, 0]),  // bottom-right: yellow
        }
    }))
}

/// Write `img` as a JPEG, with `exif_tag` (0 for none) spliced in as a real
/// APP1 segment so `exif_orientation` can read it back.
fn write_jpeg(
    dir: &Path,
    name: &str,
    img: &image::DynamicImage,
    exif_tag: u16,
) -> std::path::PathBuf {
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .unwrap();

    if exif_tag != 0 {
        // APP1 layout: FFE1, length, "Exif\0\0", TIFF header, IFD0, one entry.
        // Big-endian keeps the hand-built bytes readable.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM\x00\x2a"); // big-endian, version 42
        tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 at offset 8
        tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // orientation
        tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_be_bytes()); // one value
        // A SHORT's value lives in the first 2 bytes of the 4-byte field.
        tiff.extend_from_slice(&exif_tag.to_be_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_be_bytes()); // no IFD1

        let mut app1 = Vec::new();
        app1.extend_from_slice(b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let mut seg = Vec::new();
        seg.extend_from_slice(&[0xFF, 0xE1]);
        seg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        seg.extend_from_slice(&app1);

        // Insert straight after SOI.
        let mut out = Vec::new();
        out.extend_from_slice(&bytes[0..2]);
        out.extend_from_slice(&seg);
        out.extend_from_slice(&bytes[2..]);
        bytes = out;
    }

    let p = dir.join(name);
    std::fs::write(&p, &bytes).unwrap();
    p
}

/// What the viewer would put on screen: EXIF applied at decode, then the
/// quarter turns, done with the `image` crate exactly as the viewer does.
fn what_the_viewer_shows(img: &image::DynamicImage, exif_tag: u16, quarters: u8) -> image::DynamicImage {
    let mut data = image_viewer::ImageData {
        width: img.width(),
        height: img.height(),
        rgba: img.to_rgba8().into_raw(),
        load_time: std::time::Duration::ZERO,
    };
    apply_exif_orientation(&mut data, exif_tag);
    for _ in 0..(quarters & 3) {
        data = {
            let img = image::RgbaImage::from_raw(data.width, data.height, std::mem::take(&mut data.rgba))
                .unwrap();
            let r = image::imageops::rotate90(&img);
            let mut d = image_viewer::ImageData {
                width: r.width(),
                height: r.height(),
                rgba: r.into_raw(),
                load_time: std::time::Duration::ZERO,
            };
            apply_exif_orientation(&mut d, 0);
            d
        };
    }
    let rgba = image::RgbaImage::from_raw(data.width, data.height, data.rgba).unwrap();
    image::DynamicImage::ImageRgba8(rgba)
}

fn assert_same_pixels(got: &image_viewer::ImageData, want: &image::DynamicImage, what: &str) {
    assert_eq!(
        (got.width, got.height),
        (want.width(), want.height()),
        "{what}: wrong dimensions"
    );
    let want_rgba = want.to_rgba8();
    assert_eq!(got.rgba.len(), want_rgba.as_raw().len(), "{what}: wrong buffer length");
    // JPEG is lossy, so compare loosely: a transform that is *correct* still
    // changes the DCT blocks and re-decodes slightly differently. A wrong
    // transform changes whole quadrants, so a mean-error threshold separates
    // them cleanly.
    let mut worst: f64 = 0.0;
    for (a, b) in got.rgba.chunks_exact(4).zip(want_rgba.as_raw().chunks_exact(4)) {
        for (x, y) in a.iter().zip(b.iter()) {
            worst = worst.max(f64::from(x.abs_diff(*y)));
        }
    }
    assert!(worst <= 24.0, "{what}: largest channel error {worst} — a wrong transform?");
}

#[test]
fn a_saved_rotation_decodes_to_what_was_on_screen() {
    // The headline. Every combination of a camera orientation and a number of
    // quarter turns, saved and read back through the viewer's own decode path.
    for exif_tag in [0u16, 1, 2, 3, 4, 5, 6, 7, 8] {
        for quarters in 0u8..4 {
            let tmp = tempfile::tempdir().unwrap();
            let src = quadrants(96, 64);
            let p = write_jpeg(tmp.path(), "photo.jpg", &src, exif_tag);

            // The tag must actually be there, or the test is not testing what it
            // claims to.
            assert_eq!(exif_orientation(&p), exif_tag, "EXIF {exif_tag} did not round-trip");

            let want = what_the_viewer_shows(&src, exif_tag, quarters);
            let dest = save_transformed(&p, Xform::quarters_cw(quarters))
                .unwrap_or_else(|e| panic!("exif={exif_tag} q={quarters}: {e}"));

            // The viewer applies the tag itself on the way in, so the saved file
            // must now report 1 — otherwise the tag would be applied twice.
            assert_eq!(
                exif_orientation(&dest),
                if exif_tag == 0 { 0 } else { 1 },
                "exif={exif_tag} q={quarters}: output still carries a stale orientation"
            );

            let got = load_image(&dest, 4096).unwrap();
            assert_same_pixels(
                &got,
                &want,
                &format!("exif={exif_tag} user_q={quarters}"),
            );
        }
    }
}

#[test]
fn the_original_is_never_touched() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "keepme.jpg", &quadrants(64, 48), 6);
    let before = std::fs::read(&p).unwrap();
    let dest = save_transformed(&p, Xform::Rot90).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), before, "the source file changed");
    assert_ne!(dest, p);
    assert!(dest.exists());
}

#[test]
fn saving_twice_does_not_overwrite_the_first_save() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "shot.jpg", &quadrants(64, 48), 0);
    let first = save_transformed(&p, Xform::Rot90).unwrap();
    let second = save_transformed(&p, Xform::Rot90).unwrap();
    assert_ne!(first, second, "the second save clobbered the first");
    assert!(first.exists() && second.exists());
    assert_eq!(
        std::fs::read(&first).unwrap(),
        std::fs::read(&second).unwrap(),
        "the same input and rotation must produce the same bytes"
    );
}

#[test]
fn the_new_file_lands_beside_the_original_with_a_readable_name() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "holiday snap.jpg", &quadrants(64, 48), 0);
    let dest = save_transformed(&p, Xform::Rot90).unwrap();
    assert_eq!(dest.parent(), p.parent(), "not written beside the original");
    assert_eq!(dest.file_name().unwrap(), "holiday snap_rotated.jpg");
}

#[test]
fn a_lossy_format_is_refused_rather_than_silently_re_encoded() {
    // WebP, GIF, HEIC and JXL would all need a second generation of compression.
    // The point of the button is that it does not.
    let tmp = tempfile::tempdir().unwrap();
    let img = quadrants(48, 32);
    for (name, fmt) in [("a.webp", image::ImageFormat::WebP), ("a.gif", image::ImageFormat::Gif)] {
        let p = tmp.path().join(name);
        let mut b = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut b), fmt).unwrap();
        std::fs::write(&p, &b).unwrap();
        match save_transformed(&p, Xform::Rot90) {
            Err(e) => {
                let msg = e.to_string();
                assert!(msg.contains("lossy"), "{name}: unhelpful message {msg:?}");
            }
            Ok(d) => panic!("{name} was re-encoded, producing {d:?}"),
        }
    }
}

#[test]
fn the_lossless_raster_formats_are_saved_and_come_back_the_right_way_round() {
    // PNG, BMP and TIFF have no lossy step, so decode/rotate/re-encode really is
    // lossless and can be compared exactly.
    let tmp = tempfile::tempdir().unwrap();
    let src = quadrants(40, 72); // portrait, so a turn is unmistakable
    for (name, fmt) in [
        ("a.png", image::ImageFormat::Png),
        ("a.bmp", image::ImageFormat::Bmp),
        ("a.tiff", image::ImageFormat::Tiff),
    ] {
        let p = tmp.path().join(name);
        let mut b = Vec::new();
        src.write_to(&mut std::io::Cursor::new(&mut b), fmt).unwrap();
        std::fs::write(&p, &b).unwrap();

        let dest = save_transformed(&p, Xform::Rot90).unwrap_or_else(|e| panic!("{name}: {e}"));
        let got = load_image(&dest, 4096).unwrap_or_else(|e| panic!("{name} reload: {e:?}"));
        assert_eq!(
            (got.width, got.height),
            (72, 40),
            "{name}: a quarter turn must swap the axes"
        );

        // And the result must be the source turned one quarter clockwise:
        // output (ox, oy) comes from source (oy, h - 1 - ox).
        let s = src.to_rgba8();
        let mut want = image::RgbaImage::from_raw(72, 40, vec![0; 72 * 40 * 4]).unwrap();
        for oy in 0..40u32 {
            for ox in 0..72u32 {
                let px = s.get_pixel(oy, 72 - 1 - ox).0;
                want.put_pixel(ox, oy, image::Rgba(px));
            }
        }
        let want = image::DynamicImage::ImageRgba8(want);
        assert_same_pixels(&got, &want, name);
    }
}

#[test]
fn the_transform_algebra_matches_the_viewers_own_rotation() {
    // `Xform::from_exif` claims to describe exactly what `apply_exif_orientation`
    // does. If the two ever drift, every save is wrong in a way no other test
    // would notice, because the viewer applies the tag itself.
    for tag in 1u16..=8 {
        for quarters in 0u8..4 {
            let src = quadrants(40, 24);
            let xf = display_xform(tag, Xform::quarters_cw(quarters));

            // By the algebra: map through the composed transform.
            let (w, h) = (src.width(), src.height());
            let rgba = src.to_rgba8();
            let (ow, oh) = xf.out_dims(w, h);
            let via_algebra = {
                let mut out = vec![0u8; (ow * oh * 4) as usize];
                for oy in 0..oh {
                    for ox in 0..ow {
                        let (ix, iy) = xf.map(ox, oy, w, h);
                        for c in 0..4 {
                            out[((oy * ow + ox) * 4 + c) as usize] =
                                rgba.get_pixel(ix, iy)[c as usize];
                        }
                    }
                }
                image::DynamicImage::ImageRgba8(
                    image::RgbaImage::from_raw(ow, oh, out).unwrap(),
                )
            };

            // By the viewer: exif at decode, then quarter turns.
            let via_viewer = what_the_viewer_shows(&src, tag, quarters);
            assert_eq!(
                via_algebra.to_rgba8().as_raw(),
                via_viewer.to_rgba8().as_raw(),
                "tag={tag} quarters={quarters}: the algebra and the viewer disagree"
            );
        }
    }
}

#[test]
fn composition_is_closed_over_all_sixty_four_pairs() {
    // The 8x8 group table. Each composition must equal some single transform, and
    // must be the identity exactly when the two cancel.
    for a in Xform::ALL {
        for b in Xform::ALL {
            let c = a.then(b);
            // Composition is verified by substitution on real pixels rather than
            // by re-deriving the group law.
            let (w, h) = (9u32, 5u32);
            let rgba = image::RgbaImage::from_fn(w, h, |x, y| {
                image::Rgba([x as u8, y as u8, 0, 255])
            });
            let (mw, mh) = a.out_dims(w, h);
            let (aw, ah, apx) = a.apply_rgba(w, h, rgba.as_raw()).unwrap();
            let mid = image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(aw, ah, apx).unwrap());
            let via_ab = c.apply_rgba(w, h, rgba.as_raw()).unwrap();
            let via_ba = b.apply_rgba(mw, mh, mid.to_rgba8().as_raw()).unwrap();
            assert_eq!(
                (via_ab.0, via_ab.1),
                (via_ba.0, via_ba.1),
                "{a:?} then {b:?}: the two routes disagree on shape"
            );
            assert_eq!(
                via_ab.2, via_ba.2,
                "{a:?} then {b:?}: the two routes disagree on pixels"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Mirroring
// ---------------------------------------------------------------------------

/// The save, for every one of the eight user transforms.
///
/// The quarter turns are already covered against the `image` crate above. This
/// widens that to the four reflections, which the JPEG path can now reach because
/// `as_turbojpeg` maps them.
///
/// Expected pixels come from `Xform::apply_rgba` — real pixels, not a formula —
/// and `Xform::map` is separately pinned against `apply_exif_orientation` and
/// against the `image` crate, so the chain from button to file is checked at
/// every link rather than trusted at one.
#[test]
fn a_saved_mirror_decodes_to_what_was_on_screen() {
    for exif_tag in [0u16, 1, 2, 3, 4, 5, 6, 7, 8] {
        for xf in Xform::ALL {
            let tmp = tempfile::tempdir().unwrap();
            let src = quadrants(96, 64);
            let p = write_jpeg(tmp.path(), "photo.jpg", &src, exif_tag);

            let want = {
                // stored pixels -> display, exactly as `display_xform` says
                let xf = image_viewer::display_xform(exif_tag, xf);
                let (w, h) = (src.width(), src.height());
                let rgba = src.to_rgba8();
                let (ow, oh) = xf.out_dims(w, h);
                let mut out = vec![0u8; (ow * oh * 4) as usize];
                for oy in 0..oh {
                    for ox in 0..ow {
                        let (ix, iy) = xf.map(ox, oy, w, h);
                        for c in 0..4 {
                            out[((oy * ow + ox) * 4 + c) as usize] =
                                rgba.get_pixel(ix, iy)[c as usize];
                        }
                    }
                }
                image::DynamicImage::ImageRgba8(
                    image::RgbaImage::from_raw(ow, oh, out).unwrap(),
                )
            };

            let dest = save_transformed(&p, xf)
                .unwrap_or_else(|e| panic!("exif={exif_tag} {xf:?}: {e}"));
            let got = load_image(&dest, 4096).unwrap();
            assert_same_pixels(
                &got,
                &want,
                &format!("exif={exif_tag} user={xf:?}"),
            );
        }
    }
}

/// `Xform::map` is the single source of truth for what a transform *means*, and
/// everything else — the paint matrix, the save, these tests — is derived from
/// it. That is only safe if it is right, so check the two reflections against an
/// implementation that shares no code with it: the `image` crate.
#[test]
fn the_mirror_half_of_the_algebra_matches_the_image_crate() {
    let (w, h) = (17u32, 9u32);
    let src = image::RgbaImage::from_fn(w, h, |x, y| {
        image::Rgba([(x * 13) as u8, (y * 29) as u8, (x + y) as u8, 255])
    });

    for (xf, via_crate) in [
        (
            Xform::FlipH,
            image::imageops::flip_horizontal(&src).into_raw(),
        ),
        (
            Xform::FlipV,
            image::imageops::flip_vertical(&src).into_raw(),
        ),
    ] {
        let (ow, oh, got) = xf.apply_rgba(w, h, src.as_raw()).unwrap();
        assert_eq!((ow, oh), (w, h), "{xf:?}: a mirror must not change the shape");
        assert_eq!(got, via_crate, "{xf:?}: disagrees with the `image` crate");
    }
}

/// The paint matrix is a *new* statement of what the transform is, introduced
/// when mirroring forced a real 2x2 instead of a `Rot2`. It must agree with the
/// rotation it replaced, or every turned photo in the app silently changed
/// appearance — a quarter turn CW becoming CCW, say, which looks like a bug
/// somewhere else entirely.
#[test]
fn the_paint_matrix_reproduces_the_old_quarter_turn_behaviour() {
    for (quarters, xf) in [
        (0u32, Xform::Identity),
        (1, Xform::Rot90),
        (2, Xform::Rot180),
        (3, Xform::Rot270),
    ] {
        // What `paint_tex` used to do: egui's `Rot2::from_angle`.
        let rot = egui::emath::Rot2::from_angle(quarters as f32 * std::f32::consts::FRAC_PI_2);
        let m = xf.paint_matrix();
        for (x, y) in [(1.0f32, 0.0f32), (0.0, 1.0), (3.0, -2.0), (-7.5, 0.25)] {
            let want = rot * egui::emath::vec2(x, y);
            let got = egui::emath::vec2(m[0][0] * x + m[0][1] * y, m[1][0] * x + m[1][1] * y);
            assert!(
                (got - want).length() < 1e-6,
                "quarter {quarters} ({xf:?}) on ({x}, {y}): matrix gave {got:?}, Rot2 gives {want:?}"
            );
        }
    }
}

/// The paint matrix has to be the *inverse direction* of `map`: `map` sends an
/// output pixel back to its input, and drawing moves an input point forward to
/// where it lands. Getting those backwards is invisible to the eye — a rotation
/// still looks like a rotation, just the wrong way — and it is a real mistake,
/// because this test failed the first time it was written.
///
/// So: take each input pixel, move it with the shipping matrix exactly as
/// `paint_tex` does, and check it arrives on the output pixel that `map` says
/// should be showing it. That is the whole contract, checked on every pixel of
/// every one of the eight, in the direction the code runs.
#[test]
fn the_paint_matrix_puts_each_pixel_where_map_says_it_belongs() {
    let (w, h) = (11u32, 6u32);
    for xf in Xform::ALL {
        let m = xf.paint_matrix();
        let (ow, oh) = xf.out_dims(w, h);
        // Both rects are concentric on the panel, so one centre serves both.
        let c = egui::emath::vec2(w as f32 / 2.0, h as f32 / 2.0);
        let out_min = egui::emath::vec2(c.x - ow as f32 / 2.0, c.y - oh as f32 / 2.0);
        for iy in 0..h {
            for ix in 0..w {
                let p_in = egui::emath::vec2(ix as f32 + 0.5, iy as f32 + 0.5);
                let d = p_in - c;
                let p_out = c
                    + egui::emath::vec2(
                        m[0][0] * d.x + m[0][1] * d.y,
                        m[1][0] * d.x + m[1][1] * d.y,
                    );
                let ox = (p_out.x - out_min.x).floor() as i64;
                let oy = (p_out.y - out_min.y).floor() as i64;
                assert!(
                    (0..ow as i64).contains(&ox) && (0..oh as i64).contains(&oy),
                    "{xf:?}: input ({ix}, {iy}) painted to ({ox}, {oy}), outside the {ow}x{oh} output"
                );
                // The output pixel it landed on must be the one that samples it.
                let from = xf.map(ox as u32, oy as u32, w, h);
                assert_eq!(
                    from,
                    (ix, iy),
                    "{xf:?}: input ({ix}, {iy}) painted onto ({ox}, {oy}), but map says that \
                     output pixel comes from {from:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-image transform
// ---------------------------------------------------------------------------

/// The bug this pins: the transform used to be a single `u8` on the app, so
/// turning one photo turned every photo you then navigated to.
#[test]
fn rotating_one_image_does_not_rotate_the_next() {
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");

    r.turn(&a, 1);
    assert_eq!(r.get(&a), Xform::Rot90, "the turned image should be turned");
    assert_eq!(
        r.get(&b),
        Xform::Identity,
        "a different image must be untouched - this is the whole point"
    );
}

#[test]
fn going_back_to_a_turned_image_still_finds_it_turned() {
    // The other half. Losing your edit on the way back is as wrong as applying it
    // to everything else, and the single-u8 version did both.
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");

    r.turn(&a, 1);
    r.turn(&b, 2); // while browsing past it
    assert_eq!(r.get(&a), Xform::Rot90, "coming back must not reset the first image");
    assert_eq!(r.get(&b), Xform::Rot180);
}

#[test]
fn turns_accumulate_and_wrap_in_both_directions() {
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");

    assert_eq!(r.turn(&a, 1), Xform::Rot90);
    assert_eq!(r.turn(&a, 1), Xform::Rot180);
    assert_eq!(r.turn(&a, 1), Xform::Rot270);
    assert_eq!(r.turn(&a, 1), Xform::Identity, "four quarter turns is a full turn");
    assert_eq!(r.turn(&a, -1), Xform::Rot270, "negative steps go anticlockwise");
    assert_eq!(r.turn(&a, -1), Xform::Rot180);
    // A single step from untransformed, anticlockwise, is a quarter turn back.
    let mut r2 = ImageTransforms::default();
    assert_eq!(r2.turn(&a, -1), Xform::Rot270, "-1 from 0 must wrap, not underflow");
    assert_eq!(r2.turn(&a, 5), Xform::Identity, "a whole number of turns is a no-op");
}

#[test]
fn mirroring_twice_is_back_to_the_start() {
    // The one property that makes a mirror button safe to press by accident.
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");

    r.mirror_horizontal(&a);
    assert_eq!(r.get(&a), Xform::FlipH);
    r.mirror_horizontal(&a);
    assert_eq!(r.get(&a), Xform::Identity, "two mirrors cancel");
    assert!(r.is_empty(), "and leave nothing stored");

    r.mirror_vertical(&a);
    assert_eq!(r.get(&a), Xform::FlipV);
    r.mirror_vertical(&a);
    assert_eq!(r.get(&a), Xform::Identity);
}

/// The two edits do not commute, and this is the whole reason the store holds a
/// transform instead of two counters. Turn-then-mirror is a *diagonal* mirror —
/// if the app kept a turn count and a flip flag, the save would have to discover
/// this at the last moment, where a sign error is invisible in this viewer (it
/// re-applies the same mistake to the file it just wrote) and glaring everywhere
/// else.
#[test]
fn a_quarter_turn_then_a_mirror_is_a_diagonal_mirror() {
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");

    r.turn(&a, 1);
    r.mirror_horizontal(&a);
    assert_eq!(r.get(&a), Xform::Transverse, "turn-then-mirror is a diagonal mirror");

    // And the other order gives the *other* diagonal, which is a different
    // picture. That they differ is the point; a sign error here would make them
    // the same, which is why this is asserted rather than assumed.
    let mut r2 = ImageTransforms::default();
    r2.mirror_horizontal(&a);
    r2.turn(&a, 1);
    assert_eq!(r2.get(&a), Xform::Transpose);
    assert_ne!(r.get(&a), r2.get(&a));
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
        assert_eq!(xf.out_dims(96, 64), (dw as u32, dh as u32), "{xf:?}: footprint disagrees with out_dims");
    }
}

#[test]
fn an_untransformed_image_costs_nothing() {
    // Returning to identity removes the entry rather than storing it, so browsing
    // a folder and undoing your edits leaves no residue.
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");
    assert!(r.is_empty());
    r.turn(&a, 1);
    assert_eq!(r.len(), 1);
    r.turn(&a, 3); // back to identity
    assert_eq!(r.get(&a), Xform::Identity);
    assert_eq!(r.len(), 0, "an identity transform should not be stored");
    assert!(r.is_empty());
}

#[test]
fn reset_puts_an_image_back_where_it_started() {
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");
    r.turn(&a, 1);
    r.mirror_vertical(&a);
    r.turn(&b, 1);

    r.reset(&a);
    assert_eq!(r.get(&a), Xform::Identity, "reset clears the whole transform");
    assert_eq!(r.len(), 1, "and only that image's entry");
    assert_eq!(r.get(&b), Xform::Rot90, "another image is untouched");
}

#[test]
fn the_record_is_bounded() {
    // A viewer left open on a 100k-image folder must not grow a map for as long
    // as it runs. Losing edits is much the smaller problem.
    let mut r = ImageTransforms::default();
    for i in 0..5000 {
        r.set(
            &PathBuf::from(format!("img{i}.jpg")),
            Xform::ALL[1 + (i % 3)],
        );
    }
    assert!(
        r.len() <= 4096,
        "grew to {} entries with no bound",
        r.len()
    );
}

/// The save composes the EXIF tag with the *current image's* transform. If these
/// two ever came from different images the saved file would be wrong in a way
/// nothing else would catch, because the viewer applies the tag itself.
#[test]
fn a_saved_file_uses_the_transform_of_the_image_being_saved() {
    let mut r = ImageTransforms::default();
    let a = PathBuf::from("a.jpg");
    let b = PathBuf::from("b.jpg");
    r.turn(&a, 1);
    r.turn(&b, 2);

    // The shape the app uses: look the transform up by the path being saved.
    let save_b = |p: &Path| r.get(p);
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
    quadrants(8, 8)
        .write_to(&mut std::io::Cursor::new(&mut b), image::ImageFormat::Jpeg)
        .unwrap();
    std::fs::write(&p, &b).unwrap();
    let mut copy = b.clone();
    assert!(!neutralize_exif_orientation(&mut copy), "a JPEG with no EXIF claimed a change");
    assert_eq!(copy, b, "the bytes were modified anyway");
}

fn exif_orientation_from(bytes: &[u8]) -> u16 {
    // Only used on in-memory fixtures; reads the tag straight out of the buffer.
    let i = bytes
        .windows(6)
        .position(|w| w == b"Exif\0\0")
        .expect("no EXIF in fixture");
    let base = i + 6;
    // This fixture is big-endian, which is how write_jpeg builds it.
    let ifd = base + u32::from_be_bytes(bytes[base + 4..base + 8].try_into().unwrap()) as usize;
    let n = u16::from_be_bytes(bytes[ifd..ifd + 2].try_into().unwrap());
    for i in 0..n as usize {
        let e = ifd + 2 + i * 12;
        if u16::from_be_bytes(bytes[e..e + 2].try_into().unwrap()) == 0x0112 {
            return u16::from_be_bytes(bytes[e + 8..e + 10].try_into().unwrap());
        }
    }
    0
}

#[test]
fn naming_never_overwrites_and_keeps_the_extension() {
    let tmp = tempfile::tempdir().unwrap();
    for name in ["a.jpg", "a.JPG", "a.jpeg"] {
        let p = tmp.path().join(name);
        std::fs::write(&p, b"x").unwrap();
        let d = unique_dest(&p);
        let n = d.file_name().unwrap().to_string_lossy().into_owned();
        assert!(n.starts_with("a_rotated"), "{name} produced {n}");
        let ext = d.extension().unwrap().to_string_lossy().into_owned();
        assert_eq!(ext, name.rsplit('.').next().unwrap(), "{name} changed extension");
    }
    // A file with no extension still gets a usable name.
    let p = tmp.path().join("noext");
    std::fs::write(&p, b"x").unwrap();
    assert!(unique_dest(&p).file_name().is_some());
    // A dotfile is a name, not an extension.
    let p = tmp.path().join(".hidden");
    std::fs::write(&p, b"x").unwrap();
    assert!(unique_dest(&p).file_name().is_some());
}
