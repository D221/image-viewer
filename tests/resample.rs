//! The linear-light prefilter, moved out of `tests/unit.rs` in one block.
//!
//! The what: the probe fixtures, the sweeps and the behavioural pins for
//! `linear_resize` and its widened prefilter, the `MIN_FILTER_SCALE` floor,
//! the screentone measurements, and the wiring and config forward-compat
//! scans for the settings they ride on (`Smooth`, the pixel-art sampling).
//! The probe banner that used to open the section notes what the pins
//! measure and — importantly — what they do not claim, and it moved with
//! the fixtures.
//!
//! The helpers below are twins of `tests/unit.rs`'s, on purpose: one test
//! target cannot import from another, so the globals lock and the
//! `with_*` family that guards it, the default-config read and the
//! source-scan collectors each exist once per file. The trap notes move
//! with their helpers, because the trap is the reason the helper looks
//! like that — `unit.rs` keeps its own copies where it still calls them,
//! including the one trap these files were built around: reading a global
//! back instead of pinning it turned a measurement arm into the wrong arm.

static FILTER_GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Set the filter globals at once, restore them after.
///
/// **One lock acquisition, never two.** These used to be separate helpers and
/// nesting them - `with_strength(s, || with_linear_resample(true, ..))` -
/// deadlocked on the second `lock()` of a non-reentrant `Mutex`, which is why
/// the strengths are swept through here rather than through a wrapper of its
/// own. A guard that only worked when the two settings were never changed
/// together would have been a guard that hung instead of testing anything.
///
/// `floor` is [`image_viewer::MIN_FILTER_SCALE`] by default. **`0.0` means the
/// pre-`MIN_FILTER_SCALE` behaviour** and is the arm several tests compare
/// against, so it has to be reachable without a second build - which is why the
/// floor is a runtime atomic and not a bare constant read at the call site.
fn with_resample_at<T>(linear: bool, strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
    let _guard = FILTER_GLOBALS.lock().unwrap_or_else(|e| e.into_inner());
    let (was_linear, was_strength, was_floor) = (
        image_viewer::linear_resample_enabled(),
        image_viewer::downscale_strength(),
        image_viewer::min_filter_scale(),
    );
    image_viewer::set_linear_resample(linear);
    image_viewer::set_downscale_strength(strength);
    image_viewer::set_min_filter_scale(floor);
    let out = f();
    image_viewer::set_min_filter_scale(was_floor);
    image_viewer::set_downscale_strength(was_strength);
    image_viewer::set_linear_resample(was_linear);
    out
}

