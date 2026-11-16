//! Configuration: the `Config` type, key bindings, and user commands.

// The implementation is config/config.rs; the name is the convention.
#[allow(clippy::module_inception)]
pub mod config;
pub use config::*;

pub mod commands;
pub use commands::*;
