//! The linear-light prefilter: the pins, and the measurements behind them.
//!
//! Split out of `tests/resample.rs`; the probes and the globals guard are in
//! `tests/common/resample_fixtures.rs`.
//!
//! `linear_resize` is two steps: reduce the full-resolution source to at most 2x
//! the target, then two Hermite passes in 12-bit linear light down to the target.
//! The first step used to be an sRGB **box** average, and a box's sinc sidelobes
//! sit near -13 dB without rolling off, so it was aliasing *more* than the kernel
//! pass it fed. On a 2223x3109 page at the real 2.879x reduction (the manga
//! half-panel target of 1080) the residual in the alias band, measured as the
//! output amplitude of a 100-code sinusoid, was 21-29 codes; with a widened
//! kernel in the prefilter it is 2-11.
//!
//! The measurements below were taken with a harness mirroring `linear_resize`
//! verbatim (same f32 order, same normalisation, byte-exact against
//! `downscale_image` for the box variant), one variable changed at a time. The
//! "box" column is what the code did before; the pins are the kernel numbers plus
//! two codes of slack, so a later filter tweak fails loudly rather than silently.
//!
//! **What is not claimed.** This reduces the artefact, it does not abolish it. A
//! tone at the source Nyquist cannot survive a 2.879x downscale without
//! aliasing; at source period 5 (1.7 output px) 23 codes still get through, and
//! the artefact is a hard 1-2 px sparkle, not a slow beat.
//!
//! The `print_*` tests are `#[ignore]`d: they are how a kernel change is
//! re-pinned from a printout rather than from a bisect, and they assert nothing.

#[path = "common/resample_fixtures.rs"]
mod resample_fixtures;

use resample_fixtures::*;

/// Mean |dx| over a fixed crop of the output, in codes per output pixel. The
/// sharpness number: applied identically at every strength, and on the same
/// crop, so the only thing that moves it is the kernel.
fn edge_grad(d: &image_viewer::ImageData, crop: (u32, u32, u32, u32)) -> f32 {
    let (cx, cy, cw, ch) = crop;
    let w = d.width as usize;
    let mut sum = 0f64;
    let mut n = 0f64;
    for y in cy as usize..(cy + ch) as usize {
        for x in cx as usize..(cx + cw - 1) as usize {
            sum += (d.rgba[(y * w + x + 1) * 4] as f64 - d.rgba[(y * w + x) * 4] as f64).abs();
            n += 1.0;
        }
    }
    if n == 0.0 { 0.0 } else { (sum / n) as f32 }
}

