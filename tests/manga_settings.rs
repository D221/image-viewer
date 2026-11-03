//! Manga-mode settings: forward-compat for `manga_mode` and `manga_rtl`, plus
//! the BOM strip that keeps an old `config.json` / `spreads.json` from silently
//! reverting every toggle.
//!
//! Moved out of `tests/spreads.rs` in one block (the "The two settings"
//! section). It touches nothing in `spreads.rs` but library items, so it lives
//! as its own target: `GallerySpreads`, `save_spread_overrides`,
//! `load_spread_overrides` and `LEAD_LONE_DEFAULT` are all imported from the
//! crate, and `CONFIG_WITH_EVERY_FLAG_FLIPPED` is defined here.

use image_viewer::{
    load_spread_overrides, save_spread_overrides, GallerySpreads, Shape, LEAD_LONE_DEFAULT,
};
use Shape::{Double, Single};

/// A config written before either manga key existed still loads, and gets the
/// values a reader of a manga library wants rather than the ones an absent field
/// would default to.
///
/// **The `#[serde(default)]` attributes are what this pins**, and both halves are
/// pinned because they are opposites: `manga_mode` must be **off**, because
/// single-image browsing is the app's original behaviour and every plan it makes
/// is pinned by `tests/flip_through.rs` — a config that turned a spread on by
/// omission would change that suite's meaning without changing a line of it. And
/// `manga_rtl` must be **on**, because a left-to-right reader who turns manga
/// mode on has asked for a manga reader and gets one; the order pages meet in is
/// the single most load-bearing thing about the feature and it is not a thing to
/// make someone find in the settings panel.
#[test]
fn a_config_written_before_manga_mode_still_loads() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.manga_mode, "manga mode is opt-in");
    assert!(cfg.manga_rtl, "right-to-left is the default for a manga library");
}

/// A leading **BOM** must not read as an empty file.
///
/// **The failure this pins is "every setting silently reverted".** `Set-Content
/// -Encoding utf8` in Windows PowerShell - and several editors' "save as UTF-8 with
/// signature" - writes `EF BB BF` before the first byte of JSON. `serde_json` does
/// not accept it: the parse fails, `Config::load` takes its error branch and returns
/// the **defaults**, and the app starts looking as though it had never been
/// configured. Nothing is printed that a user would read, the folder is forgotten,
/// every toggle is back where it was, and the file on disk is perfectly valid JSON -
/// so the obvious thing to try does not help.
///
/// Every field asserted is a bool, deliberately: `false` where the default is `true`
/// and `true` where the default is `false`, so a reader who deletes the whole assert
/// block still cannot mistake this for a defaults-are-correct test. Both manga keys
/// are in it, because those are the settings whose loss is hardest to explain.
///
/// The **premise is asserted too**, against `serde_json` directly. Without it this
/// test would pass on any future serde that learned to skip a BOM, and the strip in
/// `Config::load` would be reported as dead code by a test that never checked
/// whether anything needed stripping.
#[test]
fn a_bom_prefixed_config_still_loads_its_settings() {
    let body = format!("\u{feff}{}", CONFIG_WITH_EVERY_FLAG_FLIPPED);
    serde_json::from_str::<image_viewer::Config>(&body).expect_err(
        "serde_json now accepts a leading BOM. The strip in Config::load is still \
         correct and harmless, but this test no longer tests anything - say so rather \
         than leaving it green.",
    );

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, &body).unwrap();

    let cfg = image_viewer::Config::load(&path);
    assert!(!cfg.load_all_on_startup, "load_all_on_startup: the BOM ate the file");
    assert!(!cfg.linear_resample, "linear_resample");
    assert!(!cfg.loop_back, "loop_back");
    assert!(!cfg.thumbnail_cache, "thumbnail_cache");
    // These two default to *off*, so the fixture has them on: the assertion is
    // flipped from the ones above, which is the point - a defaults-are-correct
    // reader cannot skim past all nine.
    assert!(cfg.wheel_navigation, "wheel_navigation");
    assert!(cfg.convert_wide_gamut, "convert_wide_gamut");
    assert!(cfg.manga_mode, "manga_mode: the whole feature, back to its default");
    assert!(!cfg.manga_rtl, "manga_rtl: and the reading direction with it");

    // The same file without the BOM parses to the same thing, so the strip is the
    // only difference between them. This is what makes the assertions above about
    // the BOM rather than about the JSON.
    let plain = dir.path().join("plain.json");
    std::fs::write(&plain, CONFIG_WITH_EVERY_FLAG_FLIPPED).unwrap();
    let without = image_viewer::Config::load(&plain);
    assert_eq!(
        format!("{cfg:?}"),
        format!("{without:?}"),
        "a BOM-prefixed config and the same bytes without one must load identically"
    );
}

