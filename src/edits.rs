//! Module `edits`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use edits::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------------------
// Per-image transform
// ---------------------------------------------------------------------------

/// What the user has done to one image: a crop, a turn, mirrors. All of it.
///
/// Crop and transform live in **one** entry rather than two maps for a practical
/// reason: they are saved by the same action, and "Reset" should mean all of it.
/// Two maps means a state where the crop is cleared and the turn is not, or where
/// one of them is bounded and the other is not, and neither state is one anybody
/// asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edit {
    /// Quarter turns and mirrors, composed as they were pressed.
    pub xform: Xform,
    /// Rectangle in **stored** coordinates, already snapped to the block grid, or
    /// `None` for no crop.
    pub crop: Option<Crop>,
}

impl Edit {
    /// Is this edit a no-op? If so it should not be stored, and the save should
    /// behave exactly as it did before the edit existed.
    pub fn is_identity(&self) -> bool {
        self.xform == Xform::Identity && self.crop.is_none()
    }

    /// A short description for the status line and the save confirmation.
    pub fn label(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(c) = &self.crop {
            parts.push(format!("cropped to {}", c.label()));
        }
        if self.xform != Xform::Identity {
            parts.push(self.xform.label().to_owned());
        }
        if parts.is_empty() {
            "no transform".to_owned()
        } else {
            parts.join(", ")
        }
    }
}

/// The view transform and crop the user has applied to each image, this session.
///
/// **Per image, not per app**, which is the whole point. This used to be a single
/// `u8` on the app struct, so turning one photo turned every photo you then
/// navigated to, and navigating back showed the original at whatever rotation
/// happened to be current. No real viewer does that — Windows Photos, FastStone,
/// IrfanView and Preview all keep the edit per image and keep it when you return —
/// and for an app whose job is showing a picture faithfully it is worse than
/// merely wrong, because there is no way to tell "I rotated this" from "this is
/// broken".
///
/// The value is a whole [`Xform`] rather than a turn count, because a mirror is
/// the same *kind* of edit as a rotation and the two do not commute: turn a photo
/// then mirror it and you have asked for a diagonal mirror, which is neither
/// operation. Composing on the way in means this map only ever holds finished
/// transforms, so display, the lossless save and the tests all read one value and
/// there is no second place for the two halves to disagree.
///
/// Not persisted: this is a view transform until it is saved, and it is lost on
/// restart like every other view state.
#[derive(Debug, Clone)]
pub struct ImageEdits {
    map: std::collections::HashMap<std::path::PathBuf, Edit>,
    cap: usize,
}

impl Default for ImageEdits {
    fn default() -> Self {
        Self {
            map: std::collections::HashMap::new(),
            cap: 4096,
        }
    }
}

impl ImageEdits {
    /// The whole edit for `path`. [`Edit::default`] when untouched.
    pub fn get(&self, path: &Path) -> Edit {
        self.map.get(path).copied().unwrap_or_default()
    }

    /// Just the transform, which is what the draw path and most callers want.
    pub fn xform(&self, path: &Path) -> Xform {
        self.get(path).xform
    }

    /// Just the crop.
    pub fn crop(&self, path: &Path) -> Option<Crop> {
        self.get(path).crop
    }

    /// Store `path`'s edit. A no-op removes the entry rather than storing it, so
    /// an untouched image costs nothing.
    pub fn set(&mut self, path: &Path, edit: Edit) {
        if edit.is_identity() {
            self.map.remove(path);
        } else {
            if self.map.len() >= self.cap {
                // Wholesale rather than evicting one entry: the map is a record
                // of a session's view state, and a viewer left open on a very
                // large folder must not grow it without bound. Losing edits is a
                // far smaller problem than unbounded growth.
                self.map.clear();
            }
            self.map.insert(path.to_path_buf(), edit);
        }
    }

    /// Replace just the crop, leaving any transform alone.
    pub fn set_crop(&mut self, path: &Path, crop: Option<Crop>) {
        let mut e = self.get(path);
        e.crop = crop;
        self.set(path, e);
    }

    /// Drop the crop, leaving any transform alone.
    pub fn clear_crop(&mut self, path: &Path) {
        self.set_crop(path, None);
    }

    /// Compose `extra` onto whatever transform `path` already has, and store the
    /// result. Any crop is left untouched — turning a picture does not move the
    /// frame around it.
    ///
    /// `extra` goes *first*: the button you just pressed is the innermost edit, so
    /// pressing rotate-then-mirror really does mean "rotate, then mirror". That
    /// ordering is invisible in the final picture — both orders are valid pictures
    /// — but it is what makes the buttons feel like they do what they say, and it
    /// is why this is written as one function rather than three setters.
    fn compose(&mut self, path: &Path, extra: Xform) -> Xform {
        let mut e = self.get(path);
        e.xform = extra.then(e.xform);
        self.set(path, e);
        e.xform
    }

    /// `path` turned by `steps` quarter turns clockwise (negative = the other way).
    pub fn turn(&mut self, path: &Path, steps: i32) -> Xform {
        self.compose(path, Xform::quarters_cw(steps.rem_euclid(4) as u8))
    }

