//! Module `decode`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use decode::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Hard cap on the long side of any decoded image (GPU texture limit).
pub const MAX_TEXTURE_DIM: u32 = 16384;

/// Don't keep a full-res copy of anything above this (RGBA = 4 bytes/pixel).
pub const MAX_FULL_PIXELS: u64 = 300_000_000;

/// Decode a JPEG straight to RGBA at the smallest DCT reduction (1/8, 1/4, 1/2, 1)
/// whose long side is still >= `max_dim`. The IDCT runs at reduced size, so a
/// 95 MP file never has to be materialised at full resolution.
pub(crate) fn decode_jpeg_scaled(
    bytes: &[u8],
    max_dim: u32,
) -> Result<(u32, u32, Vec<u8>), LoadError> {
    use turbojpeg::{Decompressor, Image, PixelFormat, ScalingFactor};
    let err = |e: turbojpeg::Error| LoadError::Jpeg(e.to_string());

    let mut dec = Decompressor::new().map_err(err)?;
    let hdr = dec.read_header(bytes).map_err(err)?;
    let long = hdr.width.max(hdr.height);

    // Smallest reduction that still gives >= max_dim, but never exceed the GPU texture limit.
    let limit = MAX_TEXTURE_DIM as usize;
    let sf = [8usize, 4, 2, 1]
        .into_iter()
        .map(|d| ScalingFactor::new(1, d))
        .find(|sf| sf.scale(long) >= max_dim as usize && sf.scale(long) <= limit)
        .or_else(|| {
            [1usize, 2, 4, 8]
                .into_iter()
                .map(|d| ScalingFactor::new(1, d))
                .find(|sf| sf.scale(long) <= limit)
        })
        .unwrap_or(ScalingFactor::new(1, 8));
    dec.set_scaling_factor(sf).map_err(err)?;
    let scaled = hdr.scaled(sf);

    let mut img = Image {
        pixels: vec![0u8; scaled.width * scaled.height * 4],
        width: scaled.width,
        pitch: scaled.width * 4,
        height: scaled.height,
        format: PixelFormat::RGBA,
    };
    dec.decompress(bytes, img.as_deref_mut()).map_err(err)?;
    Ok((scaled.width as u32, scaled.height as u32, img.pixels))
}

/// Full-resolution decode of `path`, for the zoom/pan tiles and the clipboard.
///
/// `page` is 0 for everything but a page of a multi-page TIFF, and is threaded
/// into the same page-aware path the preview uses rather than having a second
/// implementation that could disagree with it.
pub fn load_image_full(path: &Path, page: u32) -> Result<ImageData, LoadError> {
    load_page_preview_full(path, MAX_TEXTURE_DIM, page).map(|r| {
        // The preview is already the full image whenever the source fitted under
        // the texture cap, which is the only way a full decode is asked for.
        r.full.unwrap_or(r.preview)
    })
}

/// True if the bytes look like a JPEG (magic bytes), regardless of file name.
pub fn is_jpeg_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0xD8, 0xFF])
}