/// Which output pixels sit on **flat tone** and which on a **strong edge**.
///
/// **Decided from the full-resolution source, once, and the same mask is scored
/// at every strength.** This is the split the two questions actually need, and
/// without it neither is answerable on a real page:
///
/// * The lattice lives in *screentone*. A page's global p95-p5 is set by its
///   black line art against white paper (242-255 codes on all three fixtures,
///   essentially unmoved by any strength), so a whole-page statistic cannot see
///   the artefact at all - measured: `print_real_page_strength_sweep` moves it
///   by 7% on 32.jpg while `grad` falls 54% at the same strength.
/// * The cost lands on *edges*. A whole-page mean gradient spends most of its
///   samples on flat tone, so it reads a sparkle removal as a sharpness loss.
///
/// Both masks are computed from the source with a 3x3 window, and neither looks
/// at the output, so the two numbers cannot flatter or punish a strength by
/// picking different pixels per arm.
fn tone_and_edge_masks(
    src: &image_viewer::ImageData,
    out_w: u32,
    out_h: u32,
) -> (Vec<bool>, Vec<bool>) {
    let (sw, sh) = (src.width as usize, src.height as usize);
    let g = |x: usize, y: usize| src.rgba[(y * sw + x) * 4] as f32;
    // Per source pixel: local range over a 3x3 window. A screentone's range is
    // its own dither amplitude; a line edge's is the full black-to-white step.
    let mut tone = Vec::with_capacity((out_w as usize) * (out_h as usize));
    let mut edge = Vec::with_capacity(tone.capacity());
    for oy in 0..out_h as usize {
        for ox in 0..out_w as usize {
            // Nearest source pixel, matching the resize's own sampling density.
            let sx = ((ox as f32 + 0.5) * sw as f32 / out_w as f32) as usize;
            let sy = ((oy as f32 + 0.5) * sh as f32 / out_h as f32) as usize;
            let x0 = sx.saturating_sub(1);
            let y0 = sy.saturating_sub(1);
            let x1 = (sx + 1).min(sw - 1);
            let y1 = (sy + 1).min(sh - 1);
            let (mut lo, mut hi) = (255f32, 0f32);
            for yy in y0..=y1 {
                for xx in x0..=x1 {
                    let v = g(xx, yy);
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
            }
            let range = hi - lo;
            // Flat tone: local range under a quarter of the full scale. Edge:
            // over half. The two cannot both be true and the gap is wide, so the
            // thresholds are not delicate.
            tone.push(range < 64.0);
            edge.push(range > 128.0);
        }
    }
    (tone, edge)
}

/// Mean |dx| over the pixels `mask` selects, in codes per output pixel.
///
/// On the **edge** mask this is the sharpness number and it should fall as the
/// kernel widens; on the **tone** mask it is the sparkle number and it is what
/// the setting is for.
fn masked_grad(d: &image_viewer::ImageData, mask: &[bool]) -> f32 {
    let w = d.width as usize;
    let mut sum = 0f64;
    let mut n = 0f64;
    for y in 0..d.height as usize {
        for x in 0..w.saturating_sub(1) {
            if !mask[y * w + x] {
                continue;
            }
            sum += (d.rgba[(y * w + x + 1) * 4] as f64 - d.rgba[(y * w + x) * 4] as f64).abs();
            n += 1.0;
        }
    }
    if n == 0.0 { 0.0 } else { (sum / n) as f32 }
}

/// Print the numbers every probe above is pinned against. `cargo test --test unit
/// -- --ignored --nocapture print_prefilter_pins`. Kept in the file so a filter
/// change can be re-pinned from a printout rather than from a bisect.
#[test]
#[ignore]
fn print_prefilter_pins() {
    let out = probe_resize(&probe_sinusoid(PROBE_W, PROBE_H, 1.0, 100.0));
    println!(
        "geometry: {}x{} -> {}x{}  R {:.4} x {:.4}",
        PROBE_W,
        PROBE_H,
        out.width,
        out.height,
        PROBE_W as f32 / out.width as f32,
        PROBE_H as f32 / out.height as f32
    );
    print!("sinusoid 100 codes: ");
    for p in [2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0] {
        let src = probe_sinusoid(PROBE_W, PROBE_H, p, 100.0);
        print!("{}:{:.0} ", p, probe_amplitude(&probe_resize(&src)));
    }
    println!();
    print!("dots 0/100 codes:  ");
    for p in [2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0] {
        let src = probe_dots(PROBE_W, PROBE_H, p, 0, 100);
        print!("{}:{:.0} ", p, probe_amplitude(&probe_resize(&src)));
    }
    println!();
    print!("8x reduction:      ");
    for p in [2.5f32, 3.0, 5.0, 6.0, 11.0] {
        let src = probe_sinusoid(1200, 1200, p, 100.0);
        let out = resize_at(&src, 150, image_viewer::FAST_STRENGTH);
        print!("{}:{:.0} ", p, probe_amplitude(&out));
    }
    println!();
    let dither = probe_dither(PROBE_W, PROBE_H);
    let flat = probe_resize(&{
        let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
        for p in rgba.chunks_exact_mut(4) {
            p.copy_from_slice(&[128, 128, 128, 255]);
        }
        image_viewer::ImageData {
            width: PROBE_W,
            height: PROBE_H,
            rgba,
            load_time: std::time::Duration::ZERO,
        }
    });
    println!(
        "50% dither mean {:.2} (in {:.2}); flat 128 mean {:.2}",
        probe_mean(&probe_resize(&dither)),
        probe_mean(&dither),
        probe_mean(&flat)
    );
}

/// Where the real-content fixtures live, if they do. `target/release/examples`
/// is a build output and gitignored, so every reader of it skips rather than
/// dying - the same exemption the criterion targets take for `test-images/`.
fn real_page(name: &str) -> Option<std::path::PathBuf> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("release")
        .join("examples")
        .join(name);
    p.exists().then_some(p)
}

/// Copy a real page into a tempdir and load it, so nothing in the fixture
/// directory is opened for writing. The real pages are read-only inputs.
fn load_real_page(dir: &std::path::Path, name: &str) -> Option<image_viewer::ImageData> {
    let src = real_page(name)?;
    let dst = dir.join(name);
    std::fs::copy(&src, &dst).ok()?;
    image_viewer::load_image(&dst, image_viewer::MAX_TEXTURE_DIM).ok()
}

