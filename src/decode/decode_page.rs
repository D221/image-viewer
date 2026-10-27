//! Module `decode_page`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use decode_page::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Load a preview in one pass, decoding the file only once.
///
/// * Large JPEGs get the cheap DCT-scaled preview; the full decode happens
///   later on dwell, as before.
/// * Everything else (PNG etc. has no reduced-decode shortcut, so preview and
///   full cost the same) is decoded once at full resolution: the result is
///   returned both as the full image and downscaled as the preview.
pub fn load_image_preview_full(
    path: &Path,
    preview_max_dim: u32,
) -> Result<PreviewAndFull, LoadError> {
    load_page_preview_full(path, preview_max_dim, 0)
}

/// [`load_image_preview_full`] for a specific page of a multi-page file.
///
/// `page` is 0-based and only TIFF has more than one; every other format ignores
/// it, because a GIF's "pages" are frames and are handled by the animation path
/// above. Page 0 goes through exactly the same code it always did — the existing
/// single-page function is this one with a zero — so nothing about how the common
/// case decodes has changed.
///
/// **Every decode the app does goes through here**, which is why the wide-gamut
/// conversion is applied by the wrapper rather than at the five places inside
/// `decode_page` that return: preview, full, the DCT path, the embedded EXIF
/// thumbnail, and animations all leave through one of them, and one that forgets is
/// exactly the bug this shape exists to make impossible. Tiles are cut from the
/// converted `full`, so they come along for free, and so does the clipboard.
pub fn load_page_preview_full(
    path: &Path,
    preview_max_dim: u32,
    page: u32,
) -> Result<PreviewAndFull, LoadError> {
    let mut out = decode_page(path, preview_max_dim, page, Purpose::View)?;
    apply_wide_gamut_conversion(&mut out);
    Ok(out)
}

/// The picture one grid cell shows: at most `target` on the long side, and never
/// a full-resolution image.
///
/// A cell is one picture at a fixed small size, so unlike the main view it gains
/// nothing from the full image — and the wide-gamut conversion is applied by the
/// same wrapper that applies it everywhere else, because a cell that converted its
/// neighbour differently from the image it opens would be its own bug.
///
/// **The downscale is here rather than inside `decode_page` on purpose.** It is the
/// one step that has to happen after *every* one of that function's return points,
/// and the same argument [`load_page_preview_full`] makes about the conversion. It
/// is also not optional: `decode_jpeg_scaled` can only pick 1/8, 1/4, 1/2 or 1, so
/// the smallest reduction that still covers a 256 px cell is 2100 px for a 17000 px
/// scan — 6 MB of texture for a 150 pt cell, and 44x what was asked for. The main
/// view deliberately does not do this: a preview is drawn at whatever density the
/// display can use, and a cell is not.
///
/// Ordered before the conversion on purpose: 166 MP/s over 2100x700 is 9 ms, over
/// 256x84 it is 0.2 ms.
pub fn load_thumbnail(path: &Path, target: u32) -> Result<ImageData, LoadError> {
    let mut out = decode_page(path, target, 0, Purpose::Thumb)?;
    if out.preview.width.max(out.preview.height) > target {
        out.preview = downscale_image(&out.preview, target);
    }
    apply_wide_gamut_conversion(&mut out);
    Ok(out.preview)
}

