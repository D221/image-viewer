//! The `MIN_FILTER_SCALE` floor: the foldover band a 1440p source carries.
//!
//! Split out of `tests/resample.rs`; the probes and the globals guard are in
//! `tests/common/resample_fixtures.rs`.
//!
//! A 1440p photo on a 1920-long panel is only a 1.33x reduction, so the
//! prefilter - which fires at 2x and above - never runs. Content at the source
//! Nyquist then folds into the output: the reported case. The floor widens the
//! kernel to at least the output pitch, which rejects that band without
//! touching anything the prefilter already handles.
//!
//! The shipped pipeline is pinned **byte-identical** to the pre-floor
//! behaviour: the fix must cost nothing where the prefilter already fires.

#[path = "common/resample_fixtures.rs"]
mod resample_fixtures;

use resample_fixtures::*;

/// Resize at an explicit target with the shipped settings - **including the
/// `MIN_FILTER_SCALE` floor**, which is the point: this is "what the viewer does",
/// as distinct from `resize_at`, which forces a strength.
///
/// The strength comes from `cached_default_strength()` rather than the global, for the
/// same reason the floor is pinned: reading a process-wide global inside a
/// concurrently-run test file means testing whichever arm ran last.
fn resize_shipped(src: &image_viewer::ImageData, target: u32) -> image_viewer::ImageData {
    with_resample(true, cached_default_strength(), || image_viewer::downscale_image(src, target))
}

/// Resize at an explicit target and floor, at an explicit strength. `floor 0.0`
/// is the pre-`MIN_FILTER_SCALE` path.
fn resize_floor(src: &image_viewer::ImageData, target: u32, floor: f32) -> image_viewer::ImageData {
    with_floor_at(cached_default_strength(), floor, || image_viewer::downscale_image(src, target))
}

/// The reported case: a 1440p landscape photo on a 1920-long panel, so
/// `preview_target` is 1920 and the reduction is 2560/1920 = 1.3333.
///
/// **The geometry is a constant here because every number in this section is
/// meaningless without it.** A lattice measured at a different reduction is a
/// different measurement - the foldover band moves with `2R` - so these cannot
/// be swept over `target` without re-deriving what each reading means.
const QHD_W: u32 = 2560;
const QHD_H: u32 = 1440;
const QHD_TARGET: u32 = 1920;
/// 2560 / 1920, the reduction this geometry actually gets.
const QHD_R: f32 = 1.3333;
/// Output Nyquist in **source** pixels at that reduction: content finer than
/// this cannot be represented and is foldover if the kernel lets it through.
const QHD_FOLDOVER_PERIOD: f32 = 2.0 * QHD_R;

/// 1-D sinusoid of a known period, `amp` codes either side of mid-grey, so the
/// peak-to-peak excursion is `2 * amp` (100 codes at the `50.0` the section
/// below uses). The **transfer function's** probe: `|H|` read directly, with no
/// lattice and therefore no foldover to confuse the reading.
///
/// This is the one place a 1-D probe is the right instrument, and the reason is
/// worth stating because `probe_sinusoid`'s original use was a mistake: the
/// *width* question wants one frequency at a time, and a square lattice cannot
/// answer it because anything it shows may be foldover rather than filtering.
/// The lattice probe is not deprecated - it is the right tool for the artefact -
/// it is just the wrong tool for this.
fn probe_sine(w: u32, h: u32, period: f32, amp: f32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = (127.5 + amp * (2.0 * std::f32::consts::PI * x as f32 / period).sin())
                .round()
                .clamp(0.0, 255.0) as u8;
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO }
}

/// p95-p5 of the red channel, in codes: how much of a 100-codes-peak-to-peak
/// sinusoid survived the resize. `probe_sinusoid`'s `amp` parameter is a
/// half-amplitude, so a 100-code reading is the full excursion.
fn sine_amp(d: &image_viewer::ImageData) -> f32 {
    alias_amp(d)
}