/// Run `f` with the downscale filter globals forced to `linear`, restoring the
/// previous setting afterwards.
fn with_linear_resample<T>(linear: bool, f: impl FnOnce() -> T) -> T {
    with_resample_at(linear, cached_default_strength(), image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit strength and the shipped settings otherwise.
///
/// **Every global is pinned to a constant, never read back.** `DOWNSCALE_STRENGTH`
/// is initialised to `SMOOTH_STRENGTH` and `MIN_FILTER_SCALE_IN_FORCE` to the
/// floor, so "whatever is in there now" is `Smooth` until a test says otherwise -
/// and reading it silently turned the *pre-fix* arm of the `MIN_FILTER_SCALE`
/// measurements into a `Smooth` arm, which reported the floor as a no-op and its
/// "before" column as already-correct. `with_floor` below is the same trap with
/// the floor instead of the strength.
fn with_resample<T>(linear: bool, strength: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(linear, strength, image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit `MIN_FILTER_SCALE` and an explicit strength. `0.0`
/// disables the floor, which is the arm the fix is measured against, so the
/// strength is a **required** argument rather than a default: leaving it to be
/// read is how the "before" column came to be a `Smooth` column.
fn with_floor_at<T>(strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(true, strength, floor, f)
}

/// Every library source file, concatenated - the whole crate except the binary.
///
/// `lib.rs` is a list of modules rather than the code (see `AGENTS.md` and
/// `tests/source_size.rs`), so a scan of `include_str!("../src/lib.rs")` would
/// read 127 lines of `pub mod` and find nothing. Read at run time rather than
/// with `include_str!`: following the modules is exactly what these scans have
/// to do, and a module added later must be covered without anyone remembering
/// to add it here.
fn main_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name == "main.rs" || name.starts_with("app/"))
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

fn lib_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name != "main.rs")
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file under `src/`, as `(relative name, text)`, sorted.
fn all_src_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out: Vec<(String, String)> = Vec::new();
    collect_src(&root, &root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn collect_src(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_src(root, &path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push((rel, std::fs::read_to_string(&path).unwrap()));
        }
    }
}
// ---------------------------------------------------------------------------
// The linear-light prefilter
//
// `linear_resize` is two steps: reduce the full-resolution source to at most 2x
// the target, then two Hermite passes in 12-bit linear light down to the target.
// The first step used to be an sRGB **box** average, and a box's sinc sidelobes
// sit near -13 dB without rolling off, so it was aliasing *more* than the kernel
// pass it fed. On a 2223x3109 page at the real 2.879x reduction (the manga
// half-panel target of 1080) the residual in the alias band, measured as the
// output amplitude of a 100-code sinusoid, was 21-29 codes; with a widened
// kernel in the prefilter it is 2-11.
//
// The measurements below were taken with a harness mirroring `linear_resize`
// verbatim (same f32 order, same normalisation, byte-exact against
// `downscale_image` for the box variant), one variable changed at a time. The
// "box" column is what the code did before; the pins are the kernel numbers plus
// two codes of slack, so a later filter tweak fails loudly rather than silently.
//
// **What is not claimed.** This reduces the artefact, it does not abolish it. A
// tone at the source Nyquist cannot survive a 2.879x downscale without
// aliasing; at source period 5 (1.7 output px) 23 codes still get through, and
// the artefact is a hard 1-2 px sparkle, not a slow beat.
// ---------------------------------------------------------------------------

/// The geometry the probes use: the same 2.880x reduction as the real
/// 2223x3109 manga page at the 1080 half-panel target, at a tenth of the pixels.
/// `downscale_image` scales so the **long** side is `max_dim`, so 1244/432 is
/// 2.880 and the result is 309x431.
const PROBE_W: u32 = 890;
const PROBE_H: u32 = 1244;
const PROBE_TARGET: u32 = 432;

/// Horizontal tone of source period `p` px, `amp` codes peak-to-peak. Varying
/// along x only makes the measurement the horizontal transfer function.
fn probe_sinusoid(w: u32, h: u32, p: f32, amp: f32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let t = (2.0 * std::f32::consts::PI * x as f32 / p).sin();
            let v = (128.0 + amp * 0.5 * t).round().clamp(0.0, 255.0) as u8;
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO }
}

/// Dot lattice: 1-px dots of `hi` every `p` source px, `lo` between them. A
/// square wave rather than a tone, so it puts energy at every odd harmonic of
/// the pitch and exercises more of the alias band than a single frequency.
fn probe_dots(w: u32, h: u32, p: f32, lo: u8, hi: u8) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    let half = (p / 2.0).max(1.0);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = if (x as f32) % p < half { hi } else { lo };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO }
}

/// Stochastic 50% black/white dither, the shape a screentone's fine dither
/// actually has. A strict period-2 lattice is *not* used for the exposure probe
/// on purpose: the box prefilter has an exact null at it whose depth depends on
/// whether a given output pixel's span happens to cover 1 or 2 source pixels, so
/// the same pattern measures 128 or 162 codes purely from the phase. A hashed
/// one averages over every alignment, which is also why it is the more honest
/// stand-in for a tone.
fn probe_dither(w: u32, h: u32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            // xorshift-ish hash of the coordinate: no lattice period to alias.
            let mut n = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (y as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            n ^= n >> 29;
            n = n.wrapping_mul(0x94D0_49BB_1331_11EB);
            n ^= n >> 31;
            let v = if n & 1 == 0 { 0u8 } else { 255 };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO }
}

/// p95 - p5 of the red channel down the middle row: the amplitude statistic.
/// The row is read whole, and the whole row is one period-multiple of a
/// horizontal tone, so this is the tone's amplitude as the resampler left it.
fn probe_amplitude(d: &image_viewer::ImageData) -> f32 {
    let w = d.width as usize;
    let mid = (d.rgba.len() / 4 / w / 2) * w * 4;
    let mut v: Vec<u8> = d.rgba[mid..mid + w * 4].iter().step_by(4).copied().collect();
    v.sort_unstable();
    let p = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize] as f32;
    p(0.95) - p(0.05)
}

