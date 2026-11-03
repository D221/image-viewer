//! Module `save`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use save::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// The picture a save would produce, as pixels, cut out of an already-decoded
/// buffer. This is what the clipboard gets.
///
/// The raster twin of [`save_transformed`]: the same edit, the same order, the
/// same pixels — but nothing written and nothing re-compressed. The lossless DCT
/// path is not available here and is not wanted: the clipboard takes pixels, so
/// there is no generation loss at all. That is a *better* answer than the save's
/// fallback for a format it cannot transform losslessly.
///
/// ## The two spaces, which is where this could have gone wrong
///
/// `oriented` is what a decode produces: **EXIF orientation applied, the user's
/// transform not**. Every `full` buffer in the app is exactly that. `crop`, on
/// the other hand, is in **stored** coordinates, because that is what TurboJPEG's
/// crop region is expressed against. So the rectangle has to be moved into the
/// buffer's space before it can be used, and getting that move backwards is the
/// [`Crop::from_display_rect`] trap: for a quarter turn it is the difference
/// between the top-left of the photo and the bottom-left, and both of them are a
/// perfectly good picture of the wrong part of it.
///
/// The direction is `to_display_rect` with an *identity user transform*, which is
/// the EXIF part alone — the forward direction, matching what
/// `crop_from_screen` computed on the way in.
///
/// ## Why crop-then-turn
///
/// It is the order [`save_transformed`] uses in both of its branches, so the
/// clipboard and the file cannot disagree, and it is the cheap one:
/// `Xform::apply_rgba` is only ever asked about the pixels inside the frame,
/// rather than all 24 megapixels of it. It is also correct in its own right —
/// `T(crop(src, R'))` is `crop(T(src), R)` when `R' = T⁻¹(R)` — which is the
/// property that makes cutting in stored space and then turning legitimate.
///
/// `None` means the crop does not fit the buffer, or the buffer disagrees with
/// its own dimensions. Neither should be reachable: the rectangle is clamped and
/// snapped to the block grid on the way in, and every decode is `normalized()`.
pub fn region_for_save(
    oriented: &ImageData,
    exif_tag: u16,
    user: Xform,
    crop: Option<Crop>,
) -> Option<ImageData> {
    let (w, h) = (oriented.width, oriented.height);
    if oriented.rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let exif = Xform::from_exif(exif_tag).unwrap_or(Xform::Identity);
    // The buffer is the stored picture with the EXIF orientation applied, so the
    // stored dimensions are its own with that axis swap undone. This is the same
    // derivation `crop_from_screen` does, and the two have to agree or the
    // rectangle names different pixels here than it does there.
    let stored = if exif.swaps_axes() { (h, w) } else { (w, h) };

    let (cw, ch, cut) = match crop {
        Some(c) => {
            let (x, y, cw, ch) = c.to_display_rect(exif, stored);
            let r = Crop::new(x, y, cw, ch)?;
            r.apply_rgba(w, h, &oriented.rgba)?
        }
        // Owned because the clipboard needs owned pixels; this is the one copy the
        // caller could not have avoided anyway.
        None => (w, h, oriented.rgba.clone()),
    };
    if user == Xform::Identity {
        return Some(ImageData { width: cw, height: ch, rgba: cut, load_time: oriented.load_time });
    }
    let (ow, oh, px) = user.apply_rgba(cw, ch, &cut)?;
    Some(ImageData { width: ow, height: oh, rgba: px, load_time: oriented.load_time })
}

/// Why a transform could not be saved, when it could not.
///
/// Deliberately has *no* `#[from] std::io::Error`: reading and writing are
/// different failures to the user, and collapsing them into one variant loses
/// which half of the operation went wrong.
#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("could not read the file: {0}")]
    Read(std::io::Error),
    #[error("decoding failed: {0}")]
    Decode(String),
    #[error("{0} would have to be re-encoded, which is lossy — this only saves losslessly")]
    Lossy(String),
    #[error("writing failed: {0}")]
    Write(std::io::Error),
    /// The destination could not be written *and* the reason is not an I/O
    /// error - an extension with no encoder behind it, or none at all.
    ///
    /// Separate from [`SaveError::Write`] because the remedy is different: an
    /// I/O failure is the disk's business and a retry may fix it, while this is
    /// the request's and no retry will ever change the answer. Both render as a
    /// refusal, because both must.
    #[error("{0}")]
    Unsupported(String),
}