/// Every boolean field set to the *opposite* of its default.
///
/// `Config::default()` is spelled here rather than taken from the library, because
/// the point of this constant is that a reader can check it against `Config::load`'s
/// default arm with their eyes: `load_all_on_startup`, `linear_resample`,
/// `loop_back` and `thumbnail_cache` default to on and are off here;
/// `block_while_loading`, `wheel_navigation`, `convert_wide_gamut` and `manga_mode`
/// default to off and are on here; `manga_rtl` defaults to on and is off.
const CONFIG_WITH_EVERY_FLAG_FLIPPED: &str = r#"{
  "load_all_on_startup": false,
  "linear_resample": false,
  "block_while_loading": true,
  "wheel_navigation": true,
  "loop_back": false,
  "convert_wide_gamut": true,
  "thumbnail_cache": false,
  "manga_mode": true,
  "manga_rtl": false
}"#;

/// `spreads.json` needs the same strip as `config.json`, and it is **more**
/// dangerous with one.
///
/// Two reasons, and the second is why this is a separate test rather than a note on
/// the config one:
///
/// * A load that fails is treated as empty, so the reader loses their pairing
///   decisions for that gallery - visible, and fixable by deleting the file.
/// * **A save that fails is a read-modify-write against the file it could not read.**
///   `save_spread_overrides` parses the whole store and writes one gallery's entry
///   back into it; a parse failure means "empty", so the write replaces every other
///   gallery's entry with nothing. The decisions are not reverted for one session -
///   they are **deleted, permanently, with no error anywhere**.
///
/// That second one is why the store is *written by the library first* and then
/// prefixed with a BOM here: a fixture hand-written as `"\u{feff}{...}"` would pass
/// against a save that ignored its input entirely, because there would be nothing in
/// the file to preserve.
#[test]
fn a_bom_prefixed_spreads_store_does_not_cost_anybody_their_choices() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("spreads.json");
    let root = dir.path().join("books");
    let page = root.join("003.png");

    let mut g = GallerySpreads::default();
    g.nudge_lead_lone(2);
    g.set_shape(&page, 1, Double);
    g.set_shape(&page, 5, Single);
    save_spread_overrides(&store, &root, &g).expect("a fresh directory is created");

    // A second gallery, so the save below has something to destroy.
    let other = dir.path().join("other-book");
    let mut h = GallerySpreads::default();
    h.set_shape(&other.join("cover.png"), 0, Double);
    h.set_shape(&other.join("cover.png"), 3, Single);
    save_spread_overrides(&store, &other, &h).expect("a second entry");

    // Now make the store BOM-prefixed the way a shell round trip would.
    let plain = std::fs::read_to_string(&store).unwrap();
    std::fs::write(&store, format!("\u{feff}{plain}")).unwrap();

    // Half one: the load honours it.
    assert_eq!(
        load_spread_overrides(&store, &root),
        g,
        "a BOM-prefixed store is not being read, so this gallery's pairing decisions are \
         gone for the session"
    );
    assert_eq!(load_spread_overrides(&store, &other).pages, h.pages, "the second gallery's too");

    // Half two: a save against it keeps every other entry.
    let mut g2 = g.clone();
    g2.nudge_lead_lone(3);
    save_spread_overrides(&store, &root, &g2).expect("a save over a BOM-prefixed store");
    assert_eq!(
        load_spread_overrides(&store, &other),
        h,
        "the save read the store as empty and replaced it with one gallery's entry. Every \
         other gallery's decisions are deleted from disk, permanently and silently."
    );
    assert_eq!(
        load_spread_overrides(&store, &root).lead_lone,
        LEAD_LONE_DEFAULT + 3,
        "and the gallery that was saved kept its new value"
    );
}
