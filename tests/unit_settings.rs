//! Portable mode, persistent roots, the settings panel, rebindable keys,
//! user commands and beta features.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

/// Portable mode is the marker file: present means beside-the-exe, absent
/// means the roaming paths — asserted against a tempdir, never the real
/// executable's folder.
#[test]
fn portable_mode_follows_the_marker_file() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(image_viewer::portable_dir_from(dir.path()), None);
    std::fs::write(dir.path().join("portable.txt"), b"").unwrap();
    assert_eq!(image_viewer::portable_dir_from(dir.path()), Some(dir.path().to_path_buf()));
}

/// All three persistent roots must consult portable mode, or portable is a
/// config file sitting next to a roaming cache.
#[test]
fn persistent_roots_consult_portable_mode() {
    let lib = lib_src();
    for f in ["pub fn default_path()", "pub fn thumb_cache_root()", "pub fn spreads_root()"] {
        let at = lib.find(f).unwrap_or_else(|| panic!("{f} is gone"));
        let body = &lib[at..(at + 600).min(lib.len())];
        assert!(body.contains("portable_dir()"), "{f} must check portable mode first");
    }
}

/// Copy-original shares the copy path with an identity edit: same sources,
/// no re-decode, EXIF-oriented but uncropped and unturned.
#[test]
fn copy_original_shares_the_copy_path() {
    let src = main_src();
    assert!(
        src.contains("fn copy_image_to_clipboard(&mut self, unedited: bool)"),
        "there must be one copy path with an unedited arm"
    );
    assert!(
        src.contains("Copy original") && src.contains("self.copy_image_to_clipboard(true)"),
        "the menu must offer the unedited copy"
    );
}

/// The settings panel must scroll: it long outgrew the window height, and
/// `main.rs` is unreachable from every test, so this is a source scan - the
/// established pattern here.
#[test]
fn the_settings_panel_scrolls() {
    let src = main_src();
    let at = src.find("Panel::right(\"settings\")").expect("the settings panel must exist");
    let arm = &src[at..(at + 1200).min(src.len())];
    // The settings pane sits inside a vertical ScrollArea so all expanded
    // sections still fit on short windows; each section is its own
    // collapsible group so it's not a "window inside window".
    assert!(
        arm.contains("ScrollArea::vertical()"),
        "settings content must sit in a vertical ScrollArea, or it runs off \
         the bottom of short windows"
    );
    assert!(arm.contains("CollapsingHeader::new"), "settings sections must be collapsible groups");
}

/// Rebindable keys must be **wired**, and `main.rs` is unreachable from every
/// test, so these are source scans - the established pattern here.
#[test]
fn the_rebindable_keys_are_wired_to_the_direction_rule() {
    let src = main_src();
    assert!(
        src.contains("set_nav_overrides(&self.config.keybindings)"),
        "startup and the settings panel must push keybindings into the nav table"
    );
    assert!(
        src.contains("fn effective_nav_keys()"),
        "overrides must be applied in one place the direction rule reads"
    );
    // The rule still owns every direction: the override table carries keys,
    // and the meanings stay NavInputs asked of nav_is_forward.
    assert!(
        src.contains("NAV_REBINDABLE"),
        "there must be one action->meaning table for rebindable keys"
    );
    assert!(
        src.contains("self.capturing = Some("),
        "Settings must offer click-to-capture rebinding"
    );
}

/// User commands must be **wired**: a chord, a runner, and the expansion.
#[test]
fn the_user_commands_are_wired_to_ctrl_digits() {
    let src = main_src();
    assert!(src.contains("fn run_user_command(&mut self"), "there must be a user-command runner");
    assert!(
        src.contains("expand_user_command(&cmd, &path)"),
        "the runner must expand {{file}}/{{dir}}/{{name}}, not spawn raw config text"
    );
    assert!(src.contains("egui::Key::Num1"), "Ctrl+1..9 must trigger the commands");
}