/// Human name for a format, for the "cannot save this" message.
pub(crate) fn format_name(bytes: &[u8]) -> &'static str {
    if is_jpeg_bytes(bytes) {
        "JPEG"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "PNG"
    } else if is_gif_bytes(bytes) {
        "GIF"
    } else if is_tiff_bytes(bytes) {
        "TIFF"
    } else if is_bmp_bytes(bytes) {
        "BMP"
    } else if is_ico_bytes(bytes) {
        "ICO"
    } else if is_qoi_bytes(bytes) {
        "QOI"
    } else if is_jxl_bytes(bytes) {
        "JPEG XL"
    } else if is_heif_bytes(bytes) {
        "HEIF/AVIF"
    } else {
        "this format"
    }
}

/// A lossless re-encode target for `bytes`, if there is one.
///
/// Only formats that can hold the transformed pixels *without* recompression. A
/// WebP, GIF, HEIC or JXL would have to be decoded and re-encoded, which is a
/// second generation of compression — and this is a button that says "save", so
/// it must never quietly do that.
pub(crate) fn lossless_raster(bytes: &[u8]) -> Option<image::ImageFormat> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(image::ImageFormat::Png)
    } else if is_tiff_bytes(bytes) {
        Some(image::ImageFormat::Tiff)
    } else if is_bmp_bytes(bytes) {
        Some(image::ImageFormat::Bmp)
    } else {
        None
    }
}

