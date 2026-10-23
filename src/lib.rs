//! Core logic for the image viewer, kept separate from the GUI so it can be
//! unit-tested, benchmarked, and fuzz-tested.

pub mod perf;

/// The application icon, drawn in code rather than committed as a binary.
/// See the module docs for why it lives here and not in `examples/`.
pub mod icon;

/// Star ratings, pick flags and tags, stored as XMP sidecars.
pub mod ratings;
pub use ratings::{Rating, Ratings};

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
// file nobody can hold in their head. Each section is now a module of its own,
// re-exported here so image_viewer::name keeps working. See AGENTS.md
// and tests/source_size.rs, which holds the ceilings.
pub mod crop_frame;
pub use crop_frame::*;
pub mod config;
pub use config::*;
pub mod commands;
pub use commands::*;
pub mod system;
pub use system::*;
pub mod scan;
pub use scan::*;
pub mod grid_geom;
pub use grid_geom::*;
pub mod renderer;
pub use renderer::*;
pub mod nav;
pub use nav::*;
pub mod spread_layout;
pub use spread_layout::*;
pub mod sorting;
pub use sorting::*;
pub mod image_data;
pub use image_data::*;
pub mod load_sched;
pub use load_sched::*;
pub mod resize;
pub use resize::*;
pub mod xform;
pub use xform::*;
pub mod crop;
pub use crop::*;
pub mod startup_view;
pub use startup_view::*;
pub mod save;
pub use save::*;
pub mod edits;
pub use edits::*;
pub mod colour;
pub use colour::*;
pub mod wide_gamut;
pub use wide_gamut::*;
pub mod exif;
pub use exif::*;
pub mod decode;
pub use decode::*;
pub mod animation;
pub use animation::*;
pub mod thumb_cache;
pub use thumb_cache::*;
pub mod watcher;
pub use watcher::*;
pub mod shell_thumb;
pub use shell_thumb::*;
pub mod spreads;
pub use spreads::*;
pub mod decode_page;
pub use decode_page::*;
pub mod resample;
pub use resample::*;
pub mod archives;
pub use archives::*;
pub mod manga;
pub use manga::*;
pub mod gallery;
pub use gallery::*;
pub mod zoom_gps;
pub use zoom_gps::*;

// The preview store on disk lives in its own module: the crate root was over the
// 1000-line target and this is the code that grew it. Re-exported so the
// names keep their place at the crate root.
mod preview_store;
pub use preview_store::*;
