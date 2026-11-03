//! The overrides: what the user said, and where it is written.
//!
//! Split out of `tests/spreads.rs`; the test that an override *beats* the detector
//! stayed in `spreads.rs` with the pairing, because that is the claim it makes.
//!
//! Everything here is about the file: an override is only worth anything if it
//! survives a restart, so this inherits the thumbnail cache's rules -
//! `%LOCALAPPDATA%` and not `%APPDATA%`, write to a temporary name and rename, and
//! **every failure is an empty override set** - nothing a user or an editor does to
//! this file may be able to stop the app starting.

use image_viewer::{
    GallerySpreads, LEAD_LONE_DEFAULT, LEAD_LONE_MAX, SPREADS_MAX_BYTES, SPREADS_MAX_PAGES, Shape,
    load_spread_overrides, save_spread_overrides, spread_root_key, spreads_root, spreads_tmp,
    thumb_cache_root,
};

use Shape::{Double, Single};

/// A set survives a restart, which is the whole reason it is written down.
///
/// **Both** fields are non-default, because a round trip that only carries the
/// pages would pass against a `lead_lone` that was never serialised. The store is
/// written to a directory that does not exist yet, since that is the first
/// gallery a user opens.
///
/// The last assertion is that a **successful** save leaves the store and nothing
/// else in the directory, which catches a write that reached the temporary name
/// and stopped there. It is deliberately not presented as a test of the rename's
/// atomicity: a mutation that writes the store directly and skips the temporary
/// altogether passes every test in this file, and that was run rather than
/// assumed. Crash-atomicity is not observable without fault injection, so it rests
/// on `fs::rename` replacing on Windows Ã¢â‚¬â€ the same thing `write_cached_thumbnail`
/// rests on, and likewise untested there.
#[test]
fn overrides_survive_a_reload_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("nested").join("spreads.json");
    let root = dir.path().join("books");
    let page = root.join("003.png");

    let mut g = GallerySpreads::default();
    g.nudge_lead_lone(1);
    g.set_shape(&page, 0, Double);
    g.set_shape(&page, 4, Single);
    save_spread_overrides(&store, &root, &g).expect("a fresh directory is created");

    let loaded = load_spread_overrides(&store, &root);
    assert_eq!(loaded, g, "everything written came back");
    assert_eq!(loaded.lead_lone, LEAD_LONE_DEFAULT + 1, "the offset is stored, not reset");
    assert_eq!(loaded.shape_for(&page, 0), Some(Double));
    assert_eq!(loaded.shape_for(&page, 4), Some(Single));
    assert_eq!(loaded.shape_for(&page, 1), None, "a page nobody overrode is still un-overridden");

    let mut left: Vec<String> = std::fs::read_dir(store.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec!["spreads.json".to_string()],
        "the temporary is renamed into place, not left beside it"
    );
}

/// "Empty" means what it says, and the second half is the one that matters.
///
/// A store that fails to read quietly producing `lead_lone = 0` would pair the
/// cover with page 2 in **every gallery opened after it** Ã¢â‚¬â€ the pairing changes,
/// nothing on screen says why, and the cause is a file the user never knew
/// existed. So the default offset is asserted by value, not by `== Default`.
fn assert_empty(g: &GallerySpreads) {
    assert!(
        g.pages.is_empty(),
        "a store that cannot be read must not name any page, got {:?}",
        g.pages
    );
    assert_eq!(
        g.lead_lone, LEAD_LONE_DEFAULT,
        "an empty override set still stands the cover alone; 0 would silently re-pair every gallery"
    );
}

/// A file this app writes must never be able to stop it starting.
///
/// Three ways of being unusable, because they fail differently and only the first
/// is obvious: not JSON at all, JSON of the wrong *shape*, and a path that cannot
/// be read at all (a directory where the file should be Ã¢â‚¬â€ the read error that is
/// not "not found"). All three are the same empty override set. There is no repair
/// and no warning anywhere, deliberately: the alternative is refusing to open a
/// folder over a cache file, which is a far worse outcome than losing an override
/// the user can re-press.
#[test]
fn a_malformed_overrides_file_is_an_empty_override_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = dir.path().join("books");

    std::fs::write(&store, "{ not json").unwrap();
    assert_empty(&load_spread_overrides(&store, &root));

    std::fs::write(&store, "[1, 2, 3]").unwrap();
    assert_empty(&load_spread_overrides(&store, &root));

    std::fs::remove_file(&store).unwrap();
    std::fs::create_dir_all(&store).unwrap();
    assert_empty(&load_spread_overrides(&store, &root));
}

