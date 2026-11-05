//! Remembered view, preload budget, panel master and the wgpu-free build.
//!
//! Split out of `tests/unit.rs`; the shared helpers live in
//! `tests/common/unit_fixtures.rs`.

#[path = "common/unit_fixtures.rs"]
mod unit_fixtures;

use unit_fixtures::*;

// ---------------------------------------------------------------------------
// Per-image view memory (the "Remember zoom and pan per image" setting)
// ---------------------------------------------------------------------------

/// A remembered view is **the view that was left** — an identity round-trip,
/// not a rescaling. Every field distinct so a swap in any of them fails.
#[test]
fn a_remembered_view_is_the_view_that_was_left() {
    let key = image_viewer::ImgKey::first(std::path::Path::new("p/manga.tiff"));
    let left = image_viewer::SavedView { fit: false, scale: 1.3729, offset: (17.25, -9.75) };
    let mut m = image_viewer::ViewMemory::new();
    m.remember(key.clone(), left);
    assert_eq!(m.restore(&key), Some(left));
}

/// A page of a document is its own picture — the reason this is keyed on
/// [`image_viewer::ImgKey`] at all. A path-keyed map would hand page 3 back
/// page 1's pan, silently, and only for multi-page files.
#[test]
fn the_next_page_of_a_document_is_not_the_last_page_s_view() {
    let p = std::path::Path::new("p/vol.tiff");
    let page0 = image_viewer::ImgKey { path: p.to_path_buf(), page: 0 };
    let page2 = image_viewer::ImgKey { path: p.to_path_buf(), page: 2 };
    let mut m = image_viewer::ViewMemory::new();
    m.remember(page0, image_viewer::SavedView { fit: false, scale: 3.0, offset: (41.0, 0.0) });
    assert!(m.restore(&page2).is_none(), "page 2 must not inherit page 0's view");
    // And the file that was NOT visited is equally empty.
    assert!(m.restore(&image_viewer::ImgKey::first(std::path::Path::new("p/other.jpg"))).is_none());
}

/// The no-memory arm is **today's behaviour, unchanged**: fit and zoom carried
/// over, pan reset to the centre. This is what `arriving_view` exists to
/// guarantee — the setting off is not its arm (the caller decides whether to
/// ask), so this is the arm that keeps "off by default changes nothing" true
/// even for images a toggled-on session never touched.
#[test]
fn an_unvisited_image_gets_the_carry() {
    let saved = image_viewer::arriving_view(None, false, 2.1875);
    assert!(!saved.fit);
    assert_eq!(saved.scale, 2.1875);
    assert_eq!(saved.offset, (0.0, 0.0), "the pan does not travel");
    // A remembered one is handed back whole, so the carry gets nothing mixed in.
    let left = image_viewer::SavedView { fit: true, scale: 0.75, offset: (-3.5, 12.0) };
    assert_eq!(
        image_viewer::arriving_view(Some(left), false, 2.1875),
        left,
        "the remembered view wins over the carry, exactly"
    );
}

/// `forget_file` drops **every page of the file and nothing of its
/// neighbours** — the same scope `invalidate_path` gives every other store it
/// clears.
#[test]
fn a_reexport_drops_the_whole_file_but_not_the_neighbours() {
    let vol1 = std::path::Path::new("p/vol1.cbz"); // archive members share a temp path
    let k1 = image_viewer::ImgKey { path: vol1.to_path_buf(), page: 0 };
    let k1b = image_viewer::ImgKey { path: vol1.to_path_buf(), page: 1 };
    let k2 = image_viewer::ImgKey::first(std::path::Path::new("p/photo.jpg"));
    let view = image_viewer::SavedView { fit: false, scale: 1.0, offset: (8.0, 8.0) };
    let mut m = image_viewer::ViewMemory::new();
    m.remember(k1.clone(), view);
    m.remember(k1b.clone(), view);
    m.remember(k2.clone(), view);
    m.forget_file(vol1);
    assert!(m.restore(&k1).is_none());
    assert!(m.restore(&k1b).is_none());
    assert_eq!(m.restore(&k2), Some(view), "the neighbour keeps its own");
}

