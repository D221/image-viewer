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
    neutralize_exif_orientation, save_transformed, unique_dest, ImageEdits, Xform,
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
            let dest = save_transformed(&p, Xform::quarters_cw(quarters), None)
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
    let dest = save_transformed(&p, Xform::Rot90, None).unwrap();
    assert_eq!(std::fs::read(&p).unwrap(), before, "the source file changed");
    assert_ne!(dest, p);
    assert!(dest.exists());
}

#[test]
fn saving_twice_does_not_overwrite_the_first_save() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "shot.jpg", &quadrants(64, 48), 0);
    let first = save_transformed(&p, Xform::Rot90, None).unwrap();
    let second = save_transformed(&p, Xform::Rot90, None).unwrap();
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
    let dest = save_transformed(&p, Xform::Rot90, None).unwrap();
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
        match save_transformed(&p, Xform::Rot90, None) {
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

        let dest = save_transformed(&p, Xform::Rot90, None).unwrap_or_else(|e| panic!("{name}: {e}"));
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

            let dest = save_transformed(&p, xf, None)
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
// Cropping
// ---------------------------------------------------------------------------

/// Every transform must be undone by its own inverse, in both orders.
///
/// The inverse exists only so a rectangle chosen on the *displayed* picture can
/// be expressed against the *stored* one. A wrong inverse does not corrupt
/// anything — it crops a perfectly good picture of the wrong part of the photo,
/// which is exactly the kind of bug that reads as "the crop is a bit off" and gets
/// lived with.
#[test]
fn every_transform_has_an_inverse_and_it_cancels_both_ways() {
    for xf in Xform::ALL {
        let inv = xf.inverse();
        assert_eq!(
            xf.then(inv),
            Xform::Identity,
            "{xf:?} then its inverse is not the identity"
        );
        assert_eq!(
            inv.then(xf),
            Xform::Identity,
            "the inverse of {xf:?} does not undo it from the other side"
        );
        // And it is a *real* inverse on real pixels, not just in the group.
        // `map` takes output coordinates plus the *input* dimensions, so going
        // forward is `xf.map(ox, oy, w, h)` and coming back is
        // `inv.map(stored, ow, oh)`. Swapping those two argument pairs is the
        // mistake this test exists to make loud.
        let (w, h) = (11u32, 7u32);
        let (ow, oh) = xf.out_dims(w, h);
        for oy in 0..oh {
            for ox in 0..ow {
                let (ix, iy) = xf.map(ox, oy, w, h);
                assert_eq!(
                    inv.map(ix, iy, ow, oh),
                    (ox, oy),
                    "{xf:?}: output ({ox}, {oy}) -> stored ({ix}, {iy}) did not come back"
                );
            }
        }
    }
}

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
                image::DynamicImage::ImageRgba8(
                    image::RgbaImage::from_raw(cw, ch, px).unwrap(),
                )
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
        "whole_rotated.jpg",
        "a no-op crop must not change the name"
    );
    let got = load_image(&dest, 4096).unwrap();
    assert_eq!((got.width, got.height), (64, 48));
}

#[test]
fn a_real_crop_is_named_for_what_it_did() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "shot.jpg", &quadrants(64, 48), 0);
    let dest = save_transformed(&p, Xform::Rot90, Some(image_viewer::Crop::new(0, 0, 32, 32).unwrap()))
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
    assert_eq!(
        partly.clamped_to(64, 48).unwrap(),
        image_viewer::Crop::new(60, 40, 4, 8).unwrap()
    );
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
    let after = crop.to_display_rect(
        image_viewer::display_xform(0, Xform::Transpose),
        stored,
    );
    assert_ne!(before, after, "the frame must move with the picture");
}

#[test]
fn an_edit_says_what_it_did() {
    let none = image_viewer::Edit::default();
    assert!(none.is_identity());
    assert_eq!(none.label(), "no transform");
    let turn = image_viewer::Edit {
        xform: Xform::Rot90,
        crop: None,
    };
    assert!(!turn.is_identity());
    assert_eq!(turn.label(), "rotated 90° CW");
    let both = image_viewer::Edit {
        xform: Xform::FlipH,
        crop: Some(image_viewer::Crop::new(16, 16, 32, 48).unwrap()),
    };
    assert_eq!(both.label(), "cropped to 32×48, mirrored left-right");
    // Crop alone, and a rotation-only edit, are both non-identity.
    assert!(!image_viewer::Edit {
        xform: Xform::Identity,
        crop: Some(image_viewer::Crop::new(0, 0, 8, 8).unwrap()),
    }
    .is_identity());
}

