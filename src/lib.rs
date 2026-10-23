//! Core logic for the image viewer, kept separate from the GUI so it can be
//! unit-tested, benchmarked, and fuzz-tested.


/// The sidecar path for an image, or `None` inside an archive.
///
/// Appends `.xmp` (`photo.png` becomes `photo.png.xmp`) so files sharing a
/// stem do not share a sidecar.
///
/// Lives here rather than in `ratings` so the archive refusal — the rule the
/// whole ratings UI depends on — sits beside `is_archive_path` and is pinned
/// by a scan against this file. Re-exported through `ratings` so the
/// `image_viewer::ratings::sidecar_path` path keeps working.
pub fn sidecar_path(image: &Path) -> Option<PathBuf> {
    if is_archive_path(image) {
        return None;
    }
    let mut s = image.as_os_str().to_os_string();
    s.push(".xmp");
    Some(PathBuf::from(s))
}

use nom_exif::{ExifTag, URational};
use serde::{Deserialize, Serialize};
use rayon::prelude::*;
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::Duration;

// Import GenericImageView to access dimensions() method
use image::{DynamicImage, GenericImageView, RgbaImage};

/// Quick low-res preview. With DCT-domain scaling this is a fraction of a full decode.
pub fn load_image_preview(path: &Path, preview_max_dim: u32) -> Result<ImageData, LoadError> {
    load_image(path, preview_max_dim.min(512))
}


// ---------------------------------------------------------------------------
// The modules
// ---------------------------------------------------------------------------
//
// This file was 13,047 lines - every rule in the crate in one file, which is a
// file nobody can hold in their head. Each topic is now a folder of modules
// under src/, re-exported here so image_viewer::name keeps working. See
// AGENTS.md and 	ests/source_size.rs, which holds the ceilings.

pub mod core;
pub use core::*;

pub mod color;
pub use color::*;

pub mod config;
pub use config::*;

pub mod system;
pub use system::*;

pub mod scan;
pub use scan::*;

pub mod renderer;
pub use renderer::*;

pub mod layout;
pub use layout::*;

pub mod resize;
pub use resize::*;

pub mod transform;
pub use transform::*;

pub mod decode;
pub use decode::*;

pub mod cache;
pub use cache::*;

pub mod watch;
pub use watch::*;

pub mod gallery;
pub use gallery::*;

