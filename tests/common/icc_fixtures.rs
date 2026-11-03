//! Synthesises minimal-but-valid ICC profiles and embeds them in a JPEG (APP2
//! `ICC_PROFILE`) or a PNG (`iCCP`), so the colour detector can be tested with
//! no binary fixtures — per the AGENTS.md rule.
//!
//! The primaries are the real, measured ones: sRGB `28578,14581,912` and
//! Display P3 `33759,15807,-69` (both D50-adapted), taken from 1001 real files.

// Each test binary uses a subset of these.
#![allow(dead_code)]

/// Red primary, D50-adapted, 15-bit XYZ. The two values that actually occur in
/// the wild, and which the classifier keys off.
pub const SRGB_RED: (i32, i32, i32) = (28578, 14581, 912);
pub const P3_RED: (i32, i32, i32) = (33759, 15807, -69);

/// A minimal ICC profile carrying a `desc` tag and an `rXYZ` tag.
///
/// `desc` is optional so the primaries-only classification path can be tested.
/// `pad_to` inflates the profile with a filler tag, to exercise the JPEG
/// multi-chunk reassembly path.
pub fn icc_profile(desc: Option<&str>, red: (i32, i32, i32), pad_to: usize) -> Vec<u8> {
    // Tag payloads, built first so their sizes are known.
    //   v2 'desc': sig(4) reserved(4) length(4) ascii+NUL, padded to 4.
    let mut desc_data: Vec<u8> = Vec::new();
    if let Some(s) = desc {
        let mut ascii = s.as_bytes().to_vec();
        ascii.push(0);
        desc_data.extend_from_slice(b"desc");
        desc_data.extend_from_slice(&0u32.to_be_bytes());
        desc_data.extend_from_slice(&(ascii.len() as u32).to_be_bytes());
        desc_data.extend_from_slice(&ascii);
        while desc_data.len() % 4 != 0 {
            desc_data.push(0);
        }
    }
    //   'XYZ ': sig(4) reserved(4) then three s15Fixed16Number.
    let mut xyz_data: Vec<u8> = Vec::new();
    xyz_data.extend_from_slice(b"XYZ ");
    xyz_data.extend_from_slice(&0u32.to_be_bytes());
    for v in [red.0, red.1, red.2] {
        xyz_data.extend_from_slice(&v.to_be_bytes());
    }
    //   filler, to push the profile past one APP2 segment.
    let mut pad_data: Vec<u8> = Vec::new();
    if pad_to > 8 {
        pad_data.extend_from_slice(b"text");
        pad_data.extend_from_slice(&0u32.to_be_bytes());
        pad_data.resize(pad_to, b' ');
    }

    let tags: Vec<(&[u8; 4], &Vec<u8>)> =
        [(b"desc" as &[u8; 4], &desc_data), (b"rXYZ", &xyz_data), (b"text", &pad_data)]
            .into_iter()
            .filter(|(_, d)| !d.is_empty())
            .collect();

    // 128-byte header, u32 tag count, then a 12-byte-per-tag table.
    let table_len = 132 + tags.len() * 12;
    let mut cursor = (table_len + 3) & !3;
    let mut placed: Vec<(&[u8; 4], usize, usize)> = Vec::new();
    for (sig, d) in &tags {
        placed.push((*sig, cursor, d.len()));
        cursor += (d.len() + 3) & !3;
    }

    let mut out = vec![0u8; table_len];
    // Header.
    out[8] = 2; // profile version 2.1
    out[9] = 0x10;
    out[12..16].copy_from_slice(b"mntr"); // device class: display
    out[16..20].copy_from_slice(b"RGB "); // data colour space
    out[20..24].copy_from_slice(b"XYZ "); // PCS
    out[36..40].copy_from_slice(b"acsp"); // profile file signature
                                          // D50 illuminant, as ICC profiles store it.
    out[68..72].copy_from_slice(&0x0000_F6D6u32.to_be_bytes());
    out[72..76].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    out[76..80].copy_from_slice(&0x0000_D32Du32.to_be_bytes());
    // Tag count and table.
    out[128..132].copy_from_slice(&(placed.len() as u32).to_be_bytes());
    for (i, (sig, off, size)) in placed.iter().enumerate() {
        let e = 132 + i * 12;
        out[e..e + 4].copy_from_slice(*sig);
        out[e + 4..e + 8].copy_from_slice(&(*off as u32).to_be_bytes());
        out[e + 8..e + 12].copy_from_slice(&(*size as u32).to_be_bytes());
    }
    // Pad up to the *first* payload offset, then append the payloads in the
    // same order the table lists them. (Padding to the final cursor instead
    // would leave a zero gap in front of the first tag's data.)
    out.resize((table_len + 3) & !3, 0);
    for (_, d) in &tags {
        out.extend_from_slice(d);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    let size = (out.len() as u32).to_be_bytes();
    out[0..4].copy_from_slice(&size);
    out
}

/// A JPEG of `w x h` carrying `profile` in APP2 `ICC_PROFILE` chunks.
pub fn jpeg_with_icc(w: u32, h: u32, profile: &[u8]) -> Vec<u8> {
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 251) as u8, (y % 241) as u8, 64, 255]);
    }
    let mut main = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut main), image::ImageFormat::Jpeg)
        .expect("encode jpeg");

    // APP2 payloads hold at most 65533 bytes, and a real profile over ~64 KB is
    // split across numbered chunks. Split at 60000 so the test exercises it.
    const CHUNK: usize = 60_000;
    let mut segs: Vec<Vec<u8>> = Vec::new();
    let total = profile.len().div_ceil(CHUNK).max(1);
    for (i, part) in profile.chunks(CHUNK).enumerate() {
        let mut d = Vec::with_capacity(part.len() + 18);
        d.extend_from_slice(b"ICC_PROFILE\0");
        d.push((i + 1) as u8);
        d.push(total as u8);
        d.extend_from_slice(part);
        let mut seg = vec![0xFF, 0xE2];
        seg.extend_from_slice(&((d.len() + 2) as u16).to_be_bytes());
        seg.extend_from_slice(&d);
        segs.push(seg);
    }

    let mut out = Vec::new();
    out.extend_from_slice(&main[0..2]); // SOI
    for s in &segs {
        out.extend_from_slice(s);
    }
    out.extend_from_slice(&main[2..]);
    out
}