fn probe_mean(d: &image_viewer::ImageData) -> f32 {
    d.rgba.iter().step_by(4).map(|&v| v as f32).sum::<f32>() / (d.rgba.len() / 4) as f32
}

// ---------------------------------------------------------------------------
// The downscale low-pass strength
//
// **The probe that found the artefact was wrong, and that is the whole reason
// this section exists.** `probe_sinusoid` varies along x only, so it can report
// "the alias band is down to 4 codes" while a square lattice is plainly visible:
// a 1-D probe structurally cannot produce a lattice, because a lattice needs two
// axis-aligned frequencies beating in *both* axes. `probe_square_lattice` below
// is axis-aligned in both axes and is what the strength is measured on.
//
// Three metrics, each stated once and applied identically at every strength:
//
// * `alias_amp` - p95-p5 of the red channel over the **whole** output, both axes.
//   For a pure lattice this is the amplitude of what survived the low-pass.
// * `beat_amp`  - the amplitude of the pattern at **its own output period**,
//   fitted by least squares. This is the visible moire: the lattice the eye
//   groups, which is not the same thing as broadband alias energy.
// * `edge_grad` - mean |dx| over a fixed crop, in codes per output pixel. The
//   cost side of the trade, so it is measured in the same runs as the benefit.
// ---------------------------------------------------------------------------

/// Square dot lattice: `d`-px dots on a `p`-px grid in **both** axes. This is
/// the probe the artefact needs; a stripe pattern in one axis only cannot make
/// one.
fn probe_square_lattice(w: u32, h: u32, p: f32, d: f32, lo: u8, hi: u8) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let on = (x as f32) % p < d && (y as f32) % p < d;
            let v = if on { hi } else { lo };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO }
}

/// p95-p5 of the red channel over the whole output. Two-dimensional on purpose:
/// a single-row statistic cannot see a vertical lattice.
fn alias_amp(d: &image_viewer::ImageData) -> f32 {
    let mut v: Vec<u8> = d.rgba.iter().step_by(4).copied().collect();
    v.sort_unstable();
    let p = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize] as f32;
    p(0.95) - p(0.05)
}

/// Amplitude of the lattice at **its own output period**, by least squares over
/// the interior (a 2-px border, so an edge clamp cannot leak in).
///
/// A square lattice of source pitch `p` reduced by `r` has output pitch `p/r`,
/// and the eye groups that into the artefact. This projects the output onto
/// `sin`/`cos` at that period in both axes and returns the combined amplitude in
/// codes - so it measures the visible lattice, not the broadband residue.
fn beat_amp(d: &image_viewer::ImageData, p_out: f32) -> f32 {
    let (w, h) = (d.width as usize, d.height as usize);
    let (mut ss, mut sc, mut ys, mut yc, mut n) = (0f64, 0f64, 0f64, 0f64, 0f64);
    let kx = 2.0 * std::f64::consts::PI / p_out as f64;
    let ky = 2.0 * std::f64::consts::PI / p_out as f64;
    for y in 2..h.saturating_sub(2) {
        for x in 2..w.saturating_sub(2) {
            let v = d.rgba[(y * w + x) * 4] as f64;
            let s = (kx * x as f64).sin() + (ky * y as f64).sin();
            let c = (kx * x as f64).cos() + (ky * y as f64).cos();
            ss += s * s;
            sc += s * c;
            ys += v * s;
            yc += v * c;
            n += 1.0;
        }
    }
    if ss <= 0.0 || n <= 0.0 {
        return 0.0;
    }
    // Solve [ss sc; sc ss] [a;b] = [ys;yc], taking the off-diagonal's weight
    // into account so a non-integer period's two phase terms are not counted as
    // independent and the amplitude does not come out inflated.
    let (a, b);
    if sc.abs() < 1e-9 * ss.max(1.0) {
        a = ys / ss;
        b = yc / ss;
    } else {
        let det = ss * ss - sc * sc;
        if det.abs() < 1e-9 {
            return 0.0;
        }
        a = (ys * ss - yc * sc) / det;
        b = (yc * ss - ys * sc) / det;
    }
    (a * a + b * b).sqrt() as f32
}

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

