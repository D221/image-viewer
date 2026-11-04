//! Module `crop_frame`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use crop_frame::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// The shape a crop frame is held to while it is being dragged.
///
/// A curated list rather than an arbitrary ratio, because this is a dropdown in
/// the settings panel and the ratios people actually want are the ones a camera
/// or a phone shoots at. `#[serde(rename_all = "lowercase")]` so the config
/// spelling matches the labels — the same trap `WgpuBackend` fell into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CropAspect {
    /// Whatever the drag makes. The original behaviour, and the default: a crop
    /// that locks a shape before anybody asked for one is a worse default than
    /// one that does not.
    #[default]
    Free,
    /// 1:1.
    Square,
    /// 16:9 — a phone held sideways.
    Widescreen,
    /// 9:16 — a phone held upright.
    Portrait,
    /// 4:3.
    Classic,
    /// 3:4.
    TallClassic,
    /// 3:2 — a 35 mm frame.
    Photo,
    /// 2:3.
    TallPhoto,
    /// The shape of the image as it is displayed, so the frame starts at the
    /// edges of the picture whatever its orientation or any turn applied to it.
    Source,
}

impl CropAspect {
    /// Every option, in the order the dropdown lists them.
    pub const ALL: [CropAspect; 9] = [
        CropAspect::Free,
        CropAspect::Square,
        CropAspect::Widescreen,
        CropAspect::Portrait,
        CropAspect::Classic,
        CropAspect::TallClassic,
        CropAspect::Photo,
        CropAspect::TallPhoto,
        CropAspect::Source,
    ];

    /// What the dropdown says. The number is in the label because "Square" and
    /// "1:1" are not obviously the same intent to somebody reaching for either.
    pub fn label(self) -> &'static str {
        match self {
            CropAspect::Free => "Free",
            CropAspect::Square => "Square 1:1",
            CropAspect::Widescreen => "Widescreen 16:9",
            CropAspect::Portrait => "Portrait 9:16",
            CropAspect::Classic => "Classic 4:3",
            CropAspect::TallClassic => "Tall 3:4",
            CropAspect::Photo => "Photo 3:2",
            CropAspect::TallPhoto => "Tall photo 2:3",
            CropAspect::Source => "Same as image",
        }
    }

    /// The ratio to hold a frame to, as width over height, or `None` for free.
    ///
    /// `display` is the image's **displayed** size in its own pixels — after the
    /// EXIF orientation and any turn — because the frame is dragged on what is on
    /// screen. `Source` therefore follows the picture as it appears, which is the
    /// only version of "the same shape" that means anything to the person
    /// dragging: a 3:2 photo turned on its side is 2:3 on screen, and a frame
    /// that insisted on 3:2 would not fit it.
    pub fn ratio(self, display: (u32, u32)) -> Option<f32> {
        let (w, h) = (display.0 as f32, display.1 as f32);
        let (nw, nh) = match self {
            CropAspect::Free => return None,
            CropAspect::Square => (1.0, 1.0),
            CropAspect::Widescreen => (16.0, 9.0),
            CropAspect::Portrait => (9.0, 16.0),
            CropAspect::Classic => (4.0, 3.0),
            CropAspect::TallClassic => (3.0, 4.0),
            CropAspect::Photo => (3.0, 2.0),
            CropAspect::TallPhoto => (2.0, 3.0),
            CropAspect::Source => {
                if w <= 0.0 || h <= 0.0 {
                    return None;
                }
                (w, h)
            }
        };
        Some(nw / nh)
    }
}

/// Hold a dragged crop frame to `aspect` (width over height), anchored at the
/// press point.
///
/// Returns `(x0, y0, x1, y1)` in the same space it was given. The axis the
/// pointer leads is kept and the **other** one is the one that gives — drag
/// right and down for a wide frame and the height follows the width. That is the
/// convention every editor uses; letting both axes chase the pointer cannot be
/// hit reliably with a mouse.
///
/// `None` is a no-op, and so is a frame with no width or no height: a rectangle
/// with nothing to it has no aspect to hold, and the crop refuses it downstream
/// rather than the frame jumping the instant the drag cleared egui's threshold.
///
/// The result is not clamped to the image here. The caller does that, and a frame
/// anchored near an edge will come out slightly the wrong shape for the same
/// reason a saved crop comes out up to one MCU block bigger than the drag: the
/// rectangle that actually gets cut is the one inside the picture.
pub fn constrain_to_aspect(
    anchor: (f32, f32),
    corner: (f32, f32),
    aspect: Option<f32>,
) -> (f32, f32, f32, f32) {
    let (ax, ay) = anchor;
    let (mut cx, mut cy) = corner;
    let free = || (ax.min(cx), ay.min(cy), ax.max(cx), ay.max(cy));
    let Some(r) = aspect.filter(|r| r.is_finite() && *r > 0.0) else {
        return free();
    };
    // No special case for a flat drag. It looks like it needs one — `dx / dy` is
    // a division by zero — but every branch lands on the same answer: a zero on
    // one side makes the other side zero too, so the frame collapses to nothing
    // and the crop refuses it. A guard here would be a branch that cannot change
    // an output, which is worse than no branch, because it reads as protection
    // that is not there.
    let (mut dx, mut dy) = ((cx - ax).abs(), (cy - ay).abs());
    if dx / dy > r {
        dx = dy * r;
    } else {
        dy = dx / r;
    }
    // Keep the direction the pointer went: dragging up and left has to frame the
    // same rectangle as dragging down and right, or the same crop needs two
    // different drags depending on which corner you started from.
    cx = if corner.0 < anchor.0 { ax - dx } else { ax + dx };
    cy = if corner.1 < anchor.1 { ay - dy } else { ay + dy };
    (ax.min(cx), ay.min(cy), ax.max(cx), ay.max(cy))
}