pub(crate) fn decode_page(
    path: &Path,
    preview_max_dim: u32,
    page: u32,
    purpose: Purpose,
) -> Result<PreviewAndFull, LoadError> {
    let start = std::time::Instant::now();
    // **The store, before the file is read.** A hit must not pay for the read:
    // a 28 MB file is 5-60 ms of I/O before a single pixel, and the whole point
    // of the store is that showing the picture again costs neither the read nor
    // the decode. The read is also where the entry's *usefulness* is decided —
    // the header carries the stamp and the target it was decoded for, so a
    // stale entry is refused here, before its payload is even touched.
    if purpose == Purpose::View && preview_store_enabled() {
        match read_cached_preview(&preview_store_root(), path, page, preview_max_dim) {
            Some(hit) => {
                perf::count(perf::C_PREVIEW_STORE_HIT, 1);
                return Ok(hit);
            }
            None => {
                perf::count(perf::C_PREVIEW_STORE_MISS, 1);
            }
        }
    }
    let bytes = std::fs::read(path)?;
    let max_dim = preview_max_dim.min(MAX_TEXTURE_DIM);
    let jpeg = is_jpeg_bytes(&bytes);
    // EXIF orientation: camera pixels are stored unrotated; the tag says how
    // to display them. Rotate after decode so preview and full agree, and fix
    // the reported source dimensions accordingly.
    let tag = if jpeg { exif_orientation(path) } else { 0 };
    // The block grid a lossless JPEG crop must align to. Read here, from the
    // bytes already in hand, rather than re-reading the file later: a crop is
    // snapped to this, and the UI needs it on the worker thread where the read
    // is free. `(1, 1)` for anything that is not a JPEG, so callers never have to
    // special-case a non-JPEG.
    let crop_grid: (u32, u32) = if jpeg {
        turbojpeg::Decompressor::new()
            .ok()
            .and_then(|mut d| d.read_header(&bytes).ok())
            .map(|h| (h.subsamp.mcu_width() as u32, h.subsamp.mcu_height() as u32))
            .unwrap_or((1, 1))
    } else {
        (1, 1)
    };
    let stored = image_dims_bytes(&bytes);
    let orig = stored.map(|(w, h)| if orientation_swaps_dims(tag) { (h, w) } else { (w, h) });
    // Colour is classified **here**, from the bytes the decode already has in
    // hand, and travels with every branch below. This is the one whole-file read
    // per job left (`color_info(path)` re-reads the file where it runs), so going
    // through the bytes is what keeps the colour line and the wide-gamut gate
    // free of their own reads.
    let colour = color_info_from_bytes(&bytes, jpeg, bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    // A page after the first, of a multi-page TIFF. Ahead of everything else
    // because no other format has pages, and a TIFF is not animated, so the
    // animation path below cannot claim it.
    //
    // `orig` is deliberately left as the *file's* first-page dimensions: it is
    // what the window fits the image into and what the zoom maths uses, and
    // pages of one document are near enough always the same size. It is
    // recomputed from the decoded page below, because a document whose pages
    // differ in size is legal and showing page 3 in page 1's frame is wrong.
    if page > 0 && is_tiff_bytes(&bytes) {
        let data = decode_tiff_page(&bytes, page, max_dim, start)?;
        let dims = (data.width, data.height);
        return Ok(PreviewAndFull {
            preview: data,
            full: None,
            anim: None,
            orig: Some(dims),
            below_target: false,
            exif_tag: tag,
            crop_grid,
            colour,
        });
    }

    // An SVG: vector, so it renders at any size — there is no source-size
    // decode, no `full` tier, and no `below_target`. The preview *is* the
    // decode. The viewport (width/height attributes) gives the natural aspect
    // ratio; missing or zero means the target is used square.
    //
    // SVG wants an explicit shape rather than optimistic dimensions: `pairable`
    // treats unknown dims as "probably fits", which would collapse a manga turn
    // when a wide SVG's shape is learned late. `decode_svg` returns the
    // viewport it actually rendered, so the caller knows the truth.
    if is_svg_bytes(&bytes) {
        if let Ok(data) = decode_svg(&bytes, max_dim, start) {
            let dims = (data.width, data.height);
            return Ok(PreviewAndFull {
                preview: data,
                full: None,
                anim: None,
                orig: Some(dims),
                below_target: true,
                exif_tag: tag,
                crop_grid,
                colour,
            });
        }
    }

    // An animation: decode the whole loop once; no second full decode, no tiles.
    //
    // GIF, APNG and animated WebP all land here, and all three used to be handled
    // differently — in fact APNG and animated WebP were not handled at all, and
    // showed their first frame forever, silently. Whether it is an animation at
    // all is asked first, because a still image of an animated format must not be
    // decoded twice: once as a "one frame animation" and again by the normal path
    // below.
    if let Some(fmt) = animated_format(&bytes) {
        if purpose == Purpose::Thumb {
            // A cell is one picture. Decoding the loop to show frame 0 is the same
            // mistake as decoding full-res for a 150 px cell, and worse: an APNG or
            // animated WebP is capped at 240 frames and 256 MB, so a grid of cells
            // could decode hundreds of megabytes to display a few kilobytes.
            //
            // `is_animated` is deliberately *not* asked first. It costs two frame
            // decodes to answer a question a cell does not have: whether the file
            // has one frame or many, the first frame is the same picture either
            // way, and the fall-through below would decode it a second time.
            if let Some(data) = first_frame(&bytes, max_dim, start, fmt) {
                let dims = (data.width, data.height);
                return Ok(PreviewAndFull {
                    preview: data,
                    full: None,
                    anim: None,
                    orig: Some(dims),
                    below_target: true,
                    exif_tag: tag,
                    crop_grid,
                    colour,
                });
            }
        } else if is_animated(&bytes, fmt) {
            if let Ok(anim) = load_animation(&bytes, PREVIEW_TEXTURE_DIM, start, fmt) {
                if anim.frames.len() > 1 {
                    let dims = (anim.frames[0].width, anim.frames[0].height);
                    return Ok(PreviewAndFull {
                        preview: anim.frames[0].clone(),
                        full: None,
                        anim: Some(anim),
                        orig: Some(dims),
                        below_target: false,
                        exif_tag: tag,
                        crop_grid,
                        colour,
                    });
                }
            }
        }
        // A still image of an animated format, or one that would not decode:
        // fall through to the normal path, which handles it once.
    }

    // A camera's embedded EXIF thumbnail, for grid-sized requests. Ahead of the
    // DCT path on purpose: that one still decompresses every coefficient, so it
    // saves RAM but not much time, whereas this reads a few KB that were already
    // encoded. No full decode happens, so `full` stays `None` and the caller
    // re-decodes if the user actually opens the image.
    if jpeg {
        if let Some(preview) = decode_embedded_thumbnail(&bytes, max_dim, stored, tag, start) {
            return Ok(PreviewAndFull {
                preview,
                full: None,
                anim: None,
                orig,
                below_target: true,
                exif_tag: tag,
                crop_grid,
                colour,
            });
        }
    }

    let full_is_cheap = orig.map_or(true, |(w, h)| w as u64 * h as u64 <= FAST_FULL_MAX_PIXELS);

    if jpeg && !full_is_cheap {
        if let Ok((w, h, rgba)) = decode_jpeg_scaled(&bytes, max_dim) {
            let mut preview = normalized(ImageData {
                width: w,
                height: h,
                rgba,
                load_time: start.elapsed(),
            });
            apply_exif_orientation(&mut preview, tag);
            // The store, for the next time this picture is wanted. The pixels
            // handed over are the **source's**: the wide-gamut conversion is
            // applied by the wrapper around this function, so an entry written
            // here is converted on the way out exactly as a decode would be.
            maybe_store_preview(
                path,
                page,
                &preview,
                orig,
                None,
                None,
                PreviewMeta {
                    decoded_for: preview_max_dim,
                    orig: orig.unwrap_or((w, h)),
                    exif_tag: tag,
                    crop_grid,
                    colour: colour.clone(),
                },
            );
            return Ok(PreviewAndFull {
                preview,
                full: None,
                anim: None,
                orig,
                // DCT preview never reaches source density for these files.
                below_target: true,
                exif_tag: tag,
                crop_grid,
                colour,
            });
        }
        eprintln!("turbojpeg failed, falling back to image crate");
    }

    // One full decode serves as both the full-res image and the preview.
    let (preview, full, below_target) = match orig {
        // A grid cell: decode at the cell's size and stop. Ahead of the
        // `MAX_FULL_PIXELS` arm below because it subsumes it — that arm decodes at
        // `max_dim` too — and because a 256 px request has no use for the
        // full-resolution `ImageData` that used to be built, returned and sent
        // across a channel for every PNG, HEIC and JXL in a folder.
        _ if purpose == Purpose::Thumb => {
            let mut preview = normalized(decode_bytes(&bytes, max_dim, start)?);
            apply_exif_orientation(&mut preview, tag);
            // A cell is always below source density by construction, and nothing
            // reads this for a cell; `true` says so rather than implying a
            // re-decode is worth issuing.
            (preview, None, true)
        }
        // Too big to keep full-res: decode only a preview-sized copy, like the
        // old preview path did.
        Some((w, h)) if w as u64 * h as u64 > MAX_FULL_PIXELS => {
            let mut preview = normalized(decode_bytes(&bytes, max_dim, start)?);
            apply_exif_orientation(&mut preview, tag);
            maybe_store_preview(
                path,
                page,
                &preview,
                orig,
                None,
                None,
                PreviewMeta {
                    decoded_for: preview_max_dim,
                    orig: orig.unwrap_or((preview.width, preview.height)),
                    exif_tag: tag,
                    crop_grid,
                    colour: colour.clone(),
                },
            );
            (preview, None, false)
        }
        _ => {
            let mut full = normalized(decode_bytes(&bytes, MAX_TEXTURE_DIM, start)?);
            apply_exif_orientation(&mut full, tag);
            // Show the full-res image right away (capped at the display
            // target) — a tiny preview texture would look blurry at
            // fit-to-window even though the full data is already in memory.
            let preview = if full.width.max(full.height) <= max_dim {
                full.clone()
            } else {
                downscale_image(&full, max_dim)
            };
            // Source bigger than the target: texture is below source density.
            let below = full.width.max(full.height) > max_dim;
            (preview, Some(full), below)
        }
    };
    Ok(PreviewAndFull {
        preview,
        full,
        anim: None,
        orig,
        below_target,
        exif_tag: tag,
        crop_grid,
        colour,
    })
}

/// Decode an SVG at `max_dim` on the long side.
///
/// SVG is vector: it renders at any size, so there is no source-size decode,
/// no `full` tier, and no `below_target` — the preview *is* the decode. The
/// viewport (width/height attributes) gives the natural aspect ratio; missing
/// or zero means the target is used square.
pub fn decode_svg(bytes: &[u8], max_dim: u32, start: std::time::Instant) -> Result<ImageData, LoadError> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &opt)
        .map_err(|e| LoadError::Other(format!("svg: {e}")))?;
    let size = tree.size();
    let (render_w, render_h) = if size.width() > 0.0 && size.height() > 0.0 {
        let scale = (max_dim as f32 / size.width().max(size.height()) as f32).min(1.0);
        let w = (size.width() * scale).round().max(1.0) as u32;
        let h = (size.height() * scale).round().max(1.0) as u32;
        (w, h)
    } else {
        (max_dim, max_dim)
    };
    let mut pixmap = resvg::tiny_skia::Pixmap::new(render_w, render_h)
        .ok_or_else(|| LoadError::Other("svg: could not allocate pixmap".to_string()))?;
    let scale_x = render_w as f32 / size.width().max(1.0);
    let scale_y = render_h as f32 / size.height().max(1.0);
    let transform = resvg::tiny_skia::Transform::from_scale(scale_x, scale_y);
    let mut pixmap_mut = pixmap.as_mut();
    resvg::render(&tree, transform, &mut pixmap_mut);
    let rgba = pixmap.data().to_vec();
    Ok(normalized(ImageData {
        width: render_w,
        height: render_h,
        rgba,
        load_time: start.elapsed(),
    }))
}

