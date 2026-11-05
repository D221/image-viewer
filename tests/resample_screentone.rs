//! Screentone: the alias band, and the pitch the widened prefilter promotes.
//!
//! Split out of `tests/resample.rs`; the probes and the globals guard are in
//! `tests/common/resample_fixtures.rs`.
//!
//! The prefilter was widened to remove the moire a box prefilter left on
//! screentone, and it does remove it at the lattice's own output pitch. What it
//! also does is move energy between pitches: on a grey tone with several
//! screens, some periods get cleaner and others get worse. That is why `Smooth`
//! is not the default, and the guard here is the finding rather than a pinned
//! frequency - a pinned frequency goes stale the moment the kernel changes,
//! which is exactly how the wrong default was chosen the first time.

#[path = "common/resample_fixtures.rs"]
mod resample_fixtures;

use resample_fixtures::*;

/// **The regression guard: `Smooth` must not be the default, and the reason must
/// not quietly stop being true.**
///
/// Two independent claims, because the second is the one that stops it coming
/// back.
///
/// *The default.* `Config::load` on an absent file must give
/// `smooth_downscale == false`. The evidence for that is on real pages and cannot
/// be reproduced here — `12.jpg`, `21.jpg` and `44.jpg` are 4-6 MB scans in a
/// gitignored build directory — so this pins the **decision**, in the one place a
/// decision like it can be re-made by accident.
/// `a_config_written_before_smooth_downscale_still_loads_and_stays_off` covers the
/// two other ways the field reaches a real user.
///
/// *The mechanism.* The synthetic probe below is the in-tree half, and it is the
/// part that reproduces. `probe_multi_pitch_screentone` is a **grey** field
/// carrying a base dot grid, a second screen at 30 degrees and a stochastic
/// dither — which is what real screentone is, and what a 0/100 test lattice is
/// not.
///
/// **What widening actually does, measured, in one image.** Amplitude at an output
/// period, Fast -> Smooth, scanning 1.05 to 4.00 output px in 0.05 steps:
///
/// * at **1.389 px** — the base grid's own pitch — 1.853 -> **0.629** codes, so
///   widening removes it, which is the effect the setting was built for;
/// * at **3.60 px** — a longer period the same image also carries — 0.295 ->
///   **0.564** codes, so widening **nearly doubles** it.
///   And 1.4 px goes 0.468 -> 0.550, 3.5 px the same.
///
/// So the setting is not a monotone improvement in the low-pass and a cost in the
/// detail. It moves energy **between** pitches, and which pitches gain depends on
/// the content. `smooth_low_passes_a_square_lattice_harder_than_fast` measures the
/// winning half and passes; this one measures the losing half, and both are true.
///
/// **The obvious explanation is wrong, and it is worth recording because it is
/// the one to reach for.** It is not about multi-pitch content: removing the
/// second screen and the dither, leaving a *pure single-pitch* 0/100 lattice at
/// source pitch 4.0, still shows a ratio of 1.25 at 4.4 output px and 1.53
/// somewhere finer. So this is not "real tone has several screens and a single
/// screen cannot". It is the filter's response meeting **foldover**: the promoted
/// component is at a period that is a *multiple* of the pattern's own, i.e. an
/// image of the pattern's own frequency in the output, and widening changes the
/// balance between a component and its folded image. The previous sweep only ever
/// measured at the lattice's own output pitch, where widening wins monotonically,
/// which is why it read as a clean fix.
///
/// **Why the assertion is a scan and not a pinned pitch.** A pinned frequency is a
/// number that goes stale the moment the kernel changes, and the previous
/// single-pitch sweep is what produced the wrong default in the first place. The
/// claim pinned here is the finding itself — in one image, some pitch improves and
/// another gets worse — which survives a kernel change if the finding does and
/// fails loudly if it stops being true.
#[test]
fn smooth_is_not_the_default_because_it_moves_energy_between_pitches() {
    // 1. The decision, in the place it is made.
    assert!(
        !default_config().smooth_downscale,
        "Smooth must default OFF: it is better on 2 of 6 real pages, unchanged on \
         1 and worse on 3"
    );

    // 2. The mechanism, on a fixture that exists on a clean clone. The claim is
    // **not** "pitch X gets worse" — a single pinned frequency is a number that
    // goes stale the moment the kernel changes, and the previous single-pitch
    // sweep is what produced the wrong default in the first place. The claim is
    // the finding itself: in ONE image, widening the low-pass suppresses one
    // pitch and promotes another. Scan for both.
    let src = probe_multi_pitch_screentone(PROBE_W, PROBE_H, 4.0, 5.1, 30.0);
    let fast = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
    let smooth = resize_at(&src, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);

    let mut worst: Option<(f32, f32, f32, f32)> = None; // ratio, period, fast, smooth
    let mut best: Option<(f32, f32, f32, f32)> = None;
    for i in 0..=59 {
        let p = 1.05 + i as f32 * 0.05;
        let (a, b) =
            (screentone_lattice_amplitude(&fast, p), screentone_lattice_amplitude(&smooth, p));
        // A ratio against a signal that is not there is not a measurement. At
        // exactly 2.00 output px both projections read 0.000 for every strength,
        // because that is the output Nyquist and the pattern is below it — the
        // frequency content there is genuinely absent, not small.
        if a < 0.05 {
            continue;
        }
        let ratio = b / a;
        if worst.map_or(true, |w| ratio > w.0) {
            worst = Some((ratio, p, a, b));
        }
        if best.map_or(true, |w| ratio < w.0) {
            best = Some((ratio, p, a, b));
        }
    }
    let (w, bst) = (worst.expect("scan produced samples"), best.expect("and more"));

    assert!(
        bst.0 <= 0.70,
        "at output period {:.2} px the widened low-pass should suppress the \
         pattern (Fast {:.3} -> Smooth {:.3} codes, ratio {:.2}); it left {:.2}. \
         The fixture no longer shows widening helping anywhere, so it is not a \
         multi-pitch trade any more and this guard is decoration.",
        bst.1,
        bst.2,
        bst.3,
        bst.0,
        bst.0
    );
    assert!(
        w.0 >= 1.50,
        "at output period {:.2} px the widened low-pass should PROMOTE the \
         pattern (Fast {:.3} -> Smooth {:.3} codes, ratio {:.2}); it left {:.2}. \
         This is the regression: the same widening that removes one pitch's beat \
         creates another's, which is why the setting cannot be the default.",
        w.1,
        w.2,
        w.3,
        w.0,
        w.0
    );
}

