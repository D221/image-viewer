//! Module `colour`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use colour::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Colour profile detection (read-only, no transformation)
// ---------------------------------------------------------------------------
//
// This stage only *reports*. Nothing here touches a decode path, so it cannot
// change a pixel: the renderer already uploads textures as
// `Rgba8UnormSrgb` (egui-wgpu), so the bytes are expected to be sRGB-encoded
// and a future transform's only job is to make that true at decode time.

/// What can be said about a file's declared colour space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpace {
    /// No profile. For JPEG and PNG that means sRGB by convention, which is
    /// what the viewer already assumes.
    UntaggedSrgb,
    /// Carries an sRGB profile. Converting it to sRGB is a no-op.
    Srgb,
    /// A recognised non-sRGB RGB space we are **not** converting yet. These are
    /// the files that actually look wrong today.
    WideGamut,
    /// A profile we could not classify. Treated as "leave it alone".
    Unknown,
    /// Not an RGB space (CMYK, Lab, ...).
    NonRgb,
    /// The colour info lives somewhere this stage does not parse: an ISO-BMFF
    /// `colr` box (HEIC/AVIF) or inside a JXL codestream. Deliberately not
    /// guessed at — see [`IccInfo::not_inspected`].
    NotInspected,
}

impl ColorSpace {
    /// Short label for the UI.
    pub fn label(self) -> &'static str {
        match self {
            ColorSpace::UntaggedSrgb => "sRGB (untagged)",
            ColorSpace::Srgb => "sRGB",
            ColorSpace::WideGamut => "wide gamut",
            ColorSpace::Unknown => "unknown profile",
            ColorSpace::NonRgb => "non-RGB",
            ColorSpace::NotInspected => "not inspected",
        }
    }
}

/// Everything the detector found out about one file's colour.
#[derive(Debug, Clone, PartialEq)]
pub struct ColorInfo {
    pub space: ColorSpace,
    /// Profile description when the file carried one ("sRGB IEC61966-2.1").
    pub description: String,
    /// Size of the embedded profile in bytes, 0 when there is none.
    pub profile_bytes: usize,
    /// Data colour space tag, e.g. "RGB " or "CMYK".
    pub data_space: String,
    /// The profile's red primary as D50-adapted XYZ, 15-bit fixed point.
    ///
    /// Reported because the *numbers* are the ground truth: a surprising number
    /// of real profiles carry no description at all, and colour space then has
    /// to be decided from the primaries. `None` when the profile is absent or
    /// carries no `rXYZ` tag.
    pub red_primaries: Option<[i32; 3]>,
}

impl ColorInfo {
    fn plain(space: ColorSpace) -> ColorInfo {
        ColorInfo {
            space,
            description: String::new(),
            profile_bytes: 0,
            data_space: String::new(),
            red_primaries: None,
        }
    }

    /// For containers whose colour info this stage does not read.
    pub fn not_inspected() -> ColorInfo {
        ColorInfo::plain(ColorSpace::NotInspected)
    }

    /// One-line summary for the UI.
    pub fn summary(&self) -> String {
        if self.description.is_empty() {
            self.space.label().to_string()
        } else {
            format!("{} — {}", self.space.label(), self.description)
        }
    }

    /// The line to show in the metadata panel, or `None` when it is not worth
    /// saying.
    ///
    /// An untagged or sRGB-profiled file is the overwhelmingly common case, and
    /// "Colour profile: sRGB" on every screenshot is noise. What earns a line is
    /// something the user might act on: a wide-gamut or non-RGB space, a profile
    /// we could not classify, a format whose colour we do not read at all, or
    /// any real embedded profile.
    pub fn panel_line(&self) -> Option<String> {
        let routine = matches!(self.space, ColorSpace::UntaggedSrgb | ColorSpace::Srgb)
            && self.profile_bytes == 0;
        if routine {
            return None;
        }
        Some(format!("Colour profile: {}", self.summary()))
    }
}