/// No file yet is the state every gallery is in until the user overrides
/// something, so it is a first-class case rather than an accident of the malformed
/// one above.
#[test]
fn a_missing_overrides_file_is_an_empty_override_set() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("not-created-yet").join("spreads.json");
    assert_empty(&load_spread_overrides(&store, &dir.path().join("books")));
}

/// The offset is nudged with a key, and a key repeats. It cannot leave the range
/// [`LEAD_LONE_MAX`] documents: past it, "is this page front matter?" has no
/// answer, and every leading page would be shown alone.
///
/// Both extremes are given as `i32::MIN`/`MAX` rather than a large literal,
/// because the interesting version of this is arithmetic that **wraps** rather than
/// arithmetic that is merely big: `0u32` minus `1` is four billion, which is in
/// range, and the result would be a gallery of nothing but single pages.
#[test]
fn nudging_lead_lone_is_clamped_to_the_documented_range() {
    let mut g = GallerySpreads::default();
    assert_eq!(g.lead_lone, LEAD_LONE_DEFAULT, "a fresh value stands the cover alone");

    g.nudge_lead_lone(0);
    assert_eq!(g.lead_lone, LEAD_LONE_DEFAULT, "a nudge of nothing changes nothing");

    for _ in 0..LEAD_LONE_DEFAULT {
        g.nudge_lead_lone(-1);
    }
    assert_eq!(g.lead_lone, 0);
    g.nudge_lead_lone(-1);
    assert_eq!(g.lead_lone, 0, "one below the floor is still the floor");
    g.nudge_lead_lone(i32::MIN);
    assert_eq!(g.lead_lone, 0, "a huge negative nudge clamps; it must not wrap");

    for _ in 0..LEAD_LONE_MAX {
        g.nudge_lead_lone(1);
    }
    assert_eq!(g.lead_lone, LEAD_LONE_MAX);
    g.nudge_lead_lone(1);
    assert_eq!(g.lead_lone, LEAD_LONE_MAX, "LEAD_LONE_MAX is the ceiling");
    g.nudge_lead_lone(i32::MAX);
    assert_eq!(g.lead_lone, LEAD_LONE_MAX, "a huge positive nudge clamps; it must not wrap");
}

/// `toggle` answers with the shape the page landed on **and stores it**, because
/// the caller needs the answer to re-plan against Ã¢â‚¬â€ the tiling it is asking about
/// has already changed by the time this returns.
///
/// The stored value is deliberately *not* the input: the same `detected` answers
/// the same way twice. That is the contract, not an accident of the order of two
/// statements Ã¢â‚¬â€ `toggle` does not read its own last answer back, so the flip is
/// always away from what the *caller* says the page is now rather than away from
/// what this store last wrote. (For the caller that passes the effective shape the
/// two agree every time; where they part company is an override that has outlived
/// the file it was set against.)
#[test]
fn toggling_a_page_reports_the_shape_it_landed_on() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("001.png");
    let mut g = GallerySpreads::default();

    assert_eq!(g.toggle(&page, 0, Double), Single, "a spread becomes one page");
    assert_eq!(g.shape_for(&page, 0), Some(Single), "and it is stored, not just returned");
    assert_eq!(g.toggle(&page, 0, Single), Double, "and the other way");
    assert_eq!(g.shape_for(&page, 0), Some(Double));

    assert_eq!(g.toggle(&page, 0, Single), Double, "the same input answers the same way");
    assert_eq!(g.shape_for(&page, 0), Some(Double), "so a second press re-asserts it");

    // Pages are separate keys: flipping page 0 must not disturb page 1.
    g.set_shape(&page, 1, Single);
    g.toggle(&page, 0, Double);
    assert_eq!(g.shape_for(&page, 1), Some(Single), "page 1 was not touched");
    assert_eq!(g.shape_for(&page, 0), Some(Single));
}

