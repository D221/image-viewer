//! Module `wide_gamut`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use wide_gamut::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Wide-gamut to sRGB: Display P3, opt-in
// ---------------------------------------------------------------------------
//
// The renderer uploads `ColorImage` as `Rgba8UnormSrgb` and egui wants a
// gamma-space surface, so the pipeline is 8-bit sRGB end to end whatever the file
// says. A Display P3 photo therefore displays without conversion and reads
// flatter than it should — 229 of 1001 images in a real library. This is the
// conversion, and it is **off by default**: it changes how photos look, so it is
// something to ask for rather than something to inherit.
//
// Two things that are easy to get wrong and are therefore spelled out:
//
// * **The primaries are derived, not transcribed.** The matrix below is built
//   from the two spaces' chromaticities. Hand-copying a published matrix is the
//   `Xform::Transverse` failure mode: it compiles, it type-checks, and it is
//   wrong in a way nothing catches. `the_derived_matrix_reproduces_the_icc_primaries_we_measured`
//   checks the derivation against the D50-adapted primary read out of 192 real
//   P3 profiles, which is a fact about files rather than about this code.
// * **The conversion happens in linear light.** Applying the matrix to gamma-space
//   values is the classic mistake, and it produces dark, muddy images rather than
//   an error. Hence the two transfer-function LUTs either side of it.

/// Whether wide-gamut pixels are converted to sRGB as they are decoded.
///
/// A process-wide toggle like `set_linear_resample`, and for the same reasons:
/// it is a decode-time decision, the workers are already threads, and plumbing a
/// parameter through every decode entry point would put the setting in the way of
/// five call sites that each have to remember it.
pub(crate) static WIDE_GAMUT_CONVERSION: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Turn conversion of Display P3 pixels to sRGB on or off. **Default: off.**
///
/// Takes effect on the next decode; like the linear-light toggle it does not
/// re-decode what is already cached, so switch it on and move to another image
/// (or restart) to see it everywhere.
pub fn set_wide_gamut_conversion(on: bool) {
    WIDE_GAMUT_CONVERSION.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the conversion on? Mostly for tests and the settings panel.
pub fn wide_gamut_conversion() -> bool {
    WIDE_GAMUT_CONVERSION.load(std::sync::atomic::Ordering::Relaxed)
}

/// sRGB's primaries as CIE 1931 chromaticities (x, y).
pub const SRGB_XY: [(f64, f64); 3] = [(0.640, 0.330), (0.300, 0.600), (0.150, 0.060)];
/// Display P3's primaries, likewise. Same blue as sRGB; wider red and green.
pub const P3_XY: [(f64, f64); 3] = [(0.680, 0.320), (0.265, 0.690), (0.150, 0.060)];
/// Both spaces are referred to D65, which is why converting between them needs no
/// chromatic adaptation: the white point is shared, and the primaries are the only
/// thing that differs. (The D50 in `P3_D50_RED` is an *ICC* convention for how
/// profiles store primaries, and it is what the detection side compares against —
/// see `the_derived_matrix_reproduces_the_icc_primaries_we_measured`.)
pub const D65_XY: (f64, f64) = (0.3127, 0.3290);

/// CIE xyY (Y = 1) to XYZ.
pub fn xy_to_xyz(xy: (f64, f64)) -> [f64; 3] {
    let (x, y) = xy;
    [x / y, 1.0, (1.0 - x - y) / y]
}

/// The RGB→XYZ matrix for a set of primaries and a white point.
///
/// Standard derivation: each primary becomes a column of XYZ scaled to Y = 1,
/// then the white point is expressed in that basis and folded back in as the
/// per-column scale.
pub fn rgb_to_xyz(primaries: [(f64, f64); 3], white: (f64, f64)) -> [[f64; 3]; 3] {
    // Rows are components, columns are primaries — that is the orientation the
    // colour equation is written in, and getting it wrong solves the *transposed*
    // system, which has an answer, just not this one. It is worth spelling out
    // because the failure is invisible: the result is a full set of plausible
    // numbers, `invert3` still round-trips to the identity, and white ends up
    // somewhere other than white.
    let mut m = [[0.0; 3]; 3];
    for (c, xy) in primaries.iter().enumerate() {
        let xyz = xy_to_xyz(*xy);
        for r in 0..3 {
            m[r][c] = xyz[r];
        }
    }
    // Solve M * s = white, then scale each column by its own s.
    let s = mul_vec(&invert3(&m), xy_to_xyz(white));
    for r in 0..3 {
        for c in 0..3 {
            m[r][c] *= s[c];
        }
    }
    m
}

pub(crate) fn mul_vec(m: &[[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    let mut out = [0.0; 3];
    for r in 0..3 {
        out[r] = m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2];
    }
    out
}

pub(crate) fn mul_mat(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    mul_mat_ref(a, b)
}

/// [`mul_mat`] by value, so the tests can use it without cloning.
pub fn mul_mat_ref(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (r, row) in out.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            *cell = a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c];
        }
    }
    out
}

