//! Module `spreads`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use spreads::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Spread overrides: what the user decided, persisted
// ---------------------------------------------------------------------------
//
// The detector above reads a page's aspect ratio, which is a good guess and blind
// to three things: a landscape insert that is not a scan of two pages, a page
// cropped enough that its ratio no longer matches its partner's, and what a given
// book wants at the front. So the user gets the last word — nudge how many
// leading pages stand alone, and flip any one page between a page and a spread —
// and this is where those decisions live.
//
// It is a small file, and it borrows the thumbnail cache's rules wholesale rather
// than inventing its own:
//
// * **Local, not roaming.** `%LOCALAPPDATA%\image-viewer\spreads.json`, beside
//   the thumbnail cache and *not* beside `config.json`. These are this machine's
//   reading of files that may be on a network share; they are worthless on
//   another machine, and a profile folder is the wrong place for them.
// * **A half-written store is unreadable.** Written to `spreads.json.tmp` and
//   renamed, and `fs::rename` replaces on Windows, so a reader — a second
//   instance of the app — sees either the old file or the new one.
// * **Every failure is an empty override set.** A missing file, a malformed one,
//   a path that cannot be read at all. Nothing a user or an editor does to this
//   file may be able to stop the app starting, and no failure here is worth a
//   warning: the loss is a pairing the user can re-press, and the alternative is
//   refusing to open a folder over a cache file.
//
// Two levels of key, and the second is a trap rather than a choice:
//
// * **The gallery's root, as the caller states it** ([`spread_root_key`]). One
//   store holds every gallery, so a save reads the whole file and replaces exactly
//   one entry: the folder, or the archive that an open gallery was extracted from.
// * **A bare file name and a page number**, inside that entry. A member of an
//   archive has no stable directory to record — the `%TEMP%` directory it was
//   extracted into changes on every re-extract — so the directory component is
//   dropped and the *root key* carries the gallery instead. Within one gallery
//   the names are unique: a folder is scanned flat, and `extract_zip` flattens
//   and de-duplicates the entries it writes.

/// What identifies a gallery's entry in the store: the root path, as a string.
///
/// Deliberately a bare path and not a "folder or archive" enum. The mistake the
/// `%TEMP%` rule exists to prevent is made *natural* by that choice rather than
/// impossible: there is no field here to put an archive's extraction directory into,
/// so reaching for it takes a deliberate argument instead of filling something in.
/// It is still reachable — a caller can write any string it likes and the load will
/// not notice — which is why [`spread_root_key`] is where the choice is actually
/// made and tested.
pub(crate) type SpreadStore = std::collections::BTreeMap<String, GallerySpreads>;

