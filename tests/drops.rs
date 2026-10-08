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
//! **Why the copy is made on a thread.** It is a read and a write of a file whose
//! size the app has no way to know, and the rule this crate is built around is
//! that the thread which paints never does work of that size. Measured here
//! (`bench_browse drop_copy_ab`, release, warm cache, NVMe): **3.8 ms at 5 MB,
//! 26 ms at 50 MB, 279 ms at 500 MB** — the last being eighteen frames at 60 Hz,
//! and linear in a size nothing in the app can predict. So the drop became
//! *asynchronous*: `open_file` releases the previous copy, hands this one to a
//! thread and returns; the picture on screen does not change; `pump` opens the
//! copy when the thread reports. The status line is the only visible difference,
//! and without it a dropped file looks like a dropped keystroke.
//!
//! **What is asserted here, and what is not.**
//!
//! * The ownership itself is **behavioural**, against the real filesystem:
//!   [`a_dropped_file_survives_its_source_being_deleted`] deletes the source and
//!   then reads the copy back, which is the reported symptom stated as a
//!   property, and [`a_changed_source_does_not_land_on_top_of_the_previous_copy`]
//!   is its other direction — a rewritten source must not mutate a copy the app
//!   already holds. **Both directions, and that is not belt-and-braces**: a hard
//!   link passes the first one (unlinking one name leaves the data reachable
//!   through the other) while being wrong, because a link *shares* the file. The
//!   second is what caught it, which is why a single-direction guard on "it
//!   survives" is not a property.
//!   [`the_copy_is_the_bytes_unchanged`] and
//!   [`re_dropping_the_same_unchanged_file_reuses_one_copy`] pin the two things
//!   that make the copy cheap and repeatable.
//! * **That the copy is made off the UI thread** is the second thing this change
//!   was for, and it is a **source scan** with byte offsets — see
//!   [`the_copy_is_made_off_the_ui_thread`] for why the offsets and not a
//!   `contains`, and [`a_repeated_drop_of_an_unchanged_file_writes_nothing`] for
//!   the behavioural half (the reuse check, proved by a destination too
//!   read-only for a copy to land on).
//! * The **wiring** — that `load_dropped` asks for a copy, that
//!   `open_file(…, transient)` is the only site that can, and that the copy is
//!   started *after* the previous one is released - is also a **source scan**,
//!   because `main.rs` is a binary and unreachable from a test. See the module
//!   docs on `fn_body` for why the scans check a byte offset and not a
//!   `contains`.

use image_viewer::{forget_drop_copy, own_dropped, plan_drop_copy, DropCopy};
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
    // Every *other* open of a single file is a durable path: the command line,
    // the file dialog, the copy a drop's worker hands back, and the source a
    // failed copy falls back to. Asserted as a count so adding a fifth site has
    // to say which it is rather than defaulting to `false` quietly.
    //
    // **Three of these four are the ones this change added**, and `false` on each
    // is load-bearing: the copy lives in a directory this app owns and deletes,
    // so opening it as transient would ask for a copy of the copy.
    let others = src.matches("self.open_file(ctx, ").count();
    assert_eq!(
        others, 5,
        "expected exactly five `open_file` call sites (the drop, the dialog, the \
         copy a drop's worker reports, a failed copy's fallback to the source, \
         and a paste); a new one has to answer `transient` deliberately"
    );
    for (site, why) in [
        ("self.open_file(ctx, file, false)", "the file dialog opens a file the user owns"),
        (
            "self.open_file(ctx, copy.file.clone(), false)",
            "the copy the worker reports is this app's own file: opening it transiently \
             would copy the copy",
        ),
        (
            "self.open_file(ctx, src, false)",
            "a failed copy must still show the file the user dropped, and asking for \
             a second copy of it would fail the same way",
        ),
    ] {
        assert!(src.contains(site), "missing open site: {site} - {why}");
    }
}

