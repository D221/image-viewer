//! Module `app_main`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.
//!
//! This file used to hold the wgpu backend plumbing: `WgpuBackend`, the
//! per-process probe that chose one, and the restart slot that carried the
//! choice back to a launch loop. All of it went with wgpu - the app renders on
//! glow and nothing chooses a backend any more - leaving the loader knobs it
//! had been keeping company with.

use crate::*;

// ---------------------------------------------------------------------------
// Background loader: a tiny priority queue served by a few worker threads
// ---------------------------------------------------------------------------
//
// `Kind`, `Job` and `Loader` live in the library (see `lib.rs`, "Load
// scheduling") so the queue can be tested directly. The stranding bug that
// emptied this queue lived in the interaction between the plan and the queue,
// and neither half could be reached from a test while it lived in a binary.

pub(crate) const PRIO_THUMB: u32 = PRIO_THUMB_BASE;
pub(crate) const PRIO_EXIF: u32 = PRIO_CURRENT_EXIF;

/// Long side of grid thumbnails.
pub(crate) const THUMB_DIM: u32 = 256;
/// Max grid thumbnails kept on the GPU (256 x ~0.3 MB, so ~80 MB).
pub(crate) const THUMB_CACHE_CAP: usize = 512;
/// Decode attempts per file before a grid cell gives up (permanent
/// placeholder). Enough for transient failures, bounded so a corrupt file
/// can't keep the worker pool busy.
pub(crate) const THUMB_TRIES: u8 = 3;