/// Image dimensions sniffed from the file content, not the extension.
/// Cheap: only the header is parsed.
///
/// Also the reason [`page_dims`] can read a prefix instead of the whole file: a
/// stream too short to carry a format's dimension marker is a `None` here rather
/// than a wrong number, so a header that falls outside the prefix costs one
/// fallback read instead of a mismeasurement. See [`PROBE_PREFIX_BYTES`].
pub fn image_dims_bytes(bytes: &[u8]) -> Option<(u32, u32)> {
    // Formats the `image` crate can't open get their own header probes.
    if is_jxl_bytes(bytes) {
        return jxl_oxide::integration::JxlDecoder::new(std::io::Cursor::new(bytes))
            .ok()
            .map(|dec| image::ImageDecoder::dimensions(&dec));
    }
    if is_heif_bytes(bytes) {
        return heic::ImageInfo::from_bytes(bytes).ok().map(|info| (info.width, info.height));
    }
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Files at or under this many pixels decode fast enough to skip the cheap
/// preview stage entirely and go straight to a full decode. ~12 MP covers
/// 4K screens and typical wallpapers; turbojpeg decodes those in well under
/// 150 ms, so two stages would only add latency.
pub const FAST_FULL_MAX_PIXELS: u64 = 12_000_000;

/// Long side of the display texture when the preview pass decoded the full
/// image. 8192 is crisp fit-to-window on an 8K screen; zooming further refines
/// with tiles from the full-res data.
pub const PREVIEW_TEXTURE_DIM: u32 = 8192;

/// What a decode is *for*, which decides how much of it is kept.
///
/// A grid cell and the main view asked for the same function with the same target
/// and used to get the same answer: a full-resolution buffer, *retained and sent
/// back to the UI thread*, for a 150 px cell. For a 25 MP PNG that is 100 MB
/// crossing a channel to be dropped, and for a 17000 px scan it is a 5851 KB GPU
/// texture — 512 of which is the thumbnail cache's ceiling, so 2.9 GB. The cell
/// genuinely never wants any of it, so the request now says so.
///
/// What this does **not** claim: that a cell never allocates at source size. For
/// PNG, HEIC, JXL and TIFF the only reduced-size read the decoders in this tree
/// offer is "decode it all, then resize", and that full-size buffer is still
/// allocated inside `dynamic_to_image_data` and thrown away there. Changing that
/// needs a row-streaming decode per format and is not attempted here; what is gone
/// is the *retention*, the second downscale, and the 44x oversized texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// The image on screen. `full` may be filled, and it rides along on the
    /// preview so a second decode is not needed to zoom.
    View,
    /// One grid cell: a small picture, nothing else. Never returns `full` or
    /// `anim`, and the result is at most the requested target on its long side.
    Thumb,
}

/// Result of a single-pass preview load: the preview to show immediately, plus
/// the full-res image when it was already decoded in the same pass.
pub struct PreviewAndFull {
    pub preview: ImageData,
    /// Set when the full-res decode already happened (small files and formats
    /// without a fast preview path, e.g. PNG).
    pub full: Option<ImageData>,
    /// Fully decoded animation when the file is an animated GIF.
    pub anim: Option<Animation>,
    /// True source dimensions from the header, when they could be sniffed.
    pub orig: Option<(u32, u32)>,
    /// Source is larger than the requested target: the preview texture is
    /// below source density and should be re-decoded if the display grows.
    pub below_target: bool,
    /// The file's EXIF orientation tag, 0 when there is none.
    ///
    /// The decode has *already* applied it to the pixels, so nothing needs this
    /// to draw. It travels with the result for the crop UI, which has to express
    /// a rectangle the user dragged on the **displayed** picture back in
    /// **stored** coordinates, and that is a composition involving the tag.
    pub exif_tag: u16,
    /// The block grid `(w, h)` a lossless JPEG crop origin must align to, and
    /// `(1, 1)` for every format with no block structure.
    ///
    /// 8 px for 4:4:4, 16 px for the usual 4:2:0. The crop UI snaps the frame it
    /// draws to this, so that what is on screen is what the save cuts — see
    /// [`Crop::snapped_to_mcu`].
    pub crop_grid: (u32, u32),
    /// The file's colour classification, classified **in this decode** from the
    /// bytes it already read.
    ///
    /// This is the answer [`color_info`] would give, computed from memory rather
    /// than from a second `fs::read` of the whole file — the file is in hand here,
    /// and one extra marker walk is microseconds against a 10–17 ms read of a real
    /// photograph. It travels with the result for two consumers: the metadata
    /// panel's colour line (which would otherwise wait on a separate job) and
    /// [`apply_wide_gamut_conversion`]'s own gate.
    pub colour: ColorInfo,
}

/// True if the bytes look like a GIF (magic bytes).
pub fn is_gif_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GIF8")
}

/// True if the bytes look like a QOI image (magic bytes).
///
/// `"qoif"` at offset 0, followed by a big-endian width and height.
pub fn is_qoi_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 14 && bytes.starts_with(b"qoif")
}

/// True if the bytes look like a Windows bitmap (magic bytes).
///
/// `BM` plus a plausible 14-byte file header. There is no other format that
/// starts `BM`, so the length check is only there to keep a two-byte file from
/// being accepted.
pub fn is_bmp_bytes(bytes: &[u8]) -> bool {
    bytes.len() >= 26 && bytes.starts_with(b"BM")
}

