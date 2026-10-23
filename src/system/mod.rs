//! System integration: single-instance guard, file associations, ratings,
//! the generated icon, and latency counters.

pub mod system;
pub use system::*;

pub mod fileassoc;
pub use fileassoc::*;

pub mod ratings;
pub use ratings::{Rating, Ratings};

pub mod icon;
pub use icon::*;

pub mod perf;
pub use perf::*;