/// True if the bytes look like a JPEG XL image (naked codestream or container).
pub fn is_jxl_bytes(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xFF, 0x0A])
        || bytes.starts_with(&[
            0x00, 0x00, 0x00, 0x0C, b'J', b'X', b'L', b' ', 0x0D, 0x0A, 0x87, 0x0A,
        ])
}

/// The 4CC brand of an ISOBMFF file (HEIC/HEIF/AVIF family), if any.
pub(crate) fn isobmff_brand(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        Some(&bytes[8..12])
    } else {
        None
    }
}

/// True if the bytes look like a HEIF-family file (HEIC/HEIF/AVIF).
pub fn is_heif_bytes(bytes: &[u8]) -> bool {
    isobmff_brand(bytes).is_some_and(|brand| {
        matches!(
            brand,
            b"avif" | b"avis" | b"heic" | b"heix" | b"mif1" | b"msf1" | b"heim" | b"heis"
                | b"hevc" | b"hevx" | b"av01"
        )
    })
}

/// Decode JPEG XL bytes via jxl-oxide (pure Rust), routed through the image crate.
pub(crate) fn decode_jxl(bytes: &[u8]) -> Result<DynamicImage, LoadError> {
    use jxl_oxide::integration::JxlDecoder;
    let dec = JxlDecoder::new(std::io::Cursor::new(bytes))?;
    Ok(DynamicImage::from_decoder(dec)?)
}