/// A PNG of `w x h` carrying `profile` in a zlib-compressed `iCCP` chunk.
/// PNG requires the profile to be compressed, which is exactly the detail that
/// made the detector report "unknown" before it was handled.
pub fn png_with_iccp(w: u32, h: u32, profile: &[u8], keyword: &str) -> Vec<u8> {
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 251) as u8, (y % 241) as u8, 64, 255]);
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode png");

    use std::io::Write;
    let mut packed = Vec::new();
    flate2::write::ZlibEncoder::new(&mut packed, flate2::Compression::default())
        .write_all(profile)
        .unwrap();

    let mut data = keyword.as_bytes().to_vec();
    data.push(0);
    data.push(0); // compression method
    data.extend_from_slice(&packed);

    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"iCCP");
    chunk.extend_from_slice(&data);
    let mut crc_input = b"iCCP".to_vec();
    crc_input.extend_from_slice(&data);
    chunk.extend_from_slice(&crc32(&crc_input).to_be_bytes());

    // Insert before the first IDAT.
    let idat = png.windows(4).position(|w| w == b"IDAT").expect("png has IDAT");
    let mut out = png[..idat].to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[idat..]);
    out
}

/// PNG with a plain `sRGB` chunk and no profile.
pub fn png_with_srgb_chunk(w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbaImage::new(w, h);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([(x % 251) as u8, (y % 241) as u8, 64, 255]);
    }
    let mut png = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode png");
    // 'rendering intent' 0 = perceptual
    let data = [0u8];
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"sRGB");
    chunk.extend_from_slice(&data);
    let mut crc_input = b"sRGB".to_vec();
    crc_input.extend_from_slice(&data);
    chunk.extend_from_slice(&crc32(&crc_input).to_be_bytes());

    let idat = png.windows(4).position(|w| w == b"IDAT").unwrap();
    let mut out = png[..idat].to_vec();
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png[idat..]);
    out
}

/// CRC-32 as PNG defines it.
fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, e) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c = table[((c ^ *b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}