#[test]
fn the_prefilter_low_passes_the_alias_band_far_deeper_than_a_box() {
    // Source periods 2.5-6 px land at 0.87-2.08 *output* px, i.e. at or below
    // the output Nyquist: everything that survives here is foldover, and it is
    // the 1-2 px sparkle the manga pages show as a lattice.
    //
    // box -> kernel prefilter, 100-code sinusoid, codes:
    //   2.5: 29 -> 4    3.0: 25 -> 2    3.5: 21 -> 5
    //   4.0: 29 -> 11   5.0: 28 -> 23   6.0: 41 -> 35
    // Pins are the kernel numbers plus one code, which is the most a different
    // target's f32 contraction could move a weighted sum.
    let cases = [(2.5f32, 5.0f32), (3.0, 3.0), (3.5, 6.0), (4.0, 12.0), (5.0, 24.0), (6.0, 36.0)];
    for (period, max_codes) in cases {
        let src = probe_sinusoid(PROBE_W, PROBE_H, period, 100.0);
        let out = probe_resize(&src);
        let amp = probe_amplitude(&out);
        assert!(
            amp <= max_codes,
            "source period {period} px ({:.2} output px): {} codes of alias \
             survived, want <= {max_codes}",
            period * out.width as f32 / src.width as f32,
            amp
        );
    }
}

