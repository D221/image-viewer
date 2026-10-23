//! The OS folder watch (`ReadDirectoryChangesW`) and the decision table the app
//! feeds it through.
//!
//! Two halves, deliberately in one file:
//!
//! - **The parser and the classifier are pure** and tested against buffers a
//!   test writes itself, because the real OS will not hand us a truncated
//!   record or a buffer-overflow marker on demand.
//! - **The watch itself is only real if it is the real call**, so the tests
//!   below start one on a `tempfile::tempdir()` and mutate the folder for real.
//!   The two premises this feature had recorded as unverified are exactly these:
//!   that a **rename-over** (what every re-export does) produces an event, and
//!   that **one handle with `bWatchSubtree`** hears a change in a subfolder.
//!
//! There is also one source scan, for the same reason `tests/drops.rs` has
//! them: the app's wiring lives in `src/main.rs`, which is unreachable from
//! every test. The scan asserts byte *positions* (the poll below the watch
//! branch, the archive guard above the start), not presence — a scan for
//! presence passed on the defect it was written for, once already.

#![cfg(windows)]

use std::path::Path;
use std::time::{Duration, Instant};

use image_viewer::{
    classify_change, coalesce_changes, parse_notify_buffer, ChangeKind, DirWatcher, FsChange,
    CHANGE_OVERFLOW, FILE_ACTION_ADDED, FILE_ACTION_MODIFIED, FILE_ACTION_REMOVED,
    FILE_ACTION_RENAMED_NEW_NAME, FILE_ACTION_RENAMED_OLD_NAME,
};

// ---------------------------------------------------------------------------
// The parser
// ---------------------------------------------------------------------------

/// One `FILE_NOTIFY_INFORMATION` record, as the OS lays it out:
/// `NextEntryOffset: u32, Action: u32, FileNameLength: u32` (bytes, UTF-16LE)
/// then the name itself.
fn record(action: u32, name: &str, next_offset: u32) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut v = Vec::new();
    v.extend_from_slice(&next_offset.to_le_bytes());
    v.extend_from_slice(&action.to_le_bytes());
    v.extend_from_slice(&((units.len() * 2) as u32).to_le_bytes());
    for u in units {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

#[test]
fn the_record_offsets_chain_the_names() {
    // The first record's `NextEntryOffset` is its own length, so the two
    // records are contiguous — which is how the OS writes a batch.
    let first = record(FILE_ACTION_ADDED, "a.jpg", 0).len() as u32;
    let mut buf = record(FILE_ACTION_ADDED, "a.jpg", first);
    buf.extend(record(FILE_ACTION_MODIFIED, "sub\\b.png", 0));

    let parsed = parse_notify_buffer(&buf);
    assert_eq!(
        parsed,
        vec![
            FsChange {
                action: FILE_ACTION_ADDED,
                name: "a.jpg".into()
            },
            FsChange {
                action: FILE_ACTION_MODIFIED,
                name: "sub\\b.png".into()
            },
        ]
    );
}

#[test]
fn a_truncated_record_ends_the_walk_instead_of_panicking() {
    let full = record(FILE_ACTION_MODIFIED, "photo.jpg", 0);
    // Every truncation, not one case: a buffer that ends inside the header, and
    // one that ends inside the name. The parser reads what the OS wrote into a
    // fixed buffer, so a short read is a decoding condition, not a bug.
    for cut in 0..full.len() {
        let partial = &full[..cut];
        let parsed = parse_notify_buffer(partial);
        assert!(
            parsed.is_empty(),
            "a buffer cut at {cut} bytes must not yield a half-decoded record"
        );
    }
    // ...and the intact one parses, so the loop above is not testing a parser
    // that returns nothing for everything.
    assert_eq!(parse_notify_buffer(&full).len(), 1);
}

#[test]
fn an_offset_that_does_not_advance_ends_the_walk() {
    // `NextEntryOffset` is bytes from the start of *this* record. A zero means
    // "last"; anything under the 12-byte header cannot advance the cursor and
    // must stop rather than loop.
    for bad in [1u32, 4, 11] {
        let buf = record(FILE_ACTION_MODIFIED, "x.jpg", bad);
        assert_eq!(parse_notify_buffer(&buf).len(), 1, "offset {bad}");
    }
}

#[test]
fn a_name_outside_ascii_survives_the_round_trip() {
    let buf = record(FILE_ACTION_ADDED, "ページ 01.jpg", 0);
    let parsed = parse_notify_buffer(&buf);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].name, "ページ 01.jpg");
}

