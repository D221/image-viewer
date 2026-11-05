//! Archives: stepping out at both ends, the show-archives toggle, explicit
//! entry, and user-command templates.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// Both ends of an archive must step **out** of it, not round its own listing.
///
/// This is the bug a reader hits as "only the first zip opens": `→` at an
/// archive's last page used to be `pos_next`'s ordinary end-of-list wrap, which
/// inside an archive wraps to the archive's *own* first member — so the folder's
/// second, third and later archives were unreachable by arrowing, and one volume
/// looped forever. The backward edge existed; the forward one did not.
#[test]
fn both_ends_of_an_archive_step_out_of_it() {
    use image_viewer::{Gallery, leave_archive_at};
    let dir = tempfile::tempdir().unwrap();
    for n in ["a.jpg", "b.jpg", "c.jpg"] {
        std::fs::write(dir.path().join(n), b"x").unwrap();
    }
    let mut g = Gallery::new(dir.path());
    assert_eq!(g.len(), 3);

    // Front and back, including the page dimension: `index + 1 == len` is the
    // last *file*, not the last *position*.
    assert!(g.at_front(), "a fresh listing starts at its front");
    assert!(!g.at_back());
    g.index = 2;
    assert!(g.at_back(), "the last file is the back of the listing");
    assert!(!g.at_front());

    // Forward out, backward out — and never the other way round. A `→` at the
    // *front* must not drop the reader into the parent folder, and a `←` at the
    // back must not either.
    assert!(leave_archive_at(true, true, true, true), "forward at the back leaves");
    assert!(leave_archive_at(false, true, true, true), "backward at the front leaves");
    assert!(!leave_archive_at(true, true, false, true), "forward mid-archive stays");
    assert!(!leave_archive_at(false, false, true, true), "backward mid-archive stays");
    assert!(!leave_archive_at(true, false, false, true), "nowhere to go, so nothing");

    // `can_leave` is asked first and overrides both edges: with looping off and
    // the archive as the folder's last entry there is nowhere to land, and being
    // dropped into the folder by a press that had nowhere to go reads as broken.
    assert!(!leave_archive_at(true, false, true, false));
    assert!(!leave_archive_at(false, true, false, false));

    // An empty listing has no back, so it cannot claim one.
    let empty = Gallery::empty();
    assert!(!empty.at_back());
    assert!(empty.at_front(), "an empty listing is trivially at its front");
}

/// Leaving an archive must not re-enter it. The original defect was:
/// `leave_archive` adopted a gallery whose cursor sat on the archive, and
/// `adopt_gallery` ended in `goto`, which *entered* an archive on the cursor.
/// The fix is not a one-shot but removing entry from `goto` entirely —
/// `goto` now positions on an archive and never enters one. `step` and the
/// grid pick are the only places that call `enter_archive_if_present`.
#[test]
fn leaving_an_archive_does_not_re_enter_it() {
    let src = main_src();

    let leave = fn_body(&src, "fn leave_archive").expect("leave_archive");
    // Leaving arms no guard — it just builds the parent gallery with the
    // cursor on the archive. `goto` no longer enters, so no guard is needed.
    assert!(
        leave.contains("just_left") == false,
        "leave_archive no longer needs a one-shot because goto does not enter"
    );

    let goto = fn_body(&src, "fn goto(&mut self, ctx: &Context)").expect("goto");
    // Goto must not enter archives at all — the one-shot is gone because
    // the whole branch is gone.
    assert!(goto.contains("just_left") == false, "goto must not contain the old one-shot");
    // The body shows the position logic without any archive entry: it sets
    // `is_nav`, `lat.begin`, then `snap_to_turn_anchor` — no `open_archive`.
    // The absence of an archive branch *is* the test.
    assert!(
        goto.contains("self.snap_to_turn_anchor()"),
        "goto runs the position setup without an archive branch"
    );

    let step = fn_body(&src, "fn step(&mut self, ctx: &Context, forward: bool)").expect("step");
    assert!(
        step.contains("archive_edge(forward)"),
        "step must ask about both edges of the archive, not only the front:\n{step}"
    );
    assert!(
        step.contains("leave_archive(ctx, forward)"),
        "the direction is what distinguishes 'back' from 'on to the next one':\n{step}"
    );
}