#[test]
fn a_dot_lattice_leaves_far_less_sparkle_than_a_box_prefilter() {
    // The same probe as a real dot lattice (1-px dots, 50% duty) rather than a
    // tone, over the alias band and a little above it.
    //
    // box -> kernel prefilter, 0/100 codes, codes:
    //   2.5: 24 -> 9    3.0: 13 -> 2    3.5: 28 -> 15   4.0: 22 -> 8
    //   5.0: 35 -> 20   6.0: 46 -> 40   8.0: 65 -> 60
    let cases = [
        (2.5f32, 10.0f32),
        (3.0, 3.0),
        (3.5, 16.0),
        (4.0, 9.0),
        (5.0, 21.0),
        (6.0, 41.0),
        (8.0, 61.0),
    ];
    for (period, max_codes) in cases {
        let src = probe_dots(PROBE_W, PROBE_H, period, 0, 100);
        let out = probe_resize(&src);
        let amp = probe_amplitude(&out);
        assert!(
            amp <= max_codes,
            "dot lattice at source period {period} px ({:.2} output px): {} codes \
             survived, want <= {max_codes}",
            period * out.width as f32 / src.width as f32,
            amp
        );
    }
}

#[test]
fn a_large_reduction_is_no_worse_than_the_box_prefilter_was() {
    // 1200 -> 150 is 8x. At that reduction the prefilter reduces by 2x and the
    // final pass by 4x, so this is where a widened filter could over-blur.
    // It does not: the kernel prefilter is lower or equal at every pitch.
    //
    // box -> kernel prefilter, 100-code sinusoid, codes:
    //   2.5: 2 -> 0   3.0: 4 -> 0   5.0: 14 -> 4   6.0: 6 -> 3   11: 10 -> 9
    let cases = [(2.5f32, 1.0f32), (3.0, 1.0), (5.0, 5.0), (6.0, 4.0), (11.0, 10.0)];
    for (period, max_codes) in cases {
        let src = probe_sinusoid(1200, 1200, period, 100.0);
        let out = with_linear_resample(true, || image_viewer::downscale_image(&src, 150));
        let amp = probe_amplitude(&out);
        assert!(
            amp <= max_codes,
            "8x reduction, source period {period} px: {amp} codes survived, want <= {max_codes}"
        );
    }
}

#[test]
fn the_prefilter_does_not_darken_a_near_nyquist_tone() {
    // THE guard on the prefilter's working space. A weighted average is only
    // exposure-preserving in the space it averages in: averaging *gamma-encoded*
    // values of a 50% dither gives ~128 (its own sRGB mean), while the tone's
    // true average reflectance is 50% linear, i.e. sRGB ~188.
    //
    //   50% near-Nyquist dither, mean R of the 309x432 result:
    //     box in sRGB (today)      164.4
    //     kernel in sRGB           130.0   <- the trap: looks like "no change"
    //     kernel in linear light   186.2   <- and within 1.4 of the single-kernel
    //                                                path's 187.6
    //
    // So a prefilter that averaged in the wrong space is not neutral, and the
    // band below is where that shows. Lower bound only: the prefilter may not
    // darken the tone, and this one brightens it toward the linear-light mean.
    let src = probe_dither(PROBE_W, PROBE_H);
    let out = probe_resize(&src);
    let m = probe_mean(&out);
    assert!(
        (180.0..=195.0).contains(&m),
        "50% dither came out at mean {m:.2}, want 180..195 (the linear-light \
         average of its tones; the box prefilter gave 164.4 and an sRGB-space \
         kernel gives 130.0)"
    );
}