/// Write `src` to a new file beside it, with the transform baked in, and return
/// the path written.
///
/// **The original is never touched.** This writes `photo_rotated.jpg` next to
/// `photo.jpg` (`photo_cropped.jpg` for a crop, `photo_copy.jpg` when neither
/// was asked for — the name says what the file is), or `photo_rotated2.jpg`
/// and onwards if that exists, so the edit is reversible and re-rotating
/// accumulates instead of compounding. There is no undo for an in-place
/// write, and a wrong transform is exactly the kind of mistake you notice
/// after closing the app.
///
/// JPEG goes through `tjTransform`, which operates on the DCT coefficients, so
/// the result is the same compressed data rearranged — no second generation,
/// and typically faster than decoding the file. PNG, BMP and TIFF are decoded
/// and re-encoded into their own lossless containers, which is lossless because
/// those formats have no lossy step.
///
/// The output's EXIF orientation is set to 1, because the pixels now *are* the
/// display orientation. See [`neutralize_exif_orientation`].
pub fn save_transformed(src: &Path, user: Xform, crop: Option<Crop>) -> Result<PathBuf, SaveError> {
    let bytes = std::fs::read(src).map_err(SaveError::Read)?;
    let is_jpeg = is_jpeg_bytes(&bytes);
    let exif_tag = if is_jpeg { exif_orientation(src) } else { 0 };
    let xform = display_xform(exif_tag, user);

    // Resolve the crop **once**, up front, rather than inside the JPEG branch.
    // It has to be settled before the file is named: a crop that keeps every
    // pixel is not a crop, and if the name were chosen from the caller's value
    // instead of the resolved one, a no-op crop would produce `photo_cropped.jpg`
    // for a file that was not cropped. That is the sort of thing that only shows
    // up when somebody tries it.
    let crop = match crop {
        None => None,
        Some(c) => {
            if is_jpeg {
                let mut dec = turbojpeg::Decompressor::new()
                    .map_err(|e| SaveError::Decode(format!("could not open a decoder: {e}")))?;
                let header = dec.read_header(&bytes).map_err(|e| {
                    SaveError::Decode(format!("could not read the JPEG header: {e}"))
                })?;
                let (iw, ih) = (header.width as u32, header.height as u32);
                if c.is_full(iw, ih) {
                    None
                } else {
                    c.snapped_to_mcu(
                        header.subsamp.mcu_width() as u32,
                        header.subsamp.mcu_height() as u32,
                        iw,
                        ih,
                    )
                }
            } else {
                // No block structure outside JPEG, so nothing to snap to; just
                // refuse one that falls off the image.
                c.clamped_to(u32::MAX, u32::MAX)
            }
        }
    };

    let mut out = bytes.clone();
    if is_jpeg {
        // A crop and a turn are done as **two** `tjTransform` calls, not one with
        // both fields set. That is a measured decision, not a stylistic one, and
        // the measurement is in `crop_and_turn_take_two_calls_because_one_will_not_do`
        // in `tests/rotate_save.rs`:
        //
        //  * TurboJPEG reads the crop region in the transform's **destination**
        //    space, not the source. With `rot180` or a flip, a rectangle meant for
        //    the source lands mirrored in the output — a valid crop of the wrong
        //    part of the photo.
        //  * For the four axis-swapping operations it refuses outright:
        //    "Invalid crop request", because the destination has different
        //    dimensions from the source.
        //
        // So cropping first (with the identity op, where source and destination
        // are the same space and the coordinates are unambiguous) and turning the
        // cropped result second is the only formulation that is right for all
        // eight. Both calls rearrange DCT coefficients and neither decompresses
        // to pixels, so this is still lossless — the size ratio in
        // `bench_rotate_save` is the evidence, and it holds.
        if let Some(c) = crop {
            let mut t1 = turbojpeg::Transform::op(turbojpeg::TransformOp::None);
            // Lossless transforms move whole MCU blocks, so a dimension that is
            // not a multiple of the block size leaves partial blocks that cannot
            // reach the new edge. Left in place they show as strips; `trim`
            // discards them instead, costing at most one block (16 px) off an
            // edge. It is a no-op when the image is already aligned, so there is
            // nothing to give up in the common case.
            t1.trim = true;
            t1.crop = Some(turbojpeg::TransformCrop {
                x: c.x as usize,
                y: c.y as usize,
                width: Some(c.w as usize),
                height: Some(c.h as usize),
            });
            let cropped = turbojpeg::transform(&t1, &bytes)
                .map_err(|e| SaveError::Decode(format!("lossless crop failed: {e}")))?;

            if xform == Xform::Identity {
                out = cropped.to_vec();
            } else {
                let mut t2 = turbojpeg::Transform::op(xform.as_turbojpeg());
                t2.trim = true;
                out = turbojpeg::transform(&t2, &cropped)
                    .map_err(|e| SaveError::Decode(format!("lossless transform failed: {e}")))?
                    .to_vec();
            }
        } else if xform != Xform::Identity {
            let mut t = turbojpeg::Transform::op(xform.as_turbojpeg());
            t.trim = true;
            out = turbojpeg::transform(&t, &bytes)
                .map_err(|e| SaveError::Decode(format!("lossless transform failed: {e}")))?
                .to_vec();
        }
        // The pixels are the display orientation now, so the tag must not fight them.
        neutralize_exif_orientation(&mut out);
    } else if let Some(fmt) = lossless_raster(&bytes) {
        let img = image::load_from_memory(&bytes).map_err(|e| SaveError::Decode(format!("{e}")))?;
        let (w, h) = (img.width(), img.height());
        let rgba = img.to_rgba8();
        // Crop the *stored* pixels first, then turn them — the same order
        // TurboJPEG uses, and the one that makes the result the region the user
        // framed rather than the same region of the wrong picture.
        let (cw, ch, cropped) = match crop {
            Some(c) => c
                .apply_rgba(w, h, &rgba)
                .ok_or_else(|| SaveError::Decode("crop fell outside the image".into()))?,
            None => (w, h, rgba.into_raw()),
        };
        let (ow, oh, px) = xform
            .apply_rgba(cw, ch, &cropped)
            .ok_or_else(|| SaveError::Decode("buffer did not match its dimensions".into()))?;
        let out_img = image::RgbaImage::from_raw(ow, oh, px)
            .ok_or_else(|| SaveError::Decode("transformed buffer was malformed".into()))?;
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgba8(out_img)
            .write_to(&mut std::io::Cursor::new(&mut buf), fmt)
            .map_err(|e| SaveError::Decode(format!("re-encoding failed: {e}")))?;
        out = buf;
    } else {
        return Err(SaveError::Lossy(format_name(&bytes).to_owned()));
    }

    let dest = unique_dest_named(src, save_infix(xform, crop));
    std::fs::write(&dest, &out).map_err(SaveError::Write)?;
    Ok(dest)
}

