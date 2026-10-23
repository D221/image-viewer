//! Module `animation`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use animation::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// A decoded animation: fully composited RGBA frames and their delays.
#[derive(Debug, Clone)]
pub struct Animation {
    pub frames: Vec<ImageData>,
    pub delays: Vec<Duration>,
}

impl Animation {
    /// Total loop duration. Zero-delay frames are floored to 20 ms (50 fps),
    /// matching how browsers clamp pathological GIF timing.
    pub fn total_duration(&self) -> Duration {
        let ms: u64 = self
            .delays
            .iter()
            .map(|d| d.as_millis().max(20) as u64)
            .sum();
        Duration::from_millis(ms)
    }
}

/// Playback-speed bounds: 0.1× is slow motion, 8× the ceiling. Zero would
/// freeze on frame 0 and negative runs time backwards — neither is a speed.
pub fn clamp_anim_speed(speed: f32) -> f32 {
    speed.clamp(0.1, 8.0)
}

/// Current frame index and time until the next boundary (in real time, for
/// repaint scheduling) for a playing animation, given elapsed playback time
/// scaled by `speed` (1.0 = file timing). Zero/short delays are floored to
/// 20 ms so a broken GIF can't spin the render loop.
pub fn anim_frame_state(anim: &Animation, elapsed: Duration, speed: f32) -> (usize, Duration) {
    let speed = clamp_anim_speed(speed);
    let scaled = elapsed.mul_f32(speed);
    let total = anim.total_duration().as_millis().max(1);
    let t = scaled.as_millis() % total;
    let mut acc = 0u128;
    for (i, d) in anim.delays.iter().enumerate() {
        let dm = d.as_millis().max(20);
        if t < acc + dm {
            return (i, Duration::from_millis((acc + dm - t) as u64).div_f32(speed));
        }
        acc += dm;
    }
    (
        anim.frames.len().saturating_sub(1),
        Duration::from_millis(20).div_f32(speed),
    )
}

/// One frame step for a paused animation (`,` / `.`, YouTube-style):
/// the frame after/before `idx`, wrapping at both ends. Animations loop,
/// so stepping never gets stuck. Zero frames (unreachable in practice —
/// an animation with no frames is never built) stays 0 rather than
/// dividing by zero.
pub fn step_frame(n: usize, idx: usize, forward: bool) -> usize {
    if n == 0 {
        return 0;
    }
    if forward {
        (idx + 1) % n
    } else {
        (idx + n.saturating_sub(1)) % n
    }
}

/// When frame `idx` starts, from loop origin: the floored delays before it.
///
/// Resuming a paused animation rewinds `start` by this, so playback
/// continues mid-loop instead of restarting — the same flooring
/// `total_duration` uses, or the resume point and the paint clock disagree
/// about which frame is on screen.
pub fn anim_frame_start(anim: &Animation, idx: usize) -> Duration {
    let ms: u64 = anim
        .delays
        .iter()
        .take(idx.min(anim.delays.len()))
        .map(|d| d.as_millis().max(20) as u64)
        .sum();
    Duration::from_millis(ms)
}

/// A zoom preset (keys `1`–`4`) as a source-pixel ratio: 1.0 is 100%.
/// Clamped to the sane range so a hand-edited config cannot park a key at
/// 0× (a divide-by-zero downstream) or 1000×. The draw path clamps again
/// against the fit scale, so this is config hygiene, not layout.
pub fn zoom_preset(presets: &[f32; 4], i: usize) -> f32 {
    presets[i.min(3)].clamp(0.1, 32.0)
}

/// Zoom percentage for the status readout: 100% is one source pixel per
/// physical pixel (`scale * ppp == 1`), whatever the window is. Against 1:1
/// rather than against fit, so the number means the same on a panorama at
/// 6% and a thumbnail at 400% — and so it stays put when the window moves
/// but the zoom does not.
pub fn zoom_percent(scale: f32, ppp: f32) -> u32 {
    (scale * ppp * 100.0).round().max(0.0) as u32
}