#[test]
fn an_empty_buffer_has_no_records() {
    assert!(parse_notify_buffer(&[]).is_empty());
    assert!(parse_notify_buffer(&[0u8; 11]).is_empty());
}

// ---------------------------------------------------------------------------
// Batch folding
// ---------------------------------------------------------------------------

/// The five records a real rename-over produces on this box, in the order the
/// OS sent them (measured, `tests/scratch_watcher.rs` during development):
/// the temporary is added and modified, the *target* is removed, then the
/// temporary is renamed onto it.
fn rename_over_batch() -> Vec<FsChange> {
    let r = |action, name: &str| FsChange {
        action,
        name: name.into(),
    };
    vec![
        r(FILE_ACTION_ADDED, "page.jpg.tmp"),
        r(FILE_ACTION_MODIFIED, "page.jpg.tmp"),
        r(FILE_ACTION_REMOVED, "page.jpg"),
        r(FILE_ACTION_RENAMED_OLD_NAME, "page.jpg.tmp"),
        r(FILE_ACTION_RENAMED_NEW_NAME, "page.jpg"),
    ]
}

#[test]
fn a_replaced_name_keeps_the_record_that_says_it_came_back() {
    let folded = coalesce_changes(rename_over_batch());
    assert!(
        !folded.iter().any(|c| c.action == FILE_ACTION_REMOVED),
        "the replace target's removal is folded away: it was never gone"
    );
    assert!(
        folded
            .iter()
            .any(|c| c.action == FILE_ACTION_RENAMED_NEW_NAME && c.name == "page.jpg"),
        "the record that says the target's bytes are new must survive"
    );
}

#[test]
fn a_real_deletion_is_not_folded_away() {
    // The fold is keyed on *the same name returning in the same batch*. A file
    // that just went is the one case a rescan is for, and dropping it would
    // leave a vanished file in the gallery until the next launch.
    let folded = coalesce_changes(vec![FsChange {
        action: FILE_ACTION_REMOVED,
        name: "gone.jpg".into(),
    }]);
    assert_eq!(folded.len(), 1);
    assert_eq!(folded[0].action, FILE_ACTION_REMOVED);
}

#[test]
fn repeated_records_are_dropped_exactly() {
    let r = |action, name: &str| FsChange {
        action,
        name: name.into(),
    };
    let folded = coalesce_changes(vec![
        r(FILE_ACTION_MODIFIED, "a.jpg"),
        r(FILE_ACTION_MODIFIED, "a.jpg"),
        // A different action on the same name is a different record: a write
        // followed by a rename is not the same news as two writes.
        r(FILE_ACTION_RENAMED_NEW_NAME, "a.jpg"),
    ]);
    assert_eq!(folded.len(), 2);
}

// ---------------------------------------------------------------------------
// The decision table
// ---------------------------------------------------------------------------

