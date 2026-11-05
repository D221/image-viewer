//! Gallery navigation, the grid, zoom/pan and fit-to-window.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

use image_viewer::{GRID_CELL, GRID_LABEL_H, Gallery, ZoomPan, grid_cols};

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

#[test]
fn test_exif_orientation_applied_to_load() {
    let tmp = tempfile::tempdir().unwrap();

    // A 3x2 JPEG tagged "rotate 90 CW" must come back 2x3.
    let p6 = tmp.path().join("rot90.jpg");
    std::fs::write(&p6, make_jpeg_with_orientation(6)).unwrap();
    let d = image_viewer::load_image(&p6, 1600).expect("load");
    assert_eq!((d.width, d.height), (2, 3));

    // Unrotated (tag 1) keeps the encoded dimensions.
    let p1 = tmp.path().join("normal.jpg");
    std::fs::write(&p1, make_jpeg_with_orientation(1)).unwrap();
    let d = image_viewer::load_image(&p1, 1600).expect("load");
    assert_eq!((d.width, d.height), (3, 2));

    // Preview path reports the rotated source dimensions too.
    let r = image_viewer::load_image_preview_full(&p6, 256).unwrap();
    assert_eq!(r.orig, Some((2, 3)));
}

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

    // zoom_by takes scroll delta; each unit is multiplied by 0.01
    zoom_pan.zoom_by(-20.0); // -20 * 0.01 = -0.2 => zoom = 0.8
    assert_eq!(zoom_pan.zoom, 0.8);

    zoom_pan.zoom_by(200.0); // 200 * 0.01 = 2.0 => zoom = 2.8
    assert_eq!(zoom_pan.zoom, 2.8);

    zoom_pan.reset();
    assert_eq!(zoom_pan.zoom, 1.0);
}

#[test]
fn test_fit_to_window() {
    let mut zoom_pan = ZoomPan::new();
    // Large image: should downscale to fit
    zoom_pan.fit_to_window(2000.0, 1000.0, 1000.0, 600.0);
    assert!(zoom_pan.zoom < 1.0);
    assert!(zoom_pan.pan_x == 0.0);
    assert!(zoom_pan.pan_y == 0.0);

    // Small image: should upscale to fit
    zoom_pan.zoom = 1.0;
    zoom_pan.fit_to_window(50.0, 50.0, 100.0, 100.0);
    assert!(zoom_pan.zoom > 1.0);
    assert!(zoom_pan.pan_x == 0.0);
    assert!(zoom_pan.pan_y == 0.0);
}

/// The grid geometry follows the cell size instead of the constant: 150pt is
/// six columns across 1000pt, 300pt is three, and the row pitch is always
/// cell + label + spacing.
#[test]
fn grid_geometry_follows_the_cell_size() {
    assert_eq!(image_viewer::grid_cols(1000.0, 150.0), 6);
    assert_eq!(image_viewer::grid_cols(1000.0, 300.0), 3);
    assert_eq!(image_viewer::grid_row_h(150.0), 178.0);
    assert_eq!(image_viewer::grid_row_h(300.0), 328.0);
}

/// Grid follow-scroll, the file-picker rule: a visible selection does not
/// move the view at all, and a selection outside it scrolls the minimum
/// distance to become visible. `None` means "leave the offset alone".
#[test]
fn grid_follow_scroll_leaves_a_visible_selection_alone() {
    // Rows are 100pt; the view shows 100..600.
    assert_eq!(image_viewer::grid_follow_offset(3, 100.0, 500.0, 100.0), None);
    // Edges count as visible: row 1 starts exactly at the top, row 5 ends
    // exactly at the bottom. Neither scrolls.
    assert_eq!(image_viewer::grid_follow_offset(1, 100.0, 500.0, 100.0), None);
    assert_eq!(image_viewer::grid_follow_offset(5, 100.0, 500.0, 100.0), None);
    // Below the view: align the row's bottom with the view's bottom.
    assert_eq!(image_viewer::grid_follow_offset(7, 100.0, 500.0, 100.0), Some(300.0));
    // Above the view: align the row's top with the view's top.
    assert_eq!(image_viewer::grid_follow_offset(0, 100.0, 500.0, 100.0), Some(0.0));
    // No measurement yet (grid just opened): pin the row to the top, which
    // is today's open behavior and the only sane thing with no viewport.
    assert_eq!(image_viewer::grid_follow_offset(9, 100.0, 0.0, 0.0), Some(900.0));
    // Never a negative offset, whatever the stored numbers say.
    assert_eq!(image_viewer::grid_follow_offset(0, 100.0, 500.0, -600.0), Some(0.0));
}