/// [`p3_to_srgb_matrix`], inverted: linear sRGB → linear Display P3.
///
/// Present so the round trip can be asserted, and so the matrix can be undone.
pub fn srgb_to_p3_matrix() -> [[f64; 3]; 3] {
    invert3(p3_to_srgb_matrix())
}

/// Inverse of a 3×3 by cofactors. Adequate here because the inputs are
/// well-conditioned colour matrices; a general solver would be a dependency for
/// one call site. Public because the ICC cross-check in `tests/color_profiles.rs`
/// needs to invert the Bradford matrix.
/// Inverse of a 3×3: the adjugate over the determinant.
///
/// Written as "cofactor at (k, r)" rather than a flat list of terms, because the
/// flat form has the off-diagonal entries in **transposed** order and the
/// transposition is invisible — it type-checks, it returns plausible numbers, and
/// `inv * m` is *not* the identity, so everything downstream is wrong by a few
/// percent while still looking like a colour matrix. `rows.zip(cols)` also
/// deliberately preserves order rather than sorting, which a filter-map over a
/// range would not.
pub fn invert3(m: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    // Signed minor: element (row, col) of the cofactor matrix.
    let cof = |row: usize, col: usize| -> f64 {
        let rows: Vec<usize> = (0..3).filter(|r| *r != row).collect();
        let cols: Vec<usize> = (0..3).filter(|c| *c != col).collect();
        let minor = m[rows[0]][cols[0]] * m[rows[1]][cols[1]]
            - m[rows[0]][cols[1]] * m[rows[1]][cols[0]];
        if (row + col) % 2 == 0 {
            minor
        } else {
            -minor
        }
    };
    // Expand along the first row: det = sum_j m[0][j] * C[0][j].
    let det = m[0][0] * cof(0, 0) + m[0][1] * cof(0, 1) + m[0][2] * cof(0, 2);
    let inv = 1.0 / det;
    let mut out = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            // adj[r][c] == C[c][r]: the adjugate is the transpose of the
            // cofactor matrix. Getting this backwards is the whole bug.
            out[r][c] = cof(c, r) * inv;
        }
    }
    out
}

/// The linear-light Display P3 → sRGB matrix, derived once.
///
/// Equals the matrix published as `lin_display_p3_to_lin_srgb`, and is built here
/// from the primaries instead so that it cannot drift from them.
pub fn p3_to_srgb_matrix() -> &'static [[f64; 3]; 3] {
    static M: std::sync::OnceLock<[[f64; 3]; 3]> = std::sync::OnceLock::new();
    M.get_or_init(|| {
        let srgb = rgb_to_xyz(SRGB_XY, D65_XY);
        let p3 = rgb_to_xyz(P3_XY, D65_XY);
        mul_mat(&invert3(&srgb), &p3)
    })
}

/// LUT resolution for the transfer functions.
///
/// 12 bits, matching the resampler's LUTs. The quantisation is in *linear* light,
/// so the error it introduces is at most one index step of the encode LUT, which
/// is under 1/255 anywhere the encode curve is steep enough to matter.
pub(crate) const TF_STEPS: usize = 4096;

/// sRGB electro-optical transfer function, exact.
pub(crate) fn srgb_to_linear_exact(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB opto-electronic transfer function, exact.
pub(crate) fn linear_to_srgb_exact(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

pub(crate) fn srgb_to_linear_lut() -> &'static [f32; TF_STEPS] {
    static L: std::sync::OnceLock<[f32; TF_STEPS]> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        let mut l = [0f32; TF_STEPS];
        for (i, v) in l.iter_mut().enumerate() {
            *v = srgb_to_linear_exact(i as f64 / (TF_STEPS - 1) as f64) as f32;
        }
        l
    })
}