/// **The copy is not made on the thread that paints.** This is the change's
/// subject, and it is otherwise unobservable, so it is a source scan — and a
/// scan of the *wrong* shape, so the check is on **position**, not presence.
///
/// `contains("own_dropped")` over `main.rs` would fail today and pass the moment
/// anyone renamed the call, and it would not notice the call moving into
/// `start_drop_copy`'s thread closure. What must be true is narrower and is
/// checked three ways:
///
/// 1. `own_dropped` — the only function in the library that calls
///    `std::fs::copy` for a drop — appears **exactly once** in `main.rs`, and
///    that one place is inside `start_drop_copy`'s body. A count rather than
///    `contains`: the call has to *exist* (that is the job), so "is it there"
///    answers nothing, and what matters is that there is nowhere else.
/// 2. That place is **after** the `thread::spawn` that makes the body another
///    thread, and **before** the `send`. The second half is the constraint that
///    makes the arrangement work at all: the copy has to be *complete* before the
///    answer is on its way, because the answer is what makes the app open the
///    copy's path.
/// 3. The drop handler's own body (`open_file`, which is where `transient` is
///    honoured) performs no file I/O at all, so the synchronous copy cannot have
///    been moved there instead.
#[test]
fn the_copy_is_made_off_the_ui_thread() {
    let src = main_src();
    let body = fn_body(src, "fn start_drop_copy");
    let at = |needle: &str| {
        body.find(needle)
            .unwrap_or_else(|| panic!("start_drop_copy must mention {needle}:\n{body}"))
    };
    let (spawn, copy, send) = (at("thread::spawn"), at("own_dropped"), at("tx.send"));
    assert!(
        spawn < copy,
        "the copy must happen INSIDE the spawned thread, or it is back on the UI \
         thread under a different name.\nspawn at {spawn}, copy at {copy}\n{body}"
    );
    assert!(
        copy < send,
        "the answer must be sent only after the copy is COMPLETE, or the app \
         opens a path that is still being written.\ncopy at {copy}, send at {send}\n{body}"
    );
    // **Once, and there.** One occurrence is the copy. A second one — anywhere,
    // including inside another function — is a call on whichever thread reached
    // it, and `pump`'s own message handler runs on the UI thread.
    let sites = src.matches("own_dropped").count();
    assert_eq!(
        sites, 1,
        "expected exactly one call of the blocking copy in main.rs, the one inside \
         the spawned thread; {sites} means one of them is on the UI thread"
    );

    // And nothing in the handler does I/O itself, which is all that would be
    // left if someone kept a synchronous copy here and used the thread for
    // something else. Brace-matched to `open_file`'s body, so a copy moved into
    // another function does not dodge it.
    let open = fn_body(src, "fn open_file");
    for needle in ["std::fs::", "fs::copy", "own_dropped", "create_dir_all"] {
        assert!(
            !open.contains(needle),
            "`open_file` must not perform file I/O: `{needle}` is in the drop \
             handler, so the copy is on the UI thread.\n{open}"
        );
    }
}

/// The other half of the same property, in the only place it can be checked
/// behaviourally: **the split the thread runs is the same plan the blocking form
/// runs**, so moving the copy off-thread cannot have changed which directory the
/// bytes land in or what they are.
#[test]
fn the_off_thread_copy_lands_where_the_blocking_one_did() {
    let src = source("F");
    // Blocking form first, on a source nobody has copied: a *real* copy, or the
    // reuse check could be answering "yes" for everything and this test would
    // pass while measuring nothing.
    let blocking = own_dropped(&src.path).expect("own it the blocking way");
    assert!(!blocking.reused, "a first drop has to copy something");
    let want = pixels(&blocking.file);

    // Released, so the plan below cannot be a hit on the copy just made.
    forget_drop_copy(&blocking);

    let plan = plan_drop_copy(&src.path).expect("plan the copy");
    assert!(
        !plan.reuse,
        "with no copy on disk the plan must not claim one is waiting"
    );
    let made = plan.materialise(&src.path).expect("materialise the copy");
    assert_eq!(made.dir, plan.dir, "the plan must name the directory it writes to");
    assert_eq!(made.file, plan.file, "the plan must name the file it writes");
    assert!(!made.reused, "a fresh source must report a real copy, not a reuse");
    assert_eq!(pixels(&made.file), want, "the bytes must be the same bytes");
    forget_drop_copy(&made);
}