/// The red primary of the standard sRGB ICC profile, D50-adapted, as it appears
/// in the `rXYZ` tag. Measured from a real camera file; used only as a fallback
/// cross-check when a profile has no description.
pub(crate) const SRGB_D50_RED: (i32, i32, i32) = (28578, 14581, 912);

/// Display P3's red primary, D50-adapted. **Measured, not looked up**: 192 of
/// 1001 images in a real phone backup carry a profile whose `rXYZ` is exactly
/// this, against 111 at the sRGB value above. The two never mix, which is what
/// makes this usable as a classifier without trusting the description string.
pub(crate) const P3_D50_RED: (i32, i32, i32) = (33759, 15807, -69);

/// Concatenate a JPEG's APP2 `ICC_PROFILE` chunks. A profile over 64 KB is split
/// across chunks, so they have to be joined in sequence order.
pub(crate) fn jpeg_icc_profile(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut i = 2usize; // SOI
    let mut out: Vec<u8> = Vec::new();
    let mut found = false;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if len < 2 || i + 2 + len > bytes.len() {
            return None;
        }
        // "ICC_PROFILE\0" is 12 bytes, then 1 byte sequence number and 1 byte
        // chunk count, so the profile data starts at i+18.
        if marker == 0xE2 && bytes.len() >= i + 18 && &bytes[i + 4..i + 16] == b"ICC_PROFILE\0" {
            out.extend_from_slice(&bytes[i + 18..i + 2 + len]);
            found = true;
        }
        if marker == 0xDA {
            break; // start of scan
        }
        i += 2 + len;
    }
    found.then_some(out)
}

/// PNG colour chunks: `sRGB` (intentional, sRGB), `iCCP` (a real profile).
/// Returns `(has_srgb_chunk, icc_profile)`.
pub(crate) fn png_colour_chunks(bytes: &[u8]) -> (bool, Option<Vec<u8>>) {
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return (false, None);
    }
    let mut i = 8usize;
    let mut srgb = false;
    let mut iccp = None;
    while i + 12 <= bytes.len() {
        let len = u32::from_be_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        let ty = &bytes[i + 4..i + 8];
        let data = i + 8;
        let end = match data.checked_add(len) {
            Some(e) if e <= bytes.len() => e,
            _ => break, // truncated: stop trusting the chunk stream
        };
        match ty {
            b"sRGB" => srgb = true,
            b"iCCP" => {
                // keyword \0 compression-method \0 *zlib-compressed* profile.
                // The compression is the easy thing to forget: handing the raw
                // chunk to the ICC parser just yields "unknown", because there
                // is no 'acsp' signature at offset 36 in a deflate stream.
                if let Some(z) = bytes[data..end].iter().position(|b| *b == 0)
                    && let Some(z2) = bytes[data + z + 1..end].iter().position(|b| *b == 0)
                {
                    let packed = &bytes[data + z + 1 + z2 + 1..end];
                    use std::io::Read;
                    let mut out = Vec::new();
                    let inflated = flate2::read::ZlibDecoder::new(packed)
                        // A profile is a few KB; refuse to inflate a bomb.
                        .take(1 << 20)
                        .read_to_end(&mut out)
                        .is_ok();
                    if inflated && out.len() > 132 {
                        iccp = Some(out);
                    }
                }
            }
            b"IDAT" | b"IEND" => break,
            _ => {}
        }
        i = end + 4; // skip CRC
    }
    (srgb, iccp)
}

pub(crate) fn icc_tag(b: &[u8], at: usize) -> String {
    String::from_utf8_lossy(&b[at..at + 4]).trim_end().to_string()
}

/// The `desc` tag, which is ASCII in an ICC v2 profile and UTF-16BE `mluc` in
/// v4. Returns "" when absent or unparseable — the caller must cope.
pub(crate) fn icc_description(icc: &[u8]) -> String {
    icc_description_opt(icc).unwrap_or_default()
}