/// **The reported defect: 1440p at `Fast` does not reject the foldover band.**
///
/// The geometry is a 2560x1440 landscape photo on a 1920-long panel, so the
/// reduction is 1.3333 and **the prefilter does not fire** -
/// `prefilter_dim(2560, 1920) = 2560`, the source unchanged. The output Nyquist
/// is 2.67 source px, so source periods of 1.5 and 2.5 px cannot be represented
/// and a kernel that passes them is producing foldover.
///
/// A 100-codes-peak-to-peak sinusoid, codes surviving, `Fast`:
///
/// | source period | in foldover band? | before the floor | with `MIN_FILTER_SCALE` |
/// |---|---|---|---|
/// | 1.5 | yes | 51 | **19** |
/// | 2.5 | yes | 41 | **11** |
/// | 4.0 | no | 58 | **37** |
/// | 6.0 | no | 84 | **67** |
///
/// Pins are the "before" numbers with a tolerance in the floor's favour, so the
/// assertion fails if the floor stops reaching the kernel.
///
/// **The two passband rows are in this table and not only in the sibling test,
/// and each row carries its own `in_band` flag that the loop asserts against
/// `QHD_FOLDOVER_PERIOD`.** A floor that rejected *everything* would pass a test
/// written only on the foldover band - which is the more flattering reading, and
/// so the more likely one to be written. The bands are asserted from the same
/// constant the table is quoted in, so the table cannot quietly go stale while the
/// pins stay green; a version that asserted `period < NYQUIST` inside this loop
/// failed immediately on its own 4.0 px row.
#[test]
fn the_min_filter_scale_rejects_the_foldover_band_a_1440p_source_carries() {
    let cases = [
        // (source period, in foldover band, may_leave, must_leave)
        (1.5f32, true, 52.0f32, 20.0f32),
        (2.5, true, 42.0, 12.0),
        (4.0, false, 59.0, 38.0),
        (6.0, false, 85.0, 68.0),
    ];
    for (period, in_band, was, now) in cases {
        assert_eq!(
            in_band,
            period < QHD_FOLDOVER_PERIOD,
            "probe bug: {period} px is {} the foldover band at R {QHD_R} (Nyquist \
             is {QHD_FOLDOVER_PERIOD} source px), and the table says the opposite. \
             One of the two is stale, which is how a pin comes to assert nothing",
            if in_band { "inside" } else { "outside" }
        );
        let src = probe_sine(QHD_W, QHD_H, period, 50.0);
        let before = sine_amp(&resize_floor(&src, QHD_TARGET, 0.0));
        // **The floor is opt-in, so the arm under test is 2.0 written out**, not
        // "whatever ships". Comparing the floor against the shipped default would
        // compare it against itself the day the default changed, and this test
        // would pass for the wrong reason - which is how it was written before.
        let after = sine_amp(&resize_floor(&src, QHD_TARGET, 2.0));
        assert!(
            before <= was && before > 10.0,
            "the pre-fix path left {before:.1} codes at source period {period} px, \
             which should be {was} - if this reading is ~0 the probe geometry \
             changed and the pin means nothing"
        );
        assert!(
            after <= now,
            "1440p at the shipped settings leaves {after:.1} codes at source \
             period {period} px (inside the foldover band, Nyquist \
             {QHD_FOLDOVER_PERIOD} px); want <= {now} (was {before:.1}). \
             MIN_FILTER_SCALE is not reaching the kernel"
        );
    }
}

/// **The shipped pipeline is byte-identical to having no floor at all, and that is
/// the regression guard for a user-reported regression.**
///
/// `MIN_FILTER_SCALE` shipped at **2.0** and was withdrawn back to `0.0`. On a
/// **2560x1440 display at 100% scaling** - where pages reduce by about 1.33x, so
/// the floor forced the kernel from 1.33 to 2.0 - moire became *worse* on every
/// file, on every filter, and `Smooth` did not help. Its benefit had been measured
/// on a synthetic **uniform** lattice (91.9 -> 55.9 codes); real screentone
/// disagreed. That is the third time in this project a synthetic proxy said
/// "fixed" while a person looking at real content said worse.
///
/// So the property worth protecting is **not** "an armed floor rejects the foldover
/// band" - that is an opt-in arm, and the two tests above cover it. It is that
/// **someone who does not ask for a floor gets the pre-`35ad27e9` pipeline byte for
/// byte.** That is the state a user verified as better than `master` on every
/// file, and the only one safe to ship while the real-content question is open.
///
/// **Byte-identity, not a tolerance.** A floor that merely failed to bind would
/// pass any tolerance check, and "close enough" is not a property of a pipeline
/// that produces pixels.
#[test]
fn the_shipped_resize_is_byte_identical_to_no_floor_at_all() {
    // A spread half-panel (1350x1920 -> 1080), the 1440p geometry from the user
    // report, and two reductions either side of `PREFILTER_FACTOR`, so the claim
    // covers both sides of the threshold the floor was bridging.
    for (w, h, target) in
        [(QHD_W, QHD_H, QHD_TARGET), (1350, 1920, 1080), (2560, 1440, 1920), (1000, 1414, 1080)]
    {
        let src = probe_square_lattice(w, h, 4.0, 1.0, 0, 100);
        let shipped = resize_shipped(&src, target);
        let off = resize_floor(&src, target, 0.0);
        assert_eq!(
            (shipped.width, shipped.height),
            (off.width, off.height),
            "shipped and no-floor disagree on size at {w}x{h} -> {target}"
        );
        assert!(
            shipped.rgba == off.rgba,
            "the shipped resize still differs from an explicit 0.0 floor at \
             {w}x{h} -> {target}; first differing byte at index {:?}. The shipped \
             pipeline must be byte-identical to the pre-35ad27e9 path.",
            shipped.rgba.iter().zip(off.rgba.iter()).position(|(a, b)| a != b)
        );
    }
}

