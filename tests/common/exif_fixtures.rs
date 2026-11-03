//! Builds a JPEG containing a synthetic APP1/Exif block with a compressed
//! thumbnail in IFD1, so the embedded-thumbnail path can be tested against every
//! orientation and both byte orders without shipping binary fixtures.
#![allow(dead_code)]

/// Byte order of the synthetic TIFF block.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Endian {
    Little,
    Big,
}

impl Endian {
    fn u16(self, v: u16) -> [u8; 2] {
        match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        }
    }
    fn u32(self, v: u32) -> [u8; 4] {
        match self {
            Endian::Little => v.to_le_bytes(),
            Endian::Big => v.to_be_bytes(),
        }
    }
    fn magic(self) -> [u8; 2] {
        match self {
            Endian::Little => *b"II",
            Endian::Big => *b"MM",
        }
    }
}

/// Accumulates IFD entries, then lays them out with a fixed geometry so the
/// thumbnail always lands at a known TIFF-relative offset.
struct TiffBuilder {
    e: Endian,
    ifd0: Vec<u8>,
    ifd1: Vec<u8>,
}

impl TiffBuilder {
    fn new(e: Endian) -> Self {
        TiffBuilder { e, ifd0: Vec::new(), ifd1: Vec::new() }
    }

    /// One 12-byte IFD entry. `value` is the raw 4-byte value field.
    fn push(&mut self, ifd1: bool, tag: u16, ty: u16, count: u32, value: [u8; 4]) {
        let mut ent = Vec::with_capacity(12);
        ent.extend_from_slice(&self.e.u16(tag));
        ent.extend_from_slice(&self.e.u16(ty));
        ent.extend_from_slice(&self.e.u32(count));
        ent.extend_from_slice(&value);
        if ifd1 {
            self.ifd1.extend_from_slice(&ent);
        } else {
            self.ifd0.extend_from_slice(&ent);
        }
    }

    /// SHORT (type 3), count 1 — the value occupies the first two bytes.
    fn short(&mut self, ifd1: bool, tag: u16, v: u16) {
        let mut val = [0u8; 4];
        val[..2].copy_from_slice(&self.e.u16(v));
        self.push(ifd1, tag, 3, 1, val);
    }

    /// LONG (type 4), count 1 — value fills all four bytes.
    fn long(&mut self, ifd1: bool, tag: u16, v: u32) {
        self.push(ifd1, tag, 4, 1, self.e.u32(v));
    }

    /// Serialise, appending `thumb` at TIFF offset 68. Returns the block from
    /// the TIFF header onwards, and the offset the IFD1 pointer advertises.
    fn finish(self, thumb: &[u8], thumb_offset_advertised: u32) -> Vec<u8> {
        let e = self.e;
        let n0 = (self.ifd0.len() / 12) as u16;
        let n1 = (self.ifd1.len() / 12) as u16;
        // header(8) + IFD0(2 + entries + 4) + IFD1(2 + entries + 4)
        let ifd0_off: u32 = 8;
        let ifd1_off: u32 = ifd0_off + 2 + u32::from(n0) * 12 + 4;
        let thumb_at: u32 = ifd1_off + 2 + u32::from(n1) * 12 + 4;

        let mut t = Vec::new();
        t.extend_from_slice(&e.magic());
        t.extend_from_slice(&e.u16(0x2A));
        t.extend_from_slice(&e.u32(ifd0_off));
        t.extend_from_slice(&e.u16(n0));
        t.extend_from_slice(&self.ifd0);
        t.extend_from_slice(&e.u32(ifd1_off));
        t.extend_from_slice(&e.u16(n1));
        t.extend_from_slice(&self.ifd1);
        t.extend_from_slice(&e.u32(0)); // IFD1 next = 0
        assert_eq!(
            t.len() as u32,
            thumb_at,
            "fixture geometry drifted: thumbnail would not sit at 68"
        );
        t.extend_from_slice(thumb);
        // The caller advertises `thumb_offset_advertised` in IFD1; if it wanted
        // something else, the bytes after `thumb` are irrelevant, so nothing to
        // do here beyond keeping the signature honest.
        let _ = thumb_offset_advertised;
        t
    }
}

/// Build a JPEG of `w x h` carrying an EXIF block with `thumb` embedded.
///
/// `thumb_at_override` lets a test advertise a bogus thumbnail offset, to
/// exercise the bounds checks on untrusted input.
pub fn jpeg_with_thumbnail(
    w: u32,
    h: u32,
    orientation: u16,
    thumb: &[u8],
    endian: Endian,
    thumb_at_override: Option<u32>,
) -> Vec<u8> {
    let main = jpeg(w, h);

    // Exactly one IFD0 entry (Orientation) and three IFD1 entries
    // (Compression, ThumbnailOffset, ThumbnailLength) => thumbnail at 68.
    let mut b = TiffBuilder::new(endian);
    b.short(false, 0x0112, orientation);
    b.short(true, 0x0103, 6); // Compression = old-style JPEG
    b.long(true, 0x0201, thumb_at_override.unwrap_or(68));
    b.long(true, 0x0202, thumb.len() as u32);
    let tiff = b.finish(thumb, thumb_at_override.unwrap_or(68));

    let mut app1 = Vec::new();
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);
    let mut seg = Vec::new();
    seg.extend_from_slice(&[0xFF, 0xE1]);
    seg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    seg.extend_from_slice(&app1);

    let mut out = Vec::with_capacity(main.len() + seg.len());
    out.extend_from_slice(&main[0..2]); // SOI
    out.extend_from_slice(&seg);
    out.extend_from_slice(&main[2..]);
    out
}

/// A small JPEG with a visible gradient, usable as a thumbnail body.
pub fn jpeg(w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([
            (x.wrapping_mul(3) % 251) as u8,
            (y.wrapping_mul(5) % 241) as u8,
            64,
            255,
        ]);
    }
    let mut v = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut v), image::ImageFormat::Jpeg)
        .expect("encode jpeg");
    v
}