/// Where the overrides live: `%LOCALAPPDATA%\image-viewer\spreads.json`.
///
/// **Local, not roaming, and deliberately not beside the config.** These are this
/// machine's decisions about files that may well be on a network share, they are
/// worthless on another machine, and a profile folder is the wrong place for
/// something the user never asked to sync. `Config::default_path` is the opposite
/// case and does live in `%APPDATA%`; keeping the two apart is the whole point of
/// `AGENTS.md`'s "a cache of any sort is `%LOCALAPPDATA%`".
///
/// `HOME` is the same fallback [`thumb_cache_root`] uses, for the one environment
/// where it is not `LOCALAPPDATA`. **No third arm**, unlike that function: it
/// falls back to a directory beside the working copy, which is the right answer
/// for a disposable cache and the wrong one for a file a person is meant to open
/// and edit. `None` means "do not persist", and the caller keeps
/// the decisions in memory for the session — the only cost is that they are
/// forgotten at exit, which is a far smaller failure than an app that will not
/// start.
pub fn spreads_root() -> Option<PathBuf> {
    if let Some(dir) = portable_dir() {
        return Some(dir.join("spreads.json"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        Some(PathBuf::from(local).join("image-viewer").join("spreads.json"))
    } else if let Some(home) = std::env::var_os("HOME") {
        Some(PathBuf::from(home).join(".cache").join("image-viewer").join("spreads.json"))
    } else {
        None
    }
}

/// Which gallery `archive`/`folder` names, as the store's key.
///
/// **For an open archive this is the archive's own path and never the `%TEMP%`
/// directory its members were extracted into.** That directory's name is a hash of
/// the archive's path, mtime and length, so it is stable only until the archive is
/// touched — an override keyed by it would appear to work for as long as nobody
/// edited the `.cbz` and then silently lose every override the first time they
/// did. `MyApp` holds both, so the choice has to be made here rather than at each
/// call site.
///
/// `None` only for the **empty** path, which is the one value that cannot name a
/// gallery: the empty string is a real key, so every gallery with no name would
/// share one entry and each one's overrides would answer for the others. Note that
/// "no parent" is *not* the test — `Path::new(".")` has no parent and is not empty,
/// so it keys as `"."`.
///
/// A function rather than an expression at the one call site that needs it, because
/// `MyApp` holds both paths and the wrong reading of which one is the gallery's is
/// silent: overrides keyed by the extraction directory load fine right up until the
/// archive is edited.
pub fn spread_root_key(archive: Option<&Path>, folder: &Path) -> Option<String> {
    let root = archive.unwrap_or(folder);
    (!root.as_os_str().is_empty()).then(|| root.to_string_lossy().into_owned())
}

/// The user's decisions about one gallery: which pages take a whole turn, and how
/// many leading pages stand alone.
///
/// Two independent things because they are two independent decisions. `lead_lone`
/// is a property of the *book* and applies to the whole gallery; `pages` is a
/// property of a *page* and names individual exceptions. They travel together
/// only because they are written to the same place, and [`Self::shape_for`] is the
/// one function a pairing asks.
///
/// `Default` is spelled out rather than derived, and it is [`LEAD_LONE_DEFAULT`]
/// rather than `0`. A derived `Default` would make "this gallery has no overrides"
/// mean "pair the cover with page 2", and `load_spread_overrides` hands out exactly
/// that value whenever the store is missing or unreadable — so every gallery would
/// open with its cover paired to the page after it, with nothing on screen to say
/// why.
///
/// `#[serde(default)]` is on the **container**, so both fields fall back to
/// *this* `Default` and there is only one spelling of the default to keep
/// correct: a file written before a field existed loads with the same values a
/// fresh value has. There is deliberately no `deny_unknown_fields`, so a store
/// from an older build — or one with a note in it — still loads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GallerySpreads {
    /// How many leading pages are shown one to a turn, by flat ordinal.
    /// Bounds: `0..=` [`LEAD_LONE_MAX`], enforced by [`Self::nudge_lead_lone`]
    /// and again on load.
    pub lead_lone: u32,
    /// Per-page overrides, keyed by **bare file name and page number**.
    ///
    /// Not keyed by path, for the archive reason above; not keyed by page alone,
    /// because a multi-page document's pages are separate decisions.
    #[serde(with = "spread_pages")]
    pub pages: HashMap<(String, u32), Shape>,
}

impl Default for GallerySpreads {
    fn default() -> Self {
        GallerySpreads { lead_lone: LEAD_LONE_DEFAULT, pages: HashMap::new() }
    }
}

/// The on-disk form of [`GallerySpreads::pages`]: file name -> page -> shape.
///
/// A map keyed by a `(String, u32)` **pair** is not something `serde_json` can
/// write: it refuses any map key that is not a string, and no derive avoids the
/// key. So the pair is split into two nested objects and the page becomes a decimal
/// string, which is also what keeps the file editable by hand: nested objects with
/// string keys, rather than one flat map keyed by an invented separator nobody could
/// type.
///
/// ```text
/// "pages": { "001.png": { "0": "double", "7": "single" } }
/// ```
///
/// **An unparseable page key is dropped, not fatal.** One mistyped key must not
/// cost a reader every override in the file, which is what the
/// `unwrap_or_default` in [`load_spread_overrides`] would otherwise do — the same
/// reasoning as a corrupt cache entry being a miss rather than a wrong answer.
mod spread_pages {
    use super::Shape;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::collections::{BTreeMap, HashMap};

    /// `file name -> page as a string -> shape`. `BTreeMap` so the written file's
    /// key order is stable, which is what makes it reviewable in a diff.
    type ByFile = BTreeMap<String, BTreeMap<String, Shape>>;

    pub fn serialize<S: Serializer>(
        pages: &HashMap<(String, u32), Shape>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut by_file: ByFile = BTreeMap::new();
        for ((name, page), shape) in pages {
            by_file.entry(name.clone()).or_default().insert(page.to_string(), *shape);
        }
        by_file.serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<HashMap<(String, u32), Shape>, D::Error> {
        let by_file = ByFile::deserialize(deserializer)?;
        let mut out = HashMap::with_capacity(by_file.len());
        for (name, per_page) in by_file {
            for (page, shape) in per_page {
                // Dropped, as the module docs say. The alternative — failing the
                // whole parse — hands back an empty store for one bad key.
                if let Ok(page) = page.parse::<u32>() {
                    out.insert((name.clone(), page), shape);
                }
            }
        }
        Ok(out)
    }
}

impl GallerySpreads {
    /// The user's decision about one page, or `None` when there is none.
    ///
    /// `None` is the ordinary answer and means "ask the detector": a page with no
    /// override pairs by its aspect ratio like any other. So this is not a
    /// fallback that guesses — an override *replaces* the detector rather than
    /// breaking its ties.
    ///
    /// Also `None` for a path with no file name, which cannot be in the store at
    /// all: [`Self::set_shape`] refuses to write one.
    pub fn shape_for(&self, path: &Path, page: u32) -> Option<Shape> {
        self.pages.get(&(file_name_of(path), page)).copied()
    }

    /// Record the user's decision about one page.
    ///
    /// **A path with no file name is refused rather than stored.** `file_name_of`
    /// answers `""` for one, and the store is keyed by that name — so every such
    /// path would share the entry `("", page)` and one would silently overwrite the
    /// others' decisions. Nothing reachable today produces such a path (a folder scan
    /// and `extract_zip` both name every file they yield), but the same asymmetry
    /// [`spread_root_key`] refuses for a *root* is worth refusing here too, and the
    /// cost is one entry that could never have been addressed anyway.
    pub fn set_shape(&mut self, path: &Path, page: u32, shape: Shape) {
        let name = file_name_of(path);
        if name.is_empty() {
            return;
        }
        self.pages.insert((name, page), shape);
    }

    /// Flip one page away from `detected`, store the answer, and return it.
    ///
    /// **`detected` is the whole answer, and it is the caller's**: what the page is
    /// judged to be *now* — an existing override if there is one, the aspect ratio
    /// otherwise. Two presses therefore flip and unflip, because the second press is
    /// handed the first press's answer.
    ///
    /// It does not give the store a second vote, which is what makes the returned
    /// shape worth anything: a `toggle` that read its own last answer back would
    /// flip away from the *stored* shape, and those two only differ when the stored
    /// override and the caller's judgement have parted company — an override set
    /// against a page whose file has since been re-exported. The same call would
    /// then answer two different questions, and only one of them was asked.
    /// `tests/spreads_overrides.rs::toggling_a_page_reports_the_shape_it_landed_on` pins it.
    pub fn toggle(&mut self, path: &Path, page: u32, detected: Shape) -> Shape {
        let next = match detected {
            Shape::Single => Shape::Double,
            Shape::Double => Shape::Single,
        };
        self.set_shape(path, page, next);
        next
    }

    /// Move the leading-lone offset by `by`, clamped to `0..=`[`LEAD_LONE_MAX`].
    ///
    /// Clamped in `i64` rather than with `saturating_*` on `u32`, because the
    /// arithmetic has to happen before it is bounded: `0u32` minus `i32::MIN` wraps
    /// to about two billion, which is *in* range and absurd, and the result would be
    /// a gallery of nothing but single pages.
    pub fn nudge_lead_lone(&mut self, by: i32) {
        let next = self.lead_lone as i64 + by as i64;
        self.lead_lone = next.clamp(0, LEAD_LONE_MAX as i64) as u32;
    }

    /// Drop every override for one file, for a file whose contents changed.
    ///
    /// **Every page, not one**, because the keys name pages: a path-keyed `remove`
    /// would be a no-op for any page past the first.
    ///
    /// This is not the same as forgetting a *measurement*, and the difference is
    /// why it is right to drop an override at all. A stale dimension is a wrong
    /// answer to a question about the current file; a stale override is a decision
    /// about pixels that no longer exist — a 12-page document re-exported as one
    /// page has no page 3 to be a spread. And because an override beats the
    /// detector outright, keeping it would hold a page a spread for the session no
    /// matter what the file now contains.
    ///
    /// Entries for a *name* that has left the gallery are not pruned by anything
    /// here: this store cannot tell a renamed file from a deleted one, and they are
    /// a few bytes each in a file the user can delete wholesale.
    pub fn forget_file(&mut self, path: &Path) {
        let name = file_name_of(path);
        self.pages.retain(|(n, _), _| *n != name);
    }
}

/// The overrides for one gallery out of the store. **Never fails.**
///
/// Every error is the same value: a missing file, a read error, and a parse that
/// did not produce the expected shape are all `GallerySpreads::default()`. That is
/// the whole contract, and it is why this returns the value rather than a `Result`
/// — a caller that could handle the error by refusing to open a folder would be a
/// worse app. Two refusals join them: a store over [`SPREADS_MAX_BYTES`] is not read
/// at all, and an entry over [`SPREADS_MAX_PAGES`] is emptied of its pages.
///
/// **An over-cap store is honoured as empty, and [`save_spread_overrides`] treats
/// the same file as untouchable.** The asymmetry is deliberate and is the whole
/// point of stating it in both places: returning empty here is safe because this
/// function only *reads*. The decisions are not honoured this session, the pairings
/// fall back to the detector, and the file is still on disk for next time. A save
/// cannot afford the same shrug, because its read is the first half of a
/// read-modify-write — so it refuses over the cap and writes nothing. Do not
/// "simplify" this by having one helper serve both.
///
/// `root` names the gallery the way [`spread_root_key`] produced it; `store` is
/// the file, which the caller passes in so a test can point it at a temporary
/// directory instead of the user's `%LOCALAPPDATA%`.
pub fn load_spread_overrides(store: &Path, root: &Path) -> GallerySpreads {
    // Checked before the read, not by reading and measuring after: this runs while
    // a gallery is opening, on the UI thread, and the point of the cap is to bound
    // that work rather than to notice it afterwards. A directory's metadata reads
    // fine and is tiny, so it falls through to the read below and fails there, as it
    // should.
    match std::fs::metadata(store) {
        Ok(m) if m.len() > SPREADS_MAX_BYTES => return GallerySpreads::default(),
        Ok(_) => {}
        Err(_) => return GallerySpreads::default(),
    }
    let Ok(text) = std::fs::read_to_string(store) else {
        return GallerySpreads::default();
    };
    let all: SpreadStore = serde_json::from_str(strip_bom(&text)).unwrap_or_default();
    let mut g = all.get(root.to_string_lossy().as_ref()).cloned().unwrap_or_default();
    // Clamped, not obeyed. This is a file a person can open and edit, so a
    // `lead_lone` of 99 is a thing that will happen, and obeying it stands the
    // first 99 pages of the gallery alone.
    g.lead_lone = g.lead_lone.min(LEAD_LONE_MAX);
    // All of them or none, as `SPREADS_MAX_PAGES` says: keeping a subset would
    // mean keeping a different subset on every launch. `lead_lone` is untouched —
    // it is one number, it is not what the cap is about, and a gallery whose pages
    // were all dropped still has a front matter the reader chose.
    if g.pages.len() > SPREADS_MAX_PAGES {
        g.pages.clear();
    }
    g
}

/// How big the store may be before [`load_spread_overrides`] stops reading it — and
/// before [`save_spread_overrides`] stops writing to it.
///
/// **A read on the UI thread, which is why it is bounded at all.** It happens while
/// a gallery is being opened (`MyApp::refresh_spreads`), on a file whose size
/// nothing else bounds.
///
/// The value is deliberately generous rather than tight, and deliberately *not*
/// justified by a bytes-per-entry figure: the store holds one small object per
/// gallery the user has ever overridden something in, and nothing in this app grows
/// it except `set_shape`. What the cap buys is that a hand-edited or hostile file
/// cannot make the app read an arbitrary amount of someone's disk.
///
/// Over the cap the load yields **nothing**, not a truncated read: a half-file does
/// not parse, so "what was readable" of an oversized store is nothing either way.
///
/// **A threshold, not a write invariant — and the two directions differ because they
/// must.** A load may treat an over-cap store as empty (it only reads; the file is
/// still there); a save may not (its read is the first half of a read-modify-write,
/// so writing what it could not read would destroy what it could not read). What
/// nothing does is keep *writes* under the cap: a save whose serialised result would
/// exceed it is not refused, and can leave a store that every later save then
/// refuses. See [`save_spread_overrides`] for what that costs and why it is accepted.
pub const SPREADS_MAX_BYTES: u64 = 1 << 20;

/// Most page overrides one gallery's entry may hold, whatever the file's size.
///
/// The size cap already bounds what a *file* can produce, so this is the second
/// line of defence for the in-memory store — and it exists because the two degrade
/// differently. It is deliberately **all of them or none**: past the cap the whole
/// page map is dropped rather than truncated to some of its entries, because a
/// `HashMap`'s iteration order is randomised, so a truncated load would hand back a
/// *different* set of overrides on every launch and nothing would ever look wrong.
/// `lead_lone` survives; it is one number and it is not what the cap is about.
pub const SPREADS_MAX_PAGES: usize = 20_000;

/// The store's own name plus `.tmp.<pid>` — the half-written-entry guard, made
/// unique per process.
///
/// **Per process, not per call.** A fixed name is what the thumbnail cache uses and
/// it is safe *there* because two writers of one cache entry are writing the same
/// bytes: same path, same stamp, same target, so the same payload. That reasoning
/// does not transfer, because this file is neither content-addressed nor
/// whole-overwritten — it is a **read-modify-write**, so two instances saving
/// different galleries milliseconds apart both open the same name with `Create` /
/// `TRUNCATE`, interleave their writes, and each renames the torn result over
/// `spreads.json`. The reader guarantee (`fs::rename` replaces, atomically) holds
/// throughout; what it hands over is a mixture. Then, because an unparseable store
/// is treated as empty and replaced, the next override press rewrites the file with
/// one gallery's entry and **every other gallery's decisions are gone**, silently
/// and with no second copy anywhere. Per-process naming means a second instance
/// writes its own temporary and renames a complete file over the first one's.
///
/// Per *process* rather than per call is enough because the app saves from one
/// thread — the UI — and the collision that matters is between two instances of the
/// app, not between two of its own threads.
///
/// **Appended, not `with_extension`.** `spreads.json`.with_extension("tmp") is
/// `spreads.tmp`: a different name for the same file, and one that reads as a
/// different file in a directory listing.
///
/// Takes the id rather than reading [`std::process::id`] itself so the naming is a
/// pure function of its arguments and a test can compare two ids' names. Public for
/// that reason, as [`thumb_cache_entry`] is.
pub fn spreads_tmp(store: &Path, pid: u32) -> PathBuf {
    let mut name = store.as_os_str().to_os_string();
    name.push(format!(".tmp.{pid}"));
    PathBuf::from(name)
}

/// Write one gallery's overrides into the store.
///
/// **A read-modify-write, not a whole-file overwrite.** One file holds every
/// gallery, so writing one gallery's entry over the top of it would delete every
/// other gallery's decisions — with no error and nothing on screen to say so. A
/// store that cannot be *parsed* is treated as empty and replaced, which is the
/// same rule [`load_spread_overrides`] reads it by.
///
/// **An over-cap store is LEFT ALONE, and that is the opposite of what a load
/// does with one.** [`load_spread_overrides`] over [`SPREADS_MAX_BYTES`] returns
/// empty: the file is simply not honoured this session, which is visible (the
/// pairings are the detected ones) and recoverable (the file is still there, and
/// shrinking or deleting it brings the decisions back). A save cannot do that,
/// because saving *is* this read: read-modify-write against a store it could not
/// read would replace that gallery's real entry — and every other gallery's, since
/// a parse failure takes the whole file — with what it read, which is nothing. That
/// is the same silent, unrecoverable loss [`spreads_tmp`] exists to prevent, one
/// level down, and it is why this returns an error over the cap instead of writing.
///
/// So: **no bytes are written, the file is left exactly as it was, and the caller
/// is told the decision was not persisted.** The app's caller ignores that error —
/// the decisions are still in memory for the session — but the contract is
/// `Err`, because a save that did not happen should not report success.
///
/// A store within the cap but over [`SPREADS_MAX_PAGES`] for one gallery is a
/// different case and is *not* refused: it parses, so replacing that one entry
/// loses only that entry, which is what the user just asked to change anyway.
///
/// **What is accepted, deliberately, because it is the mirror of the rule above.**
/// Nothing refuses a write whose serialised result would exceed
/// [`SPREADS_MAX_BYTES`], so a save *can* leave the store over the cap — and after
/// that every later save refuses, until the file is shrunk or deleted. Nothing is
/// ever corrupted: the decisions stay in the file, and they come back the moment it
/// is under the cap. So the visible consequence is that the override key appears to
/// do nothing, which is bounded and points the safe way — the alternative rule
/// (refuse the oversized write) would mean refusing to save at all once a store had
/// drifted over, which loses the very decisions the cap exists to protect. The
/// threshold is deliberately not an invariant on writes; see [`SPREADS_MAX_BYTES`].
pub fn save_spread_overrides(store: &Path, root: &Path, g: &GallerySpreads) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(store) {
        if meta.len() > SPREADS_MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "the overrides file is {} bytes, over the {SPREADS_MAX_BYTES}-byte \
                     cap, so it was not read and not written",
                    meta.len()
                ),
            ));
        }
    }
    let mut all: SpreadStore = std::fs::read_to_string(store)
        .ok()
        .as_deref()
        // Stripped rather than parsed raw, and this is the load-bearing of the two
        // sites: an unparseable store is *replaced*, so a BOM that made this parse
        // fail would turn the next override press into a delete of every other
        // gallery's entry. See `strip_bom`.
        .and_then(|text| serde_json::from_str(strip_bom(text)).ok())
        .unwrap_or_default();
    all.insert(root.to_string_lossy().into_owned(), g.clone());

    if let Some(parent) = store.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&all)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    // Write beside the store and rename into place, the thumbnail cache's rule:
    // `fs::rename` replaces on Windows, so a reader sees either the old file or
    // this one, never a half-written one. The temporary carries this process's id
    // (`spreads_tmp`) so a second instance of the app cannot tear this write — the
    // one thing that is *not* inherited from the thumbnail cache, and the reason
    // this is not a fixed name.
    //
    // A partial temporary is never renamed, so nothing can read one, and it is not
    // deleted on failure either: the next save from this process overwrites the same
    // name.
    let tmp = spreads_tmp(store, std::process::id());
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, store)
}