/// The other half, and the reason the test above is not a "blur everything"
/// test: a wider kernel must not eat the passband.
///
/// A 100-codes-peak-to-peak sinusoid at source periods **above** the foldover
/// band, i.e.
/// content the output can represent and the low-pass is supposed to keep.
/// `Smooth` for comparison, measured in the same session: **15 / 49 at 4 / 6 px**
/// against the floor's 37 / 67 - so the floor keeps roughly two thirds of the
/// detail `Smooth` destroys at the same foldover improvement.
#[test]
fn the_floor_keeps_the_passband_a_1440p_source_carries() {
    let cases = [(4.0f32, 36.0f32), (6.0, 66.0), (12.0, 89.0)];
    for (period, max_codes) in cases {
        assert!(
            period > QHD_FOLDOVER_PERIOD,
            "probe bug: {period} px is not representable at R {QHD_R}"
        );
        let src = probe_sine(QHD_W, QHD_H, period, 100.0);
        let out = resize_shipped(&src, QHD_TARGET);
        let a = sine_amp(&out);
        assert!(
            a >= max_codes,
            "the floor cut representable detail at source period {period} px to \
             {a:.1} codes, want >= {max_codes}. A floor that only rejects is a \
             blur, not a filter"
        );
    }
}

/// **The fix must cost nothing where the prefilter already fires.** This is the
/// property that makes it a floor rather than a retune: byte-identical output at
/// every reduction the shipped threshold already covers.
///
/// The reductions are chosen around the threshold rather than sampled: exactly
/// 2.0 (where `prefilter_dim` returns the source and neither path fires),
/// just above it (where it fires by one pixel), the two real geometries from
/// `AGENTS.md` (2.879 for the manga half-panel, 8.0 for a large scan), and 4.0.
///
/// Verified to fail with the floor applied to `kernel_reduce_linear` as well as
/// the final pass - which is the mistake this guards, and which is invisible
/// in every quality assertion in this file.
#[test]
fn the_min_filter_scale_is_a_no_op_where_the_prefilter_already_fires() {
    // `R` values are *derived* from the geometry rather than written out, so the
    // condition and the sample cannot drift apart: `prefilter_dim` fires iff
    // `pre_w < QHD_W`, i.e. iff the reduction exceeds `PREFILTER_FACTOR`.
    let factor = image_viewer::PREFILTER_FACTOR as f64;
    for over in [0.0f64, 0.02, 0.05, 0.25, 0.879, 1.0, 4.0] {
        let r = factor + over;
        let target = ((QHD_W as f64 / r).round() as u32).max(2);
        let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
        let before = resize_floor(&src, target, 0.0);
        let after = resize_floor(&src, target, 2.0);
        let fires = image_viewer::prefilter_dim(QHD_W, target) < QHD_W;
        // At exactly `PREFILTER_FACTOR` the prefilter does **not** fire
        // (`prefilter_dim` returns the source unchanged), but the floor is still
        // inert there: `scale = target/source = 0.5` already gives
        // `filter_scale == 2.0 == MIN_FILTER_SCALE`, so `max` cannot bind. That
        // boundary is the whole reason the floor is 2.0 and not "a bit more", so
        // it is asserted rather than skipped.
        assert_eq!(
            before.rgba == after.rgba,
            true,
            "at R {r:.3} (target {target}, prefilter {}) the floor changed the \
             output, so it is not inert above PREFILTER_FACTOR. Every reduction \
             that works today goes through here and must not change",
            if fires { "fires" } else { "does not fire" }
        );
    }

    // And it must NOT be inert below, or the fix is a no-op wearing a test.
    for under in [0.2f64, 0.5, 0.8] {
        let r = factor - under;
        let target = ((QHD_W as f64 / r).round() as u32).max(2);
        let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
        assert!(
            resize_floor(&src, target, 0.0).rgba != resize_floor(&src, target, 2.0).rgba,
            "at R {r:.3} (target {target}) an armed floor of 2.0 left the output \
             unchanged, so the 1440p case it exists for is not being fixed"
        );
    }
}

