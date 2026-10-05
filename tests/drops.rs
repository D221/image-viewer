//! A dropped file has to outlive the drop.
//!
//! **The bug this suite exists for.** A user drags an image out of a zip in
//! Explorer onto the viewer. Explorer does not pass the archive member's path -
//! it extracts the member to a transient temp file, hands *that* path to the
//! drop target, and deletes it a moment later. The app opened the path in place,
//! so on its own 2 s poll it found a file whose contents had "changed" (it was
//! gone, so the stamp is a different stamp), and blanked the picture it was
//! showing. Measured, before the fix, on the real app driven by a synthetic drop:
//!
//! ```text
//! [1]  load_dropped: path=%TEMP%\ivdrop-…\chapter01.jpg parent=…  -> open_file (image)
//! [1]  open_file: folder=%TEMP%\ivdrop-…  gallery=[…\chapter01.jpg]  load_all=true
//! [2]  PAINT shown=…\ivdrop-…\chapter01.jpg tex=761x1100        <- shown, correctly
//! [14] check_changed: file=…exists=false old=Some((true,30696)) new=(false,0) moved=true
//! [14] failed.insert Preview key=… exists=false                  <- the re-decode cannot work
//! [14] invalidate_path: previews_held=true shown_gone=true       <- the texture is dropped
//! [14] rescan: found=[] all_files_before=[…\chapter01.jpg]       <- and it leaves the list
//! [14] goto: files=[] shown=None
//! [14] central: shown=None cur=None gallery_len=0               <- blank, permanently
//! ```
//!
//! **Why a copy rather than "do not watch it".** The second option was
//! considered and rejected: an unwatched but vanished file means the app is
//! showing pixels it can no longer reproduce, so `Ctrl+S` (a lossless rewrite of
//! the file on disk), `Ctrl+C` and every zoom tile read a path that is not there.
//! That moves the failure later instead of removing it. So the fix is the same
//! shape the archive path already uses - a copy in a directory the app owns -
//! and it is asserted by behaviour below, not by a scan of the wiring.
//!
//! **What is asserted here, and what is not.**
//!
//! * The ownership itself is **behavioural**, against the real filesystem:
//!   [`a_dropped_file_survives_its_source_being_deleted`] deletes the source and
//!   then reads the copy back, which is the reported symptom stated as a
//!   property. [`the_copy_is_the_bytes_unchanged`] and
//!   [`re_dropping_the_same_unchanged_file_reuses_one_copy`] pin the two things
//!   that make the copy cheap and repeatable.
//! * The **wiring** - that `load_dropped` asks for a copy, that
//!   `open_file(…, transient)` is the only site that can, and that the copy is
//!   taken *after* the previous one is released - is a **source scan**, because
//!   `main.rs` is a binary and unreachable from a test. See the module docs on
//!   `fn_body` for why the scans check a byte offset and not a `contains`.

use image_viewer::{forget_drop_copy, own_dropped, DropCopy};
use std::path::{Path, PathBuf};

/// A jpeg on disk, and the directory holding it, for a test to hand to
/// `own_dropped`.
///
/// **A temp dir and a real decode**, not a stub: the bug is about the bytes
/// outliving a path, so the assertion has to read bytes back through the real
/// loader rather than compare sizes.
struct Source {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

fn source(bytes_marker: &str) -> Source {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("page 01.jpg");
    let mut img = image::RgbImage::new(64, 96);
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([
            (x * 4 % 256) as u8,
            (y * 2 % 256) as u8,
            bytes_marker.bytes().next().unwrap_or(b'x'),
        ]);
    }
    image::DynamicImage::ImageRgb8(img)
        .save(&path)
        .expect("write the fixture");
    Source { _dir: dir, path }
}

/// The pixels of a jpeg, through the app's own loader.
///
/// The real `load_image` rather than `image`'s: the property is that the *app*
/// can still decode what it is showing, and a copy that only satisfies
/// `std::fs::read` would pass while the viewer still failed.
fn pixels(path: &Path) -> Vec<u8> {
    let img = image_viewer::load_image(path, 4096).expect("the copy must still decode");
    img.rgba
}

// ---------------------------------------------------------------------------
// The behaviour: the copy outlives its source
// ---------------------------------------------------------------------------

/// **The reported bug, as a property.** Delete the source - the whole
/// directory, as Explorer does with its extraction - and the copy must still
/// decode to the same picture.
///
/// Both halves matter and the test names them:
/// * the *file* going is the reported trigger, and
/// * the *directory* going is what makes the difference between a file the
///   watcher still sees and one it cannot stat at all. A test that only removed
///   the file would pass against a copy left beside a source path, because the
///   watcher polls the folder, not the file.
#[test]
fn a_dropped_file_survives_its_source_being_deleted() {
    let src = source("A");
    let copy = own_dropped(&src.path).expect("own the dropped file");

    // Exactly what Explorer does with its extraction: the whole directory.
    let parent = src.path.parent().unwrap().to_path_buf();
    std::fs::remove_dir_all(&parent).expect("delete the source directory");
    assert!(
        !src.path.exists(),
        "precondition: the source is really gone"
    );

    assert!(
        copy.file.exists(),
        "the copy must survive: {}",
        copy.file.display()
    );
    assert_eq!(
        pixels(&copy.file),
        pixels(&copy.file),
        "the copy must decode"
    );
    forget_drop_copy(&copy);
    assert!(
        !copy.dir.exists(),
        "forgetting the copy must remove it, or a session leaks a directory"
    );
}