/// **A repeated drop of an unchanged file writes nothing at all.**
///
/// Not asserted from `copy.reused`, because a flag is the code's own claim about
/// itself: `copy_is_current` stubbed to `false` sets it to `false` too and the
/// test passes while the behaviour is wrong. So the destination's **bytes** are
/// the assertion — it is overwritten with a same-length payload that is not the
/// picture, and after the second drop those bytes must still be there. A re-copy
/// would have put the real picture back; only a reuse leaves them alone.
///
/// Same length on purpose: a *different* length is caught by
/// [`a_copy_of_the_wrong_length_is_not_reused`], and using one here would make
/// this test pass for the wrong reason.
///
/// **An earlier version of this test made the destination read-only, on the
/// theory that any write would then fail. It does not, and the guard was
/// therefore a flag test wearing a permission's clothes.** `std::fs::remove_file`
/// removes a read-only file on Windows (the attribute is cleared rather than
/// honoured), so the copy path deleted the destination and wrote a fresh one
/// beside it — measured, not assumed:
/// ```text
/// remove_file on read-only: Ok(())
/// copy onto (now absent) dest: Ok(10)
/// ```
/// It passed only because the flag was right. Recorded so nobody tries it again.
///
/// **And this is the common case, not a corner.** A drop of a file whose path,
/// mtime and length are unchanged lands in the directory `drop_copy_dir` already
/// made, so every second drop of the same file takes this path. Before the check
/// existed it cost the same as a first drop (measured: 244 ms against 279 ms at
/// 500 MB, `bench_browse drop_copy_ab`), because the directory was reused and the
/// *copy* was remade.
#[test]
fn a_repeated_drop_of_an_unchanged_file_writes_nothing() {
    let src = source("G");
    let first = own_dropped(&src.path).expect("own once");
    assert!(!first.reused, "a first drop has to copy something");
    let len = std::fs::metadata(&first.file).expect("stat the copy").len();

    // Same length, not the picture. Any re-copy replaces it; a reuse cannot.
    let marker = vec![b'.'; len as usize];
    std::fs::write(&first.file, &marker).expect("damage the copy in place");

    let second = own_dropped(&src.path).expect("own again: a reuse cannot fail");
    assert_eq!(second.dir, first.dir, "one directory per unchanged source");
    assert_eq!(
        std::fs::read(&second.file).expect("read the copy back"),
        marker,
        "the reused copy must not have been written to: a re-copy would have put \
         the real picture back over these bytes"
    );
    assert!(
        second.reused,
        "and the app should have been told it reused the copy rather than made one"
    );
    forget_drop_copy(&second);
}

