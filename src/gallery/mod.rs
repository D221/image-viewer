//! The gallery: paging, the file list, per-image edits, archives, and saving.

// The implementation is gallery/gallery.rs; the name is the convention.
#[allow(clippy::module_inception)]
pub mod gallery;
pub use gallery::*;

pub mod edits;
pub use edits::*;

pub mod archives;
pub use archives::*;

pub mod save;
pub use save::*;
