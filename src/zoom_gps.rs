//! Module `zoom_gps`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use zoom_gps::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Zoom and pan state for the image viewport.
#[derive(Debug, Clone)]
pub struct ZoomPan {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ZoomPan {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoomPan {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }

    /// Apply a scroll delta to zoom, clamped to [0.1, 8.0].
    pub fn zoom_by(&mut self, delta: f32) {
        self.zoom = (self.zoom + delta * 0.01).clamp(0.1, 8.0);
    }

    /// Reset to fit-to-screen defaults.
    pub fn reset(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Fit the image to the available window area, maintaining aspect ratio.
    /// Always applies the fit scale (downscales large images, upscales small ones).
    pub fn fit_to_window(&mut self, tex_w: f32, tex_h: f32, avail_w: f32, avail_h: f32) {
        let scale_x = avail_w / tex_w;
        let scale_y = avail_h / tex_h;
        self.zoom = scale_x.min(scale_y).clamp(0.1, 8.0);
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }
}

/// GPS latitude/longitude in signed decimal degrees (N/E positive).
/// DMS rationals [deg, min, sec] combined with the N/S, E/W ref tags.
pub fn gps_coords(exif: &nom_exif::Exif) -> Option<(f64, f64)> {
    let lat = dms_to_deg(exif.get(ExifTag::GPSLatitude).and_then(|e| e.as_urational_slice()))?;
    let lon = dms_to_deg(exif.get(ExifTag::GPSLongitude).and_then(|e| e.as_urational_slice()))?;
    let lat_ref = exif
        .get(ExifTag::GPSLatitudeRef)
        .and_then(|v| v.as_str().map(|s| s.trim().to_ascii_uppercase()))
        .unwrap_or_else(|| "N".into());
    let lon_ref = exif
        .get(ExifTag::GPSLongitudeRef)
        .and_then(|v| v.as_str().map(|s| s.trim().to_ascii_uppercase()))
        .unwrap_or_else(|| "E".into());
    let lat = if lat_ref.starts_with('S') { -lat } else { lat };
    let lon = if lon_ref.starts_with('W') { -lon } else { lon };
    Some((lat, lon))
}

/// [deg, min, sec] rationals -> decimal degrees.
pub fn dms_to_deg(parts: Option<&[URational]>) -> Option<f64> {
    let parts = parts?;
    if parts.len() != 3 {
        return None;
    }
    let d = parts[0].to_f64()?;
    let m = parts[1].to_f64()?;
    let s = parts[2].to_f64()?;
    Some(d + m / 60.0 + s / 3600.0)
}

/// GPS altitude in meters (below-sea-level encoded as negative by the ref tag).
pub fn gps_altitude(exif: &nom_exif::Exif) -> Option<f64> {
    let alt = exif
        .get(ExifTag::GPSAltitude)
        .and_then(|e| e.as_urational())
        .and_then(|r| r.to_f64())?;
    let below = exif
        .get(ExifTag::GPSAltitudeRef)
        .and_then(|v| v.as_u8())
        .unwrap_or(0);
    Some(if below == 1 { -alt } else { alt })
}