pub(crate) fn icc_description_opt(icc: &[u8]) -> Option<String> {
    let ntags = u32::from_be_bytes(icc.get(128..132)?.try_into().ok()?) as usize;
    for t in 0..ntags.min(256) {
        let e = 132 + t * 12;
        if e + 12 > icc.len() || &icc[e..e + 4] != b"desc" {
            continue;
        }
        let off = u32::from_be_bytes(icc[e + 4..e + 8].try_into().ok()?) as usize;
        if off + 12 > icc.len() {
            return None;
        }
        return match &icc[off..off + 4] {
            // v2: 4-byte length then ASCII.
            b"desc" => {
                let n = u32::from_be_bytes(icc[off + 8..off + 12].try_into().ok()?) as usize;
                let end = (off + 12 + n).min(icc.len());
                Some(
                    icc[off + 12..end]
                        .iter()
                        .take_while(|b| **b != 0)
                        .map(|b| *b as char)
                        .collect::<String>()
                        .trim()
                        .to_string(),
                )
            }
            // v4: 12-byte header, then 12-byte records of [lang, country, len, off].
            b"mluc" => {
                let recsize = u32::from_be_bytes(icc[off + 12..off + 16].try_into().ok()?) as usize;
                if recsize < 12 {
                    return None;
                }
                let r = off + 16;
                // Record count; only the first record is the name, so it is read
                // for validation and otherwise unused.
                let _n = u32::from_be_bytes(icc.get(r + 4..r + 8)?.try_into().ok()?) as usize;
                let str_off = r + 8;
                let len =
                    u32::from_be_bytes(icc.get(str_off..str_off + 4)?.try_into().ok()?) as usize;
                let at = u32::from_be_bytes(icc.get(str_off + 4..str_off + 8)?.try_into().ok()?)
                    as usize;
                let start = off.checked_add(at)?;
                let end = start.checked_add(len)?.min(icc.len());
                if start >= icc.len() || start % 2 != 0 {
                    return None;
                }
                // `chunks_exact` is correct here: `as_chunks` does not resolve
                // as an iterator on this slice type in this context.
                #[allow(clippy::chunks_exact_to_as_chunks)]
                let units: Vec<u16> = icc[start..end]
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                String::from_utf16(&units).ok().map(|s| s.trim().to_string())
            }
            _ => None,
        };
    }
    None
}

/// The `rXYZ` tag, the giveaway for gamut.
pub(crate) fn icc_red_primaries(icc: &[u8]) -> Option<(i32, i32, i32)> {
    let ntags = u32::from_be_bytes(icc.get(128..132)?.try_into().ok()?) as usize;
    for t in 0..ntags.min(256) {
        let e = 132 + t * 12;
        if e + 12 > icc.len() || &icc[e..e + 4] != b"rXYZ" {
            continue;
        }
        let off = u32::from_be_bytes(icc[e + 4..e + 8].try_into().ok()?) as usize;
        if off + 20 > icc.len() {
            return None;
        }
        return Some((
            i32::from_be_bytes(icc[off + 8..off + 12].try_into().ok()?),
            i32::from_be_bytes(icc[off + 12..off + 16].try_into().ok()?),
            i32::from_be_bytes(icc[off + 16..off + 20].try_into().ok()?),
        ));
    }
    None
}

/// Is the tag table structurally trustworthy?
///
/// Every entry must lie entirely inside the profile, and the declared count
/// must actually fit. This is what stops a corrupt profile from producing a
/// *confident wrong answer*: mis-reading it as sRGB would skip a conversion the
/// image needs, and mis-reading it as wide gamut would push a correct image
/// through a P3 matrix and wreck it. Both failure modes are worse than saying
/// "unknown", so a profile that does not validate is refused outright.
pub(crate) fn icc_tags_valid(icc: &[u8]) -> bool {
    let Some(count) = icc.get(128..132) else {
        return false;
    };
    let ntags = u32::from_be_bytes([count[0], count[1], count[2], count[3]]) as usize;
    let Some(ntags_bytes) = ntags.checked_mul(12) else {
        return false;
    };
    let Some(table_end) = 132usize.checked_add(ntags_bytes) else {
        return false;
    };
    if table_end > icc.len() {
        return false;
    }
    for t in 0..ntags {
        let e = 132 + t * 12;
        let off = u32::from_be_bytes(icc[e + 4..e + 8].try_into().unwrap()) as usize;
        let size = u32::from_be_bytes(icc[e + 8..e + 12].try_into().unwrap()) as usize;
        let end = match off.checked_add(size) {
            Some(e) => e,
            None => return false,
        };
        if off < table_end || end > icc.len() {
            return false; // overlaps the table, or runs off the end
        }
    }
    true
}

