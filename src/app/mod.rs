//! The binary's GUI: `MyApp`, egui panels, input handling, dialogs, and
//! Windows integration. Each file here used to be a top-level `app_*.rs`
//! file next to `main.rs`.

pub mod actions;
pub use actions::*;

pub mod central;
pub use central::*;

pub mod consts;
pub use consts::*;

pub mod edits;
pub use edits::*;

pub mod files;
pub use files::*;

pub mod frame;
pub use frame::*;

pub mod gallery_ops;
pub use gallery_ops::*;

pub mod goto;
pub use goto::*;

pub mod grid;
pub use grid::*;

pub mod main;
pub use main::*;

pub mod nav;
pub use nav::*;

pub mod open;
pub use open::*;

pub mod output;
pub use output::*;

pub mod paint;
pub use paint::*;

pub mod pump;
pub use pump::*;

pub mod save_as;
pub use save_as::*;

pub mod status;
pub use status::*;

pub mod model;
pub use model::*;

pub mod types;
pub use types::*;

pub mod ui;
pub use ui::*;

pub mod watch;
pub use watch::*;