/// One axis of the tile loop's source range: which buffer pixels the visible
/// interval covers, clamped to the buffer.
///
/// `buf_to_src` is source pixels per buffer pixel — 1.0 when the tile buffer
/// is the full decode, 2.0 when it is a half-res DCT decode (a source past
/// `MAX_TEXTURE_DIM`, whose full texture would not fit the GPU). Dividing by
/// it is what keeps the range in *buffer* pixels: without it the range is
/// named in source pixels and every tile is cut from the wrong place.
pub fn tile_axis_range(
    vis_min: f32,
    vis_max: f32,
    paint_min: f32,
    scale: f32,
    buf_to_src: f32,
    buf_dim: u32,
) -> (u32, u32) {
    let k = buf_to_src.max(f32::EPSILON);
    let lo = ((vis_min - paint_min) / scale / k).floor().max(0.0) as u32;
    let hi = ((vis_max - paint_min) / scale / k).ceil().max(0.0) as u32;
    (lo.min(buf_dim), hi.min(buf_dim))
}

/// Where one tile lands on screen: the tile's buffer span, pitched up to
/// source pixels, laid out from the page's paint origin at `scale`.
///
/// The pitch-up is the same `buf_to_src` ratio as [`tile_axis_range`], and
/// for the same reason: a tile cut from a half-res buffer and laid out at
/// source pitch lands every tile shifted and squeezed, composing a small
/// copy of the whole image over the image's own top-left. At 1.0 this is
/// exactly the old arithmetic, so full-res sources paint bit-identical
/// rects.
pub fn tile_draw_rect(
    paint_min: (f32, f32),
    px: u32,
    py: u32,
    tw: u32,
    th: u32,
    scale: f32,
    kx: f32,
    ky: f32,
) -> [f32; 4] {
    [
        paint_min.0 + px as f32 * kx * scale,
        paint_min.1 + py as f32 * ky * scale,
        tw as f32 * kx * scale,
        th as f32 * ky * scale,
    ]
}

/// Window size hugging `content` (the picture as drawn, in points): the gap
/// between the window's inner size and the central area is the side panels
/// and bars, which keep whatever they have — so adding the same gap back
/// wraps the picture with the chrome unchanged. Clamped to the monitor (an
/// 8× spread is bigger than any screen) and floored (degenerate content
/// must not collapse the window into something ungrabbable).
pub fn fit_window_size(
    content: (f32, f32),
    inner: (f32, f32),
    central: (f32, f32),
    monitor: Option<(f32, f32)>,
) -> (f32, f32) {
    let chrome = (
        (inner.0 - central.0).max(0.0),
        (inner.1 - central.1).max(0.0),
    );
    let mut out = (content.0 + chrome.0, content.1 + chrome.1);
    if let Some(m) = monitor {
        out.0 = out.0.min(m.0);
        out.1 = out.1.min(m.1);
    }
    (out.0.max(200.0), out.1.max(120.0))
}

/// Where the outer frame must sit so a hide/show-title-bar toggle moves only
/// the chrome, never the picture. Left alone, winit keeps a decorations
/// change's *outer* rect and lets the client area absorb (hide) or give up
/// (show) the title strip — the picture jumps, re-fits, and in fit-window
/// mode the window's hug breaks. The caller keeps the client where it was by
/// re-asserting its size and putting the frame at the returned top:
///
/// - hiding (`strip_after` 0): the frame top lands where the client's top
///   was, so the strip the frame is losing can simply be reclaimed by it;
/// - showing: the frame sits `strip_after` ([`Config::title_strip`], the
///   remembered measurement) above the client's top, because that strip is
///   about to appear in front of it and the bottom edge must not move.
///
/// Clamped at 0 — the top of the *virtual screen*, not of the current
/// monitor, the same naive assumption the fit-window centring makes — so a
/// window with less headroom than the strip gives its top rows up to the
/// re-added chrome instead of parking the title bar off-screen.
pub fn outer_top_for_decoration_toggle(inner_top: f32, strip_after: f32) -> f32 {
    (inner_top - strip_after.max(0.0)).max(0.0)
}