pub(crate) fn linear_to_srgb_lut() -> &'static [f32; TF_STEPS] {
    static L: std::sync::OnceLock<[f32; TF_STEPS]> = std::sync::OnceLock::new();
    L.get_or_init(|| {
        let mut l = [0f32; TF_STEPS];
        for (i, v) in l.iter_mut().enumerate() {
            *v = (linear_to_srgb_exact(i as f64 / (TF_STEPS - 1) as f64) * 255.0).round() as f32;
        }
        l
    })
}

/// Convert 8-bit sRGB-encoded Display P3 pixels to sRGB, in place.
///
/// Alpha is untouched. Out-of-gamut results are clamped, which is the only thing
/// that *can* be done with them: a saturated P3 primary has no sRGB equivalent, and
/// clipping it is what every other tool does. This is lossy for those colours by
/// construction — which is the honest argument for the setting being off by
/// default.
pub fn convert_wide_gamut_rgba(rgba: &mut [u8]) {
    let m = p3_to_srgb_matrix();
    let dec = srgb_to_linear_lut();
    let enc = linear_to_srgb_lut();
    // The LUTs are `TF_STEPS` wide and the samples are 8-bit, so an index has to
    // be scaled across. Reading `dec[v]` directly decodes 255 as 255/4095 — about
    // 6% grey — which turns every white pixel dark grey and every photo into a
    // silhouette. The `* (TF_STEPS - 1) / 255` is the whole fix.
    let to_index = |v: u8| (v as usize * (TF_STEPS - 1)) / 255;
    let last = (TF_STEPS - 1) as f32;

    // Rayon, like the resampler: this runs on the decode path and a full-res
    // 24 MP buffer is 96 MB of pixels, which is 600 ms on one thread. The
    // per-pixel work is identical either way — no reduction, no ordering — so the
    // parallel version cannot differ from the serial one.
    rgba.par_chunks_mut(4).for_each(|px| {
        let r = dec[to_index(px[0])];
        let g = dec[to_index(px[1])];
        let b = dec[to_index(px[2])];
        // The matrix itself: linear P3 -> linear sRGB. Out-of-range on purpose.
        let lr = m[0][0] as f32 * r + m[0][1] as f32 * g + m[0][2] as f32 * b;
        let lg = m[1][0] as f32 * r + m[1][1] as f32 * g + m[1][2] as f32 * b;
        let lb = m[2][0] as f32 * r + m[2][1] as f32 * g + m[2][2] as f32 * b;
        for (slot, v) in [(0, lr), (1, lg), (2, lb)] {
            let v = v.clamp(0.0, 1.0) * last;
            px[slot] = enc[v as usize].clamp(0.0, 255.0) as u8;
        }
    });
}

/// Convert `out` in place, but only if the setting is on and the file really is
/// wide gamut.
///
/// **Both checks are after the early return**, so the default costs nothing at
/// all: no second read of the file, no classification, no arithmetic. `color_info`
/// reads the file again, which is free against the page cache but not free, and
/// there is no reason for an opt-in feature to pay that when it is off.
pub(crate) fn apply_wide_gamut_conversion(out: &mut PreviewAndFull) {
    if !wide_gamut_conversion() {
        return;
    }
    // `WideGamut` and nothing else. A profile we could not classify must not be
    // pushed through a matrix on a guess, and an untagged file is already sRGB.
    //
    // The answer is `out.colour`, classified by this decode from its own bytes —
    // the old shape asked `color_info(path)`, which re-read the **whole file**
    // here, per decode, behind a setting most users never turn on.
    if out.colour.space != ColorSpace::WideGamut {
        return;
    }
    convert_wide_gamut_rgba(&mut out.preview.rgba);
    if let Some(full) = out.full.as_mut() {
        convert_wide_gamut_rgba(&mut full.rgba);
    }
    if let Some(anim) = out.anim.as_mut() {
        for frame in &mut anim.frames {
            convert_wide_gamut_rgba(&mut frame.rgba);
        }
    }
}