/// The strength a **first run** gets. Read from `Config::load` on an absent file
/// rather than named here, so a probe that says "the default path" cannot go on
/// measuring the other one. That is not hypothetical: the pins below were taken
/// while the default was `Smooth` and were still being read as the default path
/// after it moved. There is no `Config::default()` — `Config::load`'s hand-built
/// struct *is* the default, which is why this goes through it rather than through
/// the field.
fn default_config() -> image_viewer::Config {
    let tmp = tempfile::tempdir().unwrap();
    image_viewer::Config::load(&tmp.path().join("absent.json"))
}

/// The strength a **fresh** config asks for. `default_strength` reads the live
/// global; this reads the default config from disk, so it cannot be moved by a
/// test that ran earlier in the file.
fn default_strength() -> f32 {
    if default_config().smooth_downscale {
        image_viewer::SMOOTH_STRENGTH
    } else {
        image_viewer::FAST_STRENGTH
    }
}

/// `default_strength`, computed once.
///
/// `default_config` builds a tempdir and parses a file, and the helpers above
/// call this on every resize - so a probe table that resizes a few hundred times
/// would otherwise spend its time in `Config::load`. A `OnceLock` is safe here
/// precisely because the answer is a property of the shipped default, which is a
/// constant: nothing in the suite writes a `Config` with `smooth_downscale` set,
/// and `a_config_written_before_smooth_downscale_still_loads_and_stays_off`
/// asserts that an older config leaves it off.
fn cached_default_strength() -> f32 {
    static ONCE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *ONCE.get_or_init(default_strength)
}

/// The **shipped default** resize. Every probe below this line that reports on
/// "the path" means this one, so it is defined as the default rather than as a
/// strength constant that has to be updated in step with the config.
fn probe_resize(src: &image_viewer::ImageData) -> image_viewer::ImageData {
    resize_at(src, PROBE_TARGET, cached_default_strength())
}

/// Resize at an explicit strength and explicit target. Both the probe geometry
/// and the strength are arguments so one table can carry the whole sweep.
fn resize_at(src: &image_viewer::ImageData, target: u32, strength: f32) -> image_viewer::ImageData {
    with_resample(true, strength, || image_viewer::downscale_image(src, target))
}

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

/// Hard vertical step edge, white left of `x0`, black right of it. The
/// sharpness probe: a wider kernel cannot help but widen the transition.
fn probe_step(w: u32, h: u32, x0: u32) -> image_viewer::ImageData {
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let v = if (x as u32) < x0 { 255u8 } else { 0 };
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO }
}