/// Decode an animation into composited frames, capped at the limits above.
///
/// Dispatch is on [`AnimatedFormat`], not on the extension, and the `image` crate
/// composites partial frames and applies disposal methods for all three.
///
/// **Why the format is passed in rather than sniffed here:** for PNG and WebP the
/// "is it actually animated" question needs the header, and asking for the header
/// of a *still* image is not free for WebP, whose `into_frames` decodes a full
/// frame. [`animation_frame_count`] is the cheap check and is called first.
pub(crate) fn load_animation(
    bytes: &[u8],
    max_dim: u32,
    start: std::time::Instant,
    format: AnimatedFormat,
) -> Result<Animation, LoadError> {
    let cur = std::io::Cursor::new(bytes);
    match format {
        AnimatedFormat::Gif => {
            collect_animation(image::codecs::gif::GifDecoder::new(cur)?, max_dim, start)
        }
        AnimatedFormat::Png => {
            // `apng()` is infallible in practice; it is `Result` upstream.
            let png = image::codecs::png::PngDecoder::new(cur)?;
            collect_animation(png.apng()?, max_dim, start)
        }
        AnimatedFormat::Webp => {
            collect_animation(image::codecs::webp::WebPDecoder::new(cur)?, max_dim, start)
        }
    }
}

/// Decode only the first frame of an animation, for a grid cell.
///
/// One `next()` and no loop, so this cannot hang on a truncated animation — the
/// same reason [`is_animated`] asks for two frames rather than counting. A cell
/// shows one picture, and the picture it has to match is `frames[0]` of
/// [`load_animation`], so this returns the same pixels without decoding the rest.
///
/// Returns `None` rather than an error so the caller can fall through to the
/// normal single-image path, which is what a *still* file of an animated format
/// needs anyway.
pub(crate) fn first_frame(
    bytes: &[u8],
    max_dim: u32,
    start: std::time::Instant,
    format: AnimatedFormat,
) -> Option<ImageData> {
    use image::AnimationDecoder;
    let cur = std::io::Cursor::new(bytes);
    let frame = match format {
        AnimatedFormat::Gif => image::codecs::gif::GifDecoder::new(cur)
            .ok()?
            .into_frames()
            .next()?,
        AnimatedFormat::Png => image::codecs::png::PngDecoder::new(cur)
            .ok()?
            .apng()
            .ok()?
            .into_frames()
            .next()?,
        AnimatedFormat::Webp => image::codecs::webp::WebPDecoder::new(cur)
            .ok()?
            .into_frames()
            .next()?,
    };
    let frame = frame.ok()?;
    let mut data = ImageData {
        width: frame.buffer().width(),
        height: frame.buffer().height(),
        rgba: frame.buffer().clone().into_raw(),
        load_time: start.elapsed(),
    };
    if data.width.max(data.height) > max_dim {
        data = downscale_image(&data, max_dim);
    }
    Some(normalized(data))
}

/// Whether this file has more than one frame, without decoding more than two of
/// them.
///
/// This is the cheap gate, and it is the difference between one decode and two: a
/// still WebP handed to `into_frames` yields one *decoded* frame which the caller
/// throws away and then decodes the file again.
///
/// **It asks for two frames, not for the count, and both of those words are load
/// bearing.**
///
/// *Asking for two* rather than counting: the first version of this was
/// `into_frames().count()`, which decodes the whole animation and then the real
/// decode does it again — so a 240-frame GIF was decoded twice before the first
/// pixel appeared. The answer only needs to be "more than one?".
///
/// *Two* rather than "iterate": a **truncated** GIF makes `into_frames` yield `Err`
/// and then keep yielding it, and `count()` never stops — so a 7.9 MB GIF that is
/// missing its last byte hung the worker forever. The image never painted, the
/// stall watchdog fired at 6 s, and the app sat there with six neighbour previews
/// decoded and none of the one you asked for. It is in the benchmark folder, so
/// this was not hypothetical. `take(2)` cannot hang whatever the file does.
///
/// PNG and WebP do not need the frames at all: both answer from the header.
pub fn is_animated(bytes: &[u8], format: AnimatedFormat) -> bool {
    use image::AnimationDecoder;
    let cur = std::io::Cursor::new(bytes);
    match format {
        AnimatedFormat::Gif => {
            let Ok(dec) = image::codecs::gif::GifDecoder::new(cur) else {
                return false;
            };
            two_ok_frames(dec.into_frames())
        }
        AnimatedFormat::Png => {
            // `is_apng` is a header read. A plain PNG must not be mistaken for a
            // one-frame animation, which would decode it twice.
            let Ok(png) = image::codecs::png::PngDecoder::new(cur) else {
                return false;
            };
            png.is_apng().unwrap_or(false)
        }
        AnimatedFormat::Webp => image::codecs::webp::WebPDecoder::new(cur)
            .map(|d| d.has_animation())
            .unwrap_or(false),
    }
}

