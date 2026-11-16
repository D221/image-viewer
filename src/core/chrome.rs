//! Chrome changes and the view.
//!
//! A panel toggle, a title-bar toggle or fullscreen changes how much room the
//! picture has without changing the picture. The view keeps its *fraction of
//! that room*: a fitted page stays fitted, a zoomed one stays as zoomed
//! relative to fit, and a pan holds the same content point in the same place
//! on the panel.

/// A view after a chrome change, in the same units as the caller's
/// `(scale, offset)` (`scale` is points per source pixel, `offset` points).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChromeView {
    pub scale: f32,
    pub offset: (f32, f32),
}

/// How far the window's chrome has to move, in points, before it counts as a
/// chrome change worth scaling the picture for. Sub-point layout jitter is
/// not a panel toggle; a real pane or status bar moves tens of points.
pub const CHROME_EPS_PTS: f32 = 1.0;

/// Did the chrome — the window's inner size minus the central panel — move
/// between two measurements?
///
/// `None` is the first measurement of a session and is never a change: there
/// is nothing to have changed from. The comparison is a distance rather than
/// one axis at a time so a pane on either side or a status bar on the bottom
/// is the same rule.
pub fn chrome_moved(prev: Option<(f32, f32)>, now: (f32, f32)) -> bool {
    let Some(prev) = prev else { return false };
    let (dx, dy) = (now.0 - prev.0, now.1 - prev.1);
    dx * dx + dy * dy > CHROME_EPS_PTS * CHROME_EPS_PTS
}

/// Rescale a view by the ratio the room changed by.
///
/// `prev_fit` and `new_fit` are the panel's fit scales before and after the
/// chrome changed, for the **same content** — the unchanged picture is what
/// makes their ratio the change in room. The result satisfies
/// `new_scale / new_fit == scale / prev_fit`, so a view at fit (or at twice
/// fit) stays at fit (or twice fit).
///
/// The offset is scaled by the same ratio: the picture moved by that factor,
/// so keeping the pan's fraction of the picture holds the same content point
/// in the same relative place. Anything non-finite or non-positive leaves the
/// view untouched — a chrome change must never turn a real scale into `NaN`
/// or zero.
pub fn rescale_for_chrome(
    scale: f32,
    offset: (f32, f32),
    prev_fit: f32,
    new_fit: f32,
) -> ChromeView {
    if !scale.is_finite()
        || !offset.0.is_finite()
        || !offset.1.is_finite()
        || !prev_fit.is_finite()
        || !new_fit.is_finite()
        || scale <= 0.0
        || prev_fit <= 0.0
        || new_fit <= 0.0
    {
        return ChromeView { scale, offset };
    }
    let ratio = new_fit / prev_fit;
    ChromeView { scale: scale * ratio, offset: (offset.0 * ratio, offset.1 * ratio) }
}
