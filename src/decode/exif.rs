//! Module `exif`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use exif::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// EXIF block: a minimal, allocation-free TIFF walker
// ---------------------------------------------------------------------------
//
// `nom_exif` can report the *values* of the thumbnail tags but not the bytes
// they point at, and it wants a path (re-reading the whole file). We already
// hold the file in memory, so this walks the APP1/Exif block directly: it costs
// one pass over a few hundred bytes, needs no allocation, and hands back the
// raw embedded JPEG.
//
// Every read is bounds-checked against the APP1 segment, because this parses
// untrusted input and a malformed offset must degrade to "no thumbnail", never
// to a panic on a worker.

/// Byte offset of a TIFF block's header ("II"/"MM"), i.e. just past `Exif\0\0`.
/// Returns `(start, end)` of the TIFF data, which is what all internal offsets
/// are relative to and what they must be bounds-checked against.
pub(crate) fn exif_tiff_span(bytes: &[u8]) -> Option<(usize, usize)> {
    let mut i = 2usize; // skip SOI
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            return None;
        }
        let marker = bytes[i + 1];
        // Standalone markers carry no length.
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        // Length lives at i+2 and counts itself, so the payload starts at i+4
        // and the whole segment ends at i+2+len.
        let len = usize::from(u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]));
        if len < 2 || i + 2 + len > bytes.len() {
            return None;
        }
        let payload = i + 4;
        if marker == 0xE1 && &bytes[payload..payload + 6] == b"Exif\0\0" {
            return Some((payload + 6, i + 2 + len));
        }
        if marker == 0xDA {
            return None; // start of scan: no more metadata
        }
        i += 2 + len;
    }
    None
}

pub(crate) struct Tiff<'a> {
    b: &'a [u8],
    /// Index of the first byte of the TIFF header.
    base: usize,
    /// One past its last byte; every offset must resolve inside this.
    end: usize,
    le: bool,
}

impl<'a> Tiff<'a> {
    fn u16_at(&self, off: usize) -> Option<u16> {
        let a = self.base.checked_add(off)?;
        let b = a.checked_add(2)?;
        if b > self.end {
            return None;
        }
        let v = [self.b[a], self.b[b - 1]];
        Some(if self.le { u16::from_le_bytes(v) } else { u16::from_be_bytes(v) })
    }

    fn u32_at(&self, off: usize) -> Option<u32> {
        let a = self.base.checked_add(off)?;
        let b = a.checked_add(4)?;
        if b > self.end {
            return None;
        }
        let v = [self.b[a], self.b[a + 1], self.b[a + 2], self.b[a + 3]];
        Some(if self.le { u32::from_le_bytes(v) } else { u32::from_be_bytes(v) })
    }

    /// Offset of the IFD after the one at `off`, i.e. the "next IFD" pointer
    /// that follows an IFD's entry table. IFD0's next pointer is how you find
    /// IFD1, where the thumbnail lives.
    fn ifd_next(&self, off: usize) -> Option<u32> {
        let n = usize::from(self.u16_at(off)?);
        self.u32_at(off.checked_add(2 + n.checked_mul(12)?)?)
    }

    /// First `count` entries of the IFD at `off`, as `(tag, entry_offset)`.
    fn ifd(&self, off: usize) -> Vec<(u16, usize)> {
        let mut out = Vec::new();
        let Some(n) = self.u16_at(off) else {
            return out;
        };
        // A sane IFD has tens of entries, not thousands; cap it so a corrupt
        // count cannot make us walk a megabyte of attacker-controlled offsets.
        for i in 0..n.min(64) {
            let Some(e) = off.checked_add(2 + usize::from(i) * 12) else {
                break;
            };
            let Some(tag) = self.u16_at(e) else {
                break;
            };
            out.push((tag, e + 8)); // +8 = the value field
        }
        out
    }
}

/// Location of the compressed thumbnail inside IFD1.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ThumbRef {
    /// Range into the *whole file*.
    pub(crate) range: (usize, usize),
    /// Dimensions read from the thumbnail's own SOF marker, when we could find it.
    pub(crate) dims: Option<(u32, u32)>,
}