/// Decode HEIC/HEIF/AVIF bytes via the pure-Rust `heic` crate (AV1 via rav1d
/// for AVIF files). Container transforms (clap/irot/imir) are applied.
pub(crate) fn decode_heif(bytes: &[u8]) -> Result<DynamicImage, LoadError> {
    let mut limits = heic::Limits::default();
    limits.max_width = Some(MAX_TEXTURE_DIM as u64);
    limits.max_height = Some(MAX_TEXTURE_DIM as u64);
    limits.max_pixels = Some(MAX_FULL_PIXELS);
    let out = heic::DecoderConfig::new()
        .decode_request(bytes)
        .with_output_layout(heic::PixelLayout::Rgba8)
        .with_limits(&limits)
        .decode()
        .map_err(|e| LoadError::Other(e.to_string()))?;
    let img = RgbaImage::from_raw(out.width, out.height, out.data).ok_or_else(|| {
        LoadError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "HEIF decode returned a mismatched pixel buffer",
        ))
    })?;
    Ok(DynamicImage::ImageRgba8(img))
}

/// Resize a decoded DynamicImage to `max_dim` and convert to ImageData.
pub(crate) fn dynamic_to_image_data(
    img: DynamicImage,
    max_dim: u32,
    start: std::time::Instant,
) -> ImageData {
    let (w, h) = img.dimensions();
    let mut data = ImageData {
        width: w,
        height: h,
        rgba: img.to_rgba8().into_raw(),
        load_time: start.elapsed(),
    };
    if w.max(h) > max_dim {
        data = downscale_image(&data, max_dim);
    }
    data
}

