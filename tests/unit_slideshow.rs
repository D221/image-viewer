//! Slideshow pacing, frame stepping, the tile loop and the histogram.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// Slideshow pacing: due once the interval has passed, never early, and a
/// zero interval floors at one second rather than strobing every frame.
#[test]
fn slideshow_advances_only_when_due() {
    let now = std::time::Instant::now();
    let ago = now - std::time::Duration::from_secs(10);
    assert!(image_viewer::slideshow_due(ago, now, 3));
    assert!(!image_viewer::slideshow_due(now, now, 3));
    assert!(image_viewer::slideshow_due(ago, now, 0));
    assert!(!image_viewer::slideshow_due(now, now, 0));
}

/// Frame stepping wraps at both ends and never divides by zero; resume
/// offsets sum the same floored delays the paint clock uses; presets clamp
/// to the sane range.
#[test]
fn stepping_wraps_and_presets_clamp() {
    assert_eq!(image_viewer::step_frame(5, 4, true), 0);
    assert_eq!(image_viewer::step_frame(5, 0, false), 4);
    assert_eq!(image_viewer::step_frame(5, 2, true), 3);
    assert_eq!(image_viewer::step_frame(0, 0, true), 0);
    assert_eq!(image_viewer::step_frame(0, 7, false), 0);

    let anim = image_viewer::Animation {
        frames: Vec::new(),
        delays: vec![
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(0),
            std::time::Duration::from_millis(50),
        ],
    };
    // 100 + floored-20 + 0 before frame 2: the middle delay floors like
    // `total_duration` floors it, or resume disagrees with paint.
    assert_eq!(image_viewer::anim_frame_start(&anim, 2), std::time::Duration::from_millis(120));
    assert_eq!(image_viewer::anim_frame_start(&anim, 99), std::time::Duration::from_millis(170));

    assert_eq!(image_viewer::zoom_preset(&[1.0, 2.0, 4.0, 8.0], 0), 1.0);
    assert_eq!(image_viewer::zoom_preset(&[1.0, 2.0, 4.0, 8.0], 9), 8.0);
    assert_eq!(image_viewer::zoom_preset(&[0.0, -3.0, 100.0, 2.0], 0), 0.1);
    assert_eq!(image_viewer::zoom_preset(&[0.0, -3.0, 100.0, 2.0], 2), 32.0);
}

/// The zoom readout is a ratio against 1:1, not against fit: 100% is one
/// source pixel per physical pixel whatever the window is, so the number
/// means the same on a panorama and a thumbnail.
#[test]
fn zoom_percent_reports_against_one_to_one() {
    // 1:1 is `scale == 1/ppp` — the value the draw path itself uses — so it
    // must read exactly 100 at every pixel ratio, not approximately.
    assert_eq!(image_viewer::zoom_percent(1.0, 1.0), 100);
    assert_eq!(image_viewer::zoom_percent(1.0 / 1.25, 1.25), 100);
    assert_eq!(image_viewer::zoom_percent(1.0 / 2.0, 2.0), 100);
    // A fitted 17000px panorama on a ~1000pt panel is single digits.
    assert_eq!(image_viewer::zoom_percent(0.06, 1.0), 6);
    // And deep zoom is the large number it is, not clamped to something neat.
    assert_eq!(image_viewer::zoom_percent(50.0, 1.0), 5000);
}

/// The tile loop's range and rect math, pinned against the deep-zoom ghost:
/// on a source past `MAX_TEXTURE_DIM` the tile buffer is a half-res DCT
/// decode, and naming the range or the rect in source pixels composes a
/// small copy of the whole image over its own top-left.
#[test]
fn tile_math_positions_tiles_in_buffer_pixels_at_source_pitch() {
    // Full-res buffer (k == 1): the old arithmetic exactly. paint at 0,
    // scale 0.5, visible 100..600 of a 2000px buffer.
    assert_eq!(
        image_viewer::tile_axis_range(100.0, 600.0, 0.0, 0.5, 1.0, 2000),
        (200, 1200),
        "k=1 reproduces the pre-fix numbers bit for bit"
    );
    assert_eq!(
        image_viewer::tile_draw_rect((0.0, 0.0), 1024, 0, 1024, 1024, 0.5, 1.0, 1.0),
        [512.0, 0.0, 512.0, 512.0]
    );
    // Half-res buffer (k == 2), the booklet shape: 17042px source in an
    // 8521px buffer at scale 0.125. The visible 0..1067.5pt is buffer
    // 0..4268, not source 0..8534 — naming it in source pixels cuts every
    // tile from twice as far along as it is drawn.
    assert_eq!(image_viewer::tile_axis_range(0.0, 1067.5, 0.0, 0.125, 2.0, 8521), (0, 4270));
    // And the rect pitches the buffer span back up: tile (2,0) covers buffer
    // 2048..3072, i.e. source 4096..6144, i.e. screen 512..768.
    assert_eq!(
        image_viewer::tile_draw_rect((0.0, 0.0), 2048, 0, 1024, 1024, 0.125, 2.0, 2.0),
        [512.0, 0.0, 256.0, 256.0]
    );
    // Content and position agree: the drawn span is the buffer span pitched
    // up, so the tile shows the source pixels it is drawn over.
    let (x0, x1) = image_viewer::tile_axis_range(512.0, 768.0, 0.0, 0.125, 2.0, 8521);
    assert_eq!((x0, x1), (2048, 3072));
    // Clamping is against the buffer, not the source.
    assert_eq!(image_viewer::tile_axis_range(-100.0, 99999.0, 0.0, 0.125, 2.0, 8521), (0, 8521));
}

