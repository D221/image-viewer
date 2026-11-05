//! The settings the prefilter rides on: bound, defaulted off, and wired up.
//!
//! Split out of `tests/resample.rs`; the probes and the globals guard are in
//! `tests/common/resample_fixtures.rs`.
//!
//! Two kinds of test here. The config ones read an **old** config file and
//! assert the field defaults rather than failing to load - `Config` must stay
//! forward-compatible, so a new setting can never make yesterday's file
//! unreadable. The wiring ones scan `src/` as text, because the arm they
//! protect (`Msg::Preview`, the settings toggle, `refilter`) lives in the
//! binary, which no integration test can reach.
//!
//! The content `Cmd::PreviewFull` arm is the one that matters: a toggle that
//! changes the sampler without invalidating the preview cache would show old
//! pixels under a new label, which reads as the toggle being broken.

#[path = "common/resample_fixtures.rs"]
mod resample_fixtures;

use resample_fixtures::*;

/// The settings toggle must be **wired**, and `main.rs` is unreachable from every
/// test, so this is a source scan - the established pattern here (`spreads.rs`,
/// `keybindings.rs`, `renderer.rs`).
///
/// Three things it checks, each a way the setting could exist and do nothing:
/// the label exists, the config load applies it, and the panel's changed arm
/// **invalidates the previews**. The third is the one that matters: a
/// decode-time setting that leaves `previews` alone flips the box, changes
/// nothing visible, and reads as a broken feature. `linear_resample` is the
/// documented counter-example in `AGENTS.md`.
#[test]
fn the_smooth_toggle_is_wired_and_invalidates_the_previews() {
    let src = main_src();

    assert!(
        src.contains("set_downscale_smoothing(config.smooth_downscale);"),
        "startup must apply config.smooth_downscale to the resampler"
    );
    assert!(
        src.contains("&mut self.config.smooth_downscale"),
        "the settings panel must have a checkbox bound to it"
    );

    // The checkbox's own arm: find the `.changed()` block that mentions the
    // field and require it to set `refilter`. Locating it by field name rather
    // than by a fixed offset is what makes this survive the panel being
    // reordered, which it will be.
    let at = src.find("&mut self.config.smooth_downscale").expect("the checkbox exists");
    let after = &src[at..];
    let arm_end = after.find("}\n                ").expect("the changed() arm closes");
    let arm = &after[..arm_end];
    assert!(
        arm.contains("set_downscale_smoothing("),
        "the changed arm must apply the setting immediately, not only on the \
         next launch"
    );
    assert!(
        arm.contains("refilter = true"),
        "the changed arm must set refilter, or the previews decoded by the old \
         kernel stay on screen and the toggle appears broken (AGENTS.md records \
         exactly this for linear_resample)"
    );
    assert!(src.contains("if refilter {"), "the refilter flag must still be acted on");
}

/// A config written before this setting existed still loads, and gets **`false`**.
///
/// Both halves, because they are opposites: it must load *and* it must come up
/// `false`. A `#[serde(default)]` with the wrong default compiles, passes every
/// other test in the suite, and hands every existing reader a setting that is
/// wrong for three of the six pages it was measured on.
///
/// **This assertion was `true` and is now `false`, and the change is the fix.**
/// The setting shipped default-on on the strength of a *uniform synthetic
/// lattice*, where more strength wins at every pitch. Judging rendered output on
/// six 2223x3109 pages, strength 2.0 was better on `32.jpg` and `30.jpg`,
/// unchanged on `52.jpg`, and worse on `12.jpg`, `21.jpg` and `44.jpg` — a black
/// dot pattern appearing in grey screentone. `Fast` was better than `master` on
/// all six. So the default follows the measurement that was taken on the artefact
/// users actually have, and the previous default was chosen on a probe that
/// cannot see the trade.
#[test]
fn a_config_written_before_smooth_downscale_still_loads_and_stays_off() {
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(
        !cfg.smooth_downscale,
        "an old config must come up with Smooth OFF: on real pages it is better on \
         two files, unchanged on one and worse on three, so it cannot be what \
         everyone gets"
    );

    // And the default a *missing file* gets, which is a different construction
    // (`Config::load`'s hand-built struct) and was a separate way for the field
    // to be forgotten.
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.smooth_downscale, "a first run must also come up with Smooth OFF");
}

#[test]
fn a_config_written_before_pixel_art_still_loads_and_stays_off() {
    // Same two constructions as the Smooth test above: an old file without
    // the key, and a missing file (`Config::load`'s hand-built struct).
    // Magnified nearest on a photo is the blocky look nobody wants, so it
    // ships OFF and neither path may forget the field.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.pixel_art, "an old config without the key must come up with Pixel art OFF");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.pixel_art, "a first run must also come up with Pixel art OFF");
}