/// A crop-mode drag, decided **once, on the press**, and then answered from on
/// every frame the button is held.
///
/// **The decision cannot be made per frame.** `InputState::pointer.primary_pressed`
/// is true for exactly one frame, so a per-frame "was the press inside the
/// frame?" test either starts a move and loses it one frame later, or — the bug
/// this replaced — fails the test on every held frame and falls through to the
/// draw arm, which paints a brand-new frame from the press point instead of
/// carrying the old one. So the answer is latched, and this type is what gets
/// latched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropDrag {
    /// Nothing was under the press: drag a new frame out of the press point.
    Draw { anchor: (f32, f32) },
    /// The press landed inside an existing frame: carry it. `base` is where the
    /// frame sat when the button went down and `grab` is where the press landed,
    /// both in screen points.
    Move { grab: (f32, f32), base: [f32; 4] },
}

/// What a crop drag means this frame: a new frame to draw from the press point,
/// or where the existing frame has been carried to.
///
/// Rectangles are `[x0, y0, x1, y1]`, the same shape [`crate::dim_bands`] and
/// [`translate_clamped`] take, so the app can convert to and from `egui::Rect`
/// at one edge instead of everywhere.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CropFrame {
    Draw { anchor: (f32, f32), corner: (f32, f32) },
    Move([f32; 4]),
}

impl CropDrag {
    /// Latch the gesture on the press.
    ///
    /// `inside` is the existing frame's screen rectangle when the press landed
    /// inside it, or `None` when it did not. `press` is where the button went
    /// down, which is the anchor for a draw and the grab point for a move.
    pub fn at_press(inside: Option<[f32; 4]>, press: (f32, f32)) -> Self {
        match inside {
            Some(base) => CropDrag::Move { grab: press, base },
            None => CropDrag::Draw { anchor: press },
        }
    }

    /// The frame this drag describes with the pointer at `pointer`. `outer` is
    /// the picture's screen rectangle, which a moved frame is held inside.
    pub fn frame(&self, pointer: (f32, f32), outer: [f32; 4]) -> CropFrame {
        match *self {
            CropDrag::Draw { anchor } => CropFrame::Draw { anchor, corner: pointer },
            CropDrag::Move { grab, base } => {
                // From the **latched** base, never from the frame as it stands
                // this frame: `pointer - grab` is the whole drag so far, so
                // adding it to a frame that already moved would apply it again
                // every frame and the frame would run away from the pointer.
                let delta = (pointer.0 - grab.0, pointer.1 - grab.1);
                CropFrame::Move(translate_clamped(base, delta, outer))
            }
        }
    }
}

/// Move `frame` by `delta`, stopping at the edges of `outer` **without changing
/// its size**.
///
/// Clamping by intersecting instead — which the first version of crop-moving
/// did — shrinks the frame the moment it is dragged past the picture edge, so a
/// single overshoot loses part of the crop and the loss is invisible until the
/// save. Sliding is what every editor does, and what keeps the frame the shape
/// the user framed.
///
/// A frame already bigger than `outer` cannot be made to fit by moving it, so
/// that axis is left where it is rather than collapsed to an edge.
pub fn translate_clamped(frame: [f32; 4], delta: (f32, f32), outer: [f32; 4]) -> [f32; 4] {
    let (fx0, fy0, fx1, fy1) = (frame[0], frame[1], frame[2], frame[3]);
    let (ox0, oy0, ox1, oy1) = (outer[0], outer[1], outer[2], outer[3]);
    // `f32::clamp` panics on an inverted range, which is exactly what an
    // oversized frame produces, so the slide is written out.
    let slide = |d: f32, lo: f32, hi: f32| if lo <= hi { d.clamp(lo, hi) } else { 0.0 };
    let dx = slide(delta.0, ox0 - fx0, ox1 - fx1);
    let dy = slide(delta.1, oy0 - fy0, oy1 - fy1);
    [fx0 + dx, fy0 + dy, fx1 + dx, fy1 + dy]
}