/// What undecodable bytes actually are, for the failure message — used only
/// when no decoder was even attempted, so a corrupt file of a supported
/// kind keeps the decode error instead of gaining a wrong explanation.
///
/// Extensions lie routinely (an MP4 renamed `.gif`), and the guesser's
/// "format could not be determined" answers neither "what is it" nor "can
/// I do anything about it". Naming the container answers both: nothing here
/// will ever decode it, and the remedy is a different program.
pub(crate) fn unsupported_error(bytes: &[u8]) -> LoadError {
    if bytes.is_empty() {
        return LoadError::Other("unsupported file: empty (0 bytes)".into());
    }
    if let Some(brand) = isobmff_brand(bytes) {
        // `is_heif_bytes` brands never reach here — they decode above — so a
        // remaining `ftyp` is something else in the same container family,
        // overwhelmingly video or audio.
        let name = match brand {
            b"isom" | b"iso2" | b"iso3" | b"iso4" | b"iso5" | b"iso6" | b"mp41"
            | b"mp42" | b"mp4v" | b"avc1" => "MP4 video",
            b"M4V " | b"M4VH" => "M4V video",
            b"M4A " => "M4A audio",
            b"qt  " => "QuickTime video",
            b"3gp4" | b"3gp5" | b"3gp6" | b"3g2a" => "3GP video",
            _ => "",
        };
        if name.is_empty() {
            let brand = String::from_utf8_lossy(brand);
            let brand: String = brand
                .chars()
                .map(|c| if c.is_ascii_graphic() { c } else { '?' })
                .collect();
            return LoadError::Other(format!(
                "unsupported file: ISO-BMFF container (brand '{brand}'), not a decodable image"
            ));
        }
        return LoadError::Other(format!(
            "unsupported file: {name} (ISO-BMFF brand '{}'), not a decodable image",
            String::from_utf8_lossy(brand)
        ));
    }
    // RIFF with a non-WEBP form: WAV and AVI start the same way, which is
    // why `looks_like_supported_image` demands the form type — and why the
    // message can name exactly which one it is.
    if bytes.starts_with(b"RIFF") && bytes.len() >= 12 {
        let form = String::from_utf8_lossy(&bytes[8..12]);
        let name = match &bytes[8..12] {
            b"WAVE" => "WAV audio",
            b"AVI " => "AVI video",
            _ => "",
        };
        if !name.is_empty() {
            return LoadError::Other(format!("unsupported file: {name}, not a decodable image"));
        }
        return LoadError::Other(format!(
            "unsupported file: RIFF container (form '{form}'), not a decodable image"
        ));
    }
    // Short ASCII magics: vector, document and archive containers. Checked
    // after the binary ones because none of them can overlap.
    let texty: &[(&[u8], &str)] = &[
        (b"<svg", "SVG vector image"),
        (b"%PDF", "PDF document"),
        (b"PK\x03\x04", "ZIP archive"),
        (b"Rar!", "RAR archive"),
        (b"7z\xBC\xAF\x27\x1C", "7z archive"),
        (b"ID3", "MP3 audio"),
        (b"OggS", "Ogg audio/video"),
        (b"MZ", "Windows executable"),
    ];
    // Leading whitespace/BOM is skipped: an SVG with an XML prolog is still
    // an SVG, and the alternative is calling pretty-printed XML unrecognized.
    let mut head = bytes;
    while head.starts_with(b"\xEF\xBB\xBF") {
        head = &head[3..];
    }
    while !head.is_empty() && matches!(head[0], b' ' | b'\t' | b'\r' | b'\n') {
        head = &head[1..];
    }
    for (magic, name) in texty {
        if head.starts_with(magic) {
            return LoadError::Other(format!("unsupported file: {name}, not a decodable image"));
        }
    }
    LoadError::Other(format!(
        "unsupported file type (starts with {:02X?})",
        &bytes[..bytes.len().min(8)]
    ))
}