/// Scan the APP1/Exif block for a compressed (old-style JPEG) thumbnail in IFD1.
///
/// Returns `None` for any JPEG without one, which is the common case for
/// screenshots, exports and anything not written by a camera.
pub(crate) fn find_embedded_thumbnail(bytes: &[u8]) -> Option<ThumbRef> {
    let (base, end) = exif_tiff_span(bytes)?;
    if end < base + 8 || bytes.len() < base + 8 {
        return None;
    }
    let le = match &bytes[base..base + 2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let t = Tiff { b: bytes, base, end, le };
    // IFD0 sits at the offset in the header; IFD1 (the thumbnail) is reachable
    // only through IFD0's *next-IFD* pointer, which is after its entry table.
    let ifd0 = t.u32_at(4)? as usize;
    let ifd1 = t.ifd_next(ifd0)? as usize;
    let entries = t.ifd(ifd1);
    if entries.is_empty() {
        return None;
    }
    let mut compression = None;
    let mut off = None;
    let mut len = None;
    for (tag, val) in entries {
        match tag {
            0x0103 => compression = t.u16_at(val), // Compression
            0x0201 => off = t.u32_at(val),         // ThumbnailOffset
            0x0202 => len = t.u32_at(val),         // ThumbnailLength
            _ => {}
        }
    }
    // 6 = old-style JPEG. Anything else (uncompressed TIFF, or newer
    // formats like HEIF thumbnails) is not something we can slice out.
    if compression != Some(6) {
        return None;
    }
    let (off, len) = (off? as usize, len? as usize);
    if off == 0 || len < 4 {
        return None;
    }
    let start = base.checked_add(off)?;
    let stop = start.checked_add(len)?;
    if stop > bytes.len() {
        return None;
    }
    let slice = &bytes[start..stop];
    // It has to actually be a JPEG: SOI at the front, EOI at the back. A
    // truncated block is common in the wild and must not reach a decoder.
    if slice.len() < 4
        || slice[0] != 0xFF
        || slice[1] != 0xD8
        || slice[stop - start - 2] != 0xFF
        || slice[stop - start - 1] != 0xD9
    {
        return None;
    }
    // SOF0..SOF15 (minus the non-frame markers DHT/JPG/DAC) carry the size.
    let dims = find_sof_dims(slice);
    Some(ThumbRef { range: (start, stop), dims })
}

/// Read `(width, height)` out of a JPEG's first SOF marker.
pub(crate) fn find_sof_dims(jpeg: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 <= jpeg.len() {
        if jpeg[i] != 0xFF {
            return None;
        }
        let m = jpeg[i + 1];
        // SOF0/1/2/3/5/6/7/9/10/11/13/14/15 all have the same layout.
        if matches!(m, 0xC0..=0xCF) && !matches!(m, 0xC4 | 0xC8 | 0xCC) {
            let h = u16::from_be_bytes([jpeg[i + 5], jpeg[i + 6]]);
            let w = u16::from_be_bytes([jpeg[i + 7], jpeg[i + 8]]);
            return Some((u32::from(w), u32::from(h)));
        }
        if m == 0xD8 || m == 0x01 || (0xD0..=0xD7).contains(&m) {
            i += 2;
            continue;
        }
        if m == 0xDA {
            return None; // entropy-coded data, no more headers
        }
        let len = usize::from(u16::from_be_bytes([jpeg[i + 2], jpeg[i + 3]]));
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// Grid cells are decoded at 256 px. Only below this target is it worth using a
/// file's embedded EXIF thumbnail: it is ~160 px and *already encoded*, so it
/// beats decoding a 24 MP file to throw 99% of it away. Above this (the main
/// view) the loss of sharpness would be visible, so we decode for real.
pub const EMBEDDED_THUMB_MAX_TARGET: u32 = 512;

/// Relative difference between two ratios, tolerant of rounding.
pub(crate) fn ratio_diff(a: f32, b: f32) -> f32 {
    let m = a.max(b).max(1e-6);
    (a - b).abs() / m
}

/// Which rotation, if any, to apply to an embedded thumbnail so that it is
/// displayed the same way the main image is.
///
/// The EXIF spec does not settle whether a camera stores the thumbnail already
/// rotated to the display orientation or in raw sensor orientation, and the
/// answer varies by maker. Guessing would show sideways photos in the grid, so
/// instead we **decide per file from the aspect ratio**: for a quarter-turn
/// orientation the two hypotheses produce mirrored aspect ratios, and the one
/// that matches the thumbnail is the right one.
///
/// Returns `None` when the evidence cannot separate them (mirrored/180
/// orientations, which preserve ratio, and near-square sources). The caller
/// then decodes the file properly — correct and merely slower.
pub(crate) fn embedded_thumb_rotation(
    main_tag: u16,
    main: (u32, u32),
    thumb: (u32, u32),
) -> Option<u16> {
    /// Aspect ratios within this of each other count as the same.
    const TOL: f32 = 0.15;
    let (tw, th) = thumb;
    if tw == 0 || th == 0 {
        return None;
    }
    let thumb_r = tw as f32 / th as f32;
    match main_tag {
        0 | 1 => Some(1), // already the display orientation
        // 2/3/4 (mirror / 180) keep the aspect ratio, so a ratio tells us
        // nothing. Rare in real photos; refuse rather than guess.
        2..=4 => None,
        5..=8 => {
            let (mw, mh) = main;
            if mw == 0 || mh == 0 {
                return None;
            }
            // A square source is ambiguous: both hypotheses give ~1.0.
            if ratio_diff(mw as f32 / mh as f32, 1.0) < TOL {
                return None;
            }
            let main_r = mw as f32 / mh as f32;
            if ratio_diff(thumb_r, main_r) < TOL {
                // Thumbnail has the *stored* aspect ratio: apply the tag.
                Some(main_tag)
            } else if ratio_diff(thumb_r, 1.0 / main_r) < TOL {
                // Already rotated: the thumbnail is the finished picture.
                Some(1)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// True if the orientation tag rotates by 90/270, swapping width and height.
pub(crate) fn orientation_swaps_dims(tag: u16) -> bool {
    matches!(tag, 5..=8)
}

/// Apply an EXIF orientation tag (1-8) to decoded RGBA pixels in place.
/// Tags 0 and 1 are no-ops; 2/4 flip, 3 rotates 180°, 6/8 rotate 90/270° CW,
/// 5/7 are the mirrored diagonals (transpose / transverse).
pub fn apply_exif_orientation(data: &mut ImageData, tag: u16) {
    let buf = std::mem::take(&mut data.rgba);
    let Some(mut img) = RgbaImage::from_raw(data.width, data.height, buf) else {
        // Buffer length doesn't match the declared dims — put back whatever
        // we got instead of leaving an empty buffer (which would trip the
        // texture-size assert later).
        data.rgba = Vec::new();
        return;
    };
    let rotated: RgbaImage = match tag {
        2 => {
            image::imageops::flip_horizontal_in_place(&mut img);
            img
        }
        3 => image::imageops::rotate180(&img),
        4 => {
            image::imageops::flip_vertical_in_place(&mut img);
            img
        }
        5 => {
            let mut img = image::imageops::rotate90(&img);
            image::imageops::flip_horizontal_in_place(&mut img);
            img
        }
        6 => image::imageops::rotate90(&img),
        7 => {
            let mut img = image::imageops::rotate270(&img);
            image::imageops::flip_horizontal_in_place(&mut img);
            img
        }
        8 => image::imageops::rotate270(&img),
        _ => {
            // Unknown or normal: put the untouched buffer back.
            data.rgba = img.into_raw();
            return;
        }
    };
    data.width = rotated.width();
    data.height = rotated.height();
    data.rgba = rotated.into_raw();
}