/// Two instances of the app must not write the same temporary.
///
/// **The reader guarantee holds either way** Ã¢â‚¬â€ `fs::rename` replaces on Windows, so
/// a reader sees either the old store or the new one, never a half-written file.
/// What a *fixed* temporary name breaks is what gets renamed: `fs::write` opens with
/// `Create`/`TRUNCATE`, so two savers milliseconds apart both hold a handle to the
/// same file, both truncate it, and their writes interleave. The first to finish
/// renames a mixture of the two over `spreads.json` Ã¢â‚¬â€ and because an unparseable
/// store is treated as empty and replaced, the next override press then rewrites the
/// file with one gallery's entry and every other gallery's decisions are gone. There
/// is no second copy anywhere, so that is not recoverable.
///
/// **Two halves, because the property has two halves.** Naming is a pure function of
/// the store path and an id, so "two ids, two names" is checkable Ã¢â‚¬â€ but that alone
/// does not pin the id the app *passes*, and a call site passing a constant would
/// sail past it. So the second half observes the real thing: a save whose rename
/// cannot succeed leaves its temporary behind, and the name it leaves is
/// `spreads.json.tmp.<this process's id>`.
///
/// The rename fails when the store path is a **directory**, which is also the only
/// way to see a temporary at all Ã¢â‚¬â€ a successful save renames it away. Nothing here
/// can see a *torn* write, and that is the part that stays untested: crash-atomicity
/// is not observable without fault injection, and the thumbnail cache has no test for
/// it either. Its fixed `.tmp` is not the precedent to copy, though Ã¢â‚¬â€ that is safe
/// only because two writers of one entry write the same bytes, and this store is a
/// read-modify-write, so they do not.
#[test]
fn two_instances_of_the_app_cannot_write_the_same_temporary() {
    let store = std::path::Path::new(r"C:\Users\me\AppData\Local\image-viewer\spreads.json");

    let one = spreads_tmp(store, 1000);
    let two = spreads_tmp(store, 2000);

    assert_ne!(one, two, "two processes must not share a temporary name");
    assert_eq!(
        one.parent(),
        store.parent(),
        "beside the store, so the rename is on one filesystem and therefore atomic"
    );
    assert_eq!(
        one,
        store.with_file_name("spreads.json.tmp.1000"),
        "the store's own name with .tmp.<pid> appended - not with_extension, which \
         would rewrite the .json and read as a different file"
    );
    assert!(two.to_string_lossy().ends_with(".tmp.2000"));

    // ...and the id the app actually passes. The store path is a directory here, so
    // the write succeeds and the rename cannot: `MoveFileEx` will not replace a
    // directory with a file.
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    std::fs::create_dir(&store).unwrap();
    let g = GallerySpreads::default();
    save_spread_overrides(&store, dir.path(), &g)
        .expect_err("renaming a file over a directory must fail, or this test proves nothing");
    // The `ErrorKind` is not asserted: Windows reports `ERROR_ACCESS_DENIED`, so it
    // is `PermissionDenied` here, and pinning it would be pinning an OS's choice
    // rather than this function's behaviour. That the save *reported* failure and
    // still left the temporary behind is the whole of what is being checked.

    let mut left: Vec<String> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    // The store path itself is in there too Ã¢â‚¬â€ it is the directory this test put
    // there Ã¢â‚¬â€ so it is excluded rather than the assertion being weakened to
    // "contains".
    left.retain(|n| n != "spreads.json");
    left.sort();
    assert_eq!(
        left,
        vec![format!("spreads.json.tmp.{}", std::process::id())],
        "the temporary this process left behind carries its own id, so a second \
         instance of the app writes beside it rather than into it"
    );
}