/// True if the bytes look like a Windows icon or cursor (magic bytes).
///
/// The container is shared: reserved word 0, type 1 (icon) or 2 (cursor), then
/// a count. Both `.ico` and `.cur` are the same format, and both hold a mixture
/// of PNG-compressed and BMP-compressed entries — which the decoder handles, so
/// nothing here looks inside.
pub fn is_ico_bytes(bytes: &[u8]) -> bool {
    // Layout: reserved u16 (0), type u16 (1 = icon, 2 = cursor), count u16.
    bytes.len() >= 6
        && bytes[0..2] == [0, 0]
        && matches!(bytes[2], 1 | 2)
        && u16::from_le_bytes([bytes[4], bytes[5]]) != 0
}

/// True if the bytes look like a TIFF (magic bytes).
///
/// Both byte orders, and both the classic and BigTIFF headers: `II*\0` /
/// `MM\0*` (42) and `II+\0` / `MM\0+` (43). Only the 42 forms are common, but
/// a BigTIFF header is just as unambiguous and rejecting it would mean a large
/// file failing for no reason.
pub fn is_tiff_bytes(bytes: &[u8]) -> bool {
    if bytes.len() < 4 {
        return false;
    }
    let order = &bytes[0..2];
    let le = order == [0x49, 0x49];
    let be = order == [0x4D, 0x4D];
    if !le && !be {
        return false;
    }
    let tag = if le {
        u16::from_le_bytes([bytes[2], bytes[3]])
    } else {
        u16::from_be_bytes([bytes[2], bytes[3]])
    };
    tag == 42 || tag == 43
}

/// True if the bytes look like an SVG document (content-based, not extension).
///
/// Accepts `<?xml ...?><svg ...>` and a bare `<svg ...>` root. An SVG has no
/// magic bytes, so this is a prefix sniff: it looks for the XML declaration
/// followed by an `<svg` element, or `<svg` at the very start. The `<svg` must
/// be followed by `>`, `/`, or whitespace — a bare `<svg` fragment is not SVG.
/// Content-based per AGENTS.md — an SVG saved as `.jpg` must load.
pub fn is_svg_bytes(bytes: &[u8]) -> bool {
    let starts_svg = |b: &[u8]| {
        b.starts_with(b"<svg")
            && (b.len() == 4 || matches!(b[4], b'>' | b'/' | b' ' | b'\t' | b'\n' | b'\r'))
    };
    if bytes.starts_with(b"<?xml") {
        // Skip the XML declaration, then look for `<svg`.
        let after = if let Some(end) = bytes.windows(2).position(|w| w == b"?>") {
            &bytes[end + 2..]
        } else {
            bytes
        };
        starts_svg(after.trim_ascii_start())
    } else {
        starts_svg(bytes.trim_ascii_start())
    }
}

/// Caps for animation decoding: stop when either limit is hit.
pub const MAX_ANIM_FRAMES: usize = 240;
pub const MAX_ANIM_BYTES: usize = 256 * 1024 * 1024;

/// A format that can carry more than one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnimatedFormat {
    Gif,
    /// Animated PNG. A *plain* PNG is the same container with no `acTL` chunk, so
    /// "is this animated" is a question about the file and not the extension.
    Png,
    /// Animated WebP. Same story: a still WebP is a VP8/VP8L still, not an ANIM
    /// chunk.
    Webp,
}

/// The animated format these bytes are, if any.
///
/// Content-based like every other format decision here, so a PNG saved as `.jpg`
/// still animates. Cheap: magic bytes only, no header parse, no decode.
pub fn animated_format(bytes: &[u8]) -> Option<AnimatedFormat> {
    if is_gif_bytes(bytes) {
        return Some(AnimatedFormat::Gif);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(AnimatedFormat::Png);
    }
    // RIFF container, and the form type at offset 8 has to be WEBP. Checking the
    // "RIFF" alone would match WAV and AVI.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some(AnimatedFormat::Webp);
    }
    None
}

/// Upper bound on the pages walked in one TIFF.
///
/// A corrupt or hostile file can chain IFDs indefinitely, and counting them is a
/// `next_image()` per page. 4096 is far past any real document, and the point of
/// the cap is that the walk terminates on *this* machine rather than on whatever
/// the file claims.
pub const MAX_TIFF_PAGES: u32 = 4096;