/// The archive visibility toggle must be wired end to end: told to the library
/// (or the next scan answers from a flag nobody set) and followed by a rescan
/// (or the panel changes the setting and the listing does not).
#[test]
fn the_show_archives_toggle_is_wired() {
    let src = main_src();
    assert!(
        src.contains("image_viewer::set_show_archives(config.show_archives)"),
        "start-up must tell the library before the first scan"
    );
    let flat = squash(&src);
    let before = flat.find("set_show_archives(config.show_archives)").expect("start-up call");
    let scan = flat.find("letfiles=Self::sorted_scan(").expect("the start-up scan");
    assert!(
        before < scan,
        "the setting has to be set before the scan that reads it, or the first \
         listing of the session answers from a stale flag"
    );

    let panel = fn_body(&src, "impl eframe::App for MyApp").expect("the app impl");
    assert!(panel.contains("&mut self.config.show_archives"), "the toggle must exist in Settings");
    let toggle = panel.find("&mut self.config.show_archives").expect("the checkbox");
    let setter = panel.find("set_show_archives(self.config.show_archives)").expect("the setter");
    let rescan = panel[toggle..].find("self.rescan(ctx, true)").expect("the rescan");
    assert!(
        toggle < setter && setter < toggle + rescan,
        "save, then tell the library, then rescan — a rescan before the setter \
         rebuilds the listing from the old flag and the toggle looks inert"
    );
}

/// Archives in the gallery must be **entered explicitly**, never entered
/// automatically and never decoded. The seam that does it is **not `goto`** —
/// `goto` is where opening a folder ends, and entering there opened whichever
/// archive happened to sort first without anyone asking. Instead, entry happens
/// in `step` (arrowing onto one) and `pick_grid_selection` (grid click/Enter):
/// the places where a person asked for something.
///
/// `goto` explicitly does NOT enter archives: it positions on them, and the
/// panel draws a placeholder. `plan_jobs` refuses archives, so nothing queued
/// a decode for one.
#[test]
fn archives_are_entered_explicitly_and_never_decoded() {
    let src = main_src();

    // `goto` no longer enters: it positions, and the body shows it.
    let goto = fn_body(&src, "fn goto(&mut self, ctx: &Context)").expect("goto");
    // The body shows the position logic without any archive entry: it sets
    // `is_nav`, `lat.begin`, then `snap_to_turn_anchor` — no `open_archive`.
    // The absence of an archive branch *is* the test.
    assert!(
        goto.contains("self.snap_to_turn_anchor()"),
        "goto runs the position setup without an archive branch"
    );
    assert!(goto.contains("open_archive") == false, "goto must not call open_archive");

    // Entry is in `step` (arrowing) and `pick_grid_selection` (grid).
    let step = fn_body(&src, "fn step(&mut self, ctx: &Context, forward: bool)").expect("step");
    assert!(
        step.contains("enter_archive_if_present(ctx)"),
        "arrowing onto an archive must call the entry helper"
    );

    let pick = fn_body(&src, "fn pick_grid_selection(&mut self, ctx: &Context)")
        .expect("pick_grid_selection");
    assert!(
        pick.contains("enter_archive_if_present(ctx)"),
        "grid click/Enter must call the entry helper"
    );

    // The decode side: plan_jobs refuses archives (library rule, tested there).
    // These are the paths that bypass plan_jobs and need their own guard.
    assert!(
        src.contains("let is_archive = is_archive_path(path);")
            && squash(&src).contains("!is_archive&&"),
        "a grid cell must not ask for a thumbnail of a container"
    );
    assert!(
        src.contains(".filter(|p| !is_archive_path(p))"),
        "the EXIF panel pushes its own job, so it needs its own archive guard"
    );

    // `Del` refuses when the cursor is ON an archive, not only when inside one.
    let del = fn_body(&src, "fn delete_current(&mut self, ctx: &Context)").expect("delete_current");
    assert!(
        del.contains("is_archive_path(&p) ||"),
        "delete must refuse when the cursor is ON an archive, not only when inside one"
    );
}

