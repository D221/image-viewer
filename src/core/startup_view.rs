//! Module `startup_view`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use startup_view::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Which image in `files` a launch should show.
///
/// Three inputs, in priority order: a file the user **named**, the **resume
/// point** for this folder, and failing both, the first image.
///
/// The named case is the one that was missing. `Gallery::from_file` set the index
/// to 0, and then start-up replaced the list with the whole folder and left the
/// index at 0 - so opening a *specific* photo, which is every double-click and
/// every "Open with", showed whichever file happened to sort first. `AGENTS.md` has
/// said for some time that an explicit argument beats resume; it was beating
/// neither.
///
/// `resume` is matched on the file *name* rather than the whole path, because that
/// is the format `Config::last_by_folder` stores so archives and folders can share
/// it.
///
/// A pure function so the rule is testable at all: the wiring lives in
/// `MyApp::new`, which is a binary.
pub fn startup_index(files: &[PathBuf], named: Option<&Path>, resume: Option<&str>) -> usize {
    if let Some(want) = named {
        return files.iter().position(|f| f == want).unwrap_or(0);
    }
    if let Some(last) = resume {
        let want = Path::new(last)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        if let Some(i) = want.and_then(|n| {
            files.iter().position(|f| {
                f.file_name()
                    .map(|x| x.to_string_lossy() == n.as_str())
                    .unwrap_or(false)
            })
        }) {
            return i;
        }
    }
    0
}

/// The transform that takes a file's *stored* pixels to what the viewer shows:
/// the EXIF orientation applied at decode, then whatever the user has done since.
///
/// The user's part is a whole [`Xform`], not a turn count, because a mirror and a
/// rotation are the same kind of edit — a quarter turn then a mirror is a
/// *diagonal* mirror, and keeping two independent counters would mean the save had
/// to work out that composition at the last moment. Composing it as it is pressed
/// means there is only ever one thing to store, one thing to display, and one
/// thing to hand TurboJPEG.
pub fn display_xform(exif_tag: u16, user: Xform) -> Xform {
    Xform::from_exif(exif_tag)
        .unwrap_or(Xform::Identity)
        .then(user)
}