/// How many pages this file has — 1 for anything that is not a readable TIFF.
///
/// **This runs on every file you navigate to**, so the non-TIFF case has to be
/// nearly free, and it is: four bytes are read to check the magic, and a
/// photo folder is not TIFFs. The first version of this read the whole file with
/// `fs::read` before sniffing, which would have meant pulling a 10 MB JPEG into
/// memory every single time you pressed the right arrow.
///
/// **Still not called during a folder scan.** A scan would open every file in the
/// folder, and start-up is the thing this project spends the most effort on. The
/// gallery remembers the answer, so this is paid once per file visited rather
/// than once per visit.
///
/// Returns 1 rather than an error for everything unreadable, because "not a
/// multi-page TIFF" and "a TIFF we cannot read" mean the same thing to every
/// caller here: one page.
pub fn tiff_page_count(path: &Path) -> u32 {
    // Four bytes, then give up. This is the whole reason the function is safe to
    // call on every navigation.
    let mut magic = [0u8; 4];
    if std::fs::File::open(path)
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut magic)
        })
        .is_err()
    {
        return 1;
    }
    if std::fs::File::open(path)
        .and_then(|mut f| {
            use std::io::Read;
            f.read_exact(&mut magic)
        })
        .is_err()
    {
        return 1;
    }
    if !is_tiff_bytes(&magic) {
        return 1;
    }
    // A TIFF's IFD chain is at the front of the file and `next_image` reads
    // directories rather than pixels, so this is a few KB of reads even for a
    // 50 MB scan. `BufReader` keeps it that way: the crate seeks a lot.
    let Ok(file) = std::fs::File::open(path) else {
        return 1;
    };
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::BufReader::new(file)) else {
        return 1;
    };
    // `Decoder::new` has already read the first IFD, so the count starts at one.
    let mut n = 1u32;
    while dec.more_images() && n < MAX_TIFF_PAGES {
        if dec.next_image().is_err() {
            // A truncated chain: report what was really there rather than
            // guessing, and rather than failing the file outright.
            break;
        }
        n += 1;
    }
    n
}

/// [`tiff_page_count`] over bytes already in hand, for callers that have the
/// whole file. Reads the first IFD chain out of the slice, so it costs nothing
/// beyond the header — the pixels are never touched.
pub fn tiff_page_count_bytes(bytes: &[u8]) -> u32 {
    if !is_tiff_bytes(bytes) {
        return 1;
    }
    let Ok(mut dec) = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)) else {
        return 1;
    };
    let mut n = 1u32;
    while dec.more_images() && n < MAX_TIFF_PAGES {
        if dec.next_image().is_err() {
            break;
        }
        n += 1;
    }
    n
}

/// How many bytes of a file [`page_dims`] reads.
///
/// 64 KB, chosen because every format's dimension marker sits near the front
/// *usually* rather than always: PNG's IHDR is at byte 8 and a GIF's screen
/// descriptor at byte 6, both fixed; a JPEG's SOF0 has to follow whatever metadata
/// segments precede it, and the spec caps a segment's payload at 65533 bytes — so a
/// camera that fills an APP1 right up to the limit puts SOF0 a few bytes **past**
/// this. This is therefore a budget that is nearly always sufficient and
/// provably not always sufficient, which is what the fallback below is for.
///
/// **A miss here is a `None`, never a wrong answer.** [`image_dims_bytes`] either
/// finds a format's marker or reports that it did not, so a header that happens to
/// sit further out falls through to a whole-file read rather than being measured
/// wrong. That distinction is the whole reason the prefix read can be aggressive:
/// the expensive path is only ever the price of being unlucky, and being unlucky
/// costs one read of a file that is rare.
///
/// The fallback is not claimed to be unreachable, and no test asserts that it is —
/// building a header past 64 KB to exercise it would mean writing a multi-megabyte
/// fixture to cover a branch that costs nothing to be correct about.
pub const PROBE_PREFIX_BYTES: usize = 65_536;

/// Read up to [`PROBE_PREFIX_BYTES`] from `path`.
///
/// **Tolerates a short file**, which is most of them: a preallocated buffer is
/// filled in a loop and truncated to what actually arrived, so a 300-byte PNG
/// comes back as 300 bytes rather than as an error. `read_exact` is the wrong tool
/// for that — it fails on a file shorter than the buffer, and a small image is not
/// a failure.
///
/// A loop rather than one `read`, because a single `read` is free to return short
/// on a file that is not short.
pub(crate) fn read_prefix(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; PROBE_PREFIX_BYTES];
    let mut filled = 0usize;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    buf.truncate(filled);
    Ok(buf)
}