    /// `path` mirrored left-to-right.
    pub fn mirror_horizontal(&mut self, path: &Path) -> Xform {
        self.compose(path, Xform::FlipH)
    }

    /// `path` mirrored top-to-bottom.
    pub fn mirror_vertical(&mut self, path: &Path) -> Xform {
        self.compose(path, Xform::FlipV)
    }

    /// Forget everything about `path` — crop and transform both.
    pub fn reset(&mut self, path: &Path) {
        self.map.remove(path);
    }

    /// How many images currently carry an edit. For tests and diagnostics.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// Two stacks per path: what has been done, and what can be redone.
///
/// **Per path, not one global list**, and that is the whole of the design
/// decision. Undo means "undo on *this* picture": someone who turns three photos
/// and then walks back to the first expects undo there to undo that photo's turn,
/// not to walk them through all three in reverse. A single global stack answers a
/// question nobody asked and gets the common case wrong.
///
/// `record` takes the *current* edit and must be called **before** the change, so
/// `undo` can hand back the state the image was in before it. That ordering is
/// the interface's contract and the reason a caller cannot get it wrong by
/// forgetting a "previous value" argument: there isn't one to pass.
///
/// Recording also clears redo for that path, because a new edit has rewritten
/// the future. That is what makes redo's absence after a new edit correct rather
/// than a bug.
///
/// The entries are whole [`Edit`]s rather than *operations*: pressing "rotate"
/// twice and then undoing once must land back on the single rotated picture, not
/// on an intermediate half-turn, and only storing states can say that. The price
/// is the ordering rule above.
#[derive(Debug, Clone)]
pub struct EditHistory {
    /// Recorded states for each path, oldest first, so the cap drops the deepest
    /// one by index rather than by searching.
    past: HashMap<PathBuf, Vec<Edit>>,
    /// States pushed aside by [`EditHistory::undo`]. A stack, not a log: undo
    /// pushes and redo pops from the same end.
    redo: HashMap<PathBuf, Vec<Edit>>,
    /// Steps kept **per path**. Bounded by count because a viewer left open over
    /// a large folder would otherwise grow this for every edit on every picture,
    /// and an `Edit` is a whole crop plus a transform.
    cap: usize,
}

impl Default for EditHistory {
    fn default() -> Self {
        Self {
            past: HashMap::new(),
            redo: HashMap::new(),
            cap: 50,
        }
    }
}

impl EditHistory {
    /// Push `current` — the state the image is in **right now** — onto `path`'s
    /// undo stack, so the change about to happen can be undone later. Recording
    /// for one path discards that path's redo stack.
    ///
    /// Over the cap, the **oldest** entry goes. Refusing the newest instead would
    /// mean a rotate that silently does not become undoable once the history is
    /// full, which is far worse than forgetting a step from an hour ago.
    pub fn record(&mut self, path: &Path, current: Edit) {
        let past = self.past.entry(path.to_path_buf()).or_default();
        past.push(current);
        if past.len() > self.cap {
            past.remove(0);
        }
        self.redo.remove(path);
    }

    /// Is there a recorded step for `path` to walk back through?
    pub fn can_undo(&self, path: &Path) -> bool {
        self.past.get(path).is_some_and(|v| !v.is_empty())
    }

    /// Is there a step for `path` to walk forward through? False both when there
    /// was never an undo here and when a later edit threw the future away.
    pub fn can_redo(&self, path: &Path) -> bool {
        self.redo.get(path).is_some_and(|v| !v.is_empty())
    }

    /// Undo one step on `path`, returning the state to restore, or `None` when
    /// there is nothing recorded. `current` — the state as it stands now — goes
    /// onto the redo stack, which is what makes `redo` a mirror rather than a
    /// second, separately-kept answer.
    ///
    /// `None` leaves both stacks alone: a key press is not a promise that
    /// anything is there, so an undo with nothing to undo is a no-op rather than
    /// an error. It is also why this cannot be written as an index the caller
    /// passes — the empty case has to be answerable without the caller having
    /// checked first.
    pub fn undo(&mut self, path: &Path, current: Edit) -> Option<Edit> {
        let previous = self.past.get_mut(path)?.pop()?;
        self.redo
            .entry(path.to_path_buf())
            .or_default()
            .push(current);
        Some(previous)
    }

    /// The mirror of [`EditHistory::undo`]: `current` goes back onto the undo
    /// stack and the popped redo entry is the state to restore. `None` when the
    /// future is empty, which is the ordinary answer after a new edit.
    pub fn redo(&mut self, path: &Path, current: Edit) -> Option<Edit> {
        let next = self.redo.get_mut(path)?.pop()?;
        self.past
            .entry(path.to_path_buf())
            .or_default()
            .push(current);
        Some(next)
    }

    /// How many steps `path` can currently undo — for tests and diagnostics.
    pub fn len_for(&self, path: &Path) -> usize {
        self.past.get(path).map_or(0, Vec::len)
    }
}