/// What a screen-size save produced, including whether it is lossless.
pub struct ScaledSave {
    /// `photo_screen.jpg` beside the original, numbered when taken.
    pub dest: PathBuf,
    /// Output dimensions: the transformed picture scaled to fit `max_long`.
    pub width: u32,
    pub height: u32,
    /// PNG, BMP and TIFF only. A JPEG screen save decodes and re-encodes at
    /// quality 85, which is a generation of compression — the caller must
    /// name that in its notice rather than calling it lossless.
    pub lossless: bool,
}

/// As [`save_transformed`], but downscaled to fit `max_long` px on the long
/// side instead of kept full-res — the "save what is on screen" to Ctrl+S's
/// "save the file". Same crop and turn, same naming rule apart from the
/// `_screen` infix, same refusals.
///
/// JPEG cannot be lossless here: there is no DCT-domain downscale to an
/// arbitrary size, so it decodes, downscales and re-encodes. That is the
/// behaviour the user asked for by asking for screen size rather than a bug,
/// and `lossless == false` is how the caller knows to say "re-encoded".
/// WebP, GIF, HEIC and JXL are still refused outright: a screen-size save of
/// those would be a *quiet* second generation, which is what the refusal is
/// for. Never upscales: a picture smaller than the display is written as-is.
pub fn save_transformed_scaled(
    src: &Path,
    user: Xform,
    crop: Option<Crop>,
    max_long: u32,
) -> Result<ScaledSave, SaveError> {
    let dest = unique_dest_named(src, "screen");
    save_transformed_to(src, user, crop, &dest, DEFAULT_JPEG_QUALITY, max_long)
}

/// The JPEG quality both re-encode paths use when the caller has no opinion.
///
/// **85, and it is stated here rather than tuned here**, because this path is
/// lossy by construction and the number only moves artefacts around. It is the
/// de-facto viewer default (JPEGView's `JPEGSaveQuality`) and it was the
/// number [`save_transformed_scaled`] already used, so naming it once is what
/// keeps the screen-save and the save-as from drifting apart.
pub const DEFAULT_JPEG_QUALITY: u8 = 85;

