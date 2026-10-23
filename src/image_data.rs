//! Module `image_data`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use image_data::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Decoded image data plus load timing metadata.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub load_time: std::time::Duration,
}

/// Per-channel levels plus clipped-pixel counts for one decoded picture.
///
/// Computed once, on the worker, from the preview-size buffer — a few hundred
/// kilobytes walked once per decode, never per frame — and carried with the
/// preview so the panel reads numbers rather than pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Histogram {
    /// Levels per channel: `bins[0]` is red, `bins[1]` green, `bins[2]` blue.
    pub bins: [[u32; 256]; 3],
    /// Pixels at exactly (0, 0, 0): detail the sensor clipped to black.
    pub black: u32,
    /// Pixels at exactly (255, 255, 255): detail clipped to white.
    pub white: u32,
}

impl Default for Histogram {
    // `[[u32; 256]; 3]` has no `Default` impl to derive, so this is written
    // out rather than derived.
    fn default() -> Self {
        Histogram {
            bins: [[0; 256]; 3],
            black: 0,
            white: 0,
        }
    }
}

impl Histogram {
    /// Pixels counted across all three channels' bins: the denominator the
    /// panel's clipping percentages are read against.
    pub fn counted(&self) -> u64 {
        self.bins[0].iter().map(|&n| n as u64).sum()
    }
}

/// Count levels and clipped pixels over an RGBA buffer. Alpha is ignored:
/// a translucent pixel still has a color, and the checkerboard behind it is
/// a display concern, not exposure data.
///
/// A short buffer (fewer than `w*h` pixels) counts what is there rather than
/// failing: the caller hands over whatever the decode produced, and a
/// histogram that refuses a truncated buffer is worse than a partial one.
pub fn histogram_rgba(rgba: &[u8], width: u32, height: u32) -> Histogram {
    let mut h = Histogram::default();
    let want = width as usize * height as usize;
    for px in rgba.chunks_exact(4).take(want) {
        h.bins[0][px[0] as usize] += 1;
        h.bins[1][px[1] as usize] += 1;
        h.bins[2][px[2] as usize] += 1;
        if px[0] == 0 && px[1] == 0 && px[2] == 0 {
            h.black += 1;
        } else if px[0] == 255 && px[1] == 255 && px[2] == 255 {
            h.white += 1;
        }
    }
    h
}