/// Every arm of `classify_change`, spelled out. The failure this guards is
/// silent in both directions: an `Ignore` where `Content` belongs is a file
/// that never reloads, and a `Membership` where `Content` belongs is a full
/// folder scan every time a program saves a file it is editing.
#[test]
fn the_change_table_is_what_makes_a_watch_useful() {
    use ChangeKind::{Content, Ignore, Membership, Overflow};
    let rows = [
        // The file the user is looking at, re-saved in place.
        (FILE_ACTION_MODIFIED, true, true, Content),
        // Re-exported by writing a temporary and renaming it over the original
        // — the commonest way an image editor saves.
        (FILE_ACTION_RENAMED_NEW_NAME, true, true, Content),
        // A new picture in the folder: the listing is stale.
        (FILE_ACTION_ADDED, false, true, Membership),
        // A program's scratch file: not ours, and not worth a scan.
        (FILE_ACTION_ADDED, false, false, Ignore),
        // A file we list was deleted or renamed away: the listing is stale.
        (FILE_ACTION_REMOVED, true, true, Membership),
        (FILE_ACTION_RENAMED_OLD_NAME, true, true, Membership),
        // A scratch file being renamed away changes nothing we show. This is
        // the arm that keeps a rename-over at one stat instead of one scan.
        (FILE_ACTION_RENAMED_OLD_NAME, false, false, Ignore),
        // A *known* file renamed to a name we do not recognise as an image:
        // the entry is leaving the listing, so it is membership.
        (FILE_ACTION_RENAMED_NEW_NAME, false, false, Ignore),
        // A file appeared and came *back* under a name we already list: the
        // listing has to be re-derived (it may now contain it twice).
        (FILE_ACTION_ADDED, true, true, Membership),
        // The OS dropped events: nothing local knows what changed.
        (CHANGE_OVERFLOW, false, false, Overflow),
        // A value from a future Windows must not be guessed at.
        (99, true, true, Ignore),
    ];
    for (action, known, image, want) in rows {
        assert_eq!(
            classify_change(action, known, image),
            want,
            "action {action}, known {known}, image {image}"
        );
    }
    // A modified file we never listed, which *is* an image, means our listing
    // missed it (events were dropped, or it appeared between scans): rescan.
    assert_eq!(
        classify_change(FILE_ACTION_MODIFIED, false, true),
        Membership
    );
}

// ---------------------------------------------------------------------------
// The real watch
// ---------------------------------------------------------------------------