/// Write the current edit to `dest`, re-encoded, at a chosen quality and size.
///
/// **The destination's extension picks the format**, not the source's: a PNG
/// saved to `out.jpg` is written as JPEG, because the alternative is a file
/// whose extension lies about its contents — and a viewer that writes one is
/// worse than useless, since the *next* program to open it fails on a file that
/// looks fine in a file listing.
///
/// `max_long` of 0 means "do not resize"; anything else is a bound on the long
/// side and **never upscales**, so a 256px source asked for 1000 stays 256px.
/// Upscaling would invent pixels nobody had, and this codebase's whole premise
/// is that what you see is what the file has.
///
/// `quality` is used for JPEG only. PNG and WebP-lossless are lossless formats
/// and the number is ignored rather than being an error, so a dialog that
/// always shows the slider does not have to hide it per format.
///
/// **Never overwrites.** `Ctrl+S` picks a `_rotated` name that does not exist;
/// save-as is the path where the user named the destination, so the *user's*
/// intent is already "this exact path", and if it exists the honest thing is
/// to report it rather than silently write `_rotated2` somewhere they are not
/// looking. The caller has already asked the user to confirm.
pub fn save_transformed_to(
    src: &Path,
    user: Xform,
    crop: Option<Crop>,
    dest: &Path,
    quality: u8,
    max_long: u32,
) -> Result<ScaledSave, SaveError> {
    let bytes = std::fs::read(src).map_err(SaveError::Read)?;
    let fmt = format_for_dest(dest)?;
    let lossless = fmt != image::ImageFormat::Jpeg;
    let is_jpeg = is_jpeg_bytes(&bytes);
    // `image`'s JPEG decoder does not apply EXIF orientation, so these are
    // stored pixels and `display_xform` is the same composition the lossless
    // path uses — the crop below is in display space either way.
    let exif_tag = if is_jpeg { exif_orientation(src) } else { 0 };
    let xform = display_xform(exif_tag, user);
    let img = image::load_from_memory(&bytes).map_err(|e| SaveError::Decode(format!("{e}")))?;
    let (w, h) = (img.width(), img.height());
    let rgba = img.to_rgba8();
    let crop = match crop {
        Some(c) if !c.is_full(w, h) => Some(
            c.clamped_to(w, h)
                .ok_or_else(|| SaveError::Decode("crop fell outside the image".into()))?,
        ),
        _ => None,
    };
    let (cw, ch, cropped) = match crop {
        Some(c) => c
            .apply_rgba(w, h, &rgba.into_raw())
            .ok_or_else(|| SaveError::Decode("crop fell outside the image".into()))?,
        None => (w, h, rgba.into_raw()),
    };
    let (ow, oh, px) = xform
        .apply_rgba(cw, ch, &cropped)
        .ok_or_else(|| SaveError::Decode("buffer did not match its dimensions".into()))?;
    // **`max_long == 0` means no resize at all**, and that is not the same as a
    // resize to 1px: `downscale_image` treats its argument as a target, so
    // `max_long.max(1)` on a 0 would quietly shrink every save-as to a
    // thumbnail. The scale is therefore computed here rather than handed over,
    // so "leave it alone" is a value the pipeline can actually express.
    let scaled = if max_long == 0 || ow.max(oh) <= max_long {
        ImageData { width: ow, height: oh, rgba: px, load_time: std::time::Duration::ZERO }
    } else {
        downscale_image(
            &ImageData { width: ow, height: oh, rgba: px, load_time: std::time::Duration::ZERO },
            max_long,
        )
    };
    let (sw, sh) = (scaled.width, scaled.height);
    let mut buf = Vec::new();
    if fmt == image::ImageFormat::Jpeg {
        // JPEG has no alpha: drop it the same way every 32→24-bit save path
        // here does, rather than asking the encoder to guess.
        let rgb: Vec<u8> =
            scaled.rgba.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]).collect();
        let mut cursor = std::io::Cursor::new(&mut buf);
        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut cursor, quality);
        enc.encode(&rgb, sw, sh, image::ExtendedColorType::Rgb8)
            .map_err(|e| SaveError::Decode(format!("re-encoding failed: {e}")))?;
    } else {
        let out_img = image::RgbaImage::from_raw(sw, sh, scaled.rgba)
            .ok_or_else(|| SaveError::Decode("downscaled buffer was malformed".into()))?;
        let mut cursor = std::io::Cursor::new(&mut buf);
        // **`quality` is deliberately not passed here, and that is not an
        // oversight.** `image` 0.25 has one JPEG encoder with a quality knob
        // and no lossy encoder for anything else, so a WebP "quality 40" would
        // be a lossless WebP wearing a label that says otherwise — and the
        // `lossless` flag below would then be the only honest thing in the
        // result. Silently honouring the number on one format and ignoring it
        // on the others, without saying which, is worse: the user sets 40 for
        // file size and gets the opposite, with nothing to tell them.
        //
        // So PNG, BMP and TIFF are genuinely lossless, WebP is lossless too,
        // and the quality slider is JPEG's. This is the "a setting that does
        // nothing on most formats" problem, and the honest fix is for the
        // dialog to say so rather than for the encoder to pretend.
        image::DynamicImage::ImageRgba8(out_img)
            .write_to(&mut cursor, fmt)
            .map_err(|e| SaveError::Decode(format!("re-encoding failed: {e}")))?;
    }
    // EXIF orientation is baked into the pixels now, so carry the tag reset
    // the lossless path does — for JPEG only, where a tag may exist. **Only when
    // the destination is a JPEG**: writing an orientation tag into a PNG that
    // has no Exif block to begin with would be a corrupt file, and the old code
    // got this right only because the destination always matched the source.
    if is_jpeg && fmt == image::ImageFormat::Jpeg {
        neutralize_exif_orientation(&mut buf);
    }
    std::fs::write(dest, &buf).map_err(SaveError::Write)?;
    Ok(ScaledSave { dest: dest.to_path_buf(), width: sw, height: sh, lossless })
}