/// 10-90% rise distance of the step edge, in **output pixels**, averaged over
/// rows and over both edges' orientations.
///
/// The unambiguous sharpness number: it is a length, not an energy, so it cannot
/// be moved by the picture's contrast, and a wider low-pass cannot do anything
/// but lengthen it. Reported for the probe's own 2.880x reduction.
fn step_rise(out: &image_viewer::ImageData) -> f32 {
    let w = out.width as usize;
    // The probe is white to the left of the edge, so the profile descends. 90%
    // is the first crossing of 229.5 and 10% the first of 25.5; interpolating
    // between the straddling pair makes the measure sub-pixel, which matters
    // because the whole effect is a fraction of an output pixel per strength
    // step.
    let (t90, t10) = (0.9f32 * 255.0, 0.1f32 * 255.0);
    // `crossing` finds the first sample below `t` and interpolates against its
    // predecessor. Scanned as "first below" rather than "straddling pair"
    // because the profile can step from above the threshold straight past it:
    // at strength 1.0 the edge is 254, 187, 21, 0, so the 10% threshold of 25.5
    // is already behind the 21 sample and a straddling-pair test never fires.
    // That was a bug in the metric, not a limit of the resampler - it reported
    // 0.000 at every strength, which `print_step_edge_profile` is what caught.
    let crossing = |get: &dyn Fn(usize) -> f32, t: f32| -> Option<f32> {
        for x in 1..w {
            let (v0, v1) = (get(x - 1), get(x));
            if v1 < t {
                return (v0 >= t).then(|| (x - 1) as f32 + (v0 - t) / (v0 - v1).max(1e-6));
            }
        }
        None
    };
    let mut total = 0f64;
    let mut n = 0f64;
    for y in 0..out.height as usize {
        let row = |x: usize| out.rgba[(y * w + x) * 4] as f32;
        let hi = crossing(&row, t90);
        let lo = crossing(&row, t10);
        if let (Some(hi), Some(lo)) = (hi, lo) {
            // `lo - hi`, not `hi - lo`: the profile **descends** (white to the
            // left), so the 10% crossing is to the *right* of the 90% one and the
            // difference is negative. Taking `.max(0.0)` on the wrong order turns
            // every row into a zero-length edge and the metric into a constant
            // 0.000 that looks like a limit on the resampler. That is what it
            // did for two runs before `print_step_edge_profile` was written.
            total += (lo - hi).abs() as f64;
            n += 1.0;
        }
    }
    if n == 0.0 { 0.0 } else { (total / n) as f32 }
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

/// The settings toggle must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here (`spreads.rs`,
/// `keybindings.rs`, `renderer.rs`).
///
/// Three things it checks, each a way the setting could exist and do nothing:
/// the label exists, the config load applies it, and the panel's changed arm
/// **invalidates the previews**. The third is the one that matters: a
/// decode-time setting that leaves `previews` alone flips the box, changes
/// nothing visible, and reads as a broken feature. `linear_resample` is the
/// documented counter-example in `AGENTS.md`.
#[test]
fn the_smooth_toggle_is_wired_and_invalidates_the_previews() {
    let src = main_src();

    assert!(
        src.contains("set_downscale_smoothing(config.smooth_downscale);"),
        "startup must apply config.smooth_downscale to the resampler"
    );
    assert!(
        src.contains("&mut self.config.smooth_downscale"),
        "the settings panel must have a checkbox bound to it"
    );

    // The checkbox's own arm: find the `.changed()` block that mentions the
    // field and require it to set `refilter`. Locating it by field name rather
    // than by a fixed offset is what makes this survive the panel being
    // reordered, which it will be.
    let at = src.find("&mut self.config.smooth_downscale").expect("the checkbox exists");
    let after = &src[at..];
    let arm_end = after.find("}\n                ").expect("the changed() arm closes");
    let arm = &after[..arm_end];
    assert!(
        arm.contains("set_downscale_smoothing("),
        "the changed arm must apply the setting immediately, not only on the \
         next launch"
    );
    assert!(
        arm.contains("refilter = true"),
        "the changed arm must set refilter, or the previews decoded by the old \
         kernel stay on screen and the toggle appears broken (AGENTS.md records \
         exactly this for linear_resample)"
    );
    assert!(src.contains("if refilter {"), "the refilter flag must still be acted on");
}

/// A config written before this setting existed still loads, and gets **`false`**.
///
/// Both halves, because they are opposites: it must load *and* it must come up
/// `false`. A `#[serde(default)]` with the wrong default compiles, passes every
/// other test in the suite, and hands every existing reader a setting that is
/// wrong for three of the six pages it was measured on.
///
/// **This assertion was `true` and is now `false`, and the change is the fix.**
/// The setting shipped default-on on the strength of a *uniform synthetic
/// lattice*, where more strength wins at every pitch. Judging rendered output on
/// six 2223x3109 pages, strength 2.0 was better on `32.jpg` and `30.jpg`,
/// unchanged on `52.jpg`, and worse on `12.jpg`, `21.jpg` and `44.jpg` — a black
/// dot pattern appearing in grey screentone. `Fast` was better than `master` on
/// all six. So the default follows the measurement that was taken on the artefact
/// users actually have, and the previous default was chosen on a probe that
/// cannot see the trade.
#[test]
fn a_config_written_before_smooth_downscale_still_loads_and_stays_off() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(
        !cfg.smooth_downscale,
        "an old config must come up with Smooth OFF: on real pages it is better on \
         two files, unchanged on one and worse on three, so it cannot be what \
         everyone gets"
    );

    // And the default a *missing file* gets, which is a different construction
    // (`Config::load`'s hand-built struct) and was a separate way for the field
    // to be forgotten.
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.smooth_downscale, "a first run must also come up with Smooth OFF");
}