/// A shuffled show saves the folder's order on start and both off-ramps
/// restore it through one funnel. Asserted as counts, because the defect
/// this guards is a *third* site setting the flag directly: a stop path
/// that writes `slideshow = false` itself skips the restore and leaves the
/// folder shuffled with no way back.
#[test]
fn a_shuffled_show_saves_its_order_and_both_off_ramps_restore_it() {
    let src = main_src();
    let start = fn_body(&src, "fn start_slideshow(&mut self)").expect("start_slideshow");
    assert!(
        start.contains("image_viewer::shuffle("),
        "starting a shuffled show must walk a shuffled copy, not the folder order"
    );
    assert!(
        start.contains("slideshow_saved = Some("),
        "and it must save the folder's own order first, or stopping has \
         nothing to put back"
    );
    let stop = fn_body(&src, "fn stop_slideshow(&mut self)").expect("stop_slideshow");
    assert!(
        stop.contains("slideshow_saved.take()"),
        "stopping must consume the saved order exactly once: a take that is \
         missing restores nothing, and one that is not a take restores twice"
    );
    // The flag is written in exactly one place — inside stop_slideshow
    // itself. A second writer anywhere else is a stop path that skips the
    // restore and leaves the folder shuffled with no way back.
    assert_eq!(
        src.matches("self.slideshow = false").count(),
        1,
        "stopping the show must go through stop_slideshow alone: every direct \
         write of the flag elsewhere is an off-ramp that skips the restore"
    );
}

/// The grid follows the selection through the measured-offset rule, not by
/// pinning the row to the top: the decision reads last frame's scroll offset
/// and viewport height, and the frame writes both back for the next one.
/// Either half missing and the rule silently stops working — an unread
/// measurement means the decision runs on zeros, an unwritten one means it
/// runs on last week's folder.
#[test]
fn the_grid_follows_through_measured_offsets_not_a_pinned_row() {
    let src = main_src();
    let grid = fn_body(&src, "fn gallery_grid(&mut self").expect("gallery_grid");
    assert!(
        grid.contains("image_viewer::grid_follow_offset("),
        "the scroll decision must go through grid_follow_offset: pinning the \
         row to the top on every press is the bug this replaces"
    );
    assert!(
        !grid.contains("(sel / cols) as f32 * pitch"),
        "the old pin-to-top arithmetic must be gone, not just bypassed"
    );
    assert!(
        grid.contains("self.grid_scroll = grid_out.state.offset.y"),
        "the frame must write the measured offset back, or the next decision \
         runs on a stale folder's scroll"
    );
    assert!(
        grid.contains("self.grid_view_h = grid_out.inner_rect.height()"),
        "and the measured viewport height with it, or the visible-range check \
         below is against a window that no longer exists"
    );
}

/// A shuffle is a permutation: same set in, same set out, in an order that
/// is not the input order. The reorder half is probabilistic, so it runs
/// over 64 files, where shuffling to identity has probability 1/64!.
#[test]
fn shuffle_keeps_every_file_and_reorders() {
    let mut files: Vec<std::path::PathBuf> =
        (0..64).map(|i| std::path::PathBuf::from(format!("{i:03}.jpg"))).collect();
    let before = files.clone();
    image_viewer::shuffle(&mut files);
    let (mut a, mut b) = (before.clone(), files.clone());
    a.sort();
    b.sort();
    assert_eq!(a, b, "a shuffle must not lose, duplicate or invent files");
    assert_ne!(files, before, "a 64-file shuffle that changes nothing shuffled nothing");
}