/// The tile loop goes through the ratio-aware math, not around it: the old
/// inline formula named the range in source pixels whatever the buffer was,
/// which is the defect above, so its absence is asserted as well as the new
/// calls' presence. Either half alone passes on a halfway revert.
#[test]
fn the_tile_loop_positions_through_the_ratio_aware_math() {
    let src = main_src();
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    assert!(
        central.contains("image_viewer::tile_axis_range("),
        "the tile range must come from tile_axis_range, which divides by the \
         buffer-to-source ratio"
    );
    assert!(
        central.contains("image_viewer::tile_draw_rect("),
        "the tile rect must come from tile_draw_rect, which pitches the buffer \
         span back up to source pixels"
    );
    assert!(
        !central.contains("(vis.min.x - paint.min.x) / scale)"),
        "the old inline range named source pixels for every buffer, which is \
         what squeezed a half-size copy over the image's own top-left"
    );
}

/// A histogram is exact on synthetic buffers: a solid color fills exactly
/// one bin per channel, a gradient fills every bin once, and clipping
/// counts are exact pixel counts. Ground truth by construction — no P3,
/// no reference renderer, just counting.
#[test]
fn histogram_counts_bins_and_clipped_pixels_exactly() {
    // Solid red 2x2: R[255]=4, G[0]=4, B[0]=4, nothing clipped.
    let red = vec![255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255];
    let h = image_viewer::histogram_rgba(&red, 2, 2);
    assert_eq!(h.bins[0][255], 4);
    assert_eq!(h.bins[1][0], 4);
    assert_eq!(h.bins[2][0], 4);
    assert_eq!(h.bins[0][0], 0);
    assert_eq!(h.black, 0);
    assert_eq!(h.white, 0);
    // Half pure black, half pure white: clipping is exact.
    let bw = vec![0u8, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255, 255, 255, 255, 255, 255];
    let h = image_viewer::histogram_rgba(&bw, 2, 2);
    assert_eq!(h.black, 2);
    assert_eq!(h.white, 2);
    // Linear ramp across all 256 levels: every bin holds exactly one pixel.
    let mut ramp = Vec::with_capacity(256 * 4);
    for v in 0..=255u8 {
        ramp.extend_from_slice(&[v, v, v, 255]);
    }
    let h = image_viewer::histogram_rgba(&ramp, 256, 1);
    for ch in 0..3 {
        for (level, &count) in h.bins[ch].iter().enumerate() {
            assert_eq!(count, 1, "channel {ch} level {level}");
        }
    }
    assert_eq!(h.black, 1);
    assert_eq!(h.white, 1);
}

/// The histogram reaches the panel through the decode worker, not through
/// the UI: `decode_preview` counts the preview buffer it already holds and
/// the pane draws the cached numbers. A pane that counted pixels itself
/// would walk a buffer per frame, and a worker that skipped the count
/// would leave the panel showing a default histogram — zeros that read as
/// "this photo is black".
#[test]
fn the_histogram_is_counted_on_the_worker_and_drawn_from_cache() {
    let src = main_src();
    let decode = fn_body(&src, "fn decode_preview(").expect("decode_preview");
    assert!(
        decode.contains("image_viewer::histogram_rgba("),
        "decode_preview must count the preview buffer it already holds: the \
         CPU pixels are dropped after upload, so later is too late"
    );
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    assert!(
        !central.contains("histogram_rgba("),
        "the paint path must not count pixels itself — that is a buffer walk \
         per frame"
    );
}