/// The decision the toggle names: nearest on **magnify**, linear on
/// **minify** — and linear everywhere when off. The two-axes shape is the
/// whole point of [`image_viewer::pixel_art_sampling`]: a decision glued to
/// the zoom level current at upload would have to re-roll a live texture the
/// moment zoom crossed the boundary — the re-upload hitch TODO §8 recorded
/// as unmeasured and unpriced. Naming the axes inverts that: one upload
/// serves both magnifying and minifying draws, and crossing 100% costs
/// nothing, because egui picks the sampler per axis at draw time.
#[test]
fn pixel_art_sampling_is_nearest_on_magnify_linear_on_minify() {
    use image_viewer::{Sampling, pixel_art_sampling};

    assert_eq!(pixel_art_sampling(false), Sampling::Linear);
    assert_eq!(pixel_art_sampling(true), Sampling::NearestMagnify);
}

/// The wiring in the unreachable binary, as a source scan — the established
/// pattern (`spreads.rs`, `keybindings.rs`, `renderer.rs`, the Smooth scan
/// above): `main.rs` cannot be run from a test. Four things it checks, each
/// a way the setting could exist and do nothing: the library owns the
/// decision, the settings arm **invalidates the previews on change**, the
/// content uploads all go through the one mapping, and the sites that
/// deliberately stay linear stay linear.
#[test]
fn the_pixel_art_toggle_is_wired_to_the_content_uploads_and_invalidates_too() {
    let main = main_src();
    let lib = lib_src();

    assert!(
        lib.contains("#[serde(default)]\n    pub pixel_art: bool,"),
        "the config field must serde-default, or an old config fails to load \
         (AGENTS.md: every new Config field needs it)"
    );
    assert!(
        lib.contains("pixel_art: false,"),
        "Config::load's hand-built default must state the off default, which is \
         the construction a first run actually takes"
    );
    assert!(
        lib.contains("pub fn pixel_art_sampling("),
        "the decision belongs in lib.rs, the only place a test can pin it"
    );

    // The settings arm, located by field name so the panel can be reordered
    // under the scan.
    let at = main
        .find("&mut self.config.pixel_art")
        .expect("the settings panel must have a checkbox bound to it");
    let after = &main[at..];
    let arm_end = after.find("}\n                ").expect("the changed() arm closes");
    let arm = &after[..arm_end];
    assert!(
        arm.contains("refilter = true"),
        "the changed arm must set refilter: textures already uploaded keep the \
         samplers they arrived with, so without it the box flips and nothing \
         on screen changes — the exact broken-feature shape the Smooth scan \
         documents"
    );

    // The four content sites — preview arrival, animation init, the two
    // in-play animation frame sets, the per-frame tile options — all go
    // through the one mapping; a site that went around it would sample that
    // texture linearly for as long as it lives.
    let uses = main.matches("content_texture_options(self.config.pixel_art)").count();
    assert_eq!(uses, 4, "every content upload must consult the toggle at upload time");
    assert!(
        main.contains("options: TextureOptions,")
            && main.contains("make_tile(&ctx, &tp.data, tx, ty, tile_options);"),
        "make_tile must take its sampling in from the caller, which is where \
         the config lives; the tile texture is a content texture too"
    );

    // The deliberate counter-examples: the grid cell and the checkerboard.
    let thumb_at = main.find("let texels = p.image.size;").expect("the thumb upload exists");
    let thumb_arm_len =
        main[thumb_at..].find("self.thumbs.insert").expect("the thumb arm ends at its insert");
    let thumb_arm = &main[thumb_at..thumb_at + thumb_arm_len];
    assert!(
        !thumb_arm.contains("content_texture_options("),
        "the grid cell must not call the mapping: 150pt of a \
         256-texel image, and the grid also browses photos"
    );
    assert!(
        thumb_arm.contains("TextureOptions::LINEAR"),
        "the grid cell stays linear on purpose, said here so a later edit that \
         changes it fails loudly instead of silently"
    );
    assert!(
        main.contains("TextureOptions::NEAREST_REPEAT"),
        "the checkerboard keeps NEAREST_REPEAT: it is artwork, not content, \
         and pixel art user content sampling is not its reason"
    );
}

#[test]
fn a_config_written_before_grid_cell_size_still_loads_at_150() {
    // Same two constructions as the Smooth test above: an old file without the
    // key (serde default) and a missing file (`Config::load`'s hand-built
    // struct). Either one forgotten parks every existing user on the wrong
    // cell size with no error.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert_eq!(cfg.grid_cell, 150.0, "an old config without the key must come up at 150pt cells");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert_eq!(fresh.grid_cell, 150.0, "a first run must also come up at 150pt cells");
}

#[test]
fn a_config_written_before_slideshow_shuffle_still_loads_with_it_off() {
    // A shuffled show surprises anyone who did not ask for one, so an old
    // config — and a new install — must come up with it off. Same two
    // constructions as the tests above.
    let cfg: image_viewer::Config =
        serde_json::from_str(r#"{"load_all_on_startup":false,"folder":null}"#)
            .expect("an old config must load");
    assert!(!cfg.slideshow_shuffle, "an old config without the key must not start shuffling shows");
    let tmp = tempfile::tempdir().unwrap();
    let fresh = image_viewer::Config::load(&tmp.path().join("absent.json"));
    assert!(!fresh.slideshow_shuffle, "a first run must also come up with shuffle off");
}