/// Spawning must be edge-triggered: `key_pressed` also fires for OS
/// key-repeat, and a repeat spawns one process per frame — a held Ctrl+1
/// opened hundreds of Paint windows and froze the machine. Spawn-class
/// actions therefore go through the raw-event matcher with `repeat: false`,
/// never `key_pressed`.
#[test]
fn the_user_command_chord_ignores_key_repeat() {
    let src = main_src();
    let at = src.find("fn action_pressed_fresh(").expect("the edge-triggered matcher must exist");
    let body = &src[at..(at + 1200).min(src.len())];
    assert!(
        body.contains("repeat: false"),
        "the matcher must filter OS key-repeat, or holding Ctrl+1 fork-bombs"
    );
    assert!(
        !body.contains("key_pressed"),
        "the matcher must read raw events, not key_pressed, which fires on repeats"
    );
    for action in ["print", "copy", "copy_path"] {
        assert!(
            src.contains(&format!("action_pressed_fresh(ctx, &kb, \"{action}\")")),
            "spawn-class action `{action}` must go through the edge-triggered matcher"
        );
    }
    // user_1..9 go through the same matcher via an explicit name table (not
    // format-built, so the action-coverage test can see every one).
    assert!(
        src.contains("\"user_1\"") && src.contains("\"user_9\""),
        "all nine user-command actions must be listed by name"
    );
}

/// Guardrails around spawning: a global cooldown consulted by every site
/// that starts an external process, so no repeating chord — this one or a
/// future one — can fork-bomb the machine again. `main.rs` is unreachable
/// from every test, so this is a source scan, the established pattern.
#[test]
fn external_spawns_are_rate_limited() {
    let src = main_src();
    assert!(
        src.contains("const SPAWN_COOLDOWN: Duration"),
        "there must be one cooldown every spawn site shares"
    );
    for (sig, what) in [
        ("fn run_user_command(&mut self", "user commands"),
        ("fn explore_current(&mut self", "Explorer"),
    ] {
        let at = src
            .find(sig)
            .unwrap_or_else(|| panic!("{what} handler is gone; update this test to match"));
        let body = &src[at..(at + 2500).min(src.len())];
        assert!(
            body.contains("spawn_guard("),
            "{what} must go through spawn_guard, or a repeating chord can \
             spawn one process per frame again"
        );
    }
}

/// The binary identifies its own build in Settings, so a copy in Downloads
/// can be told apart from the current checkout. Stale copies already caused
/// two ghost reports (a chord that "did nothing", a freeze that was fixed).
#[test]
fn the_settings_panel_shows_the_build_id() {
    let src = main_src();
    assert!(
        src.contains("env!(\"IV_BUILD_ID\")"),
        "Settings must show the build fingerprint, or stale binaries are \
         undebuggable"
    );
}

/// Beta features wear a red marker in the panel, next to the label — the
/// same marker for both, matching the README's beta sections.
#[test]
fn beta_features_are_marked_beta_in_red() {
    let src = main_src();
    for base in ["Double page mode", "User commands (Ctrl+1..9):"] {
        let at = src.find(base).unwrap_or_else(|| panic!("{base} is gone from the settings panel"));
        let arm = &src[at..(at + 600).min(src.len())];
        assert!(
            arm.contains("(beta)") && arm.contains("warn_fg_color"),
            "{base} must be followed by a red (beta) marker"
        );
    }
}

/// Each side pane hides alone (`L` left, `P` right) under the `I` master, and
/// the EXIF read follows the *effective* visibility — both halves of the
/// gate, or hiding the pane leaves a worker reading a file nobody displays.
#[test]
fn side_panes_hide_individually_and_gate_their_reads() {
    let src = main_src();
    assert!(
        src.contains("self.show_panels && self.show_left"),
        "the left pane and its EXIF read must gate on master and flag"
    );
    assert!(
        src.contains("self.show_panels && self.show_right"),
        "the right pane must gate on master and flag"
    );
    assert!(
        src.contains("self.show_left = !self.show_left")
            && src.contains("self.show_right = !self.show_right"),
        "L and P must each flip their own flag"
    );
    // Each press must say what it did: with the master off a toggle changes
    // nothing visible, and without the notice a working key reads as dead.
    assert!(
        src.contains("\"Left pane hidden\"") && src.contains("\"Right pane hidden\""),
        "both pane toggles must leave a notice naming the outcome"
    );
    // Position, not presence: the master `if` used to wrap *both* side panes,
    // so gating the left one on its flag hid the right one too — and every
    // presence check above passed on that code. The left `if` must close
    // (a `}` at its own indent) before the right panel is shown.
    let left_at =
        src.find("if self.show_panels && self.show_left {").expect("the left pane gate must exist");
    let right_at = src[left_at..]
        .find("Panel::right(\"settings\")")
        .map(|i| left_at + i)
        .expect("the right panel must exist");
    assert!(
        src[left_at..right_at].lines().any(|l| l == "        }"),
        "the left gate must close before the right panel, or L hides both"
    );
}
