//! Module `resample`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use resample::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Global toggle for linear-light downsampling (set from the config toggle in
/// main.rs). Workers read it; the UI thread writes it.
pub(crate) static LINEAR_RESAMPLE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Enable/disable linear-light downsampling at runtime.
pub fn set_linear_resample(on: bool) {
    LINEAR_RESAMPLE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is linear-light downsampling on? Public so tests can save/restore it
/// around assertions that only hold on one of the two filter paths.
pub fn linear_resample_enabled() -> bool {
    LINEAR_RESAMPLE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Downsampling filter for the linear-light path, selectable at runtime.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DownscaleFilter {
    /// B=0 C=0 cubic: smallest support (2 px), no negative lobes — softest,
    /// zero ringing. JPEGView's default for downscaling.
    #[default]
    Hermite,
    /// B=0 C=0.5 cubic: sharp like CatmullRom with milder ringing.
    Catrom,
    /// 3-lobe sinc: sharpest, most visible ringing on hard edges.
    Lanczos3,
}

pub(crate) static DOWNSCALE_FILTER: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Set the downsampling filter at runtime (workers read it per-resize).
pub fn set_downscale_filter(f: DownscaleFilter) {
    let v = match f {
        DownscaleFilter::Hermite => 0,
        DownscaleFilter::Catrom => 1,
        DownscaleFilter::Lanczos3 => 2,
    };
    DOWNSCALE_FILTER.store(v, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn downscale_filter() -> DownscaleFilter {
    match DOWNSCALE_FILTER.load(std::sync::atomic::Ordering::Relaxed) {
        1 => DownscaleFilter::Catrom,
        2 => DownscaleFilter::Lanczos3,
        _ => DownscaleFilter::Hermite,
    }
}

/// What sampling a **viewer content texture** is uploaded with: the preview,
/// the zoom tiles and the animation frames. A plain enum because this module
/// has no egui dependency on purpose — the binary maps each value onto
/// `TextureOptions`, and that mapping is the only place that names egui.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sampling {
    /// Linear in both directions — what every texture did before this axis
    /// existed, and what grid thumbnails and the checkerboard keep regardless.
    #[default]
    Linear,
    /// Nearest when texels are **magnified** (the drawing is bigger than the
    /// texels it samples), linear when minified. The sprites this setting
    /// exists for are magnified already at fit on this box's panels, because
    /// a small source is decoded at its own size and the GPU stretches it to
    /// the panel — which is exactly the smear the toggle removes.
    NearestMagnify,
}

/// The sampling the pixel-art toggle asks for, decided once per texture
/// upload. It names the two sampler axes and **not** a zoom level, so the
/// choice does not depend on where the user was when the decode finished and
/// never has to flip a texture that is already live: egui carries
/// `magnification` and `minification` separately and the draw reads each.
/// Nothing re-uploads as the zoom changes; the flip's own cost is previews
/// re-requested (`refilter` in the settings arm), which re-upload under the
/// new sampling when they arrive.
pub fn pixel_art_sampling(pixel_art: bool) -> Sampling {
    match pixel_art {
        true => Sampling::NearestMagnify,
        false => Sampling::Linear,
    }
}

/// How much wider than the Nyquist minimum the resampling kernel runs when
/// `Smooth` is on. **This is the strength axis, and it is the only one this
/// codebase has.**
///
/// `FAST_STRENGTH` is exactly the width the kernel had before the setting
/// existed, so `Fast` is the old behaviour and not a third flavour of it.
/// `SMOOTH_STRENGTH` is measured rather than chosen: see
/// `tests/unit.rs::print_square_lattice_sweep` and `print_real_page_strength_sweep`
/// for the sweep it came out of, and `smooth_costs_edge_sharpness_and_the_cost_is_bounded`
/// for the cost it was accepted at.
///
/// **Why a multiplier on `filter_scale` and not on the tap count.**
/// `contributions` already divides the kernel argument by `filter_scale`, so
/// widening the *span* while leaving `filter_scale` alone evaluates every extra
/// tap at exactly zero and is bit-identical - that is a measured result, not an
/// inference. Widening `filter_scale` widens the argument *and* the span
/// together, so the extra taps are real. And it is the one lever that reaches
/// both halves of the resize: the pre-reduction's own `filter_scale` and the
/// final pass's.
pub const FAST_STRENGTH: f32 = 1.0;
pub const SMOOTH_STRENGTH: f32 = 2.0;

/// The strength a worker sees, as a float in an atomic. Workers read it; the UI
/// thread writes it, from the settings toggle.
pub(crate) static DOWNSCALE_STRENGTH: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(SMOOTH_STRENGTH.to_bits());

/// Set the kernel strength directly. `set_downscale_smoothing` is the user-facing
/// spelling; this exists so a test can put an arbitrary strength on the axis and
/// sweep it.
pub fn set_downscale_strength(strength: f32) {
    DOWNSCALE_STRENGTH.store(strength.to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// The kernel strength in force, as read by the resampler.
pub fn downscale_strength() -> f32 {
    f32::from_bits(DOWNSCALE_STRENGTH.load(std::sync::atomic::Ordering::Relaxed))
}

/// The `Smooth` / `Fast` toggle. `true` is `SMOOTH_STRENGTH`, `false` is
/// `FAST_STRENGTH`, which is what the path did before either existed.
pub fn set_downscale_smoothing(on: bool) {
    set_downscale_strength(if on { SMOOTH_STRENGTH } else { FAST_STRENGTH });
}

/// Is the strength at least `SMOOTH_STRENGTH`? Public so tests can save and
/// restore it around assertions that only hold on one side of the toggle, the
/// same reason `linear_resample_enabled` exists.
pub fn downscale_smoothing() -> bool {
    downscale_strength() >= SMOOTH_STRENGTH
}

/// Kernel weight and support radius for each filter.
pub(crate) fn kernel(f: DownscaleFilter) -> (Box<dyn Fn(f32) -> f32>, f32) {
    match f {
        DownscaleFilter::Hermite => (
            Box::new(|x: f32| {
                // Hermite = cubic B-spline with B=0, C=0 (a.k.a. "smoothstep").
                let x = x.abs();
                if x < 1.0 {
                    1.0 - 2.5 * x * x + 1.5 * x * x * x
                } else {
                    0.0
                }
            }),
            1.0,
        ),
        DownscaleFilter::Catrom => (
            Box::new(|x: f32| {
                // Catmull-Rom = cubic B=0, C=0.5.
                let x = x.abs();
                if x < 1.0 {
                    1.5 * x * x * x - 2.5 * x * x + 1.0
                } else if x < 2.0 {
                    -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
                } else {
                    0.0
                }
            }),
            2.0,
        ),
        DownscaleFilter::Lanczos3 => (
            Box::new(|x: f32| {
                let x = x.abs();
                if x <= f32::EPSILON {
                    1.0
                } else if x < 3.0 {
                    let pix = std::f32::consts::PI * x;
                    (pix.sin() / pix) * ((pix / 3.0).sin() / (pix / 3.0))
                } else {
                    0.0
                }
            }),
            3.0,
        ),
    }
}

/// sRGB <-> 12-bit-linear LUTs (JPEGView-style): resizing in linear light
/// avoids darkened edges and moiré on lineart/screentones.
pub(crate) fn lin_tables() -> (&'static [u16; 256], &'static [u8; 4096]) {
    static TABLES: std::sync::OnceLock<(Box<[u16; 256]>, Box<[u8; 4096]>)> =
        std::sync::OnceLock::new();
    let tables = TABLES.get_or_init(|| {
        let mut to_lin = Box::new([0u16; 256]);
        for (i, v) in to_lin.iter_mut().enumerate() {
            let s = i as f64 / 255.0;
            let lin = if s <= 0.04045 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            };
            *v = (lin * 4095.0).round() as u16;
        }
        let mut to_srgb = Box::new([0u8; 4096]);
        for (i, v) in to_srgb.iter_mut().enumerate() {
            let lin = i as f64 / 4095.0;
            let s = if lin <= 0.003_130_8 {
                lin * 12.92
            } else {
                1.055 * lin.powf(1.0 / 2.4) - 0.055
            };
            *v = ((s * 255.0).round()).clamp(0.0, 255.0) as u8;
        }
        (to_lin, to_srgb)
    });
    (&tables.0, &tables.1)
}

/// Downscale a full ImageData (RGBA) to a smaller preview size. When the
/// linear-light toggle is on, this converts to 12-bit linear light and uses a
/// CatmullRom filter (sharper, correct edges on lineart); otherwise it uses the
/// fast gamma-space Triangle filter.
pub fn downscale_image(data: &ImageData, max_dim: u32) -> ImageData {
    let scale = (max_dim as f32 / data.width as f32)
        .min(max_dim as f32 / data.height as f32)
        .min(1.0);
    if scale >= 1.0 {
        return ImageData {
            width: data.width,
            height: data.height,
            rgba: data.rgba.clone(),
            load_time: data.load_time, // keep original decode time
        };
    }
    let (new_w, new_h) = (
        (data.width as f32 * scale).max(1.0) as u32,
        (data.height as f32 * scale).max(1.0) as u32,
    );
    let out = if linear_resample_enabled() {
        ImageData {
            width: new_w,
            height: new_h,
            rgba: linear_resize(data, new_w, new_h),
            load_time: data.load_time, // keep original decode time
        }
    } else {
        let img = DynamicImage::ImageRgba8(
            RgbaImage::from_raw(data.width, data.height, data.rgba.clone()).unwrap(),
        );
        // **Report the size the library actually produced, not the size we asked
        // for.** `DynamicImage::resize` runs `resize_dimensions(fill = false)`,
        // which *preserves the aspect ratio* (`ratio = min(nw/w, nh/h)`), so it
        // routinely hands back an image a pixel or two smaller than requested:
        // 980x552 asked for 300x168 comes back 298x168. Pairing our own
        // `new_w`/`new_h` with its shorter buffer violated the
        // width*height*4 == rgba.len() invariant, and
        // `ColorImage::from_rgba_unmultiplied` asserts on exactly that — so the
        // worker panicked, the app reported "decoder panicked", and the image was
        // then poisoned in `failed` for the rest of the session.
        //
        // It only showed up once previews were sized to the display area
        // (11d778c) instead of a fixed 4096: a small target rounds much harder,
        // so the aspect-ratio clamp started shaving pixels off. That is also why
        // it looked machine- and window-dependent, and why only the gamma-space
        // path failed — `linear_resize` writes its own exact buffer.
        let resized = img
            .resize(new_w, new_h, image::imageops::FilterType::Triangle)
            .to_rgba8();
        ImageData {
            width: resized.width(),
            height: resized.height(),
            rgba: resized.into_raw(),
            load_time: data.load_time, // keep original decode time
        }
    };
    // Belt and braces: the downstream texture-size assert is unforgiving and a
    // worker panic there is unrecoverable, so no downscale may escape without
    // re-establishing the buffer/dimension invariant.
    normalized(out)
}

/// `PERF_PHASE=1` prints a per-phase breakdown of `linear_resize`. Cached: this
/// is read from inside the resampler.
pub(crate) static PHASE_TIMING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Width or height the linear-light prefilter reduces to: **at most
/// [`PREFILTER_FACTOR`]x the target**, and never larger than the dimension itself.
///
/// **This is also the threshold [`needs_refine`] judges an over-sized texture by**,
/// and that sharing is the point rather than tidiness: the factor is what the
/// pipeline filters at, so a texture more than that above the target it is painted
/// at is a picture nothing in this crate has low-passed — and at a request above
/// the source's own long side the pre-reduction cannot fire at all, which is how
/// a texture ends up **unfiltered altogether**.
///
/// This is the memory bound for `linear_resize`. The 12-bit linear conversion is
/// the expensive step, so it runs on the pre-reduced buffer rather than on the
/// full-resolution source: on a 6.9 MP page that is 26.7 MB instead of 55 MB,
/// and deleting the step roughly doubles the conversion cost as well. It is a
/// function rather than an inline expression so a test can pin it — nothing
/// observable from outside says how big the intermediate is, and a single kernel
/// pass scores the same or better on every quality measurement, so a deleted
/// pre-reduction is invisible except in memory.
///
/// `saturating_mul`, not `*`: `target` is a display dimension and the largest
/// this codebase can be handed is `MAX_TEXTURE_DIM`, but the arithmetic should
/// not depend on that staying true.
/// **This is a memory bound and not a quality knob, which is worth being explicit
/// about because it was read as a quality knob for a long time** — it is the
/// number the reported 1440p moiré was blamed on. Measured: `contributions`
/// derives `filter_scale` from each stage's *own* ratio, so the stages of a split
/// decimation multiply back to the reduction exactly (`s1 * s2 == R`), and
/// lowering this factor cannot widen the low-pass — it only changes how large the
/// buffer the 12-bit conversion runs over is. What sets the width is
/// [`MIN_FILTER_SCALE`].
pub fn prefilter_dim(dim: u32, target: u32) -> u32 {
    dim.min(target.saturating_mul(PREFILTER_FACTOR)).max(1)
}

/// Source span `(start, len)` covered by each of `n` output samples when
/// Separable **kernel** pre-reduction of an RGBA8 buffer to `pre_w x pre_h`,
/// averaging in 12-bit linear light and handing back sRGB8 at exactly the size
/// the box this replaced used.
///
/// **Why a kernel and not a box.** A box average's sinc sidelobes sit near
/// -13 dB and do not roll off, so at the 2.879x reduction of a 2223x3109 page
/// (the manga half-panel target of 1080) the prefilter was aliasing *more* than
/// the Hermite pass it fed. Measured as the output amplitude of a 100-code
/// sinusoid — i.e. the transfer function — the box left 21-29 codes in the band
/// below source period 4 px, where a widened kernel leaves 2-11, and the two
/// tie above source period ~6 px. At every reduction from 2x to 16x the kernel
/// is lower or equal.
///
/// **Why linear light, and not sRGB.** A weighted average is exposure-preserving
/// only in the space it averages in. Averaging *gamma-encoded* values of a 50%
/// near-Nyquist dither gives ~128 — the pattern's own sRGB mean — where the
/// tone's true average reflectance is 50% linear, i.e. sRGB ~188. Measured on
/// real manga pages, an sRGB-space kernel prefilter came out **5-7 codes darker
/// per page** than the box it replaced, which is a far worse bug than the
/// sparkle it removes. In linear light the same prefilter lands within 0.3
/// codes of what a single kernel pass over the whole source produces, i.e. it
/// stops second-guessing the space the rest of the path works in.
///
/// The taps are converted per tap rather than by buffering the source in 12-bit:
/// a full-resolution 12-bit buffer is 55 MB on a 6.9 MP page against 26.7 MB
/// for the intermediate, and AGENTS.md's memory section is explicit that this
/// conversion must not run over the full-resolution source. Row-converting
/// first (one `u16` row per worker) was measured and is *slower* — 565-707 ms
/// against 428-519 ms on the 6.9 MP page — so the LUT stays in the inner loop.
///
/// Both passes are row-parallel, which the box's vertical pass could not be: it
/// summed a sliding window down a column accumulator, so it ran on one thread.
/// That, not the arithmetic, is where the speedup comes from.
///
/// **`floor` is a parameter rather than a read, and `linear_resize` passes a
/// literal `0.0` for it.** The pre-reduction runs at exactly
/// `PREFILTER_FACTOR`, so `scale == 0.5` and `filter_scale == 2.0` already —
/// above any floor worth having — and a floor here can therefore only widen a
/// kernel that already meets it. Measured, not inferred: handing this stage the
/// real floor changes the output at R 2.02 and above, i.e. at every reduction the
/// shipped threshold already covers.
pub(crate) fn kernel_reduce_linear(data: &ImageData, pre_w: u32, pre_h: u32, strength: f32, floor: f32) -> Vec<u8> {
    let (to_lin, to_srgb) = lin_tables();
    let (sw, sh) = (data.width as usize, data.height as usize);
    let (pw, ph) = (pre_w as usize, pre_h as usize);
    let (kern, support) = kernel(downscale_filter());
    let horizontal = contributions(sw as u32, pw as u32, &kern, support, strength, floor);
    let vertical = contributions(sh as u32, ph as u32, &kern, support, strength, floor);

    // Horizontal: source width -> pre_w, at full height.
    let mut tmp = vec![0u8; pw * sh * 4];
    let src_stride = sw * 4;
    let src = &data.rgba;
    tmp.par_chunks_mut(pw * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
            let row = &src[y * src_stride..(y + 1) * src_stride];
            for (x, con) in horizontal.iter().enumerate() {
                let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
                for &(si, w) in con {
                    let p = &row[si * 4..si * 4 + 4];
                    r += to_lin[p[0] as usize] as f32 * w;
                    g += to_lin[p[1] as usize] as f32 * w;
                    b += to_lin[p[2] as usize] as f32 * w;
                    a += (p[3] as f32) * 16.0 * w;
                }
                let o = x * 4;
                out_row[o] = to_srgb[r.max(0.0).min(4095.0) as usize];
                out_row[o + 1] = to_srgb[g.max(0.0).min(4095.0) as usize];
                out_row[o + 2] = to_srgb[b.max(0.0).min(4095.0) as usize];
                out_row[o + 3] = ((a.max(0.0).min(4080.0) / 16.0).round()) as u8;
            }
        });
    if ph == sh {
        return tmp;
    }

    // Vertical: source height -> pre_h. Alpha is linear by definition and needs
    // no LUT; it rides the same 0..4080 scale `lin` uses for it.
    let mut out = vec![0u8; pw * ph * 4];
    let tmp_stride = pw * 4;
    out.par_chunks_mut(tmp_stride)
        .enumerate()
        .for_each(|(y, out_row)| {
            for x in 0..pw {
                let i = x * 4;
                let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
                for &(si, w) in &vertical[y] {
                    let p = &tmp[si * tmp_stride + i..si * tmp_stride + i + 4];
                    r += to_lin[p[0] as usize] as f32 * w;
                    g += to_lin[p[1] as usize] as f32 * w;
                    b += to_lin[p[2] as usize] as f32 * w;
                    a += (p[3] as f32) * 16.0 * w;
                }
                out_row[i] = to_srgb[r.max(0.0).min(4095.0) as usize];
                out_row[i + 1] = to_srgb[g.max(0.0).min(4095.0) as usize];
                out_row[i + 2] = to_srgb[b.max(0.0).min(4095.0) as usize];
                out_row[i + 3] = ((a.max(0.0).min(4080.0) / 16.0).round()) as u8;
            }
        });
    out
}


/// Resample in 12-bit linear light with a separable kernel.
///
/// Large reductions get a **kernel pre-reduction** first, down to at most 2x the
/// target ([`prefilter_dim`]). Measured on a 40 MP source this took the resample
/// from ~790ms to well under 200ms, and it is a quality win too: a big reduction
/// straight into Lanczos undersamples and aliases, because its ~6 taps end up
/// spread over thousands of source pixels.
///
/// Two things about the shape are load-bearing and both were measured rather than
/// chosen. **The intermediate size is the memory bound**: the 12-bit conversion
/// runs on the pre-reduced buffer, never on the full-resolution source (55 MB vs
/// 26.7 MB on a 6.9 MP page), so the pre-reduction cannot be deleted for quality
/// — a single kernel pass scores the *same or better* on every quality probe and
/// still costs twice the memory. **The prefilter averages in linear light**:
/// `kernel_reduce_linear` is a wrapper for the same reason
/// `apply_wide_gamut_conversion` is, because the conversion has to be applied at
/// the one place the values are combined rather than at each of a function's
/// return points.
///
/// The final pass therefore still sees `dFactor` 2.0000, unchanged from when the
/// prefilter was a box — its **tap count**, however, is whatever the strength
/// asks for: Hermite's 4-5 at `FAST_STRENGTH` and about twice that at
/// `SMOOTH_STRENGTH`, which is the whole of the extra ~25 ms a resize pays.
pub(crate) fn linear_resize(data: &ImageData, new_w: u32, new_h: u32) -> Vec<u8> {
    let (to_lin, to_srgb) = lin_tables();

    // **One read of the strength, one value, both halves.** The pre-reduction
    // and the final passes each call `contributions`, and the pre-reduction's
    // output is the final pass's input - so a strength read separately in each
    // would let the two halves disagree, i.e. resize with a prefilter of one
    // width feeding a final pass of another. Read once, thread it.
    let strength = downscale_strength();
    // **One read of the floor, one value, both axes.** The same argument and the
    // same count-checked test as the strength above: the two axes are separate
    // `contributions` calls, so a per-call read would let a toggle pressed
    // between them produce a picture low-passed one way horizontally and another
    // vertically.
    let floor = min_filter_scale();
    let pre_w = prefilter_dim(data.width, new_w);
    let pre_h = prefilter_dim(data.height, new_h);

    // Pass 1: reduce to at most 2x the target, in linear light, then convert
    // *that* to 12-bit linear. The conversion is the expensive part, so it must
    // not run over the full-resolution source.
    let t0 = std::time::Instant::now();
    let (small, lw, lh): (Cow<'_, [u8]>, u32, u32) = if pre_w < data.width || pre_h < data.height {
        (
            // **The literal `0.0`, and it is load-bearing** - see
            // `kernel_reduce_linear`'s floor parameter. Passing `floor` here
            // instead is the mistake this line is written to prevent, and
            // `the_floor_reaches_both_final_axes_and_only_those` is the check.
            Cow::Owned(kernel_reduce_linear(data, pre_w, pre_h, strength, 0.0)),
            pre_w,
            pre_h,
        )
    } else {
        (Cow::Borrowed(&data.rgba), data.width, data.height)
    };
    let t1 = std::time::Instant::now();
    let mut lin = vec![0u16; (lw as usize) * (lh as usize) * 4];
    small
        .par_chunks_exact(lw as usize * 4)
        .zip(lin.par_chunks_mut(lw as usize * 4))
        .for_each(|(srow, lrow)| {
            for (c, chunk) in srow.chunks_exact(4).enumerate() {
                lrow[c * 4] = to_lin[chunk[0] as usize];
                lrow[c * 4 + 1] = to_lin[chunk[1] as usize];
                lrow[c * 4 + 2] = to_lin[chunk[2] as usize];
                // alpha is linear by definition
                lrow[c * 4 + 3] = (chunk[3] as u16) << 4;
            }
        });

    let t2 = std::time::Instant::now();
    // Separable kernel resample in 12-bit linear light with precomputed weights.
    let (kern, support) = kernel(downscale_filter());
    let horizontal = contributions(lw, new_w, &kern, support, strength, floor);
    let vertical = contributions(lh, new_h, &kern, support, strength, floor);

    // Horizontal pass: width -> new_w.
    let mut tmp = vec![0u16; (new_w as usize) * (lh as usize) * 4];
    let pre_stride = lw as usize * 4;
    let new_wu = new_w as usize;
    tmp.par_chunks_mut(new_wu * 4)
        .enumerate()
        .for_each(|(y, out_row)| {
        let row = &lin[y * pre_stride..(y + 1) * pre_stride];
        for (x, con) in horizontal.iter().enumerate() {
            let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
            for &(si, w) in con {
                let p = &row[si * 4..si * 4 + 4];
                r += p[0] as f32 * w;
                g += p[1] as f32 * w;
                b += p[2] as f32 * w;
                a += p[3] as f32 * w;
            }
            let o = x * 4;
            out_row[o] = r as u16;
            out_row[o + 1] = g as u16;
            out_row[o + 2] = b as u16;
            out_row[o + 3] = a as u16;
        }
    });

    let t3 = std::time::Instant::now();
    // Vertical pass: height -> new_h, back to sRGB8.
    let mut out = vec![0u8; (new_w as usize) * (new_h as usize) * 4];
    let stride = new_w as usize * 4;
    out.par_chunks_mut(stride).enumerate().for_each(|(y, out_row)| {
        {
        for x in 0..new_w as usize {
            let (mut r, mut g, mut b, mut a) = (0f32, 0f32, 0f32, 0f32);
            for &(si, w) in &vertical[y] {
                let p = &tmp[si * stride + x * 4..si * stride + x * 4 + 4];
                r += p[0] as f32 * w;
                g += p[1] as f32 * w;
                b += p[2] as f32 * w;
                a += p[3] as f32 * w;
            }
            // Clamp to the 12-bit linear range (filters overshoot on edges).
            let o = x * 4;
            out_row[o] = to_srgb[(r.max(0.0).min(4095.0)) as usize];
            out_row[o + 1] = to_srgb[(g.max(0.0).min(4095.0)) as usize];
            out_row[o + 2] = to_srgb[(b.max(0.0).min(4095.0)) as usize];
            out_row[o + 3] = ((a.max(0.0).min(4080.0) / 16.0).round()) as u8;
        }
        }
    });
    if *PHASE_TIMING.get_or_init(|| std::env::var("PERF_PHASE").is_ok()) {
        eprintln!(
            "  linear_resize {}x{}->{}x{}: prefilter={:?} to_linear={:?} weights={:?} hpass={:?} vpass={:?}",
            data.width, data.height, new_w, new_h,
            t1 - t0, t2 - t1, t3 - t2, t3.elapsed(), t3.elapsed()
        );
    }
    out
}

/// Precomputed (source index, weight) pairs per destination pixel.
pub(crate) fn contributions(
    src_len: u32,
    dst_len: u32,
    kern: &impl Fn(f32) -> f32,
    support: f32,
    strength: f32,
    floor: f32,
) -> Vec<Vec<(usize, f32)>> {
    let mut out = Vec::with_capacity(dst_len as usize);
    let scale = dst_len as f32 / src_len as f32;
    // When downscaling, widen the filter by the inverse ratio (anti-aliasing),
    // then by `strength` on top: one multiplier reaching the pre-reduction and
    // the final passes alike, rather than a per-call-site decision that could
    // disagree with itself.
    //
    // **`floor` is [`MIN_FILTER_SCALE`] on the final pass and `0.0` on the
    // pre-reduction, and the asymmetry is the entire mechanism.** `scale` is each
    // stage's *own* ratio and the product of a split's two stages is the
    // reduction exactly, so the floor can only ever bind on a stage whose ratio
    // is above `1 / MIN_FILTER_SCALE` — which, for the pre-reduction, is no stage
    // at all, since it reduces by exactly `PREFILTER_FACTOR`. Hence `max` here
    // and `0.0` there, and hence a no-op on every reduction the prefilter
    // already covers.
    let filter_scale = if scale < 1.0 { (strength / scale).max(floor) } else { 1.0 };
    let radius = support * filter_scale;
    for dst in 0..dst_len as usize {
        let center = (dst as f32 + 0.5) / scale - 0.5;
        let lo = ((center - radius).floor() as i32).max(0) as usize;
        let hi = ((center + radius).ceil() as i32).min(src_len as i32 - 1) as usize;
        let mut con: Vec<(usize, f32)> = Vec::with_capacity((hi - lo + 1) as usize);
        let mut sum = 0f32;
        for si in lo..=hi {
            let w = kern((si as f32 - center) / filter_scale);
            if w != 0.0 {
                con.push((si, w));
                sum += w;
            }
        }
        // Normalize so weights sum to 1 (keeps brightness exact).
        for c in &mut con {
            c.1 /= sum;
        }
        out.push(con);
    }
    out
}

/// Normalize a decoded ImageData so dims and buffer agree. If the decoder's
/// buffer length doesn't match the declared dimensions, trust the buffer and
/// recompute dims when possible, otherwise pad the buffer. This guards
/// against decoders that mis-report sizes on damaged files.
pub fn normalized(mut data: ImageData) -> ImageData {
    let want = data.width as usize * data.height as usize * 4;
    if data.rgba.len() == want {
        return data;
    }
    let got = data.rgba.len();
    // If the buffer is self-consistent at some other height (or width), trust
    // it: that's what a decoder that mis-reported its size looks like.
    if data.width > 0 && got % (data.width as usize * 4) == 0 && got >= data.width as usize * 4 {
        data.height = (got / (data.width as usize * 4)) as u32;
        return data;
    }
    if data.height > 0 && got % (data.height as usize * 4) == 0 && got >= data.height as usize * 4 {
        data.width = (got / (data.height as usize * 4)) as u32;
        return data;
    }
    eprintln!(
        "image buffer {} bytes does not match {}x{}; padding to size",
        got, data.width, data.height
    );
    data.rgba.resize(want, 0);
    data
}

/// True if any pixel has a non-opaque alpha channel (used to decide whether
/// to draw the transparency checkerboard).
pub fn buffer_has_alpha(rgba: &[u8]) -> bool {
    rgba.chunks_exact(4).any(|p| p[3] != 255)
}

