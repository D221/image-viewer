//! System integration: single-instance guard, file associations, ratings,
//! the generated icon, and latency counters.

// The implementation is system/system.rs; the name is the convention.
#[allow(clippy::module_inception)]
pub mod system;
pub use system::*;

pub mod fullscreen;
pub use fullscreen::*;

pub mod fileassoc;
pub use fileassoc::*;

pub mod ratings;
pub use ratings::{Rating, Ratings};

pub mod icon;
pub use icon::*;

pub mod perf;
pub use perf::*;