/// UserCmd expansion (Tier 1 #2): {file}/{dir}/{name} expand, unknowns stay.
#[test]
fn user_command_templates_expand_against_the_current_file() {
    let cmd = image_viewer::UserCommand {
        name: "x".into(),
        program: "{dir}/tool.exe".into(),
        args: vec!["{file}".into(), "{name}".into(), "{x}".into()],
    };
    let file = std::path::Path::new("C:/pics/a.jpg");
    let (prog, args) = image_viewer::expand_user_command(&cmd, file);
    assert_eq!(prog, "C:/pics/tool.exe");
    assert_eq!(args[0], "C:/pics/a.jpg");
    assert_eq!(args[1], "a.jpg");
    assert_eq!(args[2], "{x}", "unknown placeholders must survive");
}

/// Empty args default to the file: "open with" nearly always means the
/// picture on screen, not a blank window.
#[test]
fn user_command_with_no_args_gets_the_file() {
    let cmd = image_viewer::UserCommand {
        name: "paint".into(),
        program: "mspaint.exe".into(),
        args: Vec::new(),
    };
    let file = std::path::Path::new("C:/pics/a.jpg");
    let (prog, args) = image_viewer::expand_user_command(&cmd, file);
    assert_eq!(prog, "mspaint.exe");
    assert_eq!(args, vec!["C:/pics/a.jpg"]);
}

/// EXIF dates parse strict-shape or not at all; touching sets exactly the
/// time asked for, readable back from the file.
#[test]
fn exif_dates_parse_strictly_and_touch_lands() {
    use std::time::{Duration, UNIX_EPOCH};
    // 2024-03-17 14:34:56 UTC = 1710686096, in both spellings.
    assert_eq!(
        image_viewer::parse_exif_datetime("2024:03:17 14:34:56"),
        UNIX_EPOCH.checked_add(Duration::from_secs(1710686096))
    );
    assert_eq!(
        image_viewer::parse_exif_datetime("2024-03-17 14:34:56"),
        UNIX_EPOCH.checked_add(Duration::from_secs(1710686096))
    );
    // Leap day exists in 2024 and not in 2023.
    assert!(image_viewer::parse_exif_datetime("2024:02:29 00:00:00").is_some());
    assert!(image_viewer::parse_exif_datetime("2023:02:29 00:00:00").is_none());
    for bad in [
        "2024:13:01 00:00:00",
        "2024:03:17",
        "2024:03:17 24:00:00",
        "2024:03:17 14:34:56 extra",
        "",
    ] {
        assert!(image_viewer::parse_exif_datetime(bad).is_none(), "{bad:?} must not parse");
    }

    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("touched.jpg");
    std::fs::write(&p, b"bytes").unwrap();
    let to = UNIX_EPOCH + Duration::from_secs(1700000000);
    image_viewer::touch_file_mtime(&p, to).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().modified().unwrap(), to);
}

/// End to end through a real file: FFA02639.jpg carries DateTimeOriginal
/// 2017-02-08 14:14:02, and the lookup-plus-parse must find it — a
/// dashes-vs-colons mismatch here is exactly the shipped "No EXIF date".
/// `test-images/` is gitignored, so this skips when absent.
#[test]
fn exif_date_reads_from_a_real_file() {
    use std::time::{Duration, UNIX_EPOCH};
    let path = std::path::Path::new("test-images/FFA02639.jpg");
    if !path.exists() {
        return;
    }
    let exif = nom_exif::read_exif(path).expect("fixture must parse");
    let raw = exif
        .get(nom_exif::ExifTag::DateTimeOriginal)
        .map(|e| e.to_string())
        .expect("fixture carries DateTimeOriginal");
    assert_eq!(
        image_viewer::parse_exif_datetime(&raw),
        UNIX_EPOCH.checked_add(Duration::from_secs(1486563242))
    );
}
