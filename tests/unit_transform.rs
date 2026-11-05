//! Transformed footprints, mesh coverage and the gallery scroll offset.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

use image_viewer::{GRID_CELL, GRID_LABEL_H, grid_row_h};

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

#[test]
fn test_xformed_footprint_only_swaps_when_the_transform_does() {
    use image_viewer::{Xform, xformed_footprint};
    let (w, h) = (4000.0_f32, 3000.0_f32);
    // The four turns and two of the four mirrors leave the shape alone; the two
    // diagonal mirrors swap it, because a diagonal reflection *is* a quarter turn
    // plus a flip.
    for (xf, swaps) in [
        (Xform::Identity, false),
        (Xform::Rot90, true),
        (Xform::Rot180, false),
        (Xform::Rot270, true),
        (Xform::FlipH, false),
        (Xform::FlipV, false),
        (Xform::Transpose, true),
        (Xform::Transverse, true),
    ] {
        assert_eq!(
            xformed_footprint(w, h, xf),
            if swaps { (h, w) } else { (w, h) },
            "{xf:?}: wrong footprint"
        );
        // And the library's own idea of the output size must agree, because the
        // fit, the pan limits and the tile grid are all computed from these two.
        assert_eq!(
            xf.out_dims(4000, 3000),
            (xformed_footprint(w, h, xf).0 as u32, xformed_footprint(w, h, xf).1 as u32),
            "{xf:?}: footprint and out_dims disagree"
        );
    }
}

#[test]
fn test_transformed_mesh_covers_the_swapped_footprint() {
    // The rotate and mirror buttons paint the image by moving its rect's mesh
    // vertices about the rect centre (see `paint_tex`). That only keeps the image
    // centred if the transform lands exactly on the footprint that fit and pan
    // clamping were computed against. If this premise were wrong, turning a photo
    // would swing it off-centre or leave a gap.
    //
    // Uses `Xform::paint_matrix` — the same matrix `paint_tex` uses — rather than
    // restating the transform here, because a copy of the mechanism in the test
    // would keep passing after the shipping code broke. (What the matrix *means*
    // is pinned separately, in `tests/rotate_save.rs`.)
    use egui::{Color32, Rect, Vec2};
    use image_viewer::Xform;

    for xform in Xform::ALL {
        let src = Vec2::new(300.0, 100.0);
        let scale = 0.5;
        let centre = egui::Pos2::new(400.0, 300.0);
        let rect = Rect::from_center_size(centre, src * scale);

        let mut mesh = egui::epaint::Mesh::with_texture(egui::TextureId::default());
        mesh.add_rect_with_uv(
            rect,
            Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
            Color32::WHITE,
        );
        if xform != Xform::Identity {
            let m = xform.paint_matrix();
            let origin = rect.center();
            for v in &mut mesh.vertices {
                let d = v.pos - origin;
                v.pos = origin
                    + Vec2::new(m[0][0] * d.x + m[0][1] * d.y, m[1][0] * d.x + m[1][1] * d.y);
            }
        }

        let (dw, dh) = image_viewer::xformed_footprint(src.x * scale, src.y * scale, xform);
        let want = Rect::from_center_size(centre, Vec2::new(dw, dh));

        let xs: Vec<f32> = mesh.vertices.iter().map(|v| v.pos.x).collect();
        let ys: Vec<f32> = mesh.vertices.iter().map(|v| v.pos.y).collect();
        let got = Rect::from_min_max(
            egui::Pos2::new(
                xs.iter().cloned().fold(f32::MAX, f32::min),
                ys.iter().cloned().fold(f32::MAX, f32::min),
            ),
            egui::Pos2::new(
                xs.iter().cloned().fold(f32::MIN, f32::max),
                ys.iter().cloned().fold(f32::MIN, f32::max),
            ),
        );
        assert!(
            (got.min.x - want.min.x).abs() < 0.01
                && (got.min.y - want.min.y).abs() < 0.01
                && (got.max.x - want.max.x).abs() < 0.01
                && (got.max.y - want.max.y).abs() < 0.01,
            "{xform:?}: transformed image {got:?} must fill the footprint {want:?}"
        );
        // And it must stay centred where the untransformed image was.
        assert!(
            (got.center() - centre).x.abs() < 0.01 && (got.center() - centre).y.abs() < 0.01,
            "{xform:?}: the transform must not move the centre"
        );
    }
}