/// True when the first two frames both decode. Two `next` calls and no loop, so it
/// terminates on any input including a file whose frame data runs out.
pub(crate) fn two_ok_frames<'a, I: Iterator<Item = image::ImageResult<image::Frame>>>(mut it: I) -> bool {
    matches!(it.next(), Some(Ok(_))) && matches!(it.next(), Some(Ok(_)))
}

/// Shared body of [`load_animation`], generic over the three decoders because
/// `image` gives them no common concrete type.
pub(crate) fn collect_animation<'a, D: image::AnimationDecoder<'a>>(
    dec: D,
    max_dim: u32,
    start: std::time::Instant,
) -> Result<Animation, LoadError> {
    // No `use image::AnimationDecoder` needed: the bound on `D` above already
    // puts the trait in scope for `into_frames`.
    let mut frames = Vec::new();
    let mut delays = Vec::new();
    let mut total = 0usize;
    for frame in dec.into_frames() {
        if frames.len() >= MAX_ANIM_FRAMES || total >= MAX_ANIM_BYTES {
            break;
        }
        // Tolerate a truncated trailing frame: keep what we have.
        let frame = match frame {
            Ok(f) => f,
            Err(_) => break,
        };
        let mut data = ImageData {
            width: frame.buffer().width(),
            height: frame.buffer().height(),
            rgba: frame.buffer().clone().into_raw(),
            load_time: start.elapsed(),
        };
        // A single frame that is itself over the cap is not a truncated animation,
        // it is a decompression bomb, and the running-total check above only
        // stops *after* it has been allocated. One 20000x20000 APNG frame is
        // 1.6 GB, and memory exhaustion here presents as a worker that never
        // answers rather than as an error. Checked before the downscale, because
        // that is the size that was actually allocated.
        if data.rgba.len() > MAX_ANIM_BYTES {
            break;
        }
        if data.width.max(data.height) > max_dim {
            data = downscale_image(&data, max_dim);
        }
        let data = normalized(data);
        total += data.rgba.len();
        delays.push(Duration::from(frame.delay()));
        frames.push(data);
    }
    if frames.is_empty() {
        return Err(LoadError::Io(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "no decodable animation frames",
        )));
    }
    Ok(Animation { frames, delays })
}

/// Decode the file's embedded EXIF thumbnail, if it is worth using.
///
/// Returns `None` — meaning "decode the file properly" — for every JPEG without
/// a usable one, which is most of them. `main` is the *stored* source size and
/// `main_tag` its EXIF orientation.
pub(crate) fn decode_embedded_thumbnail(
    bytes: &[u8],
    target: u32,
    main: Option<(u32, u32)>,
    main_tag: u16,
    start: std::time::Instant,
) -> Option<ImageData> {
    // Only for grid-sized requests. See EMBEDDED_THUMB_MAX_TARGET.
    if target == 0 || target > EMBEDDED_THUMB_MAX_TARGET {
        return None;
    }
    let Some(t) = find_embedded_thumbnail(bytes) else {
        return None;
    };
    let (w, h) = t.dims?;
    let (mw, mh) = main?;
    // An upscale means the embedded copy is too small to be worth anything, and
    // a "thumbnail" as large as the image is a mislabelled full-res copy.
    if w.max(h) * 2 > mw.max(h) {
        return None;
    }
    // Decide the rotation from the evidence; bail out if it is ambiguous.
    embedded_thumb_rotation(main_tag, (mw, mh), (w, h))?;

    let raw = bytes.get(t.range.0..t.range.1)?;
    let img = image::load_from_memory_with_format(raw, image::ImageFormat::Jpeg).ok()?;
    let rgba = img.to_rgba8();
    let mut data = normalized(ImageData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        load_time: start.elapsed(),
    });
    // Apply whichever transform the aspect ratio said was needed. Computed
    // again from the *decoded* size so the dims always end up self-consistent.
    let want = match embedded_thumb_rotation(main_tag, (mw, mh), (data.width, data.height)) {
        Some(1) => 1,
        Some(t) => t,
        None => return None,
    };
    if want != 1 {
        apply_exif_orientation(&mut data, want);
    }
    if data.width.max(data.height) > target {
        data = downscale_image(&data, target);
    }
    Some(data)
}