/// `#[serde(default)]` is load-bearing here like on every other added
/// `Config` field — an old config without this key must load and keep the
/// shipped behaviour off.
#[test]
fn an_old_config_without_the_remember_setting_lands_on_the_default() {
    let old = r#"{"load_all_on_startup": true, "thumbnail_cache": false}"#;
    let c: image_viewer::Config = serde_json::from_str(old).unwrap();
    assert!(
        !c.remember_view,
        "a config with no remember_view key must get the default, which is off"
    );
    let on = r#"{"load_all_on_startup": true, "remember_view": true}"#;
    let c2: image_viewer::Config = serde_json::from_str(on).unwrap();
    assert!(c2.remember_view, "an explicit true must be honoured");
}

/// The renderer setting is **gone from the file, and a file that still has it
/// must still load.**
///
/// wgpu stopped being built when the app moved to glow, so `renderer` is now an
/// unknown key. Unknown keys are ignored rather than rejected (there is no
/// `deny_unknown_fields`), and that is the whole reason every `config.json`
/// written by an older build keeps working. A test that only checked a *missing*
/// key would not notice the day someone made unknown keys fatal; this one feeds
/// back a key this version has no field for.
#[test]
fn a_config_written_by_the_wgpu_build_still_loads() {
    let old = r#"{
        "load_all_on_startup": true,
        "renderer": { "choice": "dx12", "probed": "gl" },
        "folder": "C:\\photos"
    }"#;
    let c: image_viewer::Config = serde_json::from_str(old).expect("an old config must load");
    assert!(c.load_all_on_startup);
    assert_eq!(c.folder.as_deref(), Some(r"C:\photos"));
}

/// **wgpu is not in the build, and that is a decision about a file the compiler
/// does not check.**
///
/// eframe's default features bring `egui-wgpu`, and with it `wgpu`, `wgpu-core`,
/// `wgpu-hal` and `naga` plus their D3D12 and Vulkan backends. Measured on this
/// machine that is 154 MB more idle memory (321 MB against 84 MB for a bare
/// window) and 269 ms more to create a device, for the **same** frame rate - and
/// its Vulkan backend access-violates during adapter enumeration even though
/// `vulkaninfo` reports a healthy device. The app renders on glow, so none of it
/// is used.
///
/// Removing `default-features = false` restores all of it silently. Nothing else
/// in the tree notices, the build only gets slower, and no test would catch it;
/// this is that test. `AGENTS.md` carries the measurements.
#[test]
fn eframe_is_built_without_wgpu() {
    // The manifest explains in comments *why* wgpu is absent, so a prose line
    // mentioning it must not be what this reads.
    let manifest: String = include_str!("../Cargo.toml")
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    // Just the eframe entry: `zip` also sets `default-features = false`, and a
    // whole-file search for that key would pass while eframe's defaults came
    // back.
    let start = manifest.find("eframe =").expect("an eframe dependency");
    let entry = &manifest[start..];
    let entry = &entry[..entry.find("] }").map(|i| i + 3).unwrap_or(entry.len())];
    assert!(
        entry.contains("default-features = false"),
        "eframe must be built with its default features off: turning them on \
         brings back wgpu, along with the Wayland/X11 and web features a \
         Windows-only glow app never uses. eframe entry: {entry}"
    );
    assert!(
        !entry.contains("\"wgpu\""),
        "wgpu is not used by this app - the renderer is glow. eframe entry: {entry}"
    );
    assert!(
        entry.contains("\"glow\""),
        "glow is the renderer; without it eframe has no backend at all. \
         eframe entry: {entry}"
    );
}