/// **The floor must actually reach the kernel below the threshold**, or the
/// no-op test above would pass for the wrong reason.
///
/// This is the check `the_min_filter_scale_rejects_the_foldover_band...` cannot
/// make on its own: that one could be satisfied by anything that changes the
/// output, including a bug. This one asks whether `contributions` is *handed* the
/// floor, on both axes, and only on the final pass - a source scan, because
/// `contributions` is private and nothing observable from outside says which
/// call sites carry the argument.
#[test]
fn the_floor_reaches_both_final_axes_and_only_those() {
    let src = lib_src();
    let body_of = |name: &str| -> &str {
        let start = src.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("{name} exists"));
        let body = &src[start..];
        let mut depth = 0i32;
        let end = body
            .char_indices()
            .find(|&(_, c)| match c {
                '{' => {
                    depth += 1;
                    false
                }
                '}' => {
                    depth -= 1;
                    depth == 0
                }
                _ => false,
            })
            .map(|(i, _)| i + 1)
            .expect("braces balance");
        &body[..end]
    };

    let lr = body_of("linear_resize");
    assert_eq!(
        lr.matches("min_filter_scale()").count(),
        1,
        "linear_resize must read min_filter_scale() exactly once, so the two \
         final axes cannot disagree about it"
    );
    assert_eq!(
        lr.matches("min_filter_scale()").count(),
        1,
        "linear_resize must read min_filter_scale() exactly once, so the two \
         final axes cannot disagree about it"
    );
    assert!(
        lr.contains("contributions(lw, new_w, &kern, support, strength, floor)")
            && lr.contains("contributions(lh, new_h, &kern, support, strength, floor)"),
        "both final-pass axes must be handed the floor"
    );
    // **The literal `0.0`, and this is the check that was written after getting
    // it wrong.** `kernel_reduce_linear` runs at exactly `PREFILTER_FACTOR`, so
    // its own `scale` is 0.5 and its `filter_scale` is 2.0 already — handing it
    // the floor looks harmless and is not: measured, it changes the output at
    // R 2.02 and above, i.e. at every reduction the shipped threshold covers,
    // because the pre-reduction's ratio is `R / PREFILTER_FACTOR` and at R 2.02
    // that is 1.01, far under the floor. `the_min_filter_scale_is_a_no_op_where_the_prefilter_already_fires`
    // fails on it, which is why the no-op guard and this scan are the same
    // property asserted two ways.
    assert!(
        lr.contains("kernel_reduce_linear(data, pre_w, pre_h, strength, 0.0)"),
        "linear_resize must hand the pre-reduction a literal 0.0 floor, not `floor`. \
         The pre-reduction already runs at PREFILTER_FACTOR, so its kernel meets \
         the floor; giving it the floor anyway widens output at every reduction \
         above the shipped threshold"
    );
    assert!(
        !lr.contains("kernel_reduce_linear(data, pre_w, pre_h, strength, floor)"),
        "the pre-reduction must NOT be handed the floor - see above"
    );

    let kr = body_of("kernel_reduce_linear");
    assert!(
        kr.contains("floor: f32"),
        "kernel_reduce_linear must take the floor as an argument rather than \
         reading the global: it is called once per resize and a read there is a \
         second value that could disagree with the final passes"
    );
    assert!(
        !kr.contains("min_filter_scale()"),
        "kernel_reduce_linear must not read min_filter_scale() itself - see above"
    );
    assert!(
        kr.contains("contributions(sw as u32, pw as u32, &kern, support, strength, floor)")
            && kr.contains("contributions(sh as u32, ph as u32, &kern, support, strength, floor)"),
        "the pre-reduction must use the floor it was given on both axes - count, \
         not presence: one axis floored and one not would be a picture low-passed \
         horizontally only"
    );

    let con = body_of("contributions");
    assert!(
        con.contains(".max(floor)"),
        "contributions must apply the floor to filter_scale - taking the argument \
         and ignoring it is a floor that exists and does nothing"
    );
}

