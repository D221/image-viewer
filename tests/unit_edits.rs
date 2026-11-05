//! Save as/paste, undo/redo history, the transform tail and ratings.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// Save-as must be **wired**, not merely present as functions.
///
/// `main.rs` is unreachable from every test, so these are source scans. Each one
/// exists because the feature it covers has a specific way of silently doing
/// nothing: a chord bound to a key the toolkit swallows, a panel that is drawn
/// after an early return so it is invisible in half the states, a defaults
/// value that differs from the behaviour it claims to match.
#[test]
fn save_as_and_paste_are_wired() {
    let src = main_src();
    let lib = lib_src();

    // --- save-as: the chord, the action, the panel, the call ---
    // The action table lives in `lib.rs`, so the *listing* is checked there and
    // the wiring here. Reading one file for both would have made this assertion
    // vacuous rather than wrong.
    assert!(
        lib.contains(r#"("save_as", "Ctrl+Shift+A", "Save as (choose file, quality, size)"),"#),
        "save_as must be a listed action so it appears in Settings and is rebindable"
    );
    assert!(
        src.contains(r#"binding_down(i, &kb, "save_as")"#),
        "the action must be read from the binding table"
    );
    assert!(
        src.contains("            } else if save_as {\n                self.open_save_as();"),
        "the chord must reach open_save_as"
    );
    assert!(
        src.contains("fn do_save_as(&mut self)"),
        "and open_save_as must lead to an actual save, not just a panel"
    );
    assert!(
        src.contains("save_transformed_to(&p, edit.xform, edit.crop, &dest, quality, max_long)"),
        "the panel's numbers must reach the library call - a save that ignores \\
         the quality slider is a slider that lies"
    );

    // **Defaults are the current behaviour.** This is the requirement, so it is
    // asserted rather than described: open_save_as must pre-fill the unique
    // name Ctrl+S would have chosen, with quality at the shared default and no
    // resize. A panel that opens on a different name means "save as" quietly
    // changed what saving does.
    let open = fn_body(&src, "fn open_save_as(&mut self)").expect("open_save_as");
    assert!(
        open.contains("self.save_as_infix(&p)"),
        "open_save_as must ask the shared rule for the name, not pick one itself \
         - hardcoding a crop test and otherwise saying `rotated` cannot tell \
         turned from untouched, and named every unedited picture `_rotated`"
    );
    assert!(
        open.contains("DEFAULT_JPEG_QUALITY"),
        "and the quality must start at the shared default"
    );
    assert!(
        open.contains("self.save_as.max_long = String::new()"),
        "and the size must start blank, which means 'do not resize'"
    );
    // The composition is asserted separately, because "called the shared rule"
    // and "asked it about the *composed* transform" are two properties and the
    // report was the second one.
    let infix = fn_body(&src, "fn save_as_infix(&self, path: &Path)").expect("save_as_infix");
    assert!(
        infix.contains("display_xform") && infix.contains("exif_orientation"),
        "the infix must be asked of the *composed* transform: Ctrl+S names from \
         display_xform(exif, user), so a photo stored sideways is `_rotated` on \
         its own and an untouched one is `_copy`"
    );
    assert!(
        infix.contains("save_infix("),
        "and the rule itself must be the library's shared save_infix, so the two \
         callers cannot answer the same question differently"
    );

    // The panel must be drawn *before* the early returns, or it is invisible
    // over the grid and over the archive placeholder - the two states where a
    // reader is most likely to want it.
    let central = fn_body(&src, "fn central(&mut self, ui: &mut egui::Ui)").expect("central");
    let panel_at = central.find("if self.save_as.open {").expect("panel draw");
    let grid_at = central.find("if self.gallery_view {").expect("grid branch");
    assert!(
        panel_at < grid_at,
        "the save-as panel must be drawn before the grid's early return, or it \\
         cannot be opened while the grid is up"
    );

    // **Esc closes the panel, and it is checked before `typing` as well as
    // before the quit.** This is a reported bug, and the arm order is the whole
    // content of the fix: the panel's path box holds focus the moment it opens,
    // so `typing` is true, so Esc landed in the search box's arm - clearing the
    // *filter* and leaving the panel standing - and one Esc later fell through
    // to the quit branch. Both halves are asserted, because "before typing"
    // alone would still let Esc clear the search filter.
    //
    // **Absolute offsets, not `fn_body`.** `fn_body` starts counting *after* its
    // marker, so `fn_body(&src, "if esc {")` returns the body of the panel arm
    // alone - the marker already consumed the `if`'s own brace - and this
    // assertion then failed on "typing esc arm" for a chain that was correct. The
    // helper answers "the body of the next thing", which is the wrong question
    // for an if/else chain: what matters is where the arms sit *relative to each
    // other*, so the offsets are taken from `if esc {` and compared.
    let esc_at = src.find("if esc {").expect("the Esc chain");
    let esc_panel =
        src[esc_at..].find("if self.save_as.open {").map(|i| i + esc_at).expect("panel esc arm");
    let esc_typing =
        src[esc_at..].find("} else if typing {").map(|i| i + esc_at).expect("typing esc arm");
    let esc_quit =
        src[esc_at..].find("self.config.esc_closes_app").map(|i| i + esc_at).expect("quit arm");
    assert!(
        esc_panel < esc_typing,
        "the save-as panel's Esc arm must come before the `typing` arm: the \
         panel's path box has focus, so `typing` is true while it is open and \
         Esc would clear the search filter instead of closing the panel"
    );
    assert!(
        esc_panel < esc_quit,
        "and before the quit branch: Esc on a modal that can write a file must \
         never close the app"
    );
}

/// The undo stack is two per-path vectors and a rule about when each moves.
/// Truth table first; the naming here matches the fields so a reader can check
/// the implementation against this comment without opening it.
#[test]
fn undo_and_redo_walk_the_edit_history() {
    use image_viewer::{Edit, EditHistory, Xform};

    let p = std::path::Path::new("a.jpg");
    let mut h = EditHistory::default();

    // Review Focus #1: undo with nothing recorded is a no-op, not a panic.
    assert_eq!(h.undo(p, Edit::default()), None, "nothing to undo");
    assert!(!h.can_undo(p));
    assert!(!h.can_redo(p));

    // Record three states: identity, then turned, then turned twice.
    //
    // `record` is called **before** each change, so three records mean three
    // changes and the image is now in a *fourth* state. That is why the walk
    // below starts from `d` and not from the last recorded state `c`: treating
    // `c` as the current state is the off-by-one that makes this test read as
    // though undo handed back the state one *below* the top of the stack, which
    // is a different data structure and a wrong one.
    let a = Edit { xform: Xform::Identity, crop: None };
    let b = Edit { xform: Xform::Rot90, crop: None };
    let c = Edit { xform: Xform::Rot180, crop: None };
    let d = Edit { xform: Xform::Rot270, crop: None };
    h.record(p, a);
    h.record(p, b);
    h.record(p, c);
    assert_eq!(h.len_for(p), 3);
    assert!(h.can_undo(p));

    // Undo returns the *previous* state and moves the current one to redo.
    assert_eq!(h.undo(p, d), Some(c), "undo hands back the earlier edit");
    assert_eq!(h.undo(p, c), Some(b), "and again");
    assert_eq!(h.undo(p, b), Some(a), "down to the original state");
    assert_eq!(h.undo(p, a), None, "past the start is a no-op");
    assert!(h.can_redo(p));

    // Redo walks back the other way, one state per press.
    assert_eq!(h.redo(p, a), Some(b));
    assert_eq!(h.redo(p, b), Some(c));
    assert_eq!(h.redo(p, c), Some(d));
    assert_eq!(h.redo(p, d), None, "and stops");
}

/// Two properties that are both one line of implementation and both invisible
/// when broken: history is **per path**, and a new edit **clears redo**.
#[test]
fn history_is_per_path_and_a_new_edit_clears_redo() {
    use image_viewer::{Edit, EditHistory, Xform};

    let a = std::path::Path::new("a.jpg");
    let b = std::path::Path::new("b.jpg");
    let mut h = EditHistory::default();
    let base = Edit { xform: Xform::Identity, crop: None };
    let turned = Edit { xform: Xform::Rot90, crop: None };

    // Review Focus #2: two images keep two independent histories. A shared
    // stack would answer B's undo with A's edit.
    h.record(a, base);
    h.record(b, base);
    assert_eq!(h.undo(a, turned), Some(base), "a has one step");
    assert_eq!(h.undo(b, turned), Some(base), "b has one step, unaffected");
    assert!(!h.can_undo(a), "a is exhausted");

    // Review Focus #4: a new edit rewrites the future.
    let mirrored = Edit { xform: Xform::FlipH, crop: None };
    h.record(a, base);
    assert!(!h.can_redo(a), "recording again clears redo for that path");
    assert_eq!(h.redo(a, mirrored), None, "and the old future is unreachable");
}

/// The cap drops the **oldest** step, so the newest edit is never the one
/// refused: a viewer that silently ignored a rotate because its history was full
/// would be worse than one that forgot the deepest step.
#[test]
fn the_history_is_capped_and_drops_the_oldest_step() {
    use image_viewer::{Crop, Edit, EditHistory};
    let p = std::path::Path::new("a.jpg");
    let mut h = EditHistory::default();

    // 60 recorded states; the cap is 50. **Every state is distinct**, and that is
    // the load-bearing part of this fixture: the crop width carries the index, so
    // "which record is this?" is answerable without doing arithmetic on the cap.
    //
    // The earlier version of this test cycled `Xform::quarters_cw(i % 4)` and
    // identified records by that cycle's phase, which discriminated drop-oldest
    // from drop-newest **only when `(60 - cap) % 4 != 0`**. At cap 50 that is
    // true by luck (drop 10), and at cap 48 it is false (drop 12), so the whole
    // assertion went blind at exactly the bug it was added to catch — a tuning
    // change to the cap would have removed the check without any test failing.
    // Distinct records make the window below a fact about the data rather than
    // about the phase of a cycle.
    //
    // The crop also keeps each state non-identity, so a future "do not record a
    // no-op edit" optimisation could not silently zero this fixture out.
    for i in 0..60u8 {
        h.record(
            p,
            Edit {
                xform: image_viewer::Xform::Identity,
                crop: Crop::new(0, 0, 1 + u32::from(i), 1),
            },
        );
    }
    let cap = h.len_for(p);
    assert_eq!(cap, 50, "the stack is bounded");

    // Undoing walks back one state per press, and one more is a no-op rather than
    // a panic or another image's edit. The count comes from `len_for`, so the
    // walk cannot silently disagree with the cap if that is ever retuned.
    let mut undone = Vec::new();
    for _ in 0..cap {
        undone.push(h.undo(p, Edit::default()).expect("one step per recorded entry"));
    }
    let width = |e: &Edit| e.crop.map(|c| c.w);

    // The **order** of what comes back is the half that pins *which* end is
    // dropped, and the length assertion above does not: dropping the newest
    // instead of the oldest leaves the stack at 50 too, and the same 50 undos
    // still bottom out in `None`. Both halves are asserted because a test that
    // cannot fail on the bug it is named for is decoration — verified by
    // mutation: with `remove(0)` swapped for `pop()` the **un-strengthened
    // version of this test** passed unchanged.
    assert_eq!(
        undone.first().and_then(width),
        Some(60),
        "undo walks back from the newest record, not the oldest"
    );
    // Dropping the oldest 10 leaves records 10..=59, whose crop widths are
    // 11..=60, so the last undo must hand back width 11. Derived from `cap`
    // rather than hardcoded, so this expectation survives a retune instead of
    // quietly agreeing with whatever the implementation happens to do.
    assert_eq!(
        undone.last().and_then(width),
        Some(61 - cap as u32),
        "the oldest surviving record is the last undo, not the first"
    );
    assert_eq!(h.undo(p, Edit::default()), None, "and it stops there");
}

/// The four transform buttons share one tail, so `record` belongs there once.
/// Asserted by **byte offset inside `apply_to_current`**, because presence is
/// exactly what was true of the defect this guards: a call present, in the right
/// function, after the edit.
#[test]
fn the_transform_tail_records_before_it_edits() {
    let src = main_src();
    let body = fn_body(&src, "fn apply_to_current(").expect("apply_to_current");
    let at = body.find("history.record(").expect("record must be called");
    let edit = body.find("f(&mut self.edits, &p.path)").expect("the edit itself");
    assert!(
        at < edit,
        "record must run BEFORE the edit is applied, or undo restores the state \
         after the change and every undo is a no-op"
    );
}

/// Both keys are rebindable actions, and both are chords rather than bare
/// letters, so neither may appear in BARE_LETTERS.
#[test]
fn undo_and_redo_are_rebindable_chord_actions() {
    let lib = lib_src();
    assert!(
        lib.contains(r#"("undo", "Ctrl+Z", "Undo view transform"),"#),
        "undo must be a KEY_ACTIONS row so it shows in Settings and can be rebound"
    );
    assert!(
        lib.contains(r#"("redo", "Ctrl+Shift+Z", "Redo view transform"),"#),
        "redo must be a rebindable action too"
    );
}

/// The buttons sit with the transforms and grey out when there is nothing to
/// undo. **Disabled state is the point**, not polish: an always-enabled undo that
/// sometimes does nothing reads as broken, and one that is never enabled reads
/// as absent.
#[test]
fn the_undo_buttons_sit_with_the_transforms_and_disable_when_empty() {
    let src = main_src();
    let at = src.find(r#""Undo""#).expect("an Undo button");
    let near = &src[at.saturating_sub(2500)..(at + 2500).min(src.len())];
    assert!(
        near.contains("can_undo") && near.contains("can_redo"),
        "the undo/redo buttons must be gated on can_undo/can_redo so they disable \
         when there is nothing to undo"
    );
}

/// The rating block lives in the **left pane, above the file's own metadata**,
/// and — this is the part that is easy to get wrong — its read must NOT be part
/// of `wants_exif`. That gate exists to stop a 7-12 ms whole-file read; a rating
/// is a 300-byte sidecar, and joining them would mean the rating vanishes from
/// the UI whenever the pane is hidden.
#[test]
fn the_rating_block_is_in_the_left_pane_and_outside_the_exif_gate() {
    // `fn ui_exif(` lives in `app/ui/exif.rs`: the rating heading, the EXIF
    // heading, and the `wants_exif` gate that wraps only the EXIF read are all
    // inside it, so their order inside this one function is the property.
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app/ui/exif.rs"),
    )
    .expect("src/app/ui/exif.rs");
    // All three markers occur once in `app_ui/exif.rs`, all inside `fn ui_exif(`:
    // the rating heading, the EXIF heading, and the `wants_exif` gate that wraps
    // only the EXIF read. The rating block must come before the gate - it is a
    // separate path and must not vanish when the pane is hidden.
    let rating = src.find(r#"ui.heading("Rating")"#).expect("the rating heading in the left pane");
    let gate = src.find("wants_exif(").expect("the EXIF gate wants_exif");
    let _heading = src.find(r#"ui.heading("EXIF")"#).expect("the EXIF heading");
    assert!(
        rating < gate,
        "the rating block precedes the EXIF gate: it is not folded into it, so \
         hiding the pane cannot hide the rating"
    );
}

/// Archives get no ratings: there is nowhere to write `vol1.cbz/p003.jpg.xmp`,
/// and a rating that disappears on leaving the archive is worse than a refusal.
#[test]
fn archives_are_refused_a_rating() {
    let lib = lib_src();
    assert!(
        fn_body(&lib, "pub fn sidecar_path(").expect("sidecar_path").contains("is_archive_path"),
        "sidecar_path must return None for an archive member, which is what \
         refuses the write"
    );
}

/// The grid must read ratings from a **map the scan filled**, not stat a
/// sidecar per cell per frame: `grid_cols` can be 24 across and the folder
/// thousands deep, so a stat per visible cell is a filesystem storm in the paint
/// frame.
#[test]
fn the_grid_reads_ratings_from_a_map_not_the_filesystem() {
    let src = main_src();
    let cell = fn_body(&src, "fn grid_cell(").expect("grid_cell");
    assert!(
        !cell.contains("Ratings::read("),
        "grid_cell must not read sidecars - that is a filesystem read per cell \
         per frame; it takes them from the ratings map"
    );
    assert!(cell.contains("ratings") || cell.contains("rating"), "and it must read a ratings map");
}