#[test]
fn a_flat_field_survives_the_prefilter_unchanged() {
    // The other half of the exposure guard, and the one that says nothing about
    // *which* filter is used: every weighted average of a constant is that
    // constant. Verified to pass with the box prefilter in place as well, so it
    // is a regression guard and not a discriminator — the discriminating half is
    // `the_prefilter_does_not_darken_a_near_nyquist_tone`.
    for v in [0u8, 1, 64, 128, 200, 254, 255] {
        let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
        for p in rgba.chunks_exact_mut(4) {
            p.copy_from_slice(&[v, v, v, 255]);
        }
        let src = image_viewer::ImageData {
            width: PROBE_W,
            height: PROBE_H,
            rgba,
            load_time: std::time::Duration::ZERO,
        };
        let out = probe_resize(&src);
        let m = probe_mean(&out);
        assert!((m - v as f32).abs() <= 1.0, "flat field {v} came out at mean {m:.2}");
    }
}

#[test]
fn the_prefilter_intermediate_is_at_most_twice_the_target() {
    // This is the memory bound. The prefilter exists so the 12-bit linear
    // conversion runs on a buffer of at most 2x the target rather than on the
    // full-resolution source: on a 6.9 MP page that is 26.7 MB instead of
    // 55 MB, and deleting the step doubles the conversion cost as well.
    let cases = [
        (890u32, 309u32, 618u32), // the probe geometry: 2x the target
        (2223, 772, 1544),        // the real 2223x3109 page at its 772 wide
        (3109, 1080, 2160),       // ... and its height
        (100, 1000, 100),         // no shrink: the dimension itself
        (1, 1, 1),                // 1 px
        (7, 3, 6),                // odd small
        (4096, 2048, 4096),       // exactly 2x
        (4097, 2048, 4096),       // just over
        (u32::MAX, 4096, 8192),   // saturating_mul must not wrap
        (u32::MAX, u32::MAX, u32::MAX),
    ];
    for (dim, target, want) in cases {
        assert_eq!(
            image_viewer::prefilter_dim(dim, target),
            want,
            "prefilter_dim({dim}, {target})"
        );
    }
}

#[test]
fn linear_resize_keeps_its_two_step_shape() {
    // The intermediate size above is a pure function and is pinned there, but
    // nothing observable says `linear_resize` still *uses* it: delete the
    // pre-reduction and every quality test in this file still passes (a single
    // kernel pass scores the same or better on all of them) while the 12-bit
    // buffer grows to the full-resolution source. So the wiring is pinned
    // against the source text, which is the established pattern in this repo
    // (`spreads.rs`, `keybindings.rs`) and the only one available here.
    //
    // It checks *position*, not just presence: `linear_resize`'s own
    // brace-matched body, not the file, and not a comment elsewhere. Verified to
    // fail both ways round — with `box_reduce_srgb` put back, and with the
    // pre-reduction deleted outright.
    let src = lib_src();
    let start = src.find("fn linear_resize(").expect("linear_resize exists");
    let body = &src[start..];
    // Brace-match the body rather than looking for the first `\n}`: a nested
    // block at column 0 would end the slice early and the scan would then pass
    // on a *truncated* function — which is the failure mode the `spreads.rs`
    // position check was written for.
    let mut depth = 0i32;
    let end = body
        .char_indices()
        .find(|&(_, c)| {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            depth == 0 && c == '}'
        })
        .map(|(i, _)| i + 1)
        .expect("linear_resize's braces balance");
    let body = &body[..end];
    assert!(
        body.contains("prefilter_dim(data.width, new_w)")
            && body.contains("prefilter_dim(data.height, new_h)"),
        "linear_resize must size both axes with prefilter_dim"
    );
    assert!(
        body.contains("kernel_reduce_linear("),
        "linear_resize must pre-reduce before the linear conversion; without it \
         the 12-bit buffer is the full-resolution source"
    );
    assert!(
        !body.contains("box_reduce"),
        "the sRGB box prefilter is gone; a box aliases more than the kernel it feeds"
    );
}