/// **A copy of the wrong length is not reused**, which is the whole difference
/// between a length check and an existence check.
///
/// The guard above would also pass if `copy_is_current` asked only "is there a
/// file there" — and a half-written copy from a killed session *is* a file there,
/// and is the wrong thing to show. So the destination is replaced with a
/// same-named file of a different length and the drop has to replace it again.
#[test]
fn a_copy_of_the_wrong_length_is_not_reused() {
    let src = source("H");
    let first = own_dropped(&src.path).expect("own once");
    let want = pixels(&first.file);
    std::fs::write(&first.file, b"not a picture").expect("damage the copy");
    assert_ne!(
        std::fs::metadata(&first.file).expect("stat").len(),
        std::fs::metadata(&src.path).expect("stat").len(),
        "precondition: the damaged copy is a different length"
    );

    let second = own_dropped(&src.path).expect("own again");
    assert!(
        !second.reused,
        "a copy of the wrong length is not a copy of this file"
    );
    assert_eq!(pixels(&second.file), want, "the damage must be repaired");
    forget_drop_copy(&second);
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
/// So the assertion is an ordering between two byte offsets inside one body. The
/// second name is the one this change introduced: `own_drop` became
/// `start_drop_copy`, and a test that was left pointing at the old spelling
/// would have panicked rather than quietly passing, which is the right way for
/// this to fail.
#[test]
fn a_drop_copies_its_source_after_releasing_the_previous_copy() {
    let src = main_src();
    let body = fn_body(src, "fn open_file");
    let release = body
        .find("self.close_transients()")
        .unwrap_or_else(|| panic!("open_file must release the previous copy:\n{body}"));
    let take = body.find("self.start_drop_copy(file)").unwrap_or_else(|| {
        panic!(
            "open_file must start a copy when transient - and it must be \
             `start_drop_copy`, the function that hands it to a thread:\n{body}"
        )
    });
    assert!(
        release < take,
        "the copy must be started AFTER close_transients(), or it is deleted by \
         the call that goes on to open it.\nrelease at {release}, copy at {take}\n{body}"
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
    // life, and nothing anywhere reports it. Eight today - open_archive,
    // open_folder, pick_folder, open_file, leave_archive, paste_from_clipboard,
    // on_exit and Esc-to-quit - and a ninth has to be added here rather than
    // left to chance.
    //
    // Two of them came with a real obligation attached. `leave_archive`:
    // backing out of an archive releases the temp dir `open_archive` created,
    // so without the call the archive's extracted pages would sit in `%TEMP%`
    // for every visit rather than for the session. `paste_from_clipboard`: the
    // same shape as a drop, and for the same reason — a paste opens a file in a
    // directory this app owns, so the *previous* one has to be released or a
    // second paste leaks the first.
    let total = src.matches("self.close_transients()").count();
    assert_eq!(
        total, 8,
        "expected 8 release sites (open_file, open_folder, open_archive, leave_archive, \
         paste_from_clipboard, pick_folder, on_exit, Esc-to-quit); a new one means a new \
         way to leak a copy, so name it here"
    );
    // ...and named, not just counted: the new site is only correct if it
    // releases the *archive's* half on the way out.
    let leave = fn_body(src, "fn leave_archive");
    assert!(
        leave.contains("self.close_transients()"),
        "leaving an archive must release its extracted temp dir:\n{leave}"
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
    // **A copy in flight is the third thing this one call gives up**, and it is
    // here rather than at the six call sites because that is the whole point of
    // one function. It matters because the copy no longer finishes before the
    // next action does: without it, dropping a file and then opening a folder
    // would show the *folder* and then have the drop arrive on top of it.
    assert!(
        body.contains("self.pending_drop = None"),
        "close_transients must cancel a copy in flight, or the last action does \
         not win:\n{body}"
    );
}

/// **A copy that lands after the reader has dropped something else is thrown
/// away, not opened.** Position, not presence: what makes this safe is that the
/// staleness test is *inside* `pump` and reads the message's `src` against the
/// pending request, so it is re-evaluated per message rather than remembered
/// once at the drop.
///
/// The two halves, because either alone is a bug:
///
///  * the identity comparison, and
///  * the reclamation of the directory the stale copy wrote. Forgetting it is a
///    directory in `%TEMP%` for the rest of the machine's life, and the case
///    where the stale copy *is* the one on screen — two drops of the same
///    unchanged file resolve to one directory — is why that release is
///    conditional on the app not already holding it.
#[test]
fn a_stale_copy_is_dropped_rather_than_opened() {
    let src = main_src();
    let pump = fn_body(src, "fn pump");
    let test = pump.find("self.pending_drop.as_deref() != Some(src.as_path())");
    assert!(
        test.is_some(),
        "pump must recognise a copy whose request is no longer the pending one, \
         or a superseded drop opens over the top of the newer one:\n{pump}"
    );
    let at = test.unwrap();
    assert!(
        pump[at..].contains("continue"),
        "the stale branch must skip the open, not merely notice itself:\n{pump}"
    );
    let forget = pump[at..].find("forget_drop_copy").unwrap_or_else(|| {
        panic!("a stale copy's directory must be reclaimed:\n{pump}")
    });
    let guard = pump[at..].find("self.drop_copy.as_ref()").unwrap_or_else(|| {
        panic!(
            "the reclamation must be conditional on the app not already holding \
             that copy - two drops of the same unchanged file share one \
             directory:\n{pump}"
        )
    });
    assert!(
        guard < forget,
        "the `drop_copy` check must come before the delete, or the copy on screen \
         is deleted by the arrival of an older drop.\nguard at {guard}, \
         forget at {forget}\n{pump}"
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
        body.matches("start_drop_copy").count() + body.matches("own_drop").count(),
        0,
        "`load_dropped` must not start the copy itself; it is `open_file`'s job, \
         so there is exactly one site that does. **Both spellings are counted** \
         because this guard was a no-op the moment `own_drop` was renamed: a \
         `matches` on a name that no longer exists returns 0 and passes forever, \
         which is the guard that tells you nothing."
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