/// The strengths a sweep is run at. One list, so a table and the thing it is
/// compared against cannot be at different strengths.
const SWEEP_STRENGTHS: &[f32] = &[1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// The synthetic square-lattice sweep. `cargo test --test unit -- --ignored
/// --nocapture print_square_lattice_sweep`. Cheap (a ~1.1 MP source), so it is
/// the one to re-run when re-pinning.
#[test]
#[ignore]
fn print_square_lattice_sweep() {
    println!("== synthetic square lattice, {PROBE_W}x{PROBE_H} -> {PROBE_TARGET} ==");
    println!(
        "{:>8} {:>6} | {:>9} {:>9} | {:>9} {:>9} | {:>9}",
        "strength", "pitch", "amp", "amp x", "beat", "beat x", "grad x"
    );
    for &pitch in &[2.5f32, 3.0, 3.5, 4.0, 5.0, 6.0, 8.0] {
        let src = probe_square_lattice(PROBE_W, PROBE_H, pitch, 1.0, 0, 100);
        let base = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let r = PROBE_W as f32 / base.width as f32;
        let p_out = pitch / r;
        let (a_base, b_base) = (alias_amp(&base), beat_amp(&base, p_out));
        let crop = (base.width / 4, base.height / 4, base.width / 2, base.height / 2);
        let g_base = edge_grad(&base, crop);
        for &strength in SWEEP_STRENGTHS {
            let out = resize_at(&src, PROBE_TARGET, strength);
            let (a, b, g) = (alias_amp(&out), beat_amp(&out, p_out), edge_grad(&out, crop));
            println!(
                "{strength:>8.2} {pitch:>6.1} | {a:>9.2} {:>9.3} | {b:>9.2} {:>9.3} | {:>9.3}",
                a / a_base.max(1e-6),
                b / b_base.max(1e-6),
                g / g_base.max(1e-6)
            );
        }
    }
}

/// The same sweep on real pages. Separate from the synthetic one because a full
/// decode of a 6.9 MP page is slow in a debug build and the two want different
/// iteration cycles.
#[test]
#[ignore]
fn print_real_page_strength_sweep() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, target) in [("32.jpg", 1080u32), ("12.jpg", 1080), ("028.jpg", 1080)] {
        let Some(page) = load_real_page(tmp.path(), name) else {
            println!("== {name}: absent, skipped ==");
            continue;
        };
        let base = resize_at(&page, target, image_viewer::FAST_STRENGTH);
        println!(
            "== {name} {}x{} -> {}x{} (R {:.4}) ==",
            page.width,
            page.height,
            base.width,
            base.height,
            page.width as f32 / base.width as f32
        );
        let (tone, edge) = tone_and_edge_masks(&page, base.width, base.height);
        let n_tone = tone.iter().filter(|b| **b).count();
        let n_edge = edge.iter().filter(|b| **b).count();
        println!(
            "   flat-tone pixels {n_tone} ({:.1}%), edge pixels {n_edge} ({:.1}%)",
            100.0 * n_tone as f64 / tone.len() as f64,
            100.0 * n_edge as f64 / edge.len() as f64
        );
        println!(
            "{:>8} | {:>10} {:>6} | {:>10} {:>6} | {:>8} {:>6}",
            "strength", "tone HF", "x", "edge grad", "x", "p95-5", "x"
        );
        let (t_base, e_base, a_base) =
            (masked_grad(&base, &tone), masked_grad(&base, &edge), alias_amp(&base));
        for &strength in SWEEP_STRENGTHS {
            let out = resize_at(&page, target, strength);
            let (t, e, a) = (masked_grad(&out, &tone), masked_grad(&out, &edge), alias_amp(&out));
            println!(
                "{strength:>8.2} | {t:>10.3} {:>6.3} | {e:>10.3} {:>6.3} | {a:>8.1} {:>6.3}",
                t / t_base.max(1e-6),
                e / e_base.max(1e-6),
                a / a_base.max(1e-6)
            );
        }
    }
}

/// Write the **same crop** of a real page at each strength, scaled 2x nearest,
/// so the artefact and the cost can be looked at rather than only measured.
///
/// `cargo test --test unit --release -- --ignored --nocapture
/// export_strength_crops`. Writes into `target/strength-probe/`. This is the one
/// piece of evidence a number cannot give, and it is why the strength was chosen
/// by eye as well as by table - a 71% wider step edge is either obviously softer
/// or not, and only a picture says which.
#[test]
#[ignore]
fn export_strength_crops() {
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target").join("strength-probe");
    std::fs::create_dir_all(&dir).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    // Crops as fractions of the output, so one entry works at either page size.
    // (x, y, w, h) in 1000ths of the output's dimensions.
    for (name, target, frac) in [
        // The screentone field: where the lattice lives.
        ("32.jpg", 1080u32, (60u32, 430u32, 500u32, 320u32)),
        // Line art: where the cost of removing it lands.
        ("028.jpg", 1080, (330, 60, 620, 420)),
    ] {
        let Some(page) = load_real_page(tmp.path(), name) else {
            println!("{name} absent, skipped");
            continue;
        };
        for &s in &[1.0f32, 1.5, 1.75, 2.0] {
            let out = resize_at(&page, target, s);
            let (w, h) = (out.width as usize, out.height as usize);
            let cx = w * frac.0 as usize / 1000;
            let cy = h * frac.1 as usize / 1000;
            let cw = (w * frac.2 as usize / 1000).min(w - cx);
            let ch = (h * frac.3 as usize / 1000).min(h - cy);
            // 2x nearest so the sparkle is visible at 1:1 on screen.
            let (sw, sh) = (cw * 2, ch * 2);
            let mut buf = vec![0u8; sw * sh * 4];
            for y in 0..sh {
                for x in 0..sw {
                    let si = ((cy + y / 2) * w + cx + x / 2) * 4;
                    let di = (y * sw + x) * 4;
                    buf[di..di + 4].copy_from_slice(&out.rgba[si..si + 4]);
                }
            }
            let img = image::RgbaImage::from_raw(sw as u32, sh as u32, buf).unwrap();
            let stem = name.trim_end_matches(".jpg");
            let p = dir.join(format!("{stem}-s{s:.2}.png"));
            img.save(&p).unwrap();
            println!("wrote {}", p.display());
        }

        // A side-by-side strip of the same crop at every strength, so the
        // comparison is one image rather than four read one at a time - which is
        // how a difference gets looked at honestly.
        let arms: Vec<image_viewer::ImageData> =
            [1.0f32, 1.5, 1.75, 2.0].iter().map(|&s| resize_at(&page, target, s)).collect();
        let base = &arms[0];
        let w = base.width as usize;
        let h = base.height as usize;
        let (cx, cy) = (
            (w * frac.0 as usize / 1000).min(w.saturating_sub(1)),
            (h * frac.1 as usize / 1000).min(h.saturating_sub(1)),
        );
        let cw = (w * frac.2 as usize / 1000).min(w - cx);
        let ch = (h * frac.3 as usize / 1000).min(h - cy);
        let (sw, sh) = (cw * 2, ch * 2);
        let strip_w = sw * arms.len() + 4 * (arms.len() - 1);
        let mut strip = vec![255u8; strip_w * sh * 4];
        for (i, arm) in arms.iter().enumerate() {
            let ox = i * (sw + 4);
            for y in 0..sh {
                for x in 0..sw {
                    let si = ((cy + y / 2) * w + cx + x / 2) * 4;
                    let di = (y * strip_w + ox + x) * 4;
                    strip[di..di + 4].copy_from_slice(&arm.rgba[si..si + 4]);
                }
            }
        }
        let stem = name.trim_end_matches(".jpg");
        let p = dir.join(format!("{stem}-strip.png"));
        image::RgbaImage::from_raw(strip_w as u32, sh as u32, strip).unwrap().save(&p).unwrap();
        println!("wrote {} (left to right: 1.00, 1.50, 1.75, 2.00)", p.display());
    }
}