/// An over-cap store is **left alone by a save**, and honoured as empty by a load.
///
/// **The two halves are separate assertions because one cannot witness the other.**
/// Both a load and a correctly-refusing save leave an over-cap store *looking* the
/// same to the next load Ã¢â‚¬â€ empty either way Ã¢â‚¬â€ so an assertion on the load alone
/// cannot tell "left alone" from "rewritten as empty", which is the failure that
/// matters: the store is a read-modify-write, so a save that could not read it would
/// replace that gallery's real entry, and every other gallery's with it, and then
/// there is no second copy anywhere. (This is the same trap as the two caps in
/// `the_two_caps_degrade_to_nothing_rather_than_to_an_error`, where one cap could
/// satisfy the other's assertion; the remedy is the same Ã¢â‚¬â€ state what each half
/// proves, and check the premises.)
///
/// The fixture is **hand-built**, because a correct save refuses to write a store
/// over the cap Ã¢â‚¬â€ asking `save_spread_overrides` to produce one would be asking for
/// the bug this test exists to catch. It is checked three ways before it is used:
/// over the cap, parseable, and still holding the entry that a clobbering save would
/// destroy.
#[test]
fn an_over_cap_store_is_left_alone_by_a_save_and_ignored_by_a_load() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = std::path::PathBuf::from(r"C:\books\vol1.cbz");

    // Wide names rather than many of them, so the *page count* stays under
    // `SPREADS_MAX_PAGES` and the byte cap is unambiguously the one in play.
    let wide: String = "p".repeat(200);
    // The root goes into a JSON string, so its backslashes are escaped. That is not
    // a detail: the first version of this fixture wrote the raw path, and the
    // "must be a valid store" premise check below caught it on the first run.
    let key = root.to_string_lossy().replace('\\', "\\\\");
    let mut json = format!(
        "{{ \"{key}\": {{ \"lead_lone\": 2, \"pages\": {{ \"001.png\": {{ \"0\": \"double\" }}"
    );
    for i in 0..5_000usize {
        json.push_str(&format!(", \"{wide}{i}.png\": {{ \"0\": \"double\" }}"));
    }
    json.push_str(" } } }");
    std::fs::write(&store, &json).unwrap();

    let len = std::fs::metadata(&store).unwrap().len();
    assert!(
        len > SPREADS_MAX_BYTES,
        "this fixture must be over the byte cap ({len} bytes) or nothing is being tested"
    );
    let before = std::fs::read_to_string(&store).unwrap();
    serde_json::from_str::<serde_json::Value>(&before).expect("the fixture must be a valid store");
    let pages_in_fixture = before.matches("\": { \"0\": \"double\" }").count();
    assert!(
        pages_in_fixture <= SPREADS_MAX_PAGES,
        "the fixture has {pages_in_fixture} page entries, over SPREADS_MAX_PAGES, so \
         the page cap could be what is being tested instead of the byte cap"
    );

    // Half one: the load honours nothing, which is safe because a load only reads.
    assert_empty(&load_spread_overrides(&store, &root));

    // Half two: the save refuses, so the bytes are untouched. An `Err` *and* no
    // write Ã¢â‚¬â€ both are asserted, because a save that reported success over the cap
    // and one that reported failure after writing it are different bugs with the
    // same consequence.
    let mut small = GallerySpreads::default();
    small.set_shape(&root.join("001.png"), 0, Shape::Single);
    assert!(
        save_spread_overrides(&store, &root, &small).is_err(),
        "a save that cannot read the store must report that it did not persist, not \
         return Ok and leave the caller thinking the decision was saved"
    );
    let after = std::fs::read_to_string(&store).unwrap();
    assert_eq!(
        after, before,
        "an over-cap store must be left byte-for-byte intact: a save that read it as \
         empty would have replaced this gallery's real entry with the value it just \
         handed in, and every other gallery's with nothing"
    );

    // ...and "intact" is checked against the *current* file's content, not only
    // against the bytes captured before: the entry that would have been destroyed is
    // still in it, parsed independently of this library's own reader.
    let parsed: serde_json::Value = serde_json::from_str(&after).expect("the file still parses");
    assert_eq!(
        parsed[&*root.to_string_lossy()]["pages"]["001.png"]["0"],
        "double",
        "the override a clobbering save would have destroyed is still in the file"
    );

    // The control, so "nothing was written" cannot be confused with "nothing could
    // be written": a store inside the cap does take the new value.
    let small_store = dir.path().join("small.json");
    save_spread_overrides(&small_store, &root, &small).unwrap();
    assert_eq!(
        load_spread_overrides(&small_store, &root).shape_for(&root.join("001.png"), 0),
        Some(Single),
        "a store inside the cap is read-modify-written as usual"
    );
}

/// The written spelling of a shape is pinned, because the file is meant to be edited
/// by hand.
///
/// `an_older_or_hand_edited_store_still_loads` proves that `"double"` *loads*, and a
/// round trip stays green under any self-consistent serde setting Ã¢â‚¬â€ including one
/// that writes `"Double"` and reads `"Double"`, which would leave a hand-edited file
/// loading as nothing at all. So this asserts the bytes, not the round trip.
#[test]
fn the_written_shape_is_the_spelling_the_file_documents() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = dir.path().join("books");

    let mut g = GallerySpreads::default();
    g.set_shape(&root.join("001.png"), 0, Double);
    g.set_shape(&root.join("002.png"), 0, Single);
    save_spread_overrides(&store, &root, &g).unwrap();

    let text = std::fs::read_to_string(&store).unwrap();
    assert!(
        text.contains("\"double\""),
        "the documented spelling must be what is written, not just what loads:\n{text}"
    );
    assert!(
        !text.contains("\"Double\""),
        "serde's own variant spelling leaked into the file:\n{text}"
    );
}