/// Decode bytes to RGBA, capped at `max_dim` on the long side. JPEGs use the
/// turbojpeg DCT-scaling fast path; everything else goes through the `image` crate
/// or the dedicated pure-Rust decoders for formats it can't handle.
pub(crate) fn decode_bytes(bytes: &[u8], max_dim: u32, start: std::time::Instant) -> Result<ImageData, LoadError> {
    if is_jpeg_bytes(bytes) {
        if let Ok((w, h, rgba)) = decode_jpeg_scaled(bytes, max_dim) {
            return Ok(ImageData {
                width: w,
                height: h,
                rgba,
                load_time: start.elapsed(),
            });
        }
        eprintln!("turbojpeg failed, falling back to image crate");
    }

    if is_jxl_bytes(bytes) {
        match decode_jxl(bytes) {
            Ok(img) => return Ok(dynamic_to_image_data(img, max_dim, start)),
            Err(e) => eprintln!("JXL decode failed: {e}"),
        }
    }

    if is_heif_bytes(bytes) {
        match decode_heif(bytes) {
            Ok(img) => return Ok(dynamic_to_image_data(img, max_dim, start)),
            Err(e) => eprintln!("HEIF/AVIF decode failed: {e}"),
        }
    }

    let img = match image::load_from_memory(bytes) {
        Ok(img) => img,
        // Bytes no decoder claims: say what they are instead of parroting the
        // guesser's "could not be determined". Gated on the magic, so a
        // corrupt file of a supported kind keeps the decode error — which is
        // the honest message there — rather than gaining a wrong explanation.
        Err(e) => {
            if looks_like_supported_image(bytes) || is_jxl_bytes(bytes) || is_heif_bytes(bytes) {
                return Err(e.into());
            }
            return Err(unsupported_error(bytes));
        }
    };
    Ok(dynamic_to_image_data(img, max_dim, start))
}

/// Load and decode an image file (format detected from content, not extension).
/// JPEGs are rotated according to their EXIF orientation tag.
pub fn load_image(path: &Path, max_dim: u32) -> Result<ImageData, LoadError> {
    let start = std::time::Instant::now();
    let bytes = std::fs::read(path)?;
    let mut data = normalized(decode_bytes(&bytes, max_dim.min(MAX_TEXTURE_DIM), start)?);
    if is_jpeg_bytes(&bytes) {
        apply_exif_orientation(&mut data, exif_orientation(path));
    }
    Ok(data)
}