#[test]
fn a_named_file_beats_the_resume_point() {
    use image_viewer::startup_index;
    let files: Vec<PathBuf> = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
        .iter()
        .map(|n| PathBuf::from("/photos").join(n))
        .collect();

    // A file was named: that file, whatever the resume point says.
    assert_eq!(startup_index(&files, Some(&files[2]), Some("a.jpg")), 2);
    // Even when the resume point is the first file and the named one is last.
    assert_eq!(startup_index(&files, Some(&files[3]), Some("a.jpg")), 3);
    // And with no resume point at all.
    assert_eq!(startup_index(&files, Some(&files[1]), None), 1);
}

#[test]
fn without_a_named_file_the_resume_point_wins() {
    use image_viewer::startup_index;
    let files: Vec<PathBuf> = ["a.jpg", "b.jpg", "c.jpg", "d.jpg"]
        .iter()
        .map(|n| PathBuf::from("/photos").join(n))
        .collect();
    assert_eq!(startup_index(&files, None, Some("c.jpg")), 2, "resume must still work");
    // The resume entry is a bare file name, so a path stored in it still matches.
    assert_eq!(startup_index(&files, None, Some("/elsewhere/c.jpg")), 2);
}

#[test]
fn an_unmatched_named_or_resume_falls_back_to_the_first_image() {
    use image_viewer::startup_index;
    let files: Vec<PathBuf> = ["a.jpg", "b.jpg"].iter().map(|n| PathBuf::from("/p").join(n)).collect();
    // A named file that is not in the listing - deleted, or a filter hid it.
    assert_eq!(startup_index(&files, Some(&PathBuf::from("/p/gone.jpg")), None), 0);
    // A resume point naming something that is gone.
    assert_eq!(startup_index(&files, None, Some("gone.jpg")), 0);
    // Nothing at all.
    assert_eq!(startup_index(&files, None, None), 0);
    // And an empty listing must not panic.
    assert_eq!(startup_index(&[], Some(&PathBuf::from("/p/a.jpg")), Some("a.jpg")), 0);
}

/// `Gallery::empty` exists so start-up can skip a directory walk whose result it
/// throws away. It has to be genuinely empty — anything left in it would be a
/// gallery the user sees populate itself a moment later.
#[test]
fn an_empty_gallery_is_empty_and_can_still_be_filled() {
    use image_viewer::Gallery;
    let tmp = tempfile::tempdir().unwrap();
    for name in ["a.jpg", "b.png", "notes.txt"] {
        std::fs::write(tmp.path().join(name), b"x").unwrap();
    }

    let mut g = Gallery::empty();
    assert!(g.is_empty(), "empty() must not scan the folder");
    assert_eq!(g.index, 0);
    assert_eq!(g.len(), 0);

    // And the path it is used on still works: filling it finds the images and
    // leaves the non-image alone.
    g.load_all_from_folder(tmp.path());
    assert_eq!(g.len(), 2, "should find the two images and skip notes.txt");
}

// --- helpers for the crop tests -------------------------------------------

/// The stored picture turned the way the viewer shows it.
///
/// **One** application of the EXIF tag, and it comes from `display_xform`, which
/// already composes it. Calling `apply_exif_orientation` here as well applies it
/// twice, which for a mirror or a quarter turn cancels to the identity and makes
/// the expectation quietly wrong for every non-trivial tag — which is how this
/// helper was first written.
///
/// The algebra this relies on is not taken on trust:
/// `the_transform_algebra_matches_the_viewers_own_rotation` pins `display_xform`
/// against `apply_exif_orientation` for all nine tags, and
/// `every_transform_has_an_inverse_and_it_cancels_both_ways` pins the inverse on
/// real pixels. So the chain from button to file is checked at every link.
fn display_xform_pixels(
    img: &image::DynamicImage,
    exif_tag: u16,
    user: Xform,
) -> image::DynamicImage {
    let rgba = img.to_rgba8();
    let (ow, oh, px) = image_viewer::display_xform(exif_tag, user)
        .apply_rgba(img.width(), img.height(), rgba.as_raw())
        .unwrap();
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(ow, oh, px).unwrap())
}

/// Cut a rectangle out of an already-displayed RGBA image, in display pixels.
///
/// Hand-rolled rather than `image::imageops::crop_imm`, which needs `&mut` and so
/// cannot be pointed at a borrow. The point of the crop tests is that this
/// expectation shares no code with the implementation, so a one-line loop is
/// better than a clever one.
fn crop_display(rgba: &image::RgbaImage, rect: (u32, u32, u32, u32)) -> (u32, u32, Vec<u8>) {
    let (x, y, w, h) = rect;
    let stride = rgba.width() as usize * 4;
    let src = rgba.as_raw();
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        let start = (row as usize) * stride + (x as usize) * 4;
        out.extend_from_slice(&src[start..start + w as usize * 4]);
    }
    (w, h, out)
}


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
        assert_eq!(xf.out_dims(96, 64), (dw as u32, dh as u32), "{xf:?}: footprint disagrees with out_dims");
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