/// A page key that is not a number costs **that page**, and nothing else.
///
/// This is the seam between two documented rules that pull in opposite directions,
/// so it is worth naming rather than leaving to be discovered:
///
/// * A store that cannot be **read at all** Ã¢â‚¬â€ missing, malformed, the wrong shape Ã¢â‚¬â€
///   is an empty override set. One unusable byte costs the user every override.
///
/// * A store that reads, with one page key that is not a number, keeps every other
///   override. The alternative is failing the parse over a key nothing can address
///   anyway, which turns one typo into the whole-empty case above.
///
/// The second is the *tolerant* reading and it is the one implemented, because the
/// cost asymmetry is not symmetric: dropping one unreachable key loses nothing, and
/// failing the parse loses everything. (The thumbnail cache's "every failure is a
/// miss" is a *per-entry* rule and its entries are separate files; here one file
/// holds every gallery, so the same reasoning has to be applied at the key rather
/// than at the file.)
#[test]
fn a_page_key_that_is_not_a_number_costs_that_page_and_no_other() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = std::path::PathBuf::from(r"C:\books\vol1.cbz");
    std::fs::write(
        &store,
        concat!(
            "{\n",
            "  \"C:\\\\books\\\\vol1.cbz\": { \"lead_lone\": 3, \"pages\": {\n",
            "    \"001.png\": { \"0\": \"double\", \"x\": \"single\", \"\": \"double\" },\n",
            "    \"002.png\": { \"0\": \"single\" }\n  } }\n",
            "}",
        ),
    )
    .unwrap();

    let g = load_spread_overrides(&store, &root);
    assert_eq!(g.lead_lone, 3, "the rest of the entry is untouched");
    assert_eq!(g.shape_for(&root.join("001.png"), 0), Some(Double));
    assert_eq!(g.shape_for(&root.join("002.png"), 0), Some(Single));
    assert_eq!(g.shape_for(&root.join("001.png"), 1), None, "no page 1 was ever named");
    assert_eq!(
        g.pages.len(),
        2,
        "the two keys that are not numbers are dropped, and nothing else is: {:?}",
        g.pages
    );
}

/// A store bigger than [`SPREADS_MAX_BYTES`] is not read, and an entry with more
/// pages than [`SPREADS_MAX_PAGES`] is dropped whole.
///
/// **Premises are checked, not assumed.** Both fixtures are written *through the
/// library*, so a typo in a hand-built JSON string could not be mistaken for a cap
/// biting; and each half asserts the *other* cap is not the one firing, so "empty
/// because it was capped" cannot pass for "empty because it did not parse".
///
/// Whole-entry-or-nothing for the entry cap is deliberate and load-bearing: a
/// `HashMap`'s iteration order is randomised per process, so keeping "the first
/// `SPREADS_MAX_PAGES` of them" would hand back a different set of overrides on
/// every launch, and a user could never reproduce what they saw.
#[test]
fn the_two_caps_degrade_to_nothing_rather_than_to_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::path::PathBuf::from(r"C:\books\vol1.cbz");

    // Entry cap: valid, and *under* the byte cap, so only the entry cap can fire.
    let mut many = GallerySpreads::default();
    let pages = SPREADS_MAX_PAGES + 1;
    for page in 0..pages as u32 {
        many.set_shape(&root.join("001.png"), page, Shape::Double);
    }
    let store = dir.path().join("entries.json");
    save_spread_overrides(&store, &root, &many).unwrap();
    let len = std::fs::metadata(&store).unwrap().len();
    assert!(
        len <= SPREADS_MAX_BYTES,
        "this fixture must stay under the byte cap ({len} bytes) or the other cap \
         would be what is being tested"
    );
    let g = load_spread_overrides(&store, &root);
    assert!(
        g.pages.is_empty(),
        "every page override is dropped over the page cap, not a subset of them: a \
         subset would be a different subset on every launch"
    );
    assert_eq!(
        g.lead_lone, LEAD_LONE_DEFAULT,
        "the offset is not what the cap is about, so it survives the emptied page map"
    );

    // Byte cap: over it, so the read is skipped whatever the contents are Ã¢â‚¬â€ and
    // **under the page cap, so the other cap cannot be what empties it.** That
    // attribution is the whole difficulty: the first version of this fixture was
    // 40 000 short-named entries, which is over both caps, and it passed with the
    // byte cap removed because the page cap caught it instead. A mutation-verified
    // guard that another guard can satisfy is not one.
    let mut huge = GallerySpreads::default();
    let wide: String = "p".repeat(200);
    let entries = 5_000usize;
    for i in 0..entries {
        huge.set_shape(&root.join(format!("{wide}{i}.png")), 0, Shape::Double);
    }
    let store = dir.path().join("huge.json");
    save_spread_overrides(&store, &root, &huge).unwrap();
    let len = std::fs::metadata(&store).unwrap().len();
    assert!(
        len > SPREADS_MAX_BYTES,
        "this fixture must be over the byte cap ({len} bytes) or nothing is being tested"
    );
    assert!(
        entries <= SPREADS_MAX_PAGES,
        "this fixture must be under the page cap ({entries} entries) or the page cap \
         would be what is being tested"
    );
    assert_empty(&load_spread_overrides(&store, &root));

    // The control: the same shape of store, comfortably inside both caps, loads.
    let mut few = GallerySpreads::default();
    few.set_shape(&root.join("001.png"), 0, Shape::Double);
    let store = dir.path().join("few.json");
    save_spread_overrides(&store, &root, &few).unwrap();
    assert_eq!(
        load_spread_overrides(&store, &root).shape_for(&root.join("001.png"), 0),
        Some(Double),
        "a store inside both caps is not affected by either"
    );
}