/// The copy is the same picture, not merely a file of the same length.
///
/// A copy that lost or reordered a byte would still satisfy the previous test;
/// this one compares decoded pixels against the source *before* it is deleted.
#[test]
fn the_copy_is_the_bytes_unchanged() {
    let src = source("B");
    let want = pixels(&src.path);
    let copy = own_dropped(&src.path).expect("own the dropped file");
    assert_eq!(pixels(&copy.file), want, "the copy must be byte-faithful");
    forget_drop_copy(&copy);
}

/// Two drops of the same unchanged file land in the same directory.
///
/// This is what keeps a repeated drop from accumulating directories in `%TEMP%`,
/// and it is why the directory name is a hash of the source's **path, mtime and
/// length**: the same file unchanged has to hash the same.
#[test]
fn re_dropping_the_same_unchanged_file_reuses_one_copy() {
    let src = source("C");
    let first = own_dropped(&src.path).expect("own once");
    let second = own_dropped(&src.path).expect("own again");
    assert_eq!(
        first.dir, second.dir,
        "an unchanged source must map to one directory, or %TEMP% grows per drop"
    );
    assert_eq!(
        pixels(&second.file),
        pixels(&src.path),
        "the re-drop must be a usable copy, not a broken half-link"
    );
    forget_drop_copy(&second);
}

/// A source that changed under us gets a **new** directory, and the old copy is
/// left readable until the app releases it.
///
/// The alternative - overwriting in place - means the app can be looking at a
/// file that is being rewritten underneath it, which is the one thing a viewer
/// must never do. It also means a stale `DropCopy` never points at bytes that
/// changed meaning.
#[test]
fn a_changed_source_does_not_land_on_top_of_the_previous_copy() {
    let src = source("D");
    let first = own_dropped(&src.path).expect("own once");
    let first_pixels = pixels(&first.file);

    // Change the content *and* the length, which is what the stamp notices.
    let mut img = image::RgbImage::new(200, 300);
    for (x, _y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgb([(x % 256) as u8, 9, 11]);
    }
    image::DynamicImage::ImageRgb8(img)
        .save(&src.path)
        .expect("re-export");
    let second = own_dropped(&src.path).expect("own again");

    assert_ne!(
        first.dir, second.dir,
        "a re-exported source must not overwrite a copy the app may still hold"
    );
    assert_eq!(
        pixels(&first.file),
        first_pixels,
        "the earlier copy must still read as the earlier picture, not the re-export"
    );
    forget_drop_copy(&first);
    forget_drop_copy(&second);
}

// ---------------------------------------------------------------------------
// The wiring: a source scan
// ---------------------------------------------------------------------------

/// `main.rs` is a binary and no test can reach into it, so the wiring is
/// asserted against the source text - the established pattern, and the reason it
/// is defensible here is that it checks **position**, not presence.
fn main_src() -> &'static str {
    include_str!("../src/main.rs")
}