#[test]
fn a_config_written_before_pixel_art_still_loads_and_stays_off() {
    // Same two constructions as the Smooth test above: an old file without
    // the key, and a missing file (`Config::load`'s hand-built struct).
    // Magnified nearest on a photo is the blocky look nobody wants, so it
    // ships OFF and neither path may forget the field.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.pixel_art, "an old config without the key must come up with Pixel art OFF");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.pixel_art, "a first run must also come up with Pixel art OFF");
}

/// The decision the toggle names: nearest on **magnify**, linear on
/// **minify** — and linear everywhere when off. The two-axes shape is the
/// whole point of [`image_viewer::pixel_art_sampling`]: a decision glued to
/// the zoom level current at upload would have to re-roll a live texture the
/// moment zoom crossed the boundary — the re-upload hitch TODO §8 recorded
/// as unmeasured and unpriced. Naming the axes inverts that: one upload
/// serves both magnifying and minifying draws, and crossing 100% costs
/// nothing, because egui picks the sampler per axis at draw time.
#[test]
fn pixel_art_sampling_is_nearest_on_magnify_linear_on_minify() {
    use image_viewer::{Sampling, pixel_art_sampling};

    assert_eq!(pixel_art_sampling(false), Sampling::Linear);
    assert_eq!(pixel_art_sampling(true), Sampling::NearestMagnify);
}

/// The wiring in the unreachable binary, as a source scan — the established
/// pattern (`spreads.rs`, `keybindings.rs`, `renderer.rs`, the Smooth scan
/// above): `main.rs` cannot be run from a test. Four things it checks, each
/// a way the setting could exist and do nothing: the library owns the
/// decision, the settings arm **invalidates the previews on change**, the
/// content uploads all go through the one mapping, and the sites that
/// deliberately stay linear stay linear.
#[test]
fn the_pixel_art_toggle_is_wired_to_the_content_uploads_and_invalidates_too() {
    let main = main_src();
    let lib = lib_src();

    assert!(
        lib.contains("#[serde(default)]\n    pub pixel_art: bool,"),
        "the config field must serde-default, or an old config fails to load \
         (AGENTS.md: every new Config field needs it)"
    );
    assert!(
        lib.contains("pixel_art: false,"),
        "Config::load's hand-built default must state the off default, which is \
         the construction a first run actually takes"
    );
    assert!(
        lib.contains("pub fn pixel_art_sampling("),
        "the decision belongs in lib.rs, the only place a test can pin it"
    );

    // The settings arm, located by field name so the panel can be reordered
    // under the scan.
    let at = main
        .find("&mut self.config.pixel_art")
        .expect("the settings panel must have a checkbox bound to it");
    let after = &main[at..];
    let arm_end = after.find("}\n                ").expect("the changed() arm closes");
    let arm = &after[..arm_end];
    assert!(
        arm.contains("refilter = true"),
        "the changed arm must set refilter: textures already uploaded keep the \
         samplers they arrived with, so without it the box flips and nothing \
         on screen changes — the exact broken-feature shape the Smooth scan \
         documents"
    );

    // The four content sites — preview arrival, animation init, the two
    // in-play animation frame sets, the per-frame tile options — all go
    // through the one mapping; a site that went around it would sample that
    // texture linearly for as long as it lives.
    let uses = main.matches("content_texture_options(self.config.pixel_art)").count();
    assert_eq!(uses, 4, "every content upload must consult the toggle at upload time");
    assert!(
        main.contains("options: TextureOptions,")
            && main.contains("make_tile(&ctx, &tp.data, tx, ty, tile_options);"),
        "make_tile must take its sampling in from the caller, which is where \
         the config lives; the tile texture is a content texture too"
    );

    // The deliberate counter-examples: the grid cell and the checkerboard.
    let thumb_at = main.find("let texels = p.image.size;").expect("the thumb upload exists");
    let thumb_arm_len =
        main[thumb_at..].find("self.thumbs.insert").expect("the thumb arm ends at its insert");
    let thumb_arm = &main[thumb_at..thumb_at + thumb_arm_len];
    assert!(
        !thumb_arm.contains("content_texture_options("),
        "the grid cell must not call the mapping: 150pt of a \
         256-texel image, and the grid also browses photos"
    );
    assert!(
        thumb_arm.contains("TextureOptions::LINEAR"),
        "the grid cell stays linear on purpose, said here so a later edit that \
         changes it fails loudly instead of silently"
    );
    assert!(
        main.contains("TextureOptions::NEAREST_REPEAT"),
        "the checkerboard keeps NEAREST_REPEAT: it is artwork, not content, \
         and pixel art user content sampling is not its reason"
    );
}