/// A path with no file name stores nothing, rather than colliding on the empty one.
///
/// `file_name_of` answers `""` for one, and the store is keyed by that name, so
/// every such path would share the entry `("", page)` and the last write would
/// silently replace the previous one's decision. `spread_root_key` refuses the same
/// empty string for a *root*; this closes the same asymmetry one level down.
///
/// Nothing reachable today produces such a path Ã¢â‚¬â€ a folder scan and `extract_zip`
/// both name every file they yield Ã¢â‚¬â€ which is why the guard is a refusal rather
/// than an error: the entry could never have been addressed again anyway.
#[test]
fn a_path_with_no_file_name_stores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut g = GallerySpreads::default();

    // Two different nameless paths: with the empty key they would share one entry.
    g.set_shape(std::path::Path::new(r"C:\"), 0, Double);
    g.set_shape(std::path::Path::new(".."), 0, Single);
    assert!(
        g.pages.is_empty(),
        "nothing can address an entry keyed by an empty name, so none is written: {:?}",
        g.pages
    );
    assert_eq!(g.shape_for(std::path::Path::new(".."), 0), None);

    // The control: a name that merely *contains* a separator is fine, which is the
    // whole point of keying on the file name rather than the path.
    g.set_shape(&dir.path().join("sub").join("001.png"), 3, Double);
    assert_eq!(g.shape_for(&dir.path().join("sub").join("001.png"), 3), Some(Double));
    assert_eq!(g.pages.len(), 1);
}

/// An archive's overrides are keyed by the **archive**, and never by the `%TEMP%`
/// directory its members were extracted into.
///
/// The trap this exists for: `MyApp::folder` holds the extraction directory while
/// an archive is open, and that directory's name is a hash of the archive's path,
/// mtime and length Ã¢â‚¬â€ so it is stable only until the archive is edited. Keying
/// overrides by it would look right for as long as nobody touched the `.cbz`, then
/// silently lose every override the first time they did.
///
/// So this asserts three separate things: the override is *found* after the
/// extraction directory changes, the store contains *no* spelling of that
/// directory, and asking for the extraction directory as the root finds nothing.
#[test]
fn an_archive_is_keyed_by_its_original_path() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let archive = std::path::PathBuf::from(r"C:\books\vol3.cbz");
    // Two extractions of one archive. Neither exists on disk: what is being pinned
    // is which key is written and read, so the names are all that matter.
    let first = std::path::PathBuf::from(r"C:\Temp\image-viewer\000000000000a01");
    let second = std::path::PathBuf::from(r"C:\Temp\image-viewer\000000000000b02");

    let mut g = GallerySpreads::default();
    g.set_shape(&first.join("012.png"), 0, Double);
    save_spread_overrides(&store, &archive, &g).unwrap();

    assert_eq!(
        load_spread_overrides(&store, &archive).shape_for(&second.join("012.png"), 0),
        Some(Double),
        "the override has to survive a re-extract, which renames the extraction directory"
    );

    let text = std::fs::read_to_string(&store).unwrap();
    assert!(text.contains("vol3.cbz"), "the archive's own path is the key:\n{text}");
    for leak in ["Temp", "image-viewer", "000000000000a01"] {
        assert!(
            !text.contains(leak),
            "an extraction path reached the store as {leak:?}, so the overrides\n\
             die with the temp directory:\n{text}"
        );
    }

    assert!(
        load_spread_overrides(&store, &first).pages.is_empty(),
        "asking for the extraction directory as the root finds nothing - which is \
         exactly what keying by `folder` would have done"
    );
}

/// One file, many galleries, and each gallery sees only its own decisions.
///
/// **The same file name in both roots**, which is what makes the scoping
/// load-bearing rather than incidental: two folders of scans each containing
/// `001.png` must not answer for one another, and the page key deliberately holds
/// a bare name (a member of an archive has no meaningful directory to record).
#[test]
fn overrides_are_scoped_to_one_gallery() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let archive = std::path::PathBuf::from(r"C:\books\vol1.cbz");
    let folder = dir.path().join("scans");
    let member = "001.png";

    let mut a = GallerySpreads::default();
    a.set_shape(&archive.join("pages").join(member), 0, Double);
    save_spread_overrides(&store, &archive, &a).unwrap();
    let mut b = GallerySpreads::default();
    b.set_shape(&folder.join(member), 0, Single);
    save_spread_overrides(&store, &folder, &b).unwrap();

    assert_eq!(
        load_spread_overrides(&store, &archive).shape_for(&archive.join("pages").join(member), 0),
        Some(Double)
    );
    assert_eq!(
        load_spread_overrides(&store, &folder).shape_for(&folder.join(member), 0),
        Some(Single),
        "the same file name in another gallery is a different page"
    );
    assert!(
        load_spread_overrides(&store, &dir.path().join("elsewhere")).pages.is_empty(),
        "a root nobody saved anything for has no overrides"
    );
}