/// The wiring: **the save must happen while the outgoing key is still in
/// hand, and the restore after the key changes** — both inside the
/// cursor-change block, both gated on the setting. `main.rs` is unreachable
/// from every test, so this is the source-scan pattern with the position
/// asserted, because the two defects it guards are both *arrangement* bugs:
/// saving after `pan_key` is reassigned would store the new image's view
/// under the new image's name (the map would say "remembered" and hold the
/// carry), and restoring outside the block would re-derive what the reader
/// is already looking at.
#[test]
fn view_memory_is_wired_to_the_cursor_change_and_the_setting() {
    let src_source = main_src();
    // `fn_body` is built for signatures *without* a trailing `{`, so the body's
    // own braces carry the depth. A brace-terminated block needs the `{` left
    // out of the search instead, or every inner `if` returning to depth 0
    // looks like the block's end. Small local matcher rather than a second
    // definition of fn_body.
    fn span_after<'a>(src: &'a str, trigger: &str) -> Option<&'a str> {
        let sig = src.find(trigger)?;
        let open = src[sig..].find('{')? + sig;
        let mut depth = 0i32;
        let mut end = None;
        for (i, b) in src.as_bytes()[open..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        Some(&src[open + 1..end?])
    }
    let src = src_source;
    let goto = fn_body(&src, "fn goto(").expect("goto");
    let block = span_after(goto, "if self.cur != self.pan_key {").expect("the cursor-change block");
    let remember = block
        .find("self.views.remember(")
        .unwrap_or_else(|| panic!("the outgoing view must be remembered in the block"));
    let key_move =
        block.find("self.pan_key = self.cur.clone();").expect("the block must still move pan_key");
    let restore = block
        .find("self.views.restore(")
        .unwrap_or_else(|| panic!("the arriving view must be restored in the block"));
    assert!(
        remember < key_move,
        "save BEFORE pan_key is reassigned, or the outgoing key is gone and its \
         view is stored under the arriving key's name"
    );
    assert!(
        key_move < restore,
        "restore AFTER the key changes, or it asks about the image being left"
    );
    let ones = block.matches("self.config.remember_view").count();
    assert!(ones >= 2, "both arms must be gated on the setting, found {ones}");
    // The arriving view goes through the pure decision, not a hand-rolled arm.
    assert!(
        block.contains("image_viewer::arriving_view("),
        "the restore must ask the pure function, so a second opinion about the \
         carry cannot form"
    );
    // And the forget: a re-export must not leave a view for pixels that no
    // longer exist, beside the page counts and shapes it already clears.
    let inv = fn_body(&src, "fn invalidate_path(").expect("invalidate_path");
    assert!(
        inv.contains("self.views.forget_file(p);"),
        "invalidate_path must drop the file's remembered views"
    );
    assert!(
        src.contains("Remember zoom and pan per image"),
        "the settings panel must carry the toggle"
    );
}

// ---------------------------------------------------------------------------
// The preload window's byte budget.
// ---------------------------------------------------------------------------

/// `main.rs` is unreachable from tests, so the wiring is a source scan with
/// the order asserted. Three things must hold, and each has a plausible
/// wrong-looking sibling: the lengths come from the **stamp registry the
/// watcher already keeps** (`self.stamps`) rather than a stat per plan — a
/// per-emit `metadata()` on the paint thread is the I/O this crate exists not
/// to do — the slice is built **before** the `Plan` literal that consumes it,
/// and the planner is configured with the shipped constant rather than a
/// literal or `u64::MAX` at the call site (a disabled budget is invisible
/// except in memory).
#[test]
fn the_preload_budget_is_wired_from_the_watcher_s_stamp_registry() {
    let src = main_src();
    let body = fn_body(&src, "fn schedule(&mut self)").expect("schedule");
    let collect = body
        .find("self.stamps.get(p)")
        .expect("schedule must read the stamp registry for the file lengths");
    let plan = body.find("plan_jobs(").expect("schedule must plan");
    assert!(collect < plan, "the slice is worthless if collected after the plan is built");
    assert!(
        body.contains("file_bytes: &file_bytes,"),
        "the plan must be handed the collected slice"
    );
    assert!(
        body.contains("preload_budget: PRELOAD_BUDGET_BYTES,"),
        "the shipped budget is the constant, not a literal or u64::MAX"
    );
    assert!(
        !body.contains("metadata("),
        "the plan path must not stat: the lengths are the watcher's already"
    );
    // And the constant sits with the radius it bounds, so the two knobs that
    // shape one window are found together.
    let radius = src.find("pub(crate) const PRELOAD_RADIUS: usize = 3;").expect("the radius");
    let budget = src.find("const PRELOAD_BUDGET_BYTES: u64 =").expect("the budget");
    assert!(
        radius < budget && budget - radius < 4000,
        "the radius and the budget tune the same window and are documented together"
    );
}