/// The output format, decided by the destination's extension.
///
/// **An extension we cannot encode is an error, not a guess.** Falling back to
/// PNG would write correct pixels under a name that says otherwise, and
/// guessing JPEG would do the same for a `.tga`; either way the user finds out
/// from another program, not from here.
///
/// WebP is deliberately encoded as **lossless**: `image` 0.25 has no lossy
/// WebP encoder, and a "lossy" WebP that is secretly lossless would make the
/// `lossless` flag on the result a lie in the one direction that costs the user
/// nothing (a bigger file) rather than the one that costs them a generation of
/// compression.
pub(crate) fn format_for_dest(dest: &Path) -> Result<image::ImageFormat, SaveError> {
    let ext = dest.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => Ok(image::ImageFormat::Jpeg),
        "png" => Ok(image::ImageFormat::Png),
        "webp" => Ok(image::ImageFormat::WebP),
        "bmp" => Ok(image::ImageFormat::Bmp),
        "tif" | "tiff" => Ok(image::ImageFormat::Tiff),
        "" => Err(SaveError::Unsupported(
            "the destination has no extension, so there is no format to write".into(),
        )),
        other => Err(SaveError::Unsupported(format!(
            "cannot write .{other} - choose .jpg, .png, .webp, .bmp or .tiff"
        ))),
    }
}

/// The infix a saved copy is named for: what the file **is**, from the resolved
/// values rather than the caller's.
///
/// `_cropped` if a crop survived, else `_rotated` if the composed transform is
/// not the identity, else `_copy`.
///
/// **Named for what happened, so an unedited save is a `_copy` and never a
/// `_rotated`.** This was a reported bug and the sharpest version of "read the
/// value at the wrong level": the save-as panel picked its infix from the
/// *user's* edit (`crop.is_some() ? "cropped" : "rotated"`), which cannot
/// distinguish "turned" from "untouched", so an unedited image was named
/// `_rotated` while `Ctrl+S` — running the identical rule through
/// [`save_transformed`] — named the very same picture `_copy`.
///
/// **Two things must be composed before this is asked, and both callers do it.**
/// A crop that covers the whole picture is not a crop (the save path resolves
/// that to `None` first), and `xform` must be the *composed* transform from
/// [`display_xform`], which folds in the EXIF tag — so a phone photo stored
/// sideways is `_rotated` on its own, and "Identity" means nothing turned
/// *whatever was asked for*. Passing a raw `Xform` here is the mistake, and it
/// is the one that produced the report.
///
/// It is a function rather than inline code because two features now name files
/// — `Ctrl+S` and save-as — and a rule duplicated between them is a rule that
/// will answer differently to the same question.
pub fn save_infix(xform: Xform, crop: Option<Crop>) -> &'static str {
    if crop.is_some() {
        "cropped"
    } else if xform != Xform::Identity {
        "rotated"
    } else {
        "copy"
    }
}

/// `photo.jpg` -> `photo_rotated.jpg`, or `photo_rotated2.jpg`, `...3` and so on
/// if that already exists. Never overwrites.
pub fn unique_dest(src: &Path) -> PathBuf {
    unique_dest_named(src, "rotated")
}

/// As [`unique_dest`], with the infix chosen by the caller.
///
/// `_rotated` or `_cropped`, depending on whether the save actually cropped. A
/// crop turns the picture too, so `_rotated` would be true and useless; `_cropped`
/// is the thing the user actually did, and the confirmation line names the
/// transform as well.
pub fn unique_dest_named(src: &Path, infix: &str) -> PathBuf {
    let parent = src.parent().unwrap_or_else(|| Path::new("."));
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned());
    let ext = src.extension().map(|s| s.to_string_lossy().into_owned());
    let (Some(stem), Some(ext)) = (stem, ext) else {
        return numbered_sibling(src, parent);
    };
    for n in 1..1000u32 {
        let name = if n == 1 {
            format!("{stem}_{infix}.{ext}")
        } else {
            format!("{stem}_{infix}{n}.{ext}")
        };
        let cand = parent.join(name);
        if !cand.exists() {
            return cand;
        }
    }
    numbered_sibling(src, parent)
}

pub(crate) fn numbered_sibling(src: &Path, parent: &Path) -> PathBuf {
    let base = src.file_name().unwrap_or_default().to_string_lossy().into_owned();
    for n in 1..1000u32 {
        let cand = parent.join(format!("{base}_rotated{n}.bin"));
        if !cand.exists() {
            return cand;
        }
    }
    parent.join(format!("{base}_rotated.bin"))
}