#[test]
fn test_gallery_scroll_offset_lands_the_selected_row_in_view() {
    // Regression: opening the gallery did not focus the current image once you
    // had browsed a few images. `Response::scroll_to_me` cannot fix that,
    // because `ScrollArea::show_rows` only lays out the rows inside the
    // viewport - a selected row that starts off-screen is never built, so it
    // never issues a scroll request, and nothing moves.
    //
    // The fix writes the scroll offset once per *selection change*. Both halves
    // matter, and both had a failure mode:
    //  * not writing it at all -> the grid opens wherever it was left;
    //  * writing it every frame -> manual scrolling is fought, because any small
    //    scroll that keeps the selected row laid out gets yanked back.
    use egui::{Context, FontDefinitions, RawInput};

    const ROWS: usize = 200;
    const COLS: usize = 4;
    const SEL_ROW: usize = 137; // far down a long folder

    let ctx = Context::default();
    ctx.set_fonts(FontDefinitions::empty());

    /// Lay out the rows and report which ones egui actually built. `offset` of
    /// `None` means "do not touch the scroll offset at all" (note `Some(0.0)`
    /// is *not* the same: it would reset the persisted offset to zero).
    fn laid_out_rows(ctx: &Context, offset: Option<f32>) -> (Vec<usize>, f32) {
        let mut rows = Vec::new();
        let mut pitch = 0.0f32;
        let _ = run_pass(ctx, RawInput::default(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                pitch = grid_row_h(GRID_CELL) + ui.spacing().item_spacing.y;
                // Mirrors the real grid: auto_shrink([false, true]) is what
                // gives the scrollbar somewhere to live.
                let mut sa = egui::ScrollArea::vertical().auto_shrink([false, true]);
                if let Some(o) = offset {
                    sa = sa.vertical_scroll_offset(o);
                }
                sa.show_rows(ui, grid_row_h(GRID_CELL), ROWS, |ui, range| {
                    rows.extend(range.clone());
                    for row in range {
                        ui.horizontal(|ui| {
                            for _ in 0..COLS {
                                let (_rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(GRID_CELL, GRID_CELL + GRID_LABEL_H),
                                    egui::Sense::click(),
                                );
                                if row == SEL_ROW {
                                    // The rejected mechanism, kept only to show
                                    // it cannot rescue an off-screen row.
                                    resp.scroll_to_me(Some(egui::Align::Center));
                                }
                            }
                        });
                    }
                });
            });
        });
        (rows, pitch)
    }

    // 1. Fresh context, offset untouched: the selected row is off-screen and
    //    never built, so scroll_to_me has nothing to act on. This is the bug.
    let (rows, pitch) = laid_out_rows(&ctx, None);
    assert!(pitch > grid_row_h(GRID_CELL), "show_rows adds item spacing to the pitch");
    assert!(
        !rows.contains(&SEL_ROW),
        "row {SEL_ROW} must start off-screen, otherwise this test is not \
         exercising the bug (got {rows:?})"
    );

    // 2. The fix: one-shot offset at the selected row lands it in the band.
    let (rows, _) = laid_out_rows(&ctx, Some(SEL_ROW as f32 * pitch));
    assert!(
        rows.contains(&SEL_ROW),
        "selected row {SEL_ROW} must be laid out, got {rows:?} (pitch {pitch})"
    );

    // 3. A following frame that does NOT touch the offset keeps the selection
    //    in view (egui persists it) - so the viewer's own scrolling is never
    //    overridden while the selection is unchanged.
    let (rows, _) = laid_out_rows(&ctx, None);
    assert!(
        rows.contains(&SEL_ROW),
        "a pass that does not set the offset must keep the selected row in \
         view; got {rows:?}"
    );
}
