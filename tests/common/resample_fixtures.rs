//! The probes and the globals guard the prefilter targets share.
//!
//! `tests/resample.rs` was split by subject (see `tests/source_size.rs`); the
//! probe fixtures, the strength/floor wrappers and the source-scan collectors
//! are used by more than one of those targets, so they live here rather than in
//! each. Every target compiles its own copy through
//! `#[path = "common/resample_fixtures.rs"]`, the pattern `tests/common/`
//! already uses.
//!
//! **The globals are process-wide, so every arm goes through one guard.**
//! `with_resample_at` takes the lock once and pins all three settings; the
//! `with_*` wrappers exist so no caller can nest two acquisitions and hang on
//! the non-reentrant mutex. The `MIN_FILTER_SCALE` floor is a runtime atomic
//! for the same reason it is a parameter here: `0.0` has to mean the
//! pre-floor behaviour, and that arm has to be reachable without a second
//! build.
#![allow(dead_code)]

pub static FILTER_GLOBALS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
pub fn with_resample_at<T>(linear: bool, strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
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
pub fn with_linear_resample<T>(linear: bool, f: impl FnOnce() -> T) -> T {
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
pub fn with_resample<T>(linear: bool, strength: f32, f: impl FnOnce() -> T) -> T {
    with_resample_at(linear, strength, image_viewer::MIN_FILTER_SCALE, f)
}

/// Run `f` at an explicit `MIN_FILTER_SCALE` and an explicit strength. `0.0`
/// disables the floor, which is the arm the fix is measured against, so the
/// strength is a **required** argument rather than a default: leaving it to be
/// read is how the "before" column came to be a `Smooth` column.
pub fn with_floor_at<T>(strength: f32, floor: f32, f: impl FnOnce() -> T) -> T {
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
pub fn main_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name == "main.rs" || name.starts_with("app/"))
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn lib_src() -> String {
    all_src_files()
        .into_iter()
        .filter(|(name, _)| name != "main.rs")
        .map(|(_, text)| text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file under `src/`, as `(relative name, text)`, sorted.
pub fn all_src_files() -> Vec<(String, String)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out: Vec<(String, String)> = Vec::new();
    collect_src(&root, &root, &mut out);
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

pub fn collect_src(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, String)>) {
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
pub const PROBE_W: u32 = 890;
pub const PROBE_H: u32 = 1244;
pub const PROBE_TARGET: u32 = 432;

/// Horizontal tone of source period `p` px, `amp` codes peak-to-peak. Varying
/// along x only makes the measurement the horizontal transfer function.
pub fn probe_sinusoid(w: u32, h: u32, p: f32, amp: f32) -> image_viewer::ImageData {
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
pub fn probe_dots(w: u32, h: u32, p: f32, lo: u8, hi: u8) -> image_viewer::ImageData {
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
pub fn probe_dither(w: u32, h: u32) -> image_viewer::ImageData {
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
pub fn probe_amplitude(d: &image_viewer::ImageData) -> f32 {
    let w = d.width as usize;
    let mid = (d.rgba.len() / 4 / w / 2) * w * 4;
    let mut v: Vec<u8> = d.rgba[mid..mid + w * 4].iter().step_by(4).copied().collect();
    v.sort_unstable();
    let p = |q: f32| v[((v.len() - 1) as f32 * q).round() as usize] as f32;
    p(0.95) - p(0.05)
}

pub fn probe_mean(d: &image_viewer::ImageData) -> f32 {
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
pub fn probe_square_lattice(
    w: u32,
    h: u32,
    p: f32,
    d: f32,
    lo: u8,
    hi: u8,
) -> image_viewer::ImageData {
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
pub fn alias_amp(d: &image_viewer::ImageData) -> f32 {
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
pub fn beat_amp(d: &image_viewer::ImageData, p_out: f32) -> f32 {
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

/// The strength a **first run** gets. Read from `Config::load` on an absent file
/// rather than named here, so a probe that says "the default path" cannot go on
/// measuring the other one. That is not hypothetical: the pins below were taken
/// while the default was `Smooth` and were still being read as the default path
/// after it moved. There is no `Config::default()` — `Config::load`'s hand-built
/// struct *is* the default, which is why this goes through it rather than through
/// the field.
pub fn default_config() -> image_viewer::Config {
    let tmp = tempfile::tempdir().unwrap();
    image_viewer::Config::load(&tmp.path().join("absent.json"))
}

/// The strength a **fresh** config asks for. `default_strength` reads the live
/// global; this reads the default config from disk, so it cannot be moved by a
/// test that ran earlier in the file.
pub fn default_strength() -> f32 {
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
pub fn cached_default_strength() -> f32 {
    static ONCE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *ONCE.get_or_init(default_strength)
}

/// The **shipped default** resize. Every probe below this line that reports on
/// "the path" means this one, so it is defined as the default rather than as a
/// strength constant that has to be updated in step with the config.
pub fn probe_resize(src: &image_viewer::ImageData) -> image_viewer::ImageData {
    resize_at(src, PROBE_TARGET, cached_default_strength())
}

/// Resize at an explicit strength and explicit target. Both the probe geometry
/// and the strength are arguments so one table can carry the whole sweep.
pub fn resize_at(
    src: &image_viewer::ImageData,
    target: u32,
    strength: f32,
) -> image_viewer::ImageData {
    with_resample(true, strength, || image_viewer::downscale_image(src, target))
}

/// Hard vertical step edge, white left of `x0`, black right of it. The
/// sharpness probe: a wider kernel cannot help but widen the transition.
pub fn probe_step(w: u32, h: u32, x0: u32) -> image_viewer::ImageData {
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
pub fn step_rise(out: &image_viewer::ImageData) -> f32 {
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
pub fn probe_multi_pitch_screentone(
    w: u32,
    h: u32,
    base_pitch: f32,
    screen_pitch: f32,
    screen_angle: f32,
) -> image_viewer::ImageData {
    let (sin30, cos30) = (std::f32::consts::PI / 180.0 * screen_angle).sin_cos();
    // A tiny deterministic LCG: `f32::sin` of an index is fine but this keeps the
    // dither reproducible across platforms without a dependency.
    let mut seed = 0x02f6_e2b1_u32;
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
pub fn screentone_lattice_amplitude(d: &image_viewer::ImageData, p_out: f32) -> f32 {
    beat_amp(d, p_out)
}