#[test]
fn test_gallery_grid_cells_fit_row_and_register_clicks() {
    // Regression test for the gallery grid (G) view. Cells must be allocated
    // explicitly with `Sense::click()`. Wrapping them in `egui::Frame::show`
    // is wrong in two ways that both looked fine in a screenshot-less build:
    //
    //  1. `Frame::show` builds its child Ui from
    //     `ui.available_rect_before_wrap()`, so the frame is as wide as the
    //     rest of the row. The first cell swallows the whole row and every
    //     later cell is laid out past the panel edge — the "offscreen grid".
    //  2. `Frame::show` allocates its space with `Sense::hover()`, so
    //     `Response::clicked()` is never true and cells cannot be opened.
    //     (In 0.27 this was `Frame::begin` / `Frame::end`; 0.36 has only
    //     `Frame::show`.)
    use egui::{Event, FontDefinitions, PointerButton, RawInput};

    let ctx = egui::Context::default();
    ctx.set_fonts(FontDefinitions::empty()); // don't load fonts in tests

    // Pass 1: lay the row out and record where each cell landed.
    let mut input = RawInput::default();
    let mut rects = Vec::new();
    let mut panel = egui::Rect::ZERO;
    let mut cols = 0usize;
    // `gap` is read by `grid_row` from the layout that actually placed the cells,
    // rather than hardcoding egui's default `item_spacing`. The assertion is
    // "the cells tile with the spacing the row used", and egui's default is a
    // moving target across upgrades: 0.27 put 8pt here, 0.36 puts 4pt.
    let mut gap = 0.0f32;
    let _ = run_pass(&ctx, input.clone(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            panel = ui.available_rect_before_wrap();
            // Same column math gallery_grid uses.
            cols = grid_cols(panel.width(), GRID_CELL);
            let mut hits = Vec::new();
            grid_row(ui, cols, &mut rects, &mut hits, &mut gap);
        });
    });

    assert!(cols >= 2, "test needs a multi-cell row, got {cols}");
    assert_eq!(rects.len(), cols, "every cell of the row must be laid out");
    for r in &rects {
        assert!(
            (r.width() - GRID_CELL).abs() < 1.0
                && (r.height() - (GRID_CELL + GRID_LABEL_H)).abs() < 1.0,
            "cell must take exactly the size it asked for, got {r:?}"
        );
        assert!(
            r.max.x <= panel.max.x + 1.0,
            "cell at x={} escapes the panel ({:?}) — row is too wide",
            r.min.x,
            panel
        );
    }
    for pair in rects.windows(2) {
        assert!(
            (pair[1].min.x - pair[0].max.x - gap).abs() < 1.0,
            "cells must sit side by side with {gap}pt spacing, got {:?} then {:?}",
            pair[0],
            pair[1]
        );
    }

    // Pass 2: press the 3rd cell. Pass 3: release there. Only it may click.
    let target = rects[2].center();
    let mut clicks = Vec::new();
    for pass in 0..2 {
        input.events.push(Event::PointerMoved(target));
        input.events.push(Event::PointerButton {
            pos: target,
            button: PointerButton::Primary,
            pressed: pass == 0,
            modifiers: egui::Modifiers::default(),
        });
        let _ = run_pass(&ctx, input.clone(), |ui| {
            egui::CentralPanel::default().show(ui, |ui| {
                let mut out = Vec::new();
                let mut gap = 0.0f32;
                grid_row(ui, cols, &mut out, &mut clicks, &mut gap);
            });
        });
    }
    assert_eq!(clicks, vec![2], "only the pressed cell may report the click");
}