// ---------------------------------------------------------------------------
// The side-panel master (`I`) is a persisted preference; fullscreen is an
// overlay on it, and never a re-write of it.
// ---------------------------------------------------------------------------

/// A config from before the key existed keeps showing panels — the default is
/// `on` for the same reason `loop_back`'s is: a tolerated older file must not
/// change what it shows. An explicit `false` must be honoured, of course.
#[test]
fn an_old_config_without_the_panel_master_lands_on_the_default() {
    let old = r#"{"load_all_on_startup": true}"#;
    let c: image_viewer::Config = serde_json::from_str(old).unwrap();
    assert!(c.show_panels, "no show_panels key must land on the default, which is on");
    let off = r#"{"load_all_on_startup": true, "show_panels": false}"#;
    let c2: image_viewer::Config = serde_json::from_str(off).unwrap();
    assert!(!c2.show_panels, "an explicit false must be honoured");
}

/// The wiring: the master is saved on its toggle and read on startup, and no
/// site is left that would force it back on — the whole-config save on exit
/// would then record the force as the user's choice. `main.rs` is unreachable
/// from tests, so source scans again, with position where the order matters.
#[test]
fn the_panel_master_is_saved_and_fullscreen_only_borrows_it() {
    fn block_after<'a>(src: &'a str, trigger: &str) -> Option<&'a str> {
        let sig = src.find(trigger)?;
        let open = src[sig..].find('{')? + sig;
        let mut depth = 0i32;
        let mut end = None;
        for (i, b) in src.as_bytes()[open..].iter().enumerate() {
            match b {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        Some(&src[open + 1..end?])
    }

    let src = main_src();

    // The funnel: save/restore only through `toggle_panels_master`, and the
    // save inside it is ordered after the flip.
    let helper = fn_body(&src, "fn toggle_panels_master(").expect("master helper");
    let flip = helper
        .find("self.show_panels = !self.show_panels;")
        .expect("the helper must flip the master");
    let saved = helper
        .find("self.config.show_panels = self.show_panels;")
        .expect("the helper must mirror the flip into the config");
    let write = helper.find("self.config.save(").expect("the helper must save on the press");
    assert!(flip < saved && saved < write, "flip, mirror, save, in order");

    // Both `I` sites (key and Plates row) route through it. A site that
    // toggles inline would skip the persist, and the first launch would
    // quietly disagree.
    let dispatch = fn_body(&src, "fn ui(").expect("the event dispatcher");
    let handlers = src.matches("self.toggle_panels_master();").count();
    let toggles_inline = src.matches("self.show_panels = !self.show_panels;").count();
    assert_eq!(
        toggles_inline, 1 as usize,
        "only the helper flips the master, inline flips found: {toggles_inline}"
    );
    assert!(
        handlers >= 2,
        "the key and menu-row sites both call the funnel, found only {handlers}"
    );

    // Startup honours the saved value — the same capture pattern the
    // constructor uses for `loop_back`.
    let ctor = fn_body(&src, "fn new(").expect("constructor");
    let capture = ctor
        .find("let show_panels = config.show_panels;")
        .expect("the constructor must read the saved master");
    let literal = ctor.find("let mut app = Self {").expect("the struct literal");
    assert!(capture < literal, "the capture happens before `config` moves into the struct");

    // No site forces the master back on. Both were hardcoded `true` once;
    // the whole-config save on exit would have recorded the force.
    assert!(
        !src.contains("self.show_panels = true"),
        "a fullscreen exit must restore what the user had, not force the master on"
    );

    // And the fullscreen overlay pairs against the stash: one entering arm
    // that stashes, two leaving arms (F11 again, and Esc) that restore.
    let f11 = block_after(&dispatch, "if f11 {").expect("the F11 block");
    assert!(!f11.contains("config.save"), "the fullscreen dance must not write the config");
    assert!(
        f11.contains("panels_before_fullscreen = Some(self.show_panels)"),
        "entering stash the master"
    );
    assert!(
        f11.contains("panels_before_fullscreen.take().unwrap_or(true)"),
        "F11 exit restores the stash"
    );
    let restores = dispatch.matches("panels_before_fullscreen.take().unwrap_or(true)").count();
    assert_eq!(
        restores, 2 as usize,
        "the Esc exit restores too — one restore, one fullscreen with no way back"
    );
}