/// One store holds every gallery, so a save is a **read-modify-write**, never a
/// whole-file overwrite.
///
/// This is the failure the scoping test above cannot see: both saves succeed, both
/// loads return the right answer, and the first gallery's overrides are gone
/// anyway because the second save replaced the file. `spreads.json` is the only
/// place the app keeps these, so that is the user losing every override in every
/// other folder Ã¢â‚¬â€ with no error and nothing on screen to say so.
#[test]
fn saving_one_gallery_keeps_the_others_overrides() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let one = dir.path().join("one");
    let two = dir.path().join("two");

    let mut a = GallerySpreads::default();
    a.nudge_lead_lone(1);
    a.set_shape(&one.join("001.png"), 0, Double);
    save_spread_overrides(&store, &one, &a).unwrap();

    let mut b = GallerySpreads::default();
    b.set_shape(&two.join("001.png"), 0, Single);
    save_spread_overrides(&store, &two, &b).unwrap();

    assert_eq!(load_spread_overrides(&store, &one), a, "gallery one survived");
    assert_eq!(load_spread_overrides(&store, &two), b);

    // ...and re-saving one gallery replaces **its own** entry rather than merging
    // with itself: a page the user has since un-overridden has to actually go.
    let mut a2 = GallerySpreads::default();
    a2.set_shape(&one.join("002.png"), 0, Double);
    save_spread_overrides(&store, &one, &a2).unwrap();
    let reloaded = load_spread_overrides(&store, &one);
    assert_eq!(reloaded.pages.len(), 1, "the entry was replaced, not added to");
    assert_eq!(reloaded.shape_for(&one.join("002.png"), 0), Some(Double));
}

/// A re-exported file's overrides are dropped, so the next read judges the pages
/// again.
///
/// **Every page, not one**, and for a sharper reason than the dimension cache's:
/// an override naming page 3 of a document that is now one page long is not a
/// stale *measurement*, it is a decision about pixels that no longer exist Ã¢â‚¬â€ and
/// because an override beats the detector, keeping it holds page 3 a spread for
/// the session no matter what the file now contains. `invalidate_path` is what
/// calls this, and it is the only thing that knows the file's contents changed.
#[test]
fn forget_file_drops_every_page_of_one_file() {
    let dir = tempfile::tempdir().unwrap();
    let a = dir.path().join("a.png");
    let b = dir.path().join("b.png");

    let mut g = GallerySpreads::default();
    g.set_shape(&a, 0, Double);
    g.set_shape(&a, 7, Double);
    g.set_shape(&b, 0, Single);

    g.forget_file(&a);
    assert_eq!(g.shape_for(&a, 0), None, "forgotten means absent, not stale");
    assert_eq!(g.shape_for(&a, 7), None, "every page of the file, not just the first");
    assert_eq!(g.shape_for(&b, 0), Some(Single), "another file's decision is not collateral");
}

/// Which gallery an open archive is keyed by, as a pure function.
///
/// The rule is here rather than in `MyApp::open_archive` because the thing it
/// exists to prevent is *silent*: with no archive open, `folder` really is the
/// gallery's root, and with one open `folder` is the extraction directory. A
/// single expression that prefers the archive path is the only version of this
/// that can be checked, and the empty-folder case is why it is `Option` at all Ã¢â‚¬â€
/// two galleries with no resolvable parent would otherwise share one entry, and
/// each one's overrides would answer for the other.
#[test]
fn an_open_archive_is_keyed_by_its_own_path_not_its_extraction_directory() {
    let archive = std::path::PathBuf::from(r"C:\books\vol3.cbz");
    let extracted = std::path::PathBuf::from(r"C:\Temp\image-viewer\000000000000a01");
    let folder = std::path::PathBuf::from(r"C:\books\vol4");

    assert_eq!(
        spread_root_key(Some(&archive), &extracted),
        Some(r"C:\books\vol3.cbz".to_string()),
        "with an archive open, `folder` is the extraction directory and must be ignored"
    );
    assert_eq!(
        spread_root_key(None, &extracted),
        Some(extracted.to_string_lossy().into_owned()),
        "with no archive open, `folder` is the root and is used"
    );
    assert_eq!(spread_root_key(None, &folder), Some(folder.to_string_lossy().into_owned()));
    assert_eq!(
        spread_root_key(Some(&archive), std::path::Path::new("")),
        Some(r"C:\books\vol3.cbz".to_string()),
        "an empty folder must not matter while an archive is open"
    );
    assert_eq!(
        spread_root_key(None, std::path::Path::new("")),
        None,
        "the empty path is the one value that names no gallery, so it keys nothing \
         rather than one shared empty entry"
    );
    // **No parent is not the test.** `.` has no parent and is not empty, so it is a
    // name like any other - and the doc comment says so, because "a root with no
    // name" reads like it covers this case and it does not.
    assert_eq!(
        spread_root_key(None, std::path::Path::new(".")),
        Some(".".to_string()),
        "a relative root is a name like any other"
    );
}

