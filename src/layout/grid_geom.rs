//! Module `grid_geom`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use grid_geom::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

/// Side of one gallery-grid (G) thumbnail. The shipped size and the default
/// every config without the key comes up on; the live value is
/// [`Config::grid_cell`], which this only feeds.
pub const GRID_CELL: f32 = 150.0;
/// Label strip reserved under each grid thumbnail for the file name.
pub const GRID_LABEL_H: f32 = 20.0;
/// Vertical pitch of one grid row for a cell size: cell + label + inter-row
/// padding. A function rather than a constant so there is one cell size in
/// the app and not a size plus a pitch that can disagree.
pub fn grid_row_h(cell: f32) -> f32 {
    cell + GRID_LABEL_H + 8.0
}

/// How many grid cells fit across a display area `panel_w` points wide.
/// Cells are laid out with egui's default 8pt item spacing, hence the +8.
pub fn grid_cols(panel_w: f32, cell: f32) -> usize {
    ((panel_w / (cell + 8.0)).floor() as usize).clamp(1, 24)
}

/// Where the grid's scroll offset should go when the selection moves, or
/// `None` for "leave it alone" — the file-picker rule.
///
/// A visible selection never moves the view; a selection outside it scrolls
/// the minimum distance to become fully visible (row bottom to view bottom
/// when below, row top to view top when above). Edges count as visible, so a
/// row exactly filling the bottom edge does not jitter the view by a pixel —
/// without that, every Down press at the edge would scroll and scroll back.
///
/// `view_h <= 0` means the viewport was never measured (the grid just
/// opened), so there is no visible range to test against: pin the row to the
/// top, which is also what opening on a deep selection has always done.
pub fn grid_follow_offset(sel_row: usize, pitch: f32, view_h: f32, cur_offset: f32) -> Option<f32> {
    let row_top = sel_row as f32 * pitch;
    if view_h <= 0.0 {
        return Some(row_top);
    }
    let row_bottom = row_top + pitch;
    if row_bottom > cur_offset + view_h {
        Some((row_bottom - view_h).max(0.0))
    } else if row_top < cur_offset {
        Some(row_top.max(0.0))
    } else {
        None
    }
}