/// The step edge's output profile, printed. Exists because `step_rise` reads
/// 0.000 at every strength and that is either a real limit or a bug in the
/// metric, and those are very different things to build a sharpness number on.
#[test]
#[ignore]
fn print_step_edge_profile() {
    let src = probe_step(PROBE_W, PROBE_H, PROBE_W / 2);
    for &s in &[1.0f32, 2.0, 3.0] {
        let out = resize_at(&src, PROBE_TARGET, s);
        let w = out.width as usize;
        let mid = out.height as usize / 2;
        let prof: Vec<u8> = (w / 2 - 5..w / 2 + 5).map(|x| out.rgba[(mid * w + x) * 4]).collect();
        println!("  strength {s}: {} values around the edge: {:?}", out.width, prof);
    }
}

/// Step-edge rise at each strength, plus the real-page numbers beside it.
#[test]
#[ignore]
fn print_step_edge_sharpness() {
    println!("== step edge, 890x1244 -> 432 (R 2.880); rise in output px ==");
    let src = probe_step(PROBE_W, PROBE_H, PROBE_W / 2);
    for &s in SWEEP_STRENGTHS {
        let out = resize_at(&src, PROBE_TARGET, s);
        println!(
            "  strength {s:>4.2}: rise {:.3} output px  ({:.3} source px)",
            step_rise(&out),
            step_rise(&out) * PROBE_W as f32 / out.width as f32
        );
    }
    // A thin-line probe too: a 1.5 source px line is what a line-art page is
    // mostly made of, and it is the case where widening the kernel is most
    // visible.
    println!("== 3-source-px bar, 890x1244 -> 432 ==");
    let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
    for y in 0..PROBE_H as usize {
        for x in 0..PROBE_W as usize {
            let v = if (x as u32) % 40 < 3 { 0u8 } else { 255 };
            let o = (y * PROBE_W as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let bars = image_viewer::ImageData {
        width: PROBE_W,
        height: PROBE_H,
        rgba,
        load_time: std::time::Duration::ZERO,
    };
    let base = resize_at(&bars, PROBE_TARGET, image_viewer::FAST_STRENGTH);
    let crop = (base.width / 4, base.height / 4, base.width / 2, base.height / 2);
    let g0 = edge_grad(&base, crop);
    for &s in SWEEP_STRENGTHS {
        let out = resize_at(&bars, PROBE_TARGET, s);
        println!(
            "  strength {s:>4.2}: bar grad {:.3} codes/px  ({:.3}x of fast)",
            edge_grad(&out, crop),
            edge_grad(&out, crop) / g0
        );
    }
}

/// Is the residual on the worst-case page still a **lattice**, or is it now
/// broadband? This is the question the honest limit turns on.
///
/// `alias_amp` going down does not say the artefact went away - it says *some*
/// amplitude went down, and a 1-px sparkle and a lattice of the same amplitude
/// are not the same defect. So: project the flat-tone pixels of the output onto
/// a 2-D sinusoid at every output period from 1.5 to 8 px, and look at the
/// **peak** and at where it sits. A lattice shows one sharp peak at the pitch of
/// the artefact; broadband residue shows a low, flat curve.
#[test]
#[ignore]
fn print_tone_residual_spectrum() {
    let tmp = tempfile::tempdir().unwrap();
    for (name, target) in [("32.jpg", 1080u32), ("12.jpg", 1080)] {
        let Some(page) = load_real_page(tmp.path(), name) else {
            println!("{name} absent, skipped");
            continue;
        };
        let base = resize_at(&page, target, image_viewer::FAST_STRENGTH);
        let (tone, _) = tone_and_edge_masks(&page, base.width, base.height);
        println!(
            "== {name} -> {}x{}, flat-tone 2-D spectrum of mean amplitude per output px ==",
            base.width, base.height
        );
        let arms: Vec<image_viewer::ImageData> =
            SWEEP_STRENGTHS.iter().map(|&s| resize_at(&page, target, s)).collect();
        let mut names: Vec<String> = SWEEP_STRENGTHS.iter().map(|s| format!("{s:.2}")).collect();
        names.push("mean".into());
        println!("{:>6} | {}", "period", names.join(" | "));
        for &p_out in &[1.5f32, 2.0, 2.5, 3.0, 4.0, 6.0, 8.0] {
            let row: Vec<f32> = arms.iter().map(|d| tone_spectrum_amp(d, &tone, p_out)).collect();
            let mean = row.iter().sum::<f32>() / row.len() as f32;
            let cells: Vec<String> = row
                .iter()
                .map(|v| format!("{v:.3}"))
                .chain(std::iter::once(format!("{mean:.3}")))
                .collect();
            println!("{p_out:>6.1} | {}", cells.join(" | "));
        }
    }
}

/// Amplitude of a 2-D sinusoid of output period `p_out`, averaged over the
/// flat-tone pixels only. Same projection as `beat_amp`, restricted to the mask.
fn tone_spectrum_amp(d: &image_viewer::ImageData, mask: &[bool], p_out: f32) -> f32 {
    let (w, h) = (d.width as usize, d.height as usize);
    let k = 2.0 * std::f64::consts::PI / p_out as f64;
    // Subtract the mask's own mean first, or a non-zero DC leaks into the
    // projection and reads as structure that is not there.
    let (mut n, mut mean) = (0f64, 0f64);
    for (i, m) in mask.iter().enumerate().take(w * h) {
        if *m {
            mean += d.rgba[i * 4] as f64;
            n += 1.0;
        }
    }
    if n < 8.0 {
        return 0.0;
    }
    mean /= n;
    let (mut sss, mut ssc, mut sys, mut syc) = (0f64, 0f64, 0f64, 0f64);
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if !mask[i] {
                continue;
            }
            let v = d.rgba[i * 4] as f64 - mean;
            let s = (k * x as f64).sin() + (k * y as f64).sin();
            let c = (k * x as f64).cos() + (k * y as f64).cos();
            sss += s * s;
            ssc += s * c;
            sys += v * s;
            syc += v * c;
        }
    }
    if sss <= 0.0 {
        return 0.0;
    }
    let (a, b) = if ssc.abs() < 1e-9 * sss.max(1.0) {
        (sys / sss, syc / sss)
    } else {
        let det = sss * sss - ssc * ssc;
        if det.abs() < 1e-9 {
            return 0.0;
        } else {
            ((sys * sss - syc * ssc) / det, (syc * sss - sys * ssc) / det)
        }
    };
    // **No `/ sqrt(n)`.** There was one, and it was wrong: for a *coherent*
    // pattern - which is what a dot screen is - the amplitude is `A` at every
    // masked pixel, so the least-squares fit above already returns `A` (up to the
    // 1/sqrt(2) of adding the two axes). Dividing that by the square root of the
    // pixel count then reports ~0.0002 where `alias_amp` on the same image reads
    // 10 codes, i.e. it divides a real signal by ~700 at this output size, and it
    // cannot resolve a coherent peak at all. Every conclusion drawn from it -
    // "no peak at any output period from 1.5 to 8 px at any strength, the residue
    // is broadband" - was read off that divided quantity. `beat_amp`, the
    // sibling projection, has never had this division, which is why it reports
    // codes and this did not.
    (a * a + b * b).sqrt() as f32
}

/// **`tone_spectrum_amp` reports codes, and not a quantity smaller by ~sqrt(n).**
///
/// The function carried a `/ sqrt(n)`, on the theory that it measured "mean
/// amplitude per output pixel". For a *coherent* pattern — which is the only kind
/// this is asked about, because a dot screen is coherent by construction — the
/// amplitude is `A` at every masked pixel, so the least-squares fit already
/// returns `A` and the extra division divides a real signal by the square root of
/// the pixel count. At a 890x1244 output with an all-true mask that is a factor of
/// 1045.
///
/// It is worth a test because the consequence was not cosmetic: every per-pitch
/// number in the strength record came from it, including the claim that the
/// surviving artefact is broadband with "no peak at any output period from 1.5 to
/// 8 px at any strength". A metric that cannot resolve a coherent peak cannot
/// support that claim, and the claim was load-bearing — it is what said the
/// lattice was "reduced, not abolished".
///
/// Calibrated here against the sibling projection `beat_amp`, which has never had
/// the division and has always reported codes: on the same image, at the same
/// output period, the two must now agree to within the 1/sqrt(2) that adding the
/// x and y axes costs, and **not** differ by three orders of magnitude. The
/// lattice's own output pitch is probed, which is where the answer is non-zero —
/// probing away from it is what let the bad metric look like a flat curve.
#[test]
fn the_tone_spectrum_metric_reports_codes() {
    for &(p, d) in &[(4.0f32, 1.0f32), (4.0, 2.0), (6.0, 2.0)] {
        let src = probe_square_lattice(PROBE_W, PROBE_H, p, d, 0, 100);
        let out = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let p_out = p / (PROBE_W as f32 / out.width as f32);
        let mask = vec![true; (out.width * out.height) as usize];
        let (a, b) = (tone_spectrum_amp(&out, &mask, p_out), beat_amp(&out, p_out));
        assert!(
            a > 0.5,
            "lattice pitch {p} dot {d} at its own output pitch {p_out:.3}: \
             tone_spectrum_amp reported {a:.5} codes, want a real amplitude - a \
             metric this small cannot resolve a coherent pattern"
        );
        // `beat_amp` fits the same two projections; the two may differ by the
        // 1/sqrt(2) of summing sin in both axes, and by the sign convention of
        // the two-term solve, but they are the same order of magnitude.
        assert!(
            a <= b.max(a) * 4.0 + 1.0 && b <= a * 4.0 + 1.0,
            "lattice pitch {p} dot {d}: tone_spectrum_amp {a:.3} and beat_amp \
             {b:.3} are not the same quantity"
        );
    }
}

/// Wall clock at the two shipped positions, on the worst-case real page.
/// Interleaved arms, both warmed before either is timed.
#[test]
#[ignore]
fn print_downscale_strength_wall_clock() {
    let tmp = tempfile::tempdir().unwrap();
    let Some(page) = load_real_page(tmp.path(), "32.jpg") else {
        println!("32.jpg absent, skipped");
        return;
    };
    println!("== wall clock, 32.jpg 2223x3109 -> 1080, ms (release) ==");
    // **Interleaved one at a time, in alternating order, not arm-after-arm.**
    // The first version of this ran all of `fast` and then all of `smooth`, and
    // the two `fast` blocks it reported on either side of `smooth` disagreed by
    // 38% (76 ms then 105 ms) - so whatever drifts here (thread pool warming,
    // the turbojpeg scratch buffers, the page cache) is worth more than the
    // effect being measured. One resize per alternating step means the drift is
    // shared between the arms instead of accumulating on one of them.
    let mut fast = Vec::new();
    let mut smooth = Vec::new();
    // Warm both first: the cache and the thread pool are shared process state,
    // and the first call into either arm pays for both.
    for &s in &[image_viewer::FAST_STRENGTH, image_viewer::SMOOTH_STRENGTH] {
        let _ = resize_at(&page, 1080, s);
    }
    for i in 0..9 {
        // Alternate which arm goes first, so neither systematically inherits the
        // other's warm-up or cool-down.
        let order: &[f32] = if i % 2 == 0 {
            &[image_viewer::FAST_STRENGTH, image_viewer::SMOOTH_STRENGTH]
        } else {
            &[image_viewer::SMOOTH_STRENGTH, image_viewer::FAST_STRENGTH]
        };
        for &s in order {
            let t = std::time::Instant::now();
            let _ = resize_at(&page, 1080, s);
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            if s == image_viewer::FAST_STRENGTH {
                fast.push(ms);
            } else {
                smooth.push(ms);
            }
        }
    }
    for (name, mut v) in [("fast 1.0", fast), ("smooth 2.0", smooth)] {
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        println!(
            "  {name:>11}: median {:.0} ms   min {:.0}   max {:.0}",
            v[v.len() / 2],
            v[0],
            v[v.len() - 1]
        );
    }
}

/// THE guard on the strength: on a **square lattice**, which is the only probe
/// that can show the artefact, `Smooth` must leave strictly less than `Fast`.
///
/// **Why a square lattice and not the sinusoid above.** `probe_sinusoid` varies
/// along x only, so it cannot produce a lattice in the first place — it reports
/// the horizontal transfer function, and a horizontal transfer function reading
/// 4 codes says nothing about a *square* grid. That is not a criticism of the
/// probe: it is exactly the right probe for the question it asks, which is why it
/// is kept. It is the wrong probe for this one, and the previous session's
/// "4 codes, therefore fixed" came from asking it this.
///
/// **Why `Fast` is the control rather than an absolute number.** An absolute
/// pin would say one strength is good enough and would go stale silently if the
/// kernel changed. The property that matters is the *direction and the size* of
/// the trade, and it is measurable without a reference implementation: more
/// strength must mean less lattice. The absolute numbers are in the comment.
#[test]
fn smooth_low_passes_a_square_lattice_harder_than_fast() {
    // alias_amp (p95-p5, whole output, both axes) at the probe's 2.880x:
    //
    //   pitch   2.5   3.0   3.5   4.0   5.0   6.0   8.0
    //   Fast   10.0   2.0  14.0  10.0  19.0  22.0  23.0
    //   Smooth  1.0   0.0   1.0   1.0   0.0   0.0   4.0
    //
    // Pins are the Smooth numbers plus one code, the same slack the prefilter
    // tests use: the most a different target's f32 contraction could move a
    // weighted sum. At every pitch the ratio is 0.00-0.17x.
    let cases =
        [(2.5f32, 2.0f32), (3.0, 1.0), (3.5, 2.0), (4.0, 2.0), (5.0, 1.0), (6.0, 1.0), (8.0, 5.0)];
    for (pitch, max_codes) in cases {
        let src = probe_square_lattice(PROBE_W, PROBE_H, pitch, 1.0, 0, 100);
        let fast = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let smooth = resize_at(&src, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);
        let (a_fast, a_smooth) = (alias_amp(&fast), alias_amp(&smooth));
        assert!(
            a_smooth <= max_codes,
            "square lattice at source pitch {pitch} px: Smooth left {a_smooth} codes, \
             want <= {max_codes} (Fast leaves {a_fast})"
        );
        assert!(
            a_smooth < a_fast,
            "square lattice at source pitch {pitch} px: Smooth {a_smooth} codes is \
             not below Fast {a_fast} - the strength is not reaching the kernel"
        );
    }
}

/// The same trade, measured as the amplitude of the lattice **at its own output
/// period** rather than as broadband spread.
///
/// The eye groups the surviving pattern into the visible artefact; it is not
/// reading a percentile. So this asks the narrower question — is the lattice's
/// own frequency still there? — and it is a separate projection from
/// `alias_amp`, so the two agreeing is evidence rather than one number twice.
#[test]
fn smooth_removes_the_lattice_at_its_own_output_pitch() {
    // beat_amp at the lattice's own output period, codes:
    //
    //   pitch   3.5   4.0   5.0   6.0
    //   Fast    1.24  2.74  5.75  7.09
    //   Smooth  0.08  0.06  0.10  0.04
    let cases = [(3.5f32, 0.2f32), (4.0, 0.2), (5.0, 0.2), (6.0, 0.2)];
    for (pitch, max_codes) in cases {
        let src = probe_square_lattice(PROBE_W, PROBE_H, pitch, 1.0, 0, 100);
        let fast = resize_at(&src, PROBE_TARGET, image_viewer::FAST_STRENGTH);
        let smooth = resize_at(&src, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);
        let r = PROBE_W as f32 / fast.width as f32;
        let p_out = pitch / r;
        let (b_fast, b_smooth) = (beat_amp(&fast, p_out), beat_amp(&smooth, p_out));
        assert!(
            b_smooth <= max_codes,
            "square lattice at source pitch {pitch} px ({p_out:.2} output px): \
             {b_smooth:.2} codes of lattice at its own pitch survived, want \
             <= {max_codes} (Fast leaves {b_fast:.2})"
        );
    }
}

/// The cost, stated as a number: `Smooth` makes a hard edge wider. Pinned so
/// that "it removes the artefact" cannot be re-earned later by a change that
/// quietly gives the sharpness back — which would mean the setting is no longer
/// doing the thing it is named for.
///
/// 10-90% rise on a step edge at the probe's 2.880x reduction, output pixels:
///
///   Fast 1.607 -> Smooth 2.747   (1.71x)
///
/// The bar probe is the same statement on content a manga page is mostly made
/// of: 3-source-px bars at 2x output. Fast 14.118 -> Smooth 7.634 codes/px.
#[test]
fn smooth_costs_edge_sharpness_and_the_cost_is_bounded() {
    let step = probe_step(PROBE_W, PROBE_H, PROBE_W / 2);
    let fast = resize_at(&step, PROBE_TARGET, image_viewer::FAST_STRENGTH);
    let smooth = resize_at(&step, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH);
    let (r_fast, r_smooth) = (step_rise(&fast), step_rise(&smooth));
    assert!(
        r_smooth > r_fast,
        "Smooth must widen the step edge ({r_fast:.3} -> {r_smooth:.3} output px); \
         it did not, so the strength is not reaching the kernel"
    );
    assert!(
        r_smooth <= r_fast * 1.75,
        "the step edge grew to {r_smooth:.3} output px from {r_fast:.3} \
         ({:.2}x) - past the 1.71x the trade was chosen at, which means the \
         default has moved",
        r_smooth / r_fast.max(1e-6)
    );

    let mut rgba = vec![0u8; (PROBE_W as usize) * (PROBE_H as usize) * 4];
    for y in 0..PROBE_H as usize {
        for x in 0..PROBE_W as usize {
            let v = if (x as u32) % 40 < 3 { 0u8 } else { 255 };
            let o = (y * PROBE_W as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let bars = image_viewer::ImageData {
        width: PROBE_W,
        height: PROBE_H,
        rgba,
        load_time: std::time::Duration::ZERO,
    };
    let crop = |d: &image_viewer::ImageData| (d.width / 4, d.height / 4, d.width / 2, d.height / 2);
    let (b_fast, b_smooth) = (
        resize_at(&bars, PROBE_TARGET, image_viewer::FAST_STRENGTH),
        resize_at(&bars, PROBE_TARGET, image_viewer::SMOOTH_STRENGTH),
    );
    let g_fast = edge_grad(&b_fast, crop(&b_fast));
    let g_smooth = edge_grad(&b_smooth, crop(&b_smooth));
    assert!(
        g_smooth < g_fast * 0.60,
        "3-px bar gradient {g_smooth:.3} vs Fast {g_fast:.3} ({:.3}x); it must fall \
         below 0.60x - 0.541x is what the default was chosen at",
        g_smooth / g_fast.max(1e-6)
    );
}

/// `Fast` is **today's kernel, bit for bit**, and that is checkable without a
/// reference build: at strength 1.0 the multiplier is the identity, so the
/// output has to equal what an unscaled `filter_scale` would produce. Held by
/// asserting `FAST_STRENGTH == 1.0` and that `set_downscale_smoothing(false)`
/// lands exactly on it — the two facts together mean the `Fast` position is the
/// pre-existing code path and not a third flavour of it.
///
/// This is the guard that would catch someone "tidying" `FAST_STRENGTH` to
/// something else, which would silently make the toggle a lie: the label would
/// say Fast and the picture would change anyway.
#[test]
fn fast_is_exactly_the_pre_existing_kernel_width() {
    assert_eq!(image_viewer::FAST_STRENGTH, 1.0);
    assert!(image_viewer::SMOOTH_STRENGTH > image_viewer::FAST_STRENGTH);

    let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let was = image_viewer::downscale_strength();
    image_viewer::set_downscale_smoothing(false);
    let at_fast = image_viewer::downscale_strength();
    let smoothing_off = image_viewer::downscale_smoothing();
    image_viewer::set_downscale_smoothing(true);
    let at_smooth = image_viewer::downscale_strength();
    let smoothing_on = image_viewer::downscale_smoothing();
    image_viewer::set_downscale_strength(was);

    assert_eq!(at_fast, 1.0, "Fast must be the identity multiplier");
    assert!(!smoothing_off);
    assert_eq!(at_smooth, image_viewer::SMOOTH_STRENGTH);
    assert!(smoothing_on);
}

/// **The strength has to reach both halves of the resize, and it has to be one
/// value.** Pinned against the source text, because the failure is not
/// observable from outside and the library cannot reach `main.rs`.
///
/// Two halves, two failure modes, both silent:
///
/// * the prefilter widened but the final passes not, or the reverse. Either way
///   the artefact is only partly removed and nothing says so. The prefilter's
///   own `filter_scale` and the final pass's are separate `contributions` calls.
/// * **the two halves reading the setting separately.** A toggle pressed between
///   the prefilter's weights and the final pass's would resize with a prefilter
///   of one width feeding a final pass of another — a picture that is neither
///   setting. That is the same class as the two manga-mode sites that were each
///   correct alone, which is why this reads the count and not just presence.
///
/// Verified to fail both ways: with `strength` dropped from either
/// `contributions` call site, and with `downscale_strength()` moved into
/// `contributions` so each call site reads it for itself.
#[test]
fn one_strength_reaches_both_halves_of_the_resize() {
    let src = lib_src();

    // Brace-match a function body rather than slicing to the first `\n}`: a
    // nested block at column 0 would end the slice early and the scan would pass
    // on a truncated function.
    let body_of = |name: &str| -> &str {
        let start = src.find(&format!("fn {name}(")).unwrap_or_else(|| panic!("{name} exists"));
        let body = &src[start..];
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
            .expect("braces balance");
        &body[..end]
    };

    let lr = body_of("linear_resize");
    assert_eq!(
        lr.matches("downscale_strength()").count(),
        1,
        "linear_resize must read downscale_strength() exactly once, so the \
         pre-reduction and the final passes cannot disagree"
    );
    assert!(
        lr.contains("kernel_reduce_linear(data, pre_w, pre_h, strength, 0.0)"),
        "the pre-reduction must be handed the strength linear_resize read"
    );
    // The floor rides along with the strength: both final-pass axes get the same
    // pair, so a floor that reached one axis and not the other would be a
    // vertically-striped picture with no error.
    assert!(
        lr.contains("contributions(lw, new_w, &kern, support, strength, floor)")
            && lr.contains("contributions(lh, new_h, &kern, support, strength, floor)"),
        "both final-pass axes must be handed the same strength and the same \
         MIN_FILTER_SCALE floor"
    );

    let kr = body_of("kernel_reduce_linear");
    assert!(
        kr.contains("strength: f32")
            && kr.contains("contributions(sw as u32, pw as u32, &kern, support, strength, floor)")
            && kr.contains("contributions(sh as u32, ph as u32, &kern, support, strength, floor)"),
        "the pre-reduction must take the strength as an argument and use it on \
         both axes - reading it for itself would let it disagree with the final \
         passes"
    );

    let con = body_of("contributions");
    assert!(
        !con.contains("downscale_strength()") && !con.contains("min_filter_scale()"),
        "contributions must take the strength and the floor as parameters, not \
         read the settings: it is called four times per resize and a read there \
         is four chances for the halves to disagree"
    );
    assert!(
        con.contains("strength / scale"),
        "the multiplier has to scale filter_scale, which is the only term that \
         widens the kernel - widening the span alone evaluates every extra tap \
         at exactly zero and is bit-identical"
    );
}