/// Drain batches until one record's name ends with `want`, or the budget runs
/// out. `ends_with` and not `contains`: a write to `page.jpg.tmp` and a rename
/// to `page.jpg` both mention the target, and only one of them is it.
fn wait_for(w: &DirWatcher, want: &str, budget: Duration) -> Option<FsChange> {
    let start = Instant::now();
    while start.elapsed() < budget {
        while let Some(batch) = w.try_recv() {
            if let Some(c) = batch.into_iter().find(|c| c.name.ends_with(want)) {
                return Some(c);
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

#[test]
fn a_rename_over_is_one_content_change_and_not_a_rescan() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("page.jpg");
    std::fs::write(&target, b"the old bytes").unwrap();
    let w = DirWatcher::start(dir.path().to_path_buf(), false, || {})
        .expect("a local temp folder must be watchable");

    // Exactly what an image editor's lossless save does: write a sibling, then
    // rename it over the original.
    let tmp = dir.path().join("page.jpg.tmp");
    std::fs::write(&tmp, b"the new bytes, longer").unwrap();
    std::fs::rename(&tmp, &target).unwrap();

    // Collect the whole batch the OS sends, not the first record. Measured on
    // this box, the sequence is `ADDED page.jpg.tmp`, `MODIFIED page.jpg.tmp`,
    // `REMOVED page.jpg`, `RENAMED_OLD page.jpg.tmp`, `RENAMED_NEW page.jpg` —
    // and the `REMOVED` arrives *before* the record that says the target is
    // back, so "the first event for the path" is the one that would trigger a
    // full rescan if the batch were taken one record at a time.
    let mut all = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(500) {
        while let Some(batch) = w.try_recv() {
            all.extend(batch);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!all.is_empty(), "a rename-over must produce events");

    let folded = coalesce_changes(all);
    let target_kinds: Vec<ChangeKind> = folded
        .iter()
        .filter(|c| c.name == "page.jpg")
        .map(|c| classify_change(c.action, true, true))
        .collect();
    assert!(
        target_kinds.contains(&ChangeKind::Content),
        "the renamed-over file must be a content change: {folded:?}"
    );
    assert!(
        !target_kinds.contains(&ChangeKind::Membership),
        "a rename-over must not ask for a folder rescan: {folded:?}"
    );

    // ...and the scratch file the editor wrote is not worth acting on. If it
    // were, every save would cost a rescan through the *other* path.
    for c in folded.iter().filter(|c| c.name == "page.jpg.tmp") {
        assert_eq!(
            classify_change(c.action, false, false),
            ChangeKind::Ignore,
            "a temporary write must not trigger work: {c:?}"
        );
    }
}

#[test]
fn a_recursive_watch_hears_a_change_in_a_subfolder() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("chapter 1");
    std::fs::create_dir(&sub).unwrap();
    let target = sub.join("p01.jpg");
    std::fs::write(&target, b"one").unwrap();

    let w = DirWatcher::start(dir.path().to_path_buf(), true, || {})
        .expect("a local temp folder must be watchable");
    std::fs::write(&target, b"two, and noticeably longer").unwrap();

    let ev = wait_for(&w, "p01.jpg", Duration::from_secs(10))
        .expect("one handle with bWatchSubtree must report files in subfolders");
    assert!(
        ev.name.contains("chapter 1"),
        "the name must be relative to the watched root, subfolder included; got {:?}",
        ev.name
    );
}

#[test]
fn a_quiet_folder_sends_no_events() {
    // The negative control: without this, `wait_for` passing could mean "the
    // watch reports everything, constantly". A watch that fires on nothing
    // would re-stat the gallery on a schedule, which is the poll it replaced.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("still.jpg"), b"x").unwrap();
    let w = DirWatcher::start(dir.path().to_path_buf(), false, || {})
        .expect("a local temp folder must be watchable");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(w.events(), 0, "a folder nobody is writing must stay silent");
    assert!(w.try_recv().is_none());
}

#[test]
fn a_watch_on_a_quiet_folder_drops_without_hanging() {
    // The thread is blocked in a call that returns only when something changes,
    // so a plain `join` would hang the UI thread on every folder switch and the
    // handle would leak one thread per folder visited. Pinned as a bound rather
    // than a comment: five start/stop cycles of a folder that never changes.
    let dir = tempfile::tempdir().unwrap();
    let t0 = Instant::now();
    for _ in 0..5 {
        let w = DirWatcher::start(dir.path().to_path_buf(), false, || {})
            .expect("a local temp folder must be watchable");
        // Let the thread reach the blocking call, so most cycles test the
        // cancel rather than the pre-block check.
        std::thread::sleep(Duration::from_millis(25));
        drop(w);
    }
    assert!(
        t0.elapsed() < Duration::from_secs(5),
        "five stops took {:?}",
        t0.elapsed()
    );
}

#[test]
fn stopping_a_watch_cancels_it_instead_of_leaking_the_thread() {
    // `Drop` returning quickly is also true of an implementation that abandons
    // the thread, and an abandoned thread holds an open directory handle for
    // the rest of the session - one per folder visited, invisible from outside.
    let before = image_viewer::watchers_detached();
    let dir = tempfile::tempdir().unwrap();
    for _ in 0..3 {
        let w = DirWatcher::start(dir.path().to_path_buf(), false, || {})
            .expect("a local temp folder must be watchable");
        std::thread::sleep(Duration::from_millis(20));
        drop(w);
    }
    assert_eq!(
        image_viewer::watchers_detached(),
        before,
        "every stop must cancel the blocking read, not abandon the thread"
    );
}

#[test]
fn a_watch_whose_folder_is_deleted_reports_itself_dead() {
    // The other way a watch can be silent: the read fails while the handle is
    // still open (a share without change notification, or the folder being
    // removed under it). The app's only defence is `alive()`, because a watch
    // that stays `Some` and never fires turns off the poll as well — the
    // viewer would stop noticing changes entirely.
    let dir = tempfile::tempdir().unwrap();
    let w = DirWatcher::start(dir.path().to_path_buf(), false, || {})
        .expect("a local temp folder must be watchable");
    std::thread::sleep(Duration::from_millis(50));
    assert!(w.alive());
    let path = dir.path().to_path_buf();
    std::fs::remove_dir_all(&path).unwrap();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) && w.alive() {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !w.alive(),
        "a watch whose folder is gone must report itself dead, or nothing will poll it"
    );
}

#[test]
fn a_failed_start_is_an_error_and_not_a_silent_empty_watch() {
    // A path that is not a directory: the app must be able to fall back to the
    // poll, which needs a real `Err` rather than a watch that never fires.
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-folder.jpg");
    std::fs::write(&file, b"x").unwrap();
    assert!(DirWatcher::start(file, false, || {}).is_err());
    assert!(DirWatcher::start(Path::new("Z:\\no-such-folder-iv").to_path_buf(), false, || {}).is_err());
}

// ---------------------------------------------------------------------------
// The app's wiring (source scan)
// ---------------------------------------------------------------------------

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

fn main_src() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut texts: Vec<String> = Vec::new();
    collect_src(&root, &mut texts);
    texts.sort();
    texts.join("\n")
}

fn collect_src(dir: &std::path::Path, texts: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_src(&path, texts);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            texts.push(std::fs::read_to_string(&path).expect("read"));
        }
    }
}