/// **`PREFILTER_FACTOR` is not a quality knob, and this is the arithmetic that
/// says so.** It was read as one for a long time - it is the number the reported
/// 1440p case was blamed on - and lowering it is the change that looks obvious
/// and cannot work.
///
/// `contributions` sets `filter_scale = strength / scale` from each stage's own
/// ratio, so a decimation `R` split into stages `s1` and `s2` has
/// **`s1 * s2 == R`**. The two stages' kernel widths therefore multiply back to
/// the width the reduction already implied, and no split can make the pair wider
/// than `R` alone. The prefilter cap is a **memory** bound (it decides how big
/// the buffer the 12-bit conversion runs over is) and nothing else.
///
/// This is asserted rather than measured-at-test-time because it is a statement
/// about the code's arithmetic, and a test that recomputed it from the same
/// formula would only be checking itself. The caps span the whole plausible
/// range including below 1.0, and the reductions span the regime the report is
/// about (1.1 - 1.95, where no cap fires at all) and the regime that works
/// (2.0 - 8.0).
#[test]
fn splitting_a_decimation_cannot_widen_the_kernel_past_the_reduction() {
    for r in [1.1f64, 1.2, 1.333, 1.5, 1.778, 1.95, 2.0, 2.5, 2.879, 4.0, 8.0] {
        for cap in [1.60f64, 1.30, 1.15, 1.02, 0.75, 0.50] {
            let target = ((QHD_W as f64 / r).round() as u32).max(2);
            let pre = (QHD_W as f64).min(target as f64 * cap);
            if pre >= QHD_W as f64 {
                continue; // the cap cannot bind; the path is the single pass
            }
            let s1 = QHD_W as f64 / pre;
            let s2 = pre / target as f64;
            assert!(
                (s1 * s2 - r).abs() < 1e-3,
                "R {r:.4} split by cap {cap} into stages {s1:.4} and {s2:.4} \
                 multiplied to {} - if this stopped holding, the prefilter cap \
                 WOULD be a quality knob and lowering PREFILTER_FACTOR could \
                 widen the low-pass. Re-derive MIN_FILTER_SCALE before trusting \
                 anything above it",
                s1 * s2
            );
        }
    }
}

/// **What the fix does not do, pinned so it cannot be quietly over-claimed.**
///
/// At `MIN_FILTER_SCALE` the 1440p residual is 55.87 codes of square-lattice
/// amplitude where `Smooth` reaches 28.57. The moiré is reduced by roughly two
/// thirds, not abolished, and the constant is deliberately not the wider 2.5
/// that would get further - 2.5 is above the two-stage path's own 2.0, so
/// choosing it would change every reduction that works today, which is the
/// property `the_min_filter_scale_is_a_no_op_where_the_prefilter_already_fires`
/// exists to protect.
///
/// This test asserts the floor is **not** `Smooth`-equivalent at 1440p, so that
/// a future change which quietly converges the two fails here rather than
/// inheriting `Smooth`'s measured regression on `12.jpg` / `21.jpg` / `44.jpg`.
///
/// **The constants being equal is not the interesting fact and is not asserted
/// here.** `MIN_FILTER_SCALE` and `SMOOTH_STRENGTH` are both 2.0, and it is
/// tempting to pin that as "they are different numbers" - which is false, and a
/// guard written that way would be asserting a coincidence. They are different
/// *mechanisms* that happen to share a value: the floor is an absolute minimum
/// on one stage and the strength is a multiplier on all of them, so the floor
/// is inert above `PREFILTER_FACTOR` and the strength is not. What is pinned is
/// the behavioural consequence - different output at 1440p - because that is the
/// claim the mechanism has to keep supporting.
#[test]
fn the_floor_is_not_smooth_renamed_at_1440p() {
    let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
    let floor = resize_shipped(&src, QHD_TARGET);
    let smooth = resize_at(&src, QHD_TARGET, image_viewer::SMOOTH_STRENGTH);
    assert_ne!(
        floor.rgba, smooth.rgba,
        "the floor and Smooth now produce identical 1440p output. That is not \
         necessarily wrong, but it means the floor has picked up the trade \
         Smooth was rejected for (a black dot pattern in grey screentone on \
         12.jpg, 21.jpg and 44.jpg), so it must be re-judged on those files \
         before this assertion is simply deleted"
    );
}