/// [`image_dims_bytes`] over the whole file, for a header that did not fit in
/// [`PROBE_PREFIX_BYTES`]. Costs a `fs::read` of the file, so it is the fallback
/// and not the path.
pub(crate) fn image_dims_path(path: &Path) -> Option<(u32, u32)> {
    image_dims_bytes(&std::fs::read(path).ok()?)
}

/// The stored dimensions of page `page`, without decoding it.
///
/// **Stored, not display-oriented.** The EXIF tag is *not* applied here, and that
/// is deliberate. `decode_page` reads the *same* pair with the same
/// [`image_dims_bytes`] call and applies the tag itself, so `orig` and this agree by
/// construction rather than by convention — one source measurement, presented in two
/// forms, because the two callers each need a different one.
///
/// **So do not "fix" a caller by folding the tag in here.** The swap is its own
/// inverse, so a second application is not a doubled value to be noticed: it is the
/// identity, which quietly puts the *stored* pair back in the shape cache and
/// restores exactly the silent failure [`Gallery::learn_shapes`] exists to prevent.
/// Applying the tag is that function's job, and it is load-bearing rather than
/// cosmetic.
///
/// Header only. This runs on arrival at a file, so it has to cost about what
/// [`tiff_page_count`] costs rather than what a decode costs: a bounded read and a
/// format sniff. The whole-file fallback is the exception, and only for a header
/// that does not fit in the prefix.
///
/// `page > 0` is only meaningful for a TIFF, the one format in this tree with
/// pages, and it walks the IFD chain because a document's pages may differ in size.
/// Nothing else is asked for page > 0: an animation's frames all belong to one
/// file, and a later page of one is answered by the decode itself, which has that
/// page's real dimensions in hand.
pub fn page_dims(path: &Path, page: u32) -> Option<(u32, u32)> {
    page_dims_and_jpeg(path, page).map(|(dims, _)| dims)
}

/// [`page_dims`], plus whether the file is a JPEG.
///
/// One function rather than two because the JPEG flag falls out of the bytes the
/// read already returned, and the condition it gates is the one `decode_page`
/// applies the EXIF orientation under — so this is the file's answer, from the same
/// bytes as its dimensions, rather than a second sniff that could disagree with them.
///
/// **It does not save the whole open.** `learn_shapes` still opens the file again
/// for `exif_orientation`, because `nom_exif::read_exif` takes a path and not bytes.
/// That call is ~0.1-0.2 ms against a 10-17 ms `fs::read`, and it happens once per
/// file rather than once per visit, so this is still a net win — but the honest
/// version is "one read for the dimensions *and* the format", not "one read instead
/// of two, full stop".
///
/// The obvious follow-up is an `exif_orientation_bytes(&[u8])` over the prefix. Not
/// done here: `find_embedded_thumbnail` already walks the APP1 block in hand, so the
/// work would be to hand *that* walk's answer out rather than to reach for `nom_exif`
/// a third time.
pub(crate) fn page_dims_and_jpeg(path: &Path, page: u32) -> Option<((u32, u32), bool)> {
    if page > 0 {
        return tiff_page_dim(path, page).map(|dims| (dims, false));
    }
    let buf = read_prefix(path).ok()?;
    let jpeg = is_jpeg_bytes(&buf);
    image_dims_bytes(&buf).or_else(|| image_dims_path(path)).map(|dims| (dims, jpeg))
}

/// Page `page`'s stored dimensions out of a TIFF's IFD chain, or `None`.
///
/// A walk with the same shape as [`tiff_page_count`]'s — the crate reads
/// directories, never pixels, and `BufReader` keeps it a few KB of reads on a
/// 50 MB scan. `Decoder::new` has already read IFD 0, so `page` further steps
/// land on the `page`th.
///
/// Bounded by the chain rather than by [`MAX_TIFF_PAGES`]: `more_images` is false
/// at the end, and the crate's own cycle detector rejects an IFD that points at
/// itself, so a caller passing an absurd `page` gets `None` rather than a walk
/// that does not terminate.
pub(crate) fn tiff_page_dim(path: &Path, page: u32) -> Option<(u32, u32)> {
    let mut dec =
        tiff::decoder::Decoder::new(std::io::BufReader::new(std::fs::File::open(path).ok()?))
            .ok()?;
    for _ in 0..page {
        if !dec.more_images() || dec.next_image().is_err() {
            return None;
        }
    }
    dec.dimensions().ok()
}