#[test]
fn a_config_written_before_grid_cell_size_still_loads_at_150() {
    // Same two constructions as the Smooth test above: an old file without the
    // key (serde default) and a missing file (`Config::load`'s hand-built
    // struct). Either one forgotten parks every existing user on the wrong
    // cell size with no error.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert_eq!(cfg.grid_cell, 150.0, "an old config without the key must come up at 150pt cells");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert_eq!(fresh.grid_cell, 150.0, "a first run must also come up at 150pt cells");
}

#[test]
fn a_config_written_before_slideshow_shuffle_still_loads_with_it_off() {
    // A shuffled show surprises anyone who did not ask for one, so an old
    // config — and a new install — must come up with it off. Same two
    // constructions as the tests above.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.slideshow_shuffle, "an old config without the key must not start shuffling shows");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.slideshow_shuffle, "a first run must also come up with shuffle off");
}

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

/// Grey screentone with energy at **more than one pitch**, which is what real tone
/// is and a 0/100 test lattice is not.
///
/// Three components, chosen so their beats land at different output periods:
///
/// * a base dot grid, the one a single-pitch probe sees,
/// * a second screen at 30 degrees, whose period is incommensurate with the first,
/// * a stochastic dither, so the pattern is not perfectly periodic and has some
///   broadband content the way real tone does.
///
/// **Grey, not 0/100, deliberately.** The artefact is a grid *in a tone*, and a
/// black-on-white lattice is a harder case that
/// `smooth_low_passes_a_square_lattice_harder_than_fast` already covers. Measured
/// rather than argued: see the guard above — the promoted pitch is not caused by
/// the multi-pitch structure, but the grey field is the content the report is
/// about and this is the fixture for it.
fn probe_multi_pitch_screentone(
    w: u32,
    h: u32,
    base_pitch: f32,
    screen_pitch: f32,
    screen_angle: f32,
) -> image_viewer::ImageData {
    let (sin30, cos30) = (std::f32::consts::PI / 180.0 * screen_angle).sin_cos();
    // A tiny deterministic LCG: `f32::sin` of an index is fine but this keeps the
    // dither reproducible across platforms without a dependency.
    let mut seed = 0x2f6e_2b1u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1u32 << 24) as f32
    };
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let (fx, fy) = (x as f32, y as f32);
            // Base grid.
            let g1 = if fx % base_pitch < base_pitch * 0.375 && fy % base_pitch < base_pitch * 0.375
            {
                1.0f32
            } else {
                0.0
            };
            // Angled screen: its own period along its own axis.
            let u = (fx * cos30 + fy * sin30) % screen_pitch;
            let v = (-fx * sin30 + fy * cos30) % screen_pitch;
            let g2 =
                if u < screen_pitch * 0.314 && v < screen_pitch * 0.314 { 1.0f32 } else { 0.0 };
            // Stochastic dither, +/- 6 codes.
            let d = (next() - 0.5) * 12.0;
            let v = (150.0 - 46.0 * g1 - 34.0 * g2 + d).round().clamp(0.0, 255.0) as u8;
            let o = (y * w as usize + x) * 4;
            rgba[o..o + 4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    image_viewer::ImageData { width: w, height: h, rgba, load_time: std::time::Duration::ZERO }
}

/// Amplitude of a 2-D pattern at output period `p_out`, in codes, by least
/// squares over the whole output. The same projection as `beat_amp` - a named
/// function rather than an inline copy, because it is what the "widening moves
/// energy between pitches" guard is decided on.
fn screentone_lattice_amplitude(d: &image_viewer::ImageData, p_out: f32) -> f32 {
    beat_amp(d, p_out)
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
/// **someone who does not ask for a floor gets the pre-`b237e7a` pipeline byte for
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
             pipeline must be byte-identical to the pre-b237e7a path.",
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