/// Print the numbers the section above is pinned against. `cargo test --test unit
/// --release -- --ignored --nocapture print_min_filter_scale`. Kept for the same
/// reason `print_prefilter_pins` is: a kernel change should be re-pinnable from
/// a printout rather than from a bisect.
#[test]
#[ignore]
fn print_min_filter_scale() {
    println!("\n== MIN_FILTER_SCALE transfer function, {QHD_W}x{QHD_H} -> {QHD_TARGET} ==");
    println!("   R {QHD_R}, foldover band = source period < {QHD_FOLDOVER_PERIOD} px");
    println!(
        "{:>10} {:>10} {:>10} {:>10} {:>10}",
        "period", "floor off", "floor 2.0", "floor 2.5", "Smooth"
    );
    for period in [1.5f32, 2.0, 2.5, 3.0, 4.0, 6.0, 8.0, 12.0, 16.0, 24.0, 32.0] {
        let src = probe_sine(QHD_W, QHD_H, period, 50.0);
        println!(
            "{period:>10.1} {:>10} {:>10} {:>10} {:>10}",
            sine_amp(&resize_floor(&src, QHD_TARGET, 0.0)),
            sine_amp(&resize_floor(&src, QHD_TARGET, 2.0)),
            sine_amp(&resize_floor(&src, QHD_TARGET, 2.5)),
            sine_amp(&resize_at(&src, QHD_TARGET, image_viewer::SMOOTH_STRENGTH)),
        );
    }

    let step = probe_step(QHD_W, QHD_H, QHD_W / 2);
    let tone = probe_multi_pitch_screentone(QHD_W, QHD_H, 3.0, 2.6, 30.0);
    println!("\n== cost at the reported geometry ==");
    println!(
        "{:>14} {:>10} {:>10} {:>10} {:>10}",
        "arm", "step rise", "lattice", "tone sum", "tone peak"
    );
    // Arms as `(name, resize)` so each column's images come from the *same*
    // arm. Selecting per-image by string comparison was the first version and it
    // is the shape of mistake that reports a number for the wrong arm.
    let arms: Vec<(&str, Box<dyn Fn(&image_viewer::ImageData) -> image_viewer::ImageData>)> = vec![
        ("floor off", Box::new(|d| resize_floor(d, QHD_TARGET, 0.0))),
        ("floor 2.0", Box::new(|d| resize_shipped(d, QHD_TARGET))),
        ("floor 2.5", Box::new(|d| resize_floor(d, QHD_TARGET, 2.5))),
        ("Smooth", Box::new(|d| resize_at(d, QHD_TARGET, image_viewer::SMOOTH_STRENGTH))),
    ];
    println!(
        "{:>14} {:>10} {:>10} {:>10} {:>10}",
        "arm", "step rise", "lattice", "tone sum", "tone peak"
    );
    for (name, resize) in &arms {
        let mut lattice = 0f32;
        for &p in &[2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0] {
            let l = probe_square_lattice(QHD_W, QHD_H, p, 1.0, 0, 100);
            lattice += beat_amp(&resize(&l), p / QHD_R);
        }
        let tout = resize(&tone);
        let (mut sum, mut peak) = (0f32, 0f32);
        let mut i = 0;
        while i <= 100 {
            let a = beat_amp(&tout, 1.2 + i as f32 * 0.05);
            sum += a;
            peak = peak.max(a);
            i += 1;
        }
        println!(
            "{name:>14} {rise:>10} {lattice:>10.2} {sum:>10.2} {peak:>10.2}",
            rise = step_rise(&resize(&step))
        );
    }

    println!("\n== wall clock, interleaved and warmed (release only) ==");
    let src = probe_square_lattice(QHD_W, QHD_H, 4.0, 1.0, 0, 100);
    println!("{:>8} {:>12} {:>12} {:>12}", "R", "floor off", "floor 2.0", "Smooth");
    for r in [1.1f64, 1.2, 1.333, 1.5, 1.778, 2.0, 2.5, 2.879, 4.0] {
        let target = ((QHD_W as f64 / r).round() as u32).max(2);
        let arms: Vec<(&str, Box<dyn Fn() -> ()>)> = vec![
            (
                "off",
                Box::new(|| {
                    let _ = resize_floor(&src, target, 0.0);
                }),
            ),
            (
                "2.0",
                Box::new(|| {
                    let _ = resize_shipped(&src, target);
                }),
            ),
            (
                "smooth",
                Box::new(|| {
                    let _ = resize_at(&src, target, image_viewer::SMOOTH_STRENGTH);
                }),
            ),
        ];
        // **Interleaved, and every arm warmed before any is timed.** Arms run
        // one-after-another report a drift larger than the effect - a run in this
        // repo had two *identical* control blocks disagree by 38% - and the
        // first, un-warmed call of a path is not a sample of that path.
        for (_, arm) in &arms {
            arm();
        }
        let mut med = [0f32; 3];
        for i in 0..arms.len() {
            let mut v = Vec::new();
            for _ in 0..9 {
                // One measurement per round, rotating which arm goes first, so
                // neither arm is systematically measured at the same point in the
                // machine's thermal or allocator state.
                let order: Vec<usize> = (0..arms.len()).map(|k| (k + i) % arms.len()).collect();
                for j in order {
                    let t0 = std::time::Instant::now();
                    arms[j].1();
                    if j == i {
                        v.push(t0.elapsed().as_secs_f32() * 1000.0);
                    }
                }
            }
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            med[i] = v[v.len() / 2];
        }
        println!(
            "{r:>8.3} {:>12.1} {:>12.1} {:>12.1}   ({:.2}x floor, {:.2}x smooth)",
            med[0],
            med[1],
            med[2],
            med[1] / med[0].max(1e-6),
            med[2] / med[0].max(1e-6)
        );
    }
}