#[test]
fn test_path_arg_skips_flags() {
    use image_viewer::path_arg;

    // The plain case: one positional path.
    assert_eq!(path_arg(["C:\\photos"]).as_deref(), Some(std::path::Path::new("C:\\photos")));

    // Regression: `--perf` is a flag, not a path. `args().nth(1)` took it as one,
    // it matched neither file nor dir, and the app silently fell back to the
    // persisted folder. That is the exact invocation AGENTS.md documents
    // (`cargo run -- --perf <folder>`), and it produced a perf report for the
    // *wrong folder* — which is how a wrong performance conclusion survived
    // long enough to be committed.
    assert_eq!(
        path_arg(["--perf", "C:\\photos"]).as_deref(),
        Some(std::path::Path::new("C:\\photos"))
    );
    assert_eq!(
        path_arg(["--perf", "C:\\photos", "extra"]).as_deref(),
        Some(std::path::Path::new("C:\\photos"))
    );
    assert_eq!(
        path_arg(["--register", "--perf", "C:\\photos"]).as_deref(),
        Some(std::path::Path::new("C:\\photos"))
    );

    // A flag with no path at all is None, not a bogus path.
    assert_eq!(path_arg(["--perf"]), None);
    assert_eq!(path_arg(Vec::<String>::new()), None);

    // A bare flag is still None, so the resume path is taken.
    assert_eq!(path_arg(["--unregister"]), None);

    // A path that merely contains a dash is not a flag.
    assert_eq!(
        path_arg(["--perf", "C:\\my-photos\\a-b.jpg"]).as_deref(),
        Some(std::path::Path::new("C:\\my-photos\\a-b.jpg"))
    );

    // An argument that *starts* with a dash is a flag whatever it means, which
    // is why a relative path can be escaped as `.\-name.jpg`. And an empty
    // argument is not a path either - it would open the process's own directory.
    assert_eq!(path_arg(["-name.jpg"]), None);
    assert_eq!(path_arg([r".\-name.jpg"]).as_deref(), Some(std::path::Path::new(r".\-name.jpg")));
    assert_eq!(path_arg(["", "C:\\photos"]).as_deref(), Some(std::path::Path::new("C:\\photos")));
}

#[test]
fn test_fit_scale_no_upscale_centres_thumbnails() {
    use image_viewer::fit_scale_no_upscale;
    let area = (150.0_f32, 150.0_f32);

    // A big landscape thumb scales down to the cell width, aspect kept.
    let s = fit_scale_no_upscale((400.0, 300.0), area);
    assert!((s - 0.375).abs() < 1e-6, "got {s}");
    assert!((400.0 * s - 150.0).abs() < 1e-4, "must fill the width");
    assert!((300.0 * s - 112.5).abs() < 1e-4, "aspect must be kept");

    // A tall portrait thumb is bounded by the height instead.
    let s = fit_scale_no_upscale((200.0, 400.0), area);
    assert!((s - 0.375).abs() < 1e-6, "got {s}");
    assert!((400.0 * s - 150.0).abs() < 1e-4, "must fill the height");

    // A panorama is bounded by the width and stays short.
    let s = fit_scale_no_upscale((1000.0, 100.0), area);
    assert!((s - 0.15).abs() < 1e-6, "got {s}");
    assert!((100.0 * s - 15.0).abs() < 1e-4, "aspect must be kept");

    // Never upscale: a small image is centred at native size rather than
    // blown up into a blurry square.
    for natural in [(40.0_f32, 30.0_f32), (1.0, 1.0), (150.0, 150.0)] {
        assert_eq!(fit_scale_no_upscale(natural, area), 1.0, "{natural:?} must not be upscaled");
    }

    // Degenerate inputs must not produce NaN (that would silently blank cells).
    assert_eq!(fit_scale_no_upscale((0.0, 0.0), area), 1.0);
    assert_eq!(fit_scale_no_upscale((100.0, 100.0), (0.0, 0.0)), 1.0);
    assert!(fit_scale_no_upscale((100.0, 100.0), (1e9, 1.9)).is_finite());
}

