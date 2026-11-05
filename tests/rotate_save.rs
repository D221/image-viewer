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
    Xform, display_xform, exif_orientation, load_image, save_transformed, unique_dest,
};
use std::path::PathBuf;

#[path = "common/rotate_fixtures.rs"]
mod rotate_fixtures;

use rotate_fixtures::*;

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
            assert_same_pixels(&got, &want, &format!("exif={exif_tag} user_q={quarters}"));
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

/// The name says what the file *is*: no turn and no crop is a copy, not a
/// rotation. Ctrl+S with nothing edited used to write `photo_rotated.jpg`.
#[test]
fn an_unedited_save_is_named_copy_not_rotated() {
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "photo.jpg", &quadrants(64, 48), 0);
    let dest = save_transformed(&p, Xform::Identity, None).unwrap();
    assert_eq!(dest.file_name().unwrap(), "photo_copy.jpg");
    // And it is still the same picture.
    let got = image_viewer::load_image(&dest, 64).unwrap();
    assert_same_pixels(&got, &quadrants(64, 48), "unedited save");
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

        let dest =
            save_transformed(&p, Xform::Rot90, None).unwrap_or_else(|e| panic!("{name}: {e}"));
        let got = load_image(&dest, 4096).unwrap_or_else(|e| panic!("{name} reload: {e:?}"));
        assert_eq!((got.width, got.height), (72, 40), "{name}: a quarter turn must swap the axes");

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
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(ow, oh, out).unwrap())
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
            let rgba =
                image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([x as u8, y as u8, 0, 255]));
            let (mw, mh) = a.out_dims(w, h);
            let (aw, ah, apx) = a.apply_rgba(w, h, rgba.as_raw()).unwrap();
            let mid =
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(aw, ah, apx).unwrap());
            let via_ab = c.apply_rgba(w, h, rgba.as_raw()).unwrap();
            let via_ba = b.apply_rgba(mw, mh, mid.to_rgba8().as_raw()).unwrap();
            assert_eq!(
                (via_ab.0, via_ab.1),
                (via_ba.0, via_ba.1),
                "{a:?} then {b:?}: the two routes disagree on shape"
            );
            assert_eq!(via_ab.2, via_ba.2, "{a:?} then {b:?}: the two routes disagree on pixels");
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
                image::DynamicImage::ImageRgba8(image::RgbaImage::from_raw(ow, oh, out).unwrap())
            };

            let dest = save_transformed(&p, xf, None)
                .unwrap_or_else(|e| panic!("exif={exif_tag} {xf:?}: {e}"));
            let got = load_image(&dest, 4096).unwrap();
            assert_same_pixels(&got, &want, &format!("exif={exif_tag} user={xf:?}"));
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
        (Xform::FlipH, image::imageops::flip_horizontal(&src).into_raw()),
        (Xform::FlipV, image::imageops::flip_vertical(&src).into_raw()),
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
    for (quarters, xf) in
        [(0u32, Xform::Identity), (1, Xform::Rot90), (2, Xform::Rot180), (3, Xform::Rot270)]
    {
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
                let p_out = c + egui::emath::vec2(
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
        assert_eq!(xf.then(inv), Xform::Identity, "{xf:?} then its inverse is not the identity");
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
    let files: Vec<PathBuf> =
        ["a.jpg", "b.jpg"].iter().map(|n| PathBuf::from("/p").join(n)).collect();
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

/// Screen-size save (Tier 1 #6): the same crop and turn as Ctrl+S, but the
/// pixels are downscaled to the display size instead of kept full-res.
#[test]
fn screen_size_save_fits_the_display_and_says_re_encoded() {
    use image_viewer::save_transformed_scaled;
    let tmp = tempfile::tempdir().unwrap();
    let p = write_jpeg(tmp.path(), "photo.jpg", &quadrants(256, 192), 0);
    let out = save_transformed_scaled(&p, Xform::Rot90, None, 100).unwrap();
    assert_eq!(
        out.dest.file_name().unwrap(),
        "photo_screen.jpg",
        "screen saves are named, not numbered beside rotated ones"
    );
    // 256x192 turned is 192x256; long side 256 must fit in 100.
    assert!(
        out.width.max(out.height) <= 100,
        "long side must fit the display: {}x{}",
        out.width,
        out.height
    );
    assert!(!out.lossless, "a JPEG screen save is a re-encode, and must say so");
    // And it must still be the turned picture, not the stored one.
    assert_eq!((out.width, out.height), (75, 100));
}

/// PNG has no lossy step, so its screen save stays lossless.
#[test]
fn screen_size_save_of_png_stays_lossless() {
    use image_viewer::save_transformed_scaled;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("a.png");
    let mut buf = Vec::new();
    quadrants(200, 100)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    std::fs::write(&p, &buf).unwrap();
    let out = save_transformed_scaled(&p, Xform::Identity, None, 100).unwrap();
    assert!(out.lossless);
    assert_eq!((out.width, out.height), (100, 50));
}

/// Formats the lossless path refuses stay refused at screen size too.
///
/// **The refusal moved and its wording changed, and both are the point.** The
/// format is now decided by the *destination*, which for a screen save is
/// derived from the source name — so a `.gif` source still lands on a `.gif`
/// destination and is still refused, but because there is no GIF encoder rather
/// than because re-encoding would be lossy. The old message ("would have to be
/// re-encoded, which is lossy") described a problem the user cannot fix by
/// choosing a different save-as format; the new one names the formats that
/// exist.
///
/// So the assertion is on the *refusal*, not on the old sentence: what must not
/// happen is a GIF coming back re-encoded under a name that says GIF.
#[test]
fn screen_size_save_refuses_a_format_it_cannot_write() {
    use image_viewer::save_transformed_scaled;
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("a.gif");
    let mut buf = Vec::new();
    quadrants(48, 32)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Gif)
        .unwrap();
    std::fs::write(&p, &buf).unwrap();
    match save_transformed_scaled(&p, Xform::Identity, None, 100) {
        Err(e) => {
            let msg = e.to_string();
            assert!(
                msg.contains("gif") || msg.contains("lossy"),
                "the refusal must name the problem, got: {msg}"
            );
        }
        Ok(d) => panic!("gif was re-encoded to {:?}", d.dest),
    }
}

/// Save-as writes exactly where it is told, in the format it is told, at the
/// quality it is told - and the numbers are all *load-bearing*, which is why
/// each is asserted rather than described.
///
/// The quality claim in particular cannot be "the slider moved": a JPEG encoder
/// given a smaller number must produce a smaller file, and the only evidence
/// that the number reached the encoder at all is the size ratio.
/// `bench_rotate_save` uses the same ratio for the lossless claim, so the two
/// assertions are the same kind of evidence.
#[test]
fn save_as_writes_the_named_path_at_the_named_quality() {
    use image_viewer::{Xform, save_transformed_to};
    let tmp = tempfile::tempdir().unwrap();
    let src = write_jpeg(tmp.path(), "photo.jpg", &quadrants(256, 192), 0);

    // A named destination, exactly that path - save-as does not invent a name.
    let dest = tmp.path().join("out.jpg");
    let out = save_transformed_to(&src, Xform::Rot90, None, &dest, 90, 0).unwrap();
    assert_eq!(out.dest, dest, "the file is where it was told to go");
    assert!(out.dest.is_file(), "and it exists");
    assert_eq!((out.width, out.height), (192, 256), "turned, and unscaled (max_long = 0)");
    assert!(!out.lossless, "a JPEG encode is a re-encode");
    assert!(image_viewer::is_jpeg_bytes(&std::fs::read(&dest).unwrap()));

    // Quality reaches the encoder: lower quality, smaller file. Both encode the
    // *same* picture, so the only variable is the number.
    let hi = tmp.path().join("hi.jpg");
    let lo = tmp.path().join("lo.jpg");
    save_transformed_to(&src, Xform::Identity, None, &hi, 100, 0).unwrap();
    save_transformed_to(&src, Xform::Identity, None, &lo, 40, 0).unwrap();
    let hi_len = std::fs::metadata(&hi).unwrap().len();
    let lo_len = std::fs::metadata(&lo).unwrap().len();
    assert!(
        lo_len < hi_len,
        "quality 40 ({} bytes) must be smaller than quality 100 ({} bytes); \
         identical sizes mean the number never reached the encoder",
        lo_len,
        hi_len
    );

    // max_long is a bound on the long side and never upscales: a 256px source
    // asked for 1000 stays 256. Upscaling would invent pixels nobody had.
    let small =
        save_transformed_to(&src, Xform::Identity, None, &tmp.path().join("s.jpg"), 85, 1000)
            .unwrap();
    assert_eq!((small.width, small.height), (256, 192), "no upscale");
}

/// The format follows the *destination*, not the source: a PNG source saved as
/// `.jpg` must be a JPEG, or the file's extension lies about its contents.
#[test]
fn save_as_encodes_the_chosen_format_not_the_source_format() {
    use image_viewer::{Xform, save_transformed_to};
    let tmp = tempfile::tempdir().unwrap();
    let mut buf = Vec::new();
    quadrants(64, 48)
        .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
        .unwrap();
    let src = tmp.path().join("src.png");
    std::fs::write(&src, &buf).unwrap();

    let as_jpeg = tmp.path().join("out.jpg");
    let out = save_transformed_to(&src, Xform::Identity, None, &as_jpeg, 85, 0).unwrap();
    assert!(
        image_viewer::is_jpeg_bytes(&std::fs::read(&as_jpeg).unwrap()),
        "a .jpg destination is written as JPEG whatever the source was"
    );
    assert!(!out.lossless, "JPEG has a lossy step, so the result says so");

    let as_png = tmp.path().join("out.png");
    let out = save_transformed_to(&src, Xform::Identity, None, &as_png, 85, 0).unwrap();
    assert!(
        std::fs::read(&as_png).unwrap().starts_with(b"\x89PNG\r\n\x1a\n"),
        "a .png destination is written as PNG"
    );
    assert!(out.lossless, "PNG has no lossy step");
}

/// What the pixels look like is the same property the lossless save is tested
/// for, and save-as must not lose it: the file it writes decodes to the
/// transform that was applied.
#[test]
fn a_save_as_file_is_the_transform_that_was_on_screen() {
    use image_viewer::{Xform, save_transformed_to};
    let tmp = tempfile::tempdir().unwrap();
    let src = write_jpeg(tmp.path(), "photo.jpg", &quadrants(128, 96), 0);

    let dest = tmp.path().join("turned.jpg");
    save_transformed_to(&src, Xform::Rot90, None, &dest, 92, 0).unwrap();

    // Decode both through `image` - the encoder used the same pixels, modulo
    // JPEG's lossy step, which flat colour survives at q92.
    let got = image::open(&dest).unwrap().to_rgb8();
    let want = quadrants(128, 96).rotate90().to_rgb8();
    assert_eq!(
        got.dimensions(),
        want.dimensions(),
        "turned dimensions: {}x{} vs {}x{}",
        got.width(),
        got.height(),
        want.width(),
        want.height()
    );
    // Compare quadrant centres: JPEG at q92 on flat colour is exact enough that
    // a wrong turn would show as a mismatch of hundreds of levels.
    for &(x, y) in &[(10, 10), (60, 10), (10, 40), (60, 40)] {
        let a = got.get_pixel(x, y).0;
        let b = want.get_pixel(x, y).0;
        for (i, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
            assert!(
                (*av as i32 - *bv as i32).abs() <= 12,
                "channel {} at ({}, {}) is {} vs {} - the saved picture is not the \
                 transformed one",
                i,
                x,
                y,
                av,
                bv
            );
        }
    }
}

/// An unwritable destination is an error, never a silent fallback to some
/// other path. Saving somewhere the user did not choose is the one outcome
/// worse than failing.
#[test]
fn save_as_refuses_a_destination_it_cannot_write() {
    use image_viewer::{Xform, save_transformed_to};
    let tmp = tempfile::tempdir().unwrap();
    let src = write_jpeg(tmp.path(), "photo.jpg", &quadrants(32, 24), 0);
    // A parent that does not exist.
    let dest = tmp.path().join("no-such-dir").join("out.jpg");
    assert!(
        save_transformed_to(&src, Xform::Identity, None, &dest, 85, 0).is_err(),
        "writing into a missing directory must fail rather than pick another path"
    );
}
/// The infix a saved copy is named for is a **shared** rule, and the report was
/// that the two callers answered differently to the same question: `Ctrl+S` said
/// `_copy` and save-as said `_rotated`, for the same unedited picture.
///
/// The assertion is on [`save_infix`] as a function, because that is what both
/// call sites use — a test that only checked one caller would still be green
/// with the other one reading the wrong value.
#[test]
fn an_unedited_save_is_a_copy_and_a_turned_one_is_a_rotated_one() {
    use image_viewer::{Crop, Xform, save_infix};

    // The reported case, exactly: nothing turned, nothing cropped.
    assert_eq!(
        save_infix(Xform::Identity, None),
        "copy",
        "an unedited save is a copy, not a rotated one"
    );

    // Every non-identity symmetry is still a rotation for naming purposes.
    for x in [
        Xform::Rot90,
        Xform::Rot180,
        Xform::Rot270,
        Xform::FlipH,
        Xform::FlipV,
        Xform::Transpose,
        Xform::Transverse,
    ] {
        assert_eq!(save_infix(x, None), "rotated", "{x:?} moves the picture");
    }

    // A crop wins over the transform name, because a crop turns the picture too
    // and `_rotated` would then be true and useless — `_cropped` is what the
    // user actually did.
    let c = Crop { x: 0, y: 0, w: 10, h: 10 };
    assert_eq!(save_infix(Xform::Identity, Some(c)), "cropped");
    assert_eq!(save_infix(Xform::Rot90, Some(c)), "cropped");
}