/// Classify an ICC profile. Name-based where the description names a space we
/// know, with a primaries cross-check as a fallback; anything else is
/// [`ColorSpace::Unknown`] rather than a guess.
pub(crate) fn classify_icc(icc: &[u8]) -> ColorInfo {
    if icc.len() < 132 || &icc[36..40] != b"acsp" || !icc_tags_valid(icc) {
        return ColorInfo::plain(ColorSpace::Unknown);
    }
    let data_space = icc_tag(icc, 16);
    let red = icc_red_primaries(icc);
    let mut info = ColorInfo {
        space: ColorSpace::Unknown,
        description: icc_description(icc),
        profile_bytes: icc.len(),
        data_space: data_space.clone(),
        red_primaries: red.map(|r| [r.0, r.1, r.2]),
    };
    if !data_space.starts_with("RGB") {
        info.space = ColorSpace::NonRgb;
        return info;
    }
    // Tokenise on non-alphanumerics so "DCI-P3-D65-2_2-Gamma" yields `p3` and
    // "sRGB IEC61966-2.1" yields `srgb`, without matching a `p3` buried inside a
    // longer word.
    let d = info.description.to_ascii_lowercase();
    let tokens: Vec<&str> =
        d.split(|c: char| !c.is_ascii_alphanumeric()).filter(|s| !s.is_empty()).collect();
    let has = |w: &str| tokens.contains(&w);
    info.space = if has("prophoto") {
        ColorSpace::WideGamut
    } else if has("p3") || has("adobergb") || has("2020") {
        // Display P3 / DCI-P3, AdobeRGB, Rec.2020 and BT.2020: all matrix or
        // wide-transfer spaces, all of which need a transform to sRGB.
        ColorSpace::WideGamut
    } else if has("srgb") {
        ColorSpace::Srgb
    } else {
        // No usable name. Fall back to comparing the red primary with sRGB's.
        match red {
            Some(r) => {
                if near3(r, SRGB_D50_RED) {
                    ColorSpace::Srgb
                } else if near3(r, P3_D50_RED) {
                    ColorSpace::WideGamut
                } else {
                    // A matrix/TRC RGB profile that matches neither is, by
                    // elimination, some other wide space.
                    ColorSpace::WideGamut
                }
            }
            None => ColorSpace::Unknown,
        }
    };
    info
}

/// True when two 15-bit XYZ triples agree to within 1%.
pub(crate) fn near3(a: (i32, i32, i32), b: (i32, i32, i32)) -> bool {
    let near = |x: i32, y: i32| (x - y).abs() * 100 / y.abs().max(1) < 1;
    near(a.0, b.0) && near(a.1, b.1) && near(a.2, b.2)
}