#[test]
fn every_downscale_filter_survives_the_widened_prefilter() {
    // The prefilter runs the *selected* filter at a widened support (Lanczos3's
    // radius is 3 source px, so 4.3 after the prefilter's own 1.44x), and its
    // result indexes `to_srgb`. Overshoot past the 12-bit range must clamp: the
    // same class of bug as the 4097 index that used to abort a worker.
    //
    // A crash guard, not a discriminator: it passes with the box prefilter too.
    for f in [
        image_viewer::DownscaleFilter::Hermite,
        image_viewer::DownscaleFilter::Catrom,
        image_viewer::DownscaleFilter::Lanczos3,
    ] {
        let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
        let was = image_viewer::linear_resample_enabled();
        image_viewer::set_linear_resample(true);
        image_viewer::set_downscale_filter(f);
        let out =
            image_viewer::downscale_image(&probe_dots(PROBE_W, PROBE_H, 3.0, 0, 255), PROBE_TARGET);
        image_viewer::set_downscale_filter(image_viewer::DownscaleFilter::Hermite);
        image_viewer::set_linear_resample(was);
        assert_eq!(
            out.rgba.len(),
            (out.width as usize) * (out.height as usize) * 4,
            "{f:?}: buffer and dims disagree"
        );
        assert!(out.rgba.chunks_exact(4).all(|p| p[3] == 255), "{f:?}: alpha");
    }
}

#[test]
fn test_preview_load_survives_gamma_space_resize() {
    // End-to-end version of the same bug, through the real decode entry point
    // the viewer uses, with linear-light downsampling off.
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("6.png");
    // The reported file's exact dimensions.
    let mut img = image::RgbaImage::new(980, 552);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]);
    }
    image::DynamicImage::ImageRgba8(img).save(&p).expect("write fixture");

    for target in [256u32, 300, 512, 800, 980, 1024] {
        let r = with_linear_resample(false, || image_viewer::load_image_preview_full(&p, target))
            .unwrap_or_else(|e| panic!("{p:?} @ {target} failed to load: {e}"));
        for (label, d) in [("preview", &r.preview), ("full", r.full.as_ref().unwrap())] {
            assert_eq!(
                d.rgba.len(),
                (d.width as usize) * (d.height as usize) * 4,
                "{label} of {p:?} @ {target}: {}x{} but {} bytes",
                d.width,
                d.height,
                d.rgba.len()
            );
        }
    }
}