#[test]
fn test_grid_thumbnail_is_centred_and_textured() {
    // Guards the gallery-grid thumbnail against two regressions that both
    // shipped once, and neither of which a compile or a screenshot-free build
    // would catch:
    //
    //  1. It stopped being *centred*: widgets place at the cursor, and a
    //     `Layout::top_down(Align::Center)` does not move them (a top-down
    //     flow's initial cursor runs to +infinity on the main axis, so
    //     `allocate_exact_size` centres only across the cross axis). The
    //     widget therefore lives in a Ui bounded to the computed rect.
    //  2. It stopped being *textured* (solid colour blocks) when the cell was
    //     painted by hand via `painter().image(...)`. egui owns that
    //     plumbing, so the widget is used and only the rect is computed here.
    //
    // A real egui pass is driven and the produced shapes are inspected.
    use egui::{Color32, ColorImage, Context, RawInput, Rect, TextureOptions, Vec2};

    let ctx = Context::default();
    ctx.set_fonts(egui::FontDefinitions::empty());

    let texels = [200_usize, 100_usize];
    let img = ColorImage::filled([texels[0], texels[1]], Color32::from_rgb(200, 30, 30));
    let tex = ctx.load_texture("thumb-regression", img, TextureOptions::LINEAR);

    // A cell's thumbnail area, as gallery_grid lays it out.
    let area = Rect::from_min_size(egui::Pos2::new(20.0, 20.0), Vec2::new(142.0, 150.0));
    let natural = Vec2::new(texels[0] as f32, texels[1] as f32);
    let scale =
        image_viewer::fit_scale_no_upscale((natural.x, natural.y), (area.width(), area.height()));
    let draw_size = (natural * scale).min(area.size());
    let draw = Rect::from_center_size(area.center(), draw_size);

    let out = run_pass(&ctx, RawInput::default(), |ui| {
        egui::CentralPanel::default().show(ui, |ui| {
            // Exactly what grid_cell does.
            let mut img_ui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(draw)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            img_ui.add(egui::Image::new(&tex).max_size(draw_size));
        });
    });

    // egui paints an image as a RectShape whose `brush` carries a texture id.
    // (0.36 moved `fill_texture_id`/`uv` off `RectShape` and behind an
    // `Option<Arc<Brush>>`; a plain `RectShape::filled` has `brush: None`.)
    let textured: Vec<_> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(r) => r.brush.as_ref().and_then(|b| {
                (b.fill_texture_id != egui::TextureId::default()).then_some((r.rect, b.uv))
            }),
            _ => None,
        })
        .collect();
    let (rect, uv) = *textured
        .first()
        .expect("no textured quad painted: the cell would show only its background");

    // Textured: a collapsed UV samples one texel and stretches it, which is
    // what a "solid colour" thumbnail is.
    assert!(uv.width() > 0.0 && uv.height() > 0.0, "UV must be non-degenerate, got {uv:?}");
    // Aspect kept.
    let drawn = rect.width() / rect.height();
    let source = texels[0] as f32 / texels[1] as f32;
    assert!(
        (drawn - source).abs() < 0.02,
        "aspect must be preserved: drew {drawn}, source {source}"
    );
    // Fits the cell, and fills it (this thumb is larger than the area).
    assert!(
        rect.width() <= area.width() + 0.5 && rect.height() <= area.height() + 0.5,
        "thumbnail {rect:?} must fit the area {area:?}"
    );
    assert!((rect.width() - area.width()).abs() < 0.5, "should fill the width");
    // Centred on both axes.
    assert!(
        (rect.center().x - area.center().x).abs() < 0.5
            && (rect.center().y - area.center().y).abs() < 0.5,
        "thumbnail {rect:?} must be centred in {area:?}, not stuck in a corner"
    );
}