/// The overrides live beside the thumbnail cache, **not** beside the config.
///
/// `AGENTS.md` ("A cache of any sort is `%LOCALAPPDATA%`, not `%APPDATA%`") and the
/// thumbnail cache's own doc comment: these are *this machine's* reading of files
/// that may be on a network share, they are worthless on another machine, and a
/// roaming profile is the wrong place for them. `config.json` is the opposite
/// case and it does live in `%APPDATA%`, which is why the two being together
/// would be a mistake rather than a tidiness question.
///
/// The file *name* is asserted because it is a published name, not an internal one:
/// the store is meant to be found, opened, edited and deleted by whoever is reading
/// it, and a path described three different ways in three places is a path nobody
/// can follow.
#[test]
fn the_overrides_live_beside_the_thumbnail_cache_not_beside_the_config() {
    let store = spreads_root().expect("LOCALAPPDATA is set on Windows");
    assert_eq!(
        store.file_name().and_then(|n| n.to_str()),
        Some("spreads.json"),
        "the documented file name: {store:?}"
    );
    assert_eq!(
        store.parent(),
        thumb_cache_root().parent(),
        "the same image-viewer folder as the thumbnail cache: local, not roaming"
    );
    let roaming = std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .expect("APPDATA is set on Windows")
        .join("image-viewer");
    assert_ne!(
        store.parent(),
        Some(roaming.as_path()),
        "a roaming profile is the wrong home for this"
    );
}

/// A store written by an earlier build, or edited by hand, still loads.
///
/// Written as **text** rather than through `save_spread_overrides`, because every
/// one of these is something a hand or an older build does and the round trip
/// cannot produce any of them:
///
/// * `"double"` Ã¢â‚¬â€ the spelling `#[serde(rename_all = "lowercase")]` exists to
///   produce. Without it serde writes `"Double"` and refuses to read `"double"`,
///   so a hand-edited file using the documented spelling loads as *nothing*, and
///   `AGENTS.md` records that exact trap for `WgpuBackend`.
/// * a root with no `lead_lone` Ã¢â‚¬â€ an older build, or a hand that added one page.
///   It gets [`LEAD_LONE_DEFAULT`], not `0`: see `assert_empty`.
/// * a root with no `pages` Ã¢â‚¬â€ likewise, and it gets none.
/// * `"lead_lone": 99` Ã¢â‚¬â€ past the documented maximum. Clamped on load, or the
///   first 99 pages of the gallery stand alone, which is a pairing no reader
///   asked for.
/// * `"note"` Ã¢â‚¬â€ an unknown key. Ignored, never rejected: that tolerance is the
///   only reason a file from an older build still loads at all.
#[test]
fn an_older_or_hand_edited_store_still_loads() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    std::fs::write(
        &store,
        concat!(
            "{\n",
            "  \"C:\\\\books\\\\vol1.cbz\": { \"pages\": { \"001.png\": { \"0\": \"double\" } } },\n",
            "  \"C:\\\\books\\\\vol2.cbz\": { \"lead_lone\": 99, \"note\": \"hand-written\" }\n",
            "}",
        ),
    )
    .unwrap();

    let one = load_spread_overrides(&store, std::path::Path::new(r"C:\books\vol1.cbz"));
    assert_eq!(
        one.shape_for(std::path::Path::new(r"C:\books\vol1\001.png"), 0),
        Some(Double),
        "\"double\" is the spelling this file documents, so it must load"
    );
    assert_eq!(one.lead_lone, LEAD_LONE_DEFAULT, "a missing key is the default, not zero");

    let two = load_spread_overrides(&store, std::path::Path::new(r"C:\books\vol2.cbz"));
    assert!(two.pages.is_empty(), "a missing `pages` key is no pages");
    assert_eq!(two.lead_lone, LEAD_LONE_MAX, "a hand-edited 99 is clamped, not obeyed");
}