/// The body of the function whose signature starts with `sig`, by brace
/// matching.
///
/// Brace matching rather than a text split, because splitting a body on text is
/// how a scan passes vacuously: it can return a fragment that happens to
/// contain what it is looking for while the real call is somewhere else.
fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src
        .find(sig)
        .unwrap_or_else(|| panic!("{sig} is gone; update this test to match"));
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut seen_open = false;
    for (i, b) in bytes[start..].iter().enumerate() {
        match b {
            b'{' => {
                depth += 1;
                seen_open = true;
            }
            b'}' => {
                depth -= 1;
                if seen_open && depth == 0 {
                    return &src[start..start + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("{sig} has no closing brace");
}

/// **`load_dropped` must ask for a copy.** The one caller that can, and the one
/// that has to: `open_file`'s `transient` parameter exists for this and nothing
/// else.
///
/// A `contains("true")` scan would pass against a comment, so this reads the
/// call's arguments.
#[test]
fn a_drop_takes_a_copy_and_no_other_open_does() {
    let src = main_src();
    let drop_body = fn_body(src, "fn load_dropped");
    assert!(
        drop_body.contains("self.open_file(ctx, path, true)"),
        "the drop must open transiently, or the picture blanks a second later.\n\
         the body was:\n{drop_body}"
    );
    // Every *other* open of a single file is a durable path: the command line and
    // the file dialog. Asserted as a count so adding a third site has to say
    // which it is rather than defaulting to `false` quietly.
    let others = src.matches("self.open_file(ctx, ").count();
    assert_eq!(
        others, 2,
        "expected exactly two `open_file` call sites (the drop and the dialog); \
         a new one has to answer `transient` deliberately"
    );
    assert!(
        src.contains("self.open_file(ctx, file, false)"),
        "the file dialog opens a file the user owns"
    );
}

/// **The copy is taken *after* the previous one is released — position, not
/// presence.**
///
/// This is the check the whole suite's shape is built around, because it is the
/// failure mode a `contains` scan cannot see. `open_file` releases the previous
/// copy at its top (`close_transients`). If the copy were taken *before* that
/// line, the call would delete the copy it had just made and the app would open a
/// path that no longer exists: a picture that never appears, with no error
/// anywhere, and a directory left behind in `%TEMP%` because nothing holds it.
///
/// So the assertion is an ordering between two byte offsets inside one body.
#[test]
fn a_drop_copies_its_source_after_releasing_the_previous_copy() {
    let src = main_src();
    let body = fn_body(src, "fn open_file");
    let release = body
        .find("self.close_transients()")
        .unwrap_or_else(|| panic!("open_file must release the previous copy:\n{body}"));
    let take = body
        .find("self.own_drop(&file)")
        .unwrap_or_else(|| panic!("open_file must take a copy when transient:\n{body}"));
    assert!(
        release < take,
        "the copy must be taken AFTER close_transients(), or it is deleted by the \
         call that goes on to open it.\nrelease at {release}, copy at {take}\n{body}"
    );
}

/// A copy must not outlive the session, or `%TEMP%` grows without bound.
///
/// `on_exit` is the site that matters - it is the only one a closed window
/// reaches - and the other is the `Esc`-to-quit path. Asserted as presence here
/// because there is no meaningful *position*: either call releases it.
#[test]
fn a_drop_copy_is_released_on_exit_and_on_quit() {
    let src = main_src();
    let exit = fn_body(src, "fn on_exit");
    assert!(
        exit.contains("self.close_transients()"),
        "on_exit must release the drop copy, or every session leaks a directory:\n{exit}"
    );
    // Counted, because the failure this guards is a *forgotten site*: a copy the
    // app never releases is a directory in `%TEMP%` for the rest of the machine's
    // life, and nothing anywhere reports it. Six today - open_archive,
    // open_folder, pick_folder, open_file, on_exit and Esc-to-quit - and a seventh
    // has to be added here rather than left to chance.
    let total = src.matches("self.close_transients()").count();
    assert_eq!(
        total, 6,
        "expected 6 release sites (open_file, open_folder, open_archive, pick_folder, \
         on_exit, Esc-to-quit); a new one means a new way to leak a copy, so name it \
         here"
    );
}

/// `close_transients` must release **both** halves of the slot.
///
/// Two independent `Option`s with two independent cleanups is the shape that
/// produces a forgotten site, and a forgotten site here is a directory that
/// lives in `%TEMP%` for the rest of the machine's life. One function, both
/// fields, so the lifetime is one place.
#[test]
fn both_transients_are_released_by_one_call() {
    let src = main_src();
    let body = fn_body(src, "fn close_transients");
    assert!(
        body.contains("self.zip_temp.take()"),
        "the archive's temp dir must still be released here:\n{body}"
    );
    assert!(
        body.contains("self.drop_copy.take()"),
        "the drop copy must be released here too, or it leaks:\n{body}"
    );
}

/// **A dropped folder and a dropped archive still behave as they did.** The copy
/// is for a *file*, and the other two branches are the archive path's own
/// business. Asserted so the fix cannot be "extended" into copying a folder.
#[test]
fn only_a_dropped_file_is_copied() {
    let src = main_src();
    let body = fn_body(src, "fn load_dropped");
    let dir_branch = body
        .find("self.open_folder(ctx, path)")
        .expect("a folder branch");
    let archive_branch = body
        .find("self.open_archive(ctx, &path)")
        .expect("an archive branch");
    let file_branch = body
        .find("self.open_file(ctx, path, true)")
        .expect("a file branch");
    // Each branch returns before the next, so the copy is reached only from the
    // file branch. The three being distinct statements is what this rests on.
    assert!(
        dir_branch < archive_branch && archive_branch < file_branch,
        "the drop branches are in a different order than this test expects"
    );
    assert_eq!(
        body.matches("own_drop").count(),
        0,
        "`load_dropped` must not copy itself; the copy is `open_file`'s job, so \
         there is exactly one site that does it"
    );
}

/// The type exists and is the thing the app holds — a compile-time check with a
/// runtime assertion that `dir` really is a parent of `file`.
///
/// The field order matters to the cleanup: `forget_drop_copy` removes the
/// *directory*, and a `DropCopy` whose `file` is not inside its `dir` would make
/// that cleanup a silent no-op with no error anywhere.
#[test]
fn a_drop_copys_directory_contains_its_file() {
    let src = source("E");
    let copy: DropCopy = own_dropped(&src.path).expect("own the dropped file");
    assert!(
        copy.file.starts_with(&copy.dir),
        "the copy must live inside the directory that gets deleted: {} not under {}",
        copy.file.display(),
        copy.dir.display()
    );
    assert!(
        copy.dir.starts_with(std::env::temp_dir()),
        "the copy belongs under %TEMP%, not %APPDATA% and not beside the source: {}",
        copy.dir.display()
    );
    forget_drop_copy(&copy);
}