#[test]
fn the_app_polls_only_when_no_watch_is_running() {
    let src = main_src();

    // `maybe_rescan` is the whole arrangement: the watch branch comes first and
    // returns, so the timer below it *is* the fallback. A poll that ran
    // unconditionally would keep the 2 s idle repaint the watch exists to
    // remove, and would look identical from outside.
    let body = fn_body(&src, "fn maybe_rescan(");
    let watch = body
        .find("if self.watcher.is_some() {")
        .expect("the watch branch");
    let poll = body.find("self.check_changed(ctx)").expect("the fallback poll");
    let ret = body.find("            return;").expect("the watch branch returns");
    assert!(
        watch < ret && ret < poll,
        "the 2 s poll must sit in the fallback arm, after the watch branch returns"
    );

    // The watch is pointed at what is browsed by *derivation*, so a folder
    // switch, the recursive toggle and opening an archive all restart or stop it
    // without a call site to forget.
    let sync = fn_body(&src, "fn sync_watcher(");
    let archive = sync
        .find("if self.zip_archive.is_some()")
        .expect("the archive guard");
    let start = sync.find("DirWatcher::start").expect("the watch is started");
    assert!(
        archive < start,
        "archives must be excluded before the watch is started: their temp dir is \
         rewritten on open and deleted on close"
    );
    assert!(
        sync.contains("self.folder.clone()") && sync.contains("self.config.recursive_folders"),
        "the wanted watch is (folder, recursive) or nothing"
    );
    let once = sync
        .find("if self.watcher_key.as_ref() == want.as_ref()")
        .expect("the once-per-folder guard");
    assert!(
        once < start,
        "the guard must come before the start: without it a folder that refuses a \
         watch is asked again every frame, each ask a stat plus a thread"
    );

    // Every event reaches the same funnel a change always went through, and the
    // only counting is the one a probe asserts on. The content half is
    // **deferred**, not acted on in the drain: a save arrives as several events
    // and decoding the first one decodes a half-written file.
    let drain = fn_body(&src, "fn drain_watch_events(");
    assert!(drain.contains("classify_change"));
    assert!(drain.contains("coalesce_changes"));
    assert!(drain.contains("self.pending_content.insert(p, now)"));
    assert!(
        drain.contains("C_WATCH_EVENTS"),
        "the received count is what tells a silent OS from an ignored event"
    );
    let flush = fn_body(&src, "fn flush_content_changes(");
    assert!(flush.contains("self.invalidate_path(&p, ctx)"));
    assert!(flush.contains("perf::C_WATCHED_CHANGES"));
    assert!(
        flush.contains("CONTENT_SETTLE"),
        "the settle is the difference between one decode and one per write chunk"
    );
    assert!(
        body.find("self.drain_watch_events(ctx)").unwrap_or(usize::MAX)
            < body.find("self.flush_content_changes(ctx)").unwrap_or(0),
        "the drain must run before the flush, or a change waits a frame"
    );
    assert!(
        body.find("!w.alive()").unwrap_or(usize::MAX)
            < body.find("if self.watcher.is_some() {").unwrap_or(0),
        "a watch that died must be dropped *before* the branch that trusts it, or the \
         viewer keeps a watch that reports nothing and skips the poll"
    );
}
