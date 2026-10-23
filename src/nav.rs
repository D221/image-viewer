//! Module `nav`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use nav::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// One navigation input, **named** rather than answered.
///
/// The point of the type is that there is nowhere else to put the question. A caller
/// cannot say "is rightwards forwards?" - it can only say which input happened and
/// ask [`nav_is_forward`], so every navigation key in the app is handed to the same
/// function and a new key is a new line that has to name itself.
///
/// `Right`/`Left` are the arrow keys, and they are the only two variants that carry
/// spatial meaning. `Next`/`Previous` are the inputs with a *sequence* but no side -
/// `Space`, `PageDown`/`PageUp` and the mouse's forward/back buttons - and
/// `WheelDown` is neither: see [`nav_is_forward`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum NavInput {
    /// The right arrow.
    Right,
    /// The left arrow.
    Left,
    /// Something that means "onward" without saying which way: `Space`,
    /// `PageDown`, the mouse's forward button.
    Next,
    /// The same, backwards.
    Previous,
    /// A wheel scrolling down.
    WheelDown,
}

/// Whether a navigation input means *forwards* in reading order.
///
/// **This function is the whole of the manga key mapping, and there is deliberately
/// nowhere else for it to live.** OpenComic inverts the navigation keys for
/// right-to-left manga and its `mangaInvert` set omits `Space`, so **Space silently
/// becomes "previous page"** there: nothing errors, the arrows are right, and the
/// space bar quietly goes the wrong way. Every input therefore names itself as a
/// [`NavInput`] and comes here, so the set of inputs that can disagree is the set of
/// variants of one enum.
///
/// # The trade-off, stated rather than discovered later
///
/// With `manga_rtl` on, `Right` is *backwards* in a book that opens to the left, so
/// `→` means "next" in single-image mode and "backwards" in a manga spread. **That is
/// the price of matching the screen**, and it is what Mihon, Komga and Kavita all
/// ship: the reader's hand moves the way the page turns, rather than having to
/// remember that this one app is reversed. A setting that made `→` mean "next" in
/// both modes would be the alternative, and it is worse - it would make the arrows
/// lie about which page is on screen.
///
/// # Why the wheel is the exception
///
/// `WheelDown` is **always** forwards, under both settings, because it is the one
/// input with no spatial meaning to preserve. An arrow has a *side*, and a book that
/// opens to the left has to move the hand with it; a wheel delta has only a sign, and
/// a sign already means "onward" in both directions. Mihon excludes it for the same
/// reason. Inverting it would mean scrolling *up* to go forwards in manga mode, which
/// is not a reading convention anywhere.
pub fn nav_is_forward(input: NavInput, rtl: bool) -> bool {
    match input {
        // Direction-agnostic in both modes, for the reason in the docs.
        NavInput::WheelDown => true,
        // Rightward on screen: forwards in a left-to-right book, backwards in a
        // right-to-left one.
        NavInput::Right | NavInput::Next => !rtl,
        NavInput::Left | NavInput::Previous => rtl,
    }
}

/// Scale factor that fits an image of `natural` size into an `area`, keeping
/// the aspect ratio and never upscaling (1.0 means "draw at native size").
///
/// The gallery grid centres thumbnails with this: upscaling a small image to
/// fill a cell only makes it blurry, so a 40x30 picture stays 40x30 and is
/// simply centred in the middle of the cell. All sizes are in the same unit
/// (the caller works in points: texture texels / `pixels_per_point`).
pub fn fit_scale_no_upscale(natural: (f32, f32), area: (f32, f32)) -> f32 {
    let (w, h) = (natural.0.max(1.0), natural.1.max(1.0));
    if w <= 0.0 || h <= 0.0 || area.0 <= 0.0 || area.1 <= 0.0 {
        return 1.0;
    }
    (area.0 / w).min(area.1 / h).min(1.0)
}

/// Scale factor that makes an image **cover** an `area`, keeping the aspect
/// ratio and cropping the overflow rather than letterboxing.
///
/// The sibling of [`fit_scale_no_upscale`] with the comparison flipped: the
/// larger ratio wins, and upscale is allowed (a smaller image covering a
/// larger area is upscaled — that is what "fill" means). Same units, same
/// degenerate-input guard, same `(1e-4, 8.0)` clamp the single-image view
/// fits itself at.
pub fn fill_scale(natural: (f32, f32), area: (f32, f32)) -> f32 {
    let (w, h) = (natural.0.max(1.0), natural.1.max(1.0));
    if w <= 0.0 || h <= 0.0 || area.0 <= 0.0 || area.1 <= 0.0 {
        return 1.0;
    }
    (area.0 / w).max(area.1 / h).clamp(1e-4, 8.0)
}