/// Decode page `page` (0-based) of a TIFF, downscaled to `max_dim`.
///
/// Goes through the `tiff` crate rather than `image`, and that is not a
/// preference: `image` 0.25's `TiffDecoder` implements only `ImageDecoder`, whose
/// `read_image` *consumes* the decoder, and it is not an `AnimationDecoder`. There
/// is no way to ask it for page 2. The `tiff` crate has `more_images` and
/// `next_image`, which is why it is a direct dependency.
///
/// The colour types handled are the ones that are a straight reinterpretation of
/// the decoded buffer, plus CMYK — which needs a real conversion and gets one by
/// asking `image` to do it. Palette, CMYKA, YCbCr, Lab and multiband pages are
/// **not**: applying a colormap or a colour transform correctly is `image`'s job
/// and there is no route to it for those types, while guessing would produce
/// pages that are subtly, silently the wrong colours — which for a scanned
/// document means invisible but for a photo means wrong. Those return an error,
/// which surfaces the same way any other decode failure does, rather than quietly
/// showing something else.
pub(crate) fn decode_tiff_page(
    bytes: &[u8],
    page: u32,
    max_dim: u32,
    start: std::time::Instant,
) -> Result<ImageData, LoadError> {
    // The `tiff` crate has its own error type and `LoadError` has no `From` for
    // it. Routing through `Other` keeps the message, which matters: a multi-page
    // file that cannot be read is otherwise an unexplained blank panel.
    fn tiff_err(e: tiff::TiffError) -> LoadError {
        LoadError::Other(format!("tiff: {e}"))
    }
    let mut dec = tiff::decoder::Decoder::new(std::io::Cursor::new(bytes)).map_err(tiff_err)?;
    for _ in 0..page {
        if !dec.more_images() {
            return Err(LoadError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!("page {page} requested, file has fewer"),
            )));
        }
        dec.next_image().map_err(tiff_err)?;
    }
    let (w, h) = dec.dimensions().map_err(tiff_err)?;
    let kind = dec.colortype().map_err(tiff_err)?;
    let result = dec.read_image().map_err(tiff_err)?;

    /// Straight reinterpretation of the decoded buffer, then the one conversion
    /// every page shares.
    ///
    /// Most types are a plain reinterpretation and nothing more. CMYK is not, and
    /// goes through `image` — see [`cmyk_via_image`]. Palette, CMYKA, YCbCr, Lab
    /// and multiband still return an error: there is no conversion we can hand to
    /// `image` for them, and approximating a colormap or a colour transform here
    /// would produce pages that are subtly and silently the wrong colours — for a
    /// photo, wrong; for a scanned document, invisible. An error is visible; that
    /// is the whole reason the refusal exists.
    ///
    /// The 16-bit buffers arrive as native-endian `u16` (the crate converts), so
    /// those are handled properly rather than reinterpreted from bytes.
    fn to_rgba8(
        w: u32,
        h: u32,
        kind: tiff::ColorType,
        result: tiff::decoder::DecodingResult,
    ) -> Result<image::RgbaImage, LoadError> {
        use tiff::ColorType as T;
        use tiff::decoder::DecodingResult as D;
        let bad = |what: &str| {
            LoadError::Other(format!(
                "TIFF page is {kind:?}, which is not supported past the first page ({what})"
            ))
        };
        let need = |got: usize, want: usize| -> Result<(), LoadError> {
            if got == want { Ok(()) } else { Err(bad(&format!("{got} samples for {want} pixels"))) }
        };
        let img: image::DynamicImage = match (kind, result) {
            (T::Gray(8), D::U8(b)) => {
                need(b.len(), (w * h) as usize)?;
                image::GrayImage::from_raw(w, h, b).ok_or_else(|| bad("bad size"))?.into()
            }
            (T::GrayA(8), D::U8(b)) => {
                need(b.len(), (w * h * 2) as usize)?;
                image::ImageBuffer::<image::LumaA<u8>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::RGB(8), D::U8(b)) => {
                need(b.len(), (w * h * 3) as usize)?;
                image::RgbImage::from_raw(w, h, b).ok_or_else(|| bad("bad size"))?.into()
            }
            (T::RGBA(8), D::U8(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                image::RgbaImage::from_raw(w, h, b).ok_or_else(|| bad("bad size"))?.into()
            }
            (T::Gray(16), D::U16(b)) => {
                need(b.len(), (w * h) as usize)?;
                image::ImageBuffer::<image::Luma<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::GrayA(16), D::U16(b)) => {
                need(b.len(), (w * h * 2) as usize)?;
                image::ImageBuffer::<image::LumaA<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::RGB(16), D::U16(b)) => {
                need(b.len(), (w * h * 3) as usize)?;
                image::ImageBuffer::<image::Rgb<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            (T::RGBA(16), D::U16(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                image::ImageBuffer::<image::Rgba<u16>, _>::from_raw(w, h, b)
                    .ok_or_else(|| bad("bad size"))?
                    .into()
            }
            // CMYK: the conversion belongs to `image`, so `image` does it. See
            // `cmyk_via_image` for why that means re-wrapping the page.
            (T::CMYK(8), D::U8(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                cmyk_via_image::<tiff::encoder::colortype::CMYK8>(w, h, &b)?.into()
            }
            (T::CMYK(16), D::U16(b)) => {
                need(b.len(), (w * h * 4) as usize)?;
                cmyk_via_image::<tiff::encoder::colortype::CMYK16>(w, h, &b)?.into()
            }
            // Palette, CMYKA, YCbCr, Lab and multiband: no conversion path we can
            // hand to `image`, so they stay refused. Those, and any mismatch
            // between the declared colour type and the samples that arrived.
            _ => return Err(bad("unsupported colour type")),
        };
        Ok(img.to_rgba8())
    }

    /// Convert a CMYK page by handing it to `image` through a one-page TIFF.
    ///
    /// `image` owns the CMYK→RGB maths — `(255 - c) * (1 - k/255)`, verified
    /// against `cmyk_to_rgb` in `image`'s TIFF decoder — and it is not ours to
    /// restate. An approximation here is how a scanned document comes out a
    /// slightly wrong shade of every colour, invisibly, which is the failure the
    /// original refusal of these types was protecting against.
    ///
    /// It cannot simply be pointed at the original file, because its TIFF decoder
    /// only ever reads page 0 — that is the entire reason this function reaches
    /// for the `tiff` crate in the first place. So the samples already in hand
    /// are re-wrapped in a fresh single-page TIFF and *that* is decoded. The cost
    /// is one copy of the page, on a path only reached for a colour type the
    /// plain reinterpretation cannot express.
    ///
    /// Generic over the sample type so the 8- and 16-bit cases share it; the
    /// `tiff` encoder takes flat samples either way, so nothing is repacked.
    fn cmyk_via_image<C: tiff::encoder::colortype::ColorType>(
        w: u32,
        h: u32,
        samples: &[C::Inner],
    ) -> Result<image::DynamicImage, LoadError>
    where
        [<C as tiff::encoder::colortype::ColorType>::Inner]: tiff::encoder::TiffValue,
    {
        let mut out = std::io::Cursor::new(Vec::new());
        // The encoder finishes (and writes the end-of-IFD marker) when it drops,
        // at the end of this statement, which is before `out` is read back.
        tiff::encoder::TiffEncoder::new(&mut out)
            .map_err(tiff_err)?
            .write_image::<C>(w, h, samples)
            .map_err(tiff_err)?;
        image::load_from_memory_with_format(&out.into_inner(), image::ImageFormat::Tiff)
            .map_err(|e| LoadError::Other(format!("tiff: {e}")))
    }

    let mut data = ImageData {
        width: w,
        height: h,
        rgba: to_rgba8(w, h, kind, result)?.into_raw(),
        load_time: start.elapsed(),
    };
    if data.width.max(data.height) > max_dim {
        data = downscale_image(&data, max_dim);
    }
    Ok(normalized(data))
}