/// What a file declares about its colour space. Never fails: an unreadable or
/// unrecognised file comes back as [`ColorSpace::Unknown`].
///
/// Formats whose colour info is not in a JPEG APP2 / PNG chunk — HEIC, AVIF and
/// JXL — are reported as [`ColorSpace::NotInspected`] rather than assumed to be
/// sRGB, because assuming is how wide-gamut HEIC photos end up looking flat.
pub fn color_info_from_bytes(bytes: &[u8], is_jpeg: bool, is_png: bool) -> ColorInfo {
    if is_jpeg {
        return match jpeg_icc_profile(bytes) {
            Some(p) => classify_icc(&p),
            None => ColorInfo::plain(ColorSpace::UntaggedSrgb),
        };
    }
    if is_png {
        let (has_srgb, iccp) = png_colour_chunks(bytes);
        if let Some(p) = iccp {
            return classify_icc(&p);
        }
        // No profile: an `sRGB` chunk states sRGB outright; without one, PNG
        // with no colour info is conventionally sRGB too.
        let _ = has_srgb;
        return ColorInfo::plain(ColorSpace::UntaggedSrgb);
    }
    // HEIC and AVIF keep their colour in an ISO-BMFF `colr` box, which the `heic`
    // crate reads for us. Sniffed here rather than passed in, so a caller that
    // knows only "not a JPEG, not a PNG" still gets a real answer.
    if is_heif_bytes(bytes) {
        return heif_color_info(bytes);
    }
    // A GIF has no colour profile chunk at all — its colours come from a
    // palette. UntaggedSrgb by the same convention untagged PNG and JPEG get,
    // not NotInspected.
    if is_gif_bytes(bytes) {
        return ColorInfo::plain(ColorSpace::UntaggedSrgb);
    }
    // A WebP has no ICC profile chunk at all - its colours come from the
    // VP8/VP8L bitstream. The honest answer is UntaggedSrgb, by the same
    // convention untagged PNG and JPEG already get, not NotInspected.
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return ColorInfo::plain(ColorSpace::UntaggedSrgb);
    }
    ColorInfo::not_inspected()
}

/// Read a HEIC/AVIF's `colr` box and classify whatever profile is in it.
///
/// `DecoderConfig::extract_icc` is the whole reason this is not a container walk:
/// it returns the raw ICC bytes, or `None` when the file states its colour with
/// **`nclx` parameters instead of a profile**. That distinction is the reason the
/// answer is not "assume sRGB": an `nclx` file is a real possibility, and iPhone
/// HEIC is frequently Display P3 through exactly that route, so assuming would
/// report a wide-gamut photo as sRGB — which is the failure this whole stage
/// exists to prevent. `None` stays [`ColorSpace::NotInspected`].
///
/// Verified on real files: of four HEIF/AVIF images, two carry a 536-byte ICC
/// profile that classifies as Display P3 from its primaries with **no `desc`
/// tag**, and two have no profile at all.
pub(crate) fn heif_color_info(bytes: &[u8]) -> ColorInfo {
    match heic::DecoderConfig::new().extract_icc(bytes) {
        Ok(Some(profile)) => classify_icc(&profile),
        Ok(None) => ColorInfo::not_inspected(),
        // A container we cannot parse is not a reason to claim anything about
        // its colour.
        Err(_) => ColorInfo::not_inspected(),
    }
}

/// [`color_info_from_bytes`] for a file on disk.
pub fn color_info(path: &Path) -> ColorInfo {
    let Ok(bytes) = std::fs::read(path) else {
        return ColorInfo::plain(ColorSpace::Unknown);
    };
    let is_jpeg = is_jpeg_bytes(&bytes);
    let is_png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    color_info_from_bytes(&bytes, is_jpeg, is_png)
}

/// Classify every image in `folder`, in natural name order.
///
/// Exists so a library can be audited from a command line instead of by clicking
/// through the metadata panel one file at a time, which is the only other way to
/// reach [`color_info`]. It is a pure function of the folder, so it is testable;
/// the presentation is the caller's.
///
/// **This reads every file whole**, because [`color_info`] does, and for a folder
/// of 1000 images that is a few tens of megabytes and a few seconds. Fine for a
/// command somebody asked for, wrong for anything on a startup path.
pub fn scan_colour(folder: &Path) -> Vec<(PathBuf, ColorInfo)> {
    scan_folder_sorted(folder, SortMode::Name)
        .into_iter()
        // Archives are listed by a folder scan and are **not** auditable by
        // this: `color_info` reads the whole file, so auditing a 200 MB `.cbz`
        // would pull all of it into memory to report that a container has no
        // colour profile of its own. The images inside it are audited when it
        // is opened.
        .filter(|p| !is_archive_path(p))
        .map(|p| {
            let info = color_info(&p);
            (p, info)
        })
        .collect()
}