/// Rewrite a JPEG's EXIF after a lossless rotation: set the orientation tag to 1
/// and unlink the embedded thumbnail.
///
/// The pixels are now already in display orientation, so any orientation tag
/// left in place gets applied *on top* of them — the classic double-rotate,
/// and the sort of thing that is invisible in this viewer because it applies the
/// tag itself and so cancels out, and obvious everywhere else.
///
/// Two edits, both of which only overwrite bytes that already exist:
///  * the orientation tag's value becomes 1, and
///  * IFD0's next-IFD pointer is zeroed, so the thumbnail in IFD1 stops being
///    reachable.
///
/// Leaving IFD1 in place but unreferenced is deliberate. Removing it properly
/// means reserialising the whole APP1 segment and every offset inside it, and a
/// malformed EXIF block is worse than a thumbnail nothing points at. Conformant
/// readers follow the pointer, and both Explorer and the browsers do.
///
/// Returns what was changed, for the caller to report. `false` with no EXIF at
/// all is not a failure: a file with no orientation tag is already correct once
/// the pixels are rotated. The buffer is never grown or shrunk, so no offset
/// inside the segment can go stale — and every offset is bounds-checked against
/// the segment's end, exactly as the reader is, because these bytes come from
/// the file and are about to be handed to other programs.
pub fn neutralize_exif_orientation(jpeg: &mut [u8]) -> bool {
    /// Read a `u16` at `at`, inside `base..end`, in the given byte order.
    fn rd16(b: &[u8], at: usize, end: usize, le: bool) -> Option<u16> {
        if at.checked_add(2)? > end || end > b.len() {
            return None;
        }
        let v = [b[at], b[at + 1]];
        Some(if le { u16::from_le_bytes(v) } else { u16::from_be_bytes(v) })
    }
    fn wr16(b: &mut [u8], at: usize, v: u16, le: bool) {
        let v = if le { v.to_le_bytes() } else { v.to_be_bytes() };
        b[at] = v[0];
        b[at + 1] = v[1];
    }
    fn rd32(b: &[u8], at: usize, end: usize, le: bool) -> Option<u32> {
        if at.checked_add(4)? > end || end > b.len() {
            return None;
        }
        let v = [b[at], b[at + 1], b[at + 2], b[at + 3]];
        Some(if le { u32::from_le_bytes(v) } else { u32::from_be_bytes(v) })
    }
    fn wr32(b: &mut [u8], at: usize, v: u32, le: bool) {
        let v = if le { v.to_le_bytes() } else { v.to_be_bytes() };
        b[at..at + 4].copy_from_slice(&v);
    }

    let Some((base, end)) = exif_tiff_span(jpeg) else {
        return false;
    };
    if end > jpeg.len() || base + 4 > end {
        return false;
    }
    let le = match &jpeg[base..base + 2] {
        [0x49, 0x49] => true,
        [0x4D, 0x4D] => false,
        _ => return false,
    };

    let Some(off) = rd32(jpeg, base + 4, end, le) else {
        return false;
    };
    let ifd0 = match base.checked_add(off as usize) {
        Some(v) if v + 2 <= end => v,
        _ => return false,
    };
    let Some(count) = rd16(jpeg, ifd0, end, le) else {
        return false;
    };
    // Capped exactly as the reader caps it: a corrupt count must not make us
    // walk attacker-controlled offsets across the whole buffer.
    let count = usize::from(count.min(64));

    // Locate the orientation entry's value field, and the next-IFD pointer.
    let mut orientation_at = None;
    for i in 0..count {
        let Some(e) = ifd0.checked_add(2 + i * 12) else {
            break;
        };
        if e + 12 > end {
            break;
        }
        match rd16(jpeg, e, end, le) {
            Some(EXIF_TAG_ORIENTATION) => {
                orientation_at = Some(e + 8);
                break;
            }
            _ => {}
        }
    }
    // The next-IFD pointer sits after the entry table.
    let next_at = ifd0 + 2 + count * 12;

    let mut changed = false;
    if let Some(at) = orientation_at {
        if at + 2 <= end {
            wr16(jpeg, at, 1, le);
            changed = true;
        }
    }
    if next_at + 4 <= end {
        // Only bother if it actually pointed somewhere.
        if rd32(jpeg, next_at, end, le).is_some_and(|v| v != 0) {
            wr32(jpeg, next_at, 0, le);
            changed = true;
        }
    }
    changed
}

/// Tag 0x0112, the orientation.
pub(crate) const EXIF_TAG_ORIENTATION: u16 = 0x0112;