/// File name as a lossy string, the key `last_by_folder` resume stores and the
/// thing a gallery filter matches against.
pub fn file_name_of(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Errors that can occur while loading an image.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] image::ImageError),
    #[error("decoder error: {0}")]
    Other(String),
    #[error("jpeg error: {0}")]
    Jpeg(String),
}

/// EXIF orientation tag (1-8) of a JPEG file, or 0 when absent/unreadable.
/// Only JPEG carries the tag in practice; callers gate on content sniffing.
pub fn exif_orientation(path: &Path) -> u16 {
    nom_exif::read_exif(path)
        .ok()
        .and_then(|exif| exif.get(nom_exif::ExifTag::Orientation).and_then(|v| v.as_u16()))
        .unwrap_or(0)
}

/// EXIF `DateTimeOriginal` ("YYYY:MM:DD HH:MM:SS") of a JPEG file, or empty
/// when absent/unreadable. Only JPEG carries the tag in practice.
pub fn exif_date_taken(path: &Path) -> String {
    nom_exif::read_exif(path)
        .ok()
        .and_then(|exif| {
            exif.get(nom_exif::ExifTag::DateTimeOriginal)
                .map(|v| v.to_string())
        })
        .unwrap_or_default()
}

/// First JPEG COM segment in `bytes`, as text.
///
/// Walks markers from SOI and stops at SOS — scan data follows it, so anything
/// shaped like a marker past it is pixels, not segments — at EOI, or at the
/// first malformed length. Every read is bounds-checked: this parses untrusted
/// input, so a truncated file is `None`, never a panic on a worker.
pub fn jpeg_comment(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 1 < bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        // Skip fill bytes: any number of 0xFF is one marker prefix.
        while i < bytes.len() && bytes[i] == 0xFF {
            i += 1;
        }
        if i >= bytes.len() {
            return None;
        }
        let marker = bytes[i];
        i += 1;
        match marker {
            // Standalone markers carry no length.
            0xD8 | 0xD9 | 0x01 | 0xD0..=0xD7 => {
                if marker == 0xD9 {
                    return None;
                }
            }
            // Start of scan: pixel data follows, so no segment past here.
            0xDA => return None,
            _ => {
                if i + 2 > bytes.len() {
                    return None;
                }
                let len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
                if len < 2 || i + len > bytes.len() {
                    return None;
                }
                if marker == 0xFE {
                    if let Some(text) = clean_comment_text(&bytes[i + 2..i + len]) {
                        return Some(text);
                    }
                }
                i += len;
            }
        }
    }
    None
}

/// EXIF UserComment payload: an 8-byte charset code (`ASCII\0\0\0`,
/// `UNICODE\0`, …) followed by the text. Unknown codes are read as bytes —
/// most writers put ASCII there whatever the code says — and a payload that
/// is empty after cleaning is no comment, not an empty line.
pub fn decode_user_comment(payload: &[u8]) -> Option<String> {
    if payload.len() < 8 {
        return clean_comment_text(payload);
    }
    let (code, text) = payload.split_at(8);
    if code == b"UNICODE\0" {
        let units: Vec<u16> = text
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        let s: String = char::decode_utf16(units.iter().copied())
            .map(|r| r.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect();
        clean_comment_text(s.as_bytes())
    } else {
        clean_comment_text(text)
    }
}

/// Comment for the metadata panel: ImageDescription first, else UserComment.
/// One line — a comment that is empty after cleaning is absent, not blank.
pub fn exif_comment(exif: &nom_exif::Exif) -> Option<String> {
    if let Some(s) = exif
        .get(nom_exif::ExifTag::ImageDescription)
        .and_then(|v| v.as_str())
        .and_then(|s| clean_comment_text(s.as_bytes()))
    {
        return Some(s);
    }
    exif
        .get(nom_exif::ExifTag::UserComment)
        .and_then(|v| v.as_undefined())
        .and_then(decode_user_comment)
}

/// Text out of a comment payload: UTF-8 where it is valid, Latin-1 mapped
/// where it is not (a camera writing Latin-1 is ordinary, and lossy mojibake
/// beats dropping the line), trimmed of NULs and whitespace.
pub(crate) fn clean_comment_text(bytes: &[u8]) -> Option<String> {
    let s = match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    };
    let s = s.trim_matches(['\0', ' ', '\t', '\r', '\n']).to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

