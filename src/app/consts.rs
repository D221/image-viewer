//! Module pp_consts, moved out of main.rs when it passed the
//! 1000-line target.
//!
//! main.rs is the binary's crate root: the imports, the type definitions,
//! n main and the module list.
//! See AGENTS.md and 	ests/source_size.rs.

use crate::*;

/// Wheel delta accumulated before a flip triggers in wheel-navigation mode.
pub(crate) const THRESH_WHEEL: f32 = 60.0;

/// The keys that navigate, each paired with what it **means**.
///
/// **The pairs are the direction rule's inputs, and this table is where a physical
/// key becomes one.** `Right` and `Left` are the arrows and they are the only two
/// entries with a side to preserve, so `manga_rtl` inverts exactly those. `Next` and
/// `Previous` are the ones that say "onward" without saying which way; they ship on
/// `PageDown` (`PageUp` for `Previous`), the mouse's forward/back buttons, and
/// nothing else — **`Space` is deliberately not navigation.** It is the zoom
/// toggle's second chord (`fit_alt`: fit ↔ 100%, JPEGView's own Space binding,
/// its `KeyMap.txt.default` line 49 `IDM_TOGGLE_FIT_TO_SCREEN_100_PERCENTS`), and
/// it moving the grid highlight is the other hand it already has. Shipping it as
/// a third "onward" key would have it holding two meanings in one hand, and the
/// alt-navigation slot (`next_alt`) ships unbound precisely so it is free.
///
/// They are one table, in one place, rather than a boolean per key read in the
/// dispatch, because "which key means what" is a decision with a failure mode and no
/// natural home if it is spread: an earlier version read each key separately and a
/// key could be attached to the wrong meaning without anything noticing. Read as a
/// table by `tests/keybindings.rs`, which asserts each row pairs the key with the
/// input it is supposed to mean.
pub(crate) const NAV_KEYS: &[(egui::Key, NavInput)] = &[
    (egui::Key::ArrowRight, NavInput::Right),
    (egui::Key::ArrowLeft, NavInput::Left),
    (egui::Key::PageDown, NavInput::Next),
    (egui::Key::PageUp, NavInput::Previous),
];

/// Mouse side buttons, in the same shape as [`NAV_KEYS`].
///
/// Extra2 is the browser's "forward" and Extra1 its "back", so they are `Next` and
/// `Previous` like every other forward-going input - not a pair of directions of
/// their own, which is what they were before this table existed.
pub(crate) const NAV_BUTTONS: &[(egui::PointerButton, NavInput)] = &[
    (egui::PointerButton::Extra2, NavInput::Next),
    (egui::PointerButton::Extra1, NavInput::Previous),
];

/// Resolve [`NAV_KEYS`] and [`NAV_BUTTONS`] into the direction they mean, if any of
/// them fired.
///
/// **The single place a direction is produced in this binary**, and `Option<bool>`
/// rather than two booleans for a reason that is easy to get wrong: with two flags,
/// every caller has to decide which wins, and a caller that writes the decision
/// itself is a second opinion about direction — which is the entire defect this
/// feature's key mapping exists to prevent. Here there is one answer, and a caller
/// that does not like it cannot invent another.
///
/// `Some(true)` is forwards and `Some(false)` is backwards. `None` is nothing was
/// pressed, which is a different answer from "backwards" and used to be spelled the
/// same way.
///
/// The two closures rather than reading the input here, because the same table has
/// to be read two ways — `key_pressed` for a press and `key_down` for
/// hold-to-browse — and passing the predicate is what stops those from being two
/// implementations that drift apart.
///
/// **Forward wins when both fired.** A frame with an arrow-left and space held is
/// two things at once, and "forwards" is what the old `next || ...` chain answered,
/// so this is parity rather than a new rule.
///
/// Rebindable keys layer on top without moving the rule: `NAV_KEYS`
/// stays the shipped default table that `tests/keybindings.rs` pins, and
/// the effective table (action id → chord, from `Config::keybindings`)
/// replaces the key a rebindable action is read from. The *meanings* never
/// change, so a rebound key still names itself as a `NavInput` and still
/// asks `nav_is_forward` — there is still exactly one function that knows
/// which way forwards is.
pub(crate) fn nav_direction(
    key_active: impl Fn(egui::Key, bool, bool, bool) -> bool,
    button_active: impl Fn(egui::PointerButton) -> bool,
    rtl: bool,
) -> Option<bool> {
    let mut forward = false;
    let mut backward = false;
    let mut note = |active: bool, meaning: NavInput| {
        if !active {
            return;
        }
        if nav_is_forward(meaning, rtl) {
            forward = true;
        } else {
            backward = true;
        }
    };
    for (b, meaning) in effective_nav_keys() {
        note(key_active(b.key, b.ctrl, b.shift, b.alt), meaning);
    }
    for (button, meaning) in NAV_BUTTONS {
        note(button_active(*button), *meaning);
    }
    if forward {
        Some(true)
    } else if backward {
        Some(false)
    } else {
        None
    }
}

/// Rebindable navigation actions and the `NavInput` each one means.
/// The key is replaceable; the meaning is not.
pub(crate) const NAV_REBINDABLE: &[(&str, NavInput)] = &[
    ("next", NavInput::Right),
    ("prev", NavInput::Left),
    ("next_alt", NavInput::Next),
    ("page_next", NavInput::Next),
    ("page_prev", NavInput::Previous),
];

/// Full key-name table for custom bindings: every `egui::Key` worth
/// pressing, plus bare `"0"`–`"9"` mapping to the numpad-less `Num0`–`Num9`
/// variants (egui has no bare digit keys, and nobody means the numpad when
/// they write `"1"`). Anything else is unbound — see `resolve_action`.
pub(crate) fn parse_key_full(name: &str) -> Option<egui::Key> {
    let name = name.trim();
    if name.len() == 1 {
        let c = name.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return match c.to_ascii_uppercase() {
                'A' => Some(egui::Key::A),
                'B' => Some(egui::Key::B),
                'C' => Some(egui::Key::C),
                'D' => Some(egui::Key::D),
                'E' => Some(egui::Key::E),
                'F' => Some(egui::Key::F),
                'G' => Some(egui::Key::G),
                'H' => Some(egui::Key::H),
                'I' => Some(egui::Key::I),
                'J' => Some(egui::Key::J),
                'K' => Some(egui::Key::K),
                'L' => Some(egui::Key::L),
                'M' => Some(egui::Key::M),
                'N' => Some(egui::Key::N),
                'O' => Some(egui::Key::O),
                'P' => Some(egui::Key::P),
                'Q' => Some(egui::Key::Q),
                'R' => Some(egui::Key::R),
                'S' => Some(egui::Key::S),
                'T' => Some(egui::Key::T),
                'U' => Some(egui::Key::U),
                'V' => Some(egui::Key::V),
                'W' => Some(egui::Key::W),
                'X' => Some(egui::Key::X),
                'Y' => Some(egui::Key::Y),
                'Z' => Some(egui::Key::Z),
                _ => None,
            };
        }
        if c.is_ascii_digit() {
            return match c {
                '0' => Some(egui::Key::Num0),
                '1' => Some(egui::Key::Num1),
                '2' => Some(egui::Key::Num2),
                '3' => Some(egui::Key::Num3),
                '4' => Some(egui::Key::Num4),
                '5' => Some(egui::Key::Num5),
                '6' => Some(egui::Key::Num6),
                '7' => Some(egui::Key::Num7),
                '8' => Some(egui::Key::Num8),
                '9' => Some(egui::Key::Num9),
                _ => None,
            };
        }
        return None;
    }
    match name {
        "Num0" => Some(egui::Key::Num0),
        "Num1" => Some(egui::Key::Num1),
        "Num2" => Some(egui::Key::Num2),
        "Num3" => Some(egui::Key::Num3),
        "Num4" => Some(egui::Key::Num4),
        "Num5" => Some(egui::Key::Num5),
        "Num6" => Some(egui::Key::Num6),
        "Num7" => Some(egui::Key::Num7),
        "Num8" => Some(egui::Key::Num8),
        "Num9" => Some(egui::Key::Num9),
        "F1" => Some(egui::Key::F1),
        "F2" => Some(egui::Key::F2),
        "F3" => Some(egui::Key::F3),
        "F4" => Some(egui::Key::F4),
        "F5" => Some(egui::Key::F5),
        "F6" => Some(egui::Key::F6),
        "F7" => Some(egui::Key::F7),
        "F8" => Some(egui::Key::F8),
        "F9" => Some(egui::Key::F9),
        "F10" => Some(egui::Key::F10),
        "F11" => Some(egui::Key::F11),
        "F12" => Some(egui::Key::F12),
        "ArrowRight" => Some(egui::Key::ArrowRight),
        "ArrowLeft" => Some(egui::Key::ArrowLeft),
        "ArrowUp" => Some(egui::Key::ArrowUp),
        "ArrowDown" => Some(egui::Key::ArrowDown),
        "Space" => Some(egui::Key::Space),
        "Enter" => Some(egui::Key::Enter),
        "Escape" => Some(egui::Key::Escape),
        "Tab" => Some(egui::Key::Tab),
        "Backspace" => Some(egui::Key::Backspace),
        "Delete" => Some(egui::Key::Delete),
        "Insert" => Some(egui::Key::Insert),
        "Home" => Some(egui::Key::Home),
        "End" => Some(egui::Key::End),
        "PageUp" => Some(egui::Key::PageUp),
        "PageDown" => Some(egui::Key::PageDown),
        "Comma" => Some(egui::Key::Comma),
        "Period" => Some(egui::Key::Period),
        "Slash" => Some(egui::Key::Slash),
        "Semicolon" => Some(egui::Key::Semicolon),
        "Quote" => Some(egui::Key::Quote),
        "OpenBracket" => Some(egui::Key::OpenBracket),
        "CloseBracket" => Some(egui::Key::CloseBracket),
        "Backslash" => Some(egui::Key::Backslash),
        "Backtick" => Some(egui::Key::Backtick),
        "Minus" => Some(egui::Key::Minus),
        "Equals" => Some(egui::Key::Equals),
        "Plus" => Some(egui::Key::Plus),
        _ => None,
    }
}

/// Modifier keys never become bindings on their own: capture waits for the
/// real key, whose event already carries the held modifiers.
pub(crate) fn is_modifier_key(key: egui::Key) -> bool {
    matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    )
}

/// Reverse of `parse_key_full`: the name capture stores and Settings
/// shows. Must cover exactly the same keys — the totality scan in
/// `tests/keybindings.rs` checks both directions against `KEY_NAMES`.
pub(crate) fn key_name(key: egui::Key) -> &'static str {
    if is_modifier_key(key) {
        // Unreachable through the capture filter, which skips modifiers —
        // belt and braces so a modifier can never become a stored name.
        return "?";
    }
    match key {
        egui::Key::A => "A",
        egui::Key::B => "B",
        egui::Key::C => "C",
        egui::Key::D => "D",
        egui::Key::E => "E",
        egui::Key::F => "F",
        egui::Key::G => "G",
        egui::Key::H => "H",
        egui::Key::I => "I",
        egui::Key::J => "J",
        egui::Key::K => "K",
        egui::Key::L => "L",
        egui::Key::M => "M",
        egui::Key::N => "N",
        egui::Key::O => "O",
        egui::Key::P => "P",
        egui::Key::Q => "Q",
        egui::Key::R => "R",
        egui::Key::S => "S",
        egui::Key::T => "T",
        egui::Key::U => "U",
        egui::Key::V => "V",
        egui::Key::W => "W",
        egui::Key::X => "X",
        egui::Key::Y => "Y",
        egui::Key::Z => "Z",
        egui::Key::Num0 => "Num0",
        egui::Key::Num1 => "Num1",
        egui::Key::Num2 => "Num2",
        egui::Key::Num3 => "Num3",
        egui::Key::Num4 => "Num4",
        egui::Key::Num5 => "Num5",
        egui::Key::Num6 => "Num6",
        egui::Key::Num7 => "Num7",
        egui::Key::Num8 => "Num8",
        egui::Key::Num9 => "Num9",
        egui::Key::F1 => "F1",
        egui::Key::F2 => "F2",
        egui::Key::F3 => "F3",
        egui::Key::F4 => "F4",
        egui::Key::F5 => "F5",
        egui::Key::F6 => "F6",
        egui::Key::F7 => "F7",
        egui::Key::F8 => "F8",
        egui::Key::F9 => "F9",
        egui::Key::F10 => "F10",
        egui::Key::F11 => "F11",
        egui::Key::F12 => "F12",
        egui::Key::ArrowRight => "ArrowRight",
        egui::Key::ArrowLeft => "ArrowLeft",
        egui::Key::ArrowUp => "ArrowUp",
        egui::Key::ArrowDown => "ArrowDown",
        egui::Key::Space => "Space",
        egui::Key::Enter => "Enter",
        egui::Key::Escape => "Escape",
        egui::Key::Tab => "Tab",
        egui::Key::Backspace => "Backspace",
        egui::Key::Delete => "Delete",
        egui::Key::Insert => "Insert",
        egui::Key::Home => "Home",
        egui::Key::End => "End",
        egui::Key::PageUp => "PageUp",
        egui::Key::PageDown => "PageDown",
        egui::Key::Comma => "Comma",
        egui::Key::Period => "Period",
        egui::Key::Slash => "Slash",
        egui::Key::Semicolon => "Semicolon",
        egui::Key::Quote => "Quote",
        egui::Key::OpenBracket => "OpenBracket",
        egui::Key::CloseBracket => "CloseBracket",
        egui::Key::Backslash => "Backslash",
        egui::Key::Backtick => "Backtick",
        egui::Key::Minus => "Minus",
        egui::Key::Equals => "Equals",
        egui::Key::Plus => "Plus",
        _ => "?",
    }
}

/// Settings label for an action id, for notices.
pub(crate) fn action_label(id: &str) -> &str {
    image_viewer::KEY_ACTIONS
        .iter()
        .find(|(a, _, _)| *a == id)
        .map(|(_, _, label)| *label)
        .unwrap_or(id)
}

/// Button text with the live chord: `("Save transformed…", "save")` gives
/// `"Save transformed… (Ctrl+S)"` today and `"Save transformed… (X)"` after
/// a rebind. Buttons never lie about keys, however they are rebound.
pub(crate) fn action_button_text(config: &image_viewer::Config, base: &str, action: &str) -> String {
    format!(
        "{base} ({})",
        image_viewer::resolve_binding(&config.keybindings, action)
    )
}
#[derive(Clone, Copy)]
/// A resolved binding: the key plus the exact modifier set. Matching is
/// exact on both: a bare letter never fires with a modifier held, and a
/// chord never fires with extra ones.
pub(crate) struct ResolvedBinding {
    key: egui::Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
}

/// Resolve one action to a chord: the user's override, else the shipped
/// default. An unparseable override falls back to the default rather than
/// unbinding the action — a typo must leave the shipped chord working, not
/// delete the feature. Returns `None` only when even the default will not
/// parse, which cannot happen for shipped rows.
pub(crate) fn resolve_action(
    bindings: &std::collections::HashMap<String, String>,
    action: &str,
) -> Option<ResolvedBinding> {
    for candidate in [
        image_viewer::resolve_binding(bindings, action),
        image_viewer::default_binding(action).to_owned(),
    ] {
        if let Some((ctrl, shift, alt, name)) = image_viewer::split_chord(&candidate) {
            if let Some(key) = parse_key_full(&name) {
                return Some(ResolvedBinding {
                    key,
                    ctrl,
                    shift,
                    alt,
                });
            }
        }
    }
    None
}

/// Resolve every action once per frame: shipped defaults with user
/// overrides applied. Owns everything, so callers keep no borrow on the
/// config across the `&mut self` dispatch that follows.
pub(crate) fn build_bindings(config: &image_viewer::Config) -> std::collections::HashMap<String, ResolvedBinding> {
    let mut m = std::collections::HashMap::new();
    for (id, _, _) in image_viewer::KEY_ACTIONS {
        if let Some(b) = resolve_action(&config.keybindings, id) {
            m.insert((*id).to_owned(), b);
        }
    }
    m
}

/// Did `action`'s chord fire: the key pressed this frame with exactly the
/// resolved modifiers. One predicate for the whole dispatch, so every site
/// below agrees on what a binding means.
pub(crate) fn action_pressed(
    ctx: &egui::Context,
    bindings: &std::collections::HashMap<String, ResolvedBinding>,
    action: &str,
) -> bool {
    ctx.input(|i| binding_down(i, bindings, action))
}

/// Same question against an already-open input state, for the tuples that
/// read several keys inside one `ctx.input` closure (nesting `ctx.input`
/// calls is not something to rely on).
pub(crate) fn binding_down(
    i: &egui::InputState,
    bindings: &std::collections::HashMap<String, ResolvedBinding>,
    action: &str,
) -> bool {
    let Some(b) = bindings.get(action) else {
        return false;
    };
    i.key_pressed(b.key) && i.modifiers.ctrl == b.ctrl && i.modifiers.shift == b.shift && i.modifiers.alt == b.alt
}

/// Physical presses only: matches raw key events with `repeat: false` and
/// the event's own modifiers. Spawning (user commands), dialogs (print),
/// and clipboard writes must be edge-triggered — `key_pressed` also fires
/// for OS key-repeat, and a held Ctrl+1 once opened hundreds of Paint
/// windows. View navigation deliberately does NOT use this: holding an
/// arrow to flip through is the feature, not the bug.
pub(crate) fn action_pressed_fresh(
    ctx: &egui::Context,
    bindings: &std::collections::HashMap<String, ResolvedBinding>,
    action: &str,
) -> bool {
    let Some(b) = bindings.get(action) else {
        return false;
    };
    ctx.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Key {
                    key: k,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } if *k == b.key
                    && modifiers.ctrl == b.ctrl
                    && modifiers.shift == b.shift
                    && modifiers.alt == b.alt
            )
        })
    })
}

/// Effective navigation table: same meanings as `NAV_KEYS`, keys (and any
/// modifiers) replaced where the user rebound the action, read from the full
/// keybindings map. A process global like the resample and thumbnail-cache
/// flags, for the same reason: `nav_direction` is called from the input
/// closures and takes only predicates, so the table it reads has to live
/// somewhere it can reach without changing every caller's shape.
pub(crate) static NAV_TABLE: std::sync::LazyLock<
    std::sync::Mutex<Vec<(ResolvedBinding, NavInput)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Rebuild the navigation table from the keybindings map. `resolve_action`
/// falls back to the shipped chord internally, so an unparseable override
/// keeps the default key rather than unbinding navigation.
pub(crate) fn set_nav_overrides(bindings: &std::collections::HashMap<String, String>) {
    let mut table = Vec::new();
    for (action, meaning) in NAV_REBINDABLE {
        if let Some(b) = resolve_action(bindings, action) {
            table.push((b, *meaning));
        }
    }
    *NAV_TABLE.lock().unwrap() = table;
}

/// The navigation table with user overrides applied: the global when set,
/// else the shipped `NAV_KEYS` with no modifiers (startup always sets it,
/// so the fallback is belt and braces for contexts without a config).
pub(crate) fn effective_nav_keys() -> Vec<(ResolvedBinding, NavInput)> {
    let table = NAV_TABLE.lock().unwrap();
    if table.is_empty() {
        return NAV_KEYS
            .iter()
            .map(|(key, meaning)| {
                (
                    ResolvedBinding {
                        key: *key,
                        ctrl: false,
                        shift: false,
                        alt: false,
                    },
                    *meaning,
                )
            })
            .collect();
    }
    table.clone()
}

/// How many images on each side of the current one get their preview preloaded.
pub(crate) const PRELOAD_RADIUS: usize = 3;

/// The preload window's whole budget in **file bytes** — what the watcher's
/// stamp registry already holds, so the cost is known before any decode.
/// Chosen so that every ordinary folder spends nothing new: radius 5 plans
/// ten neighbours, and 10 × 24 MB photographs — the largest folder a photo
/// library brings — sits under the budget, so the walk on it plans exactly
/// what it always did. What it removes is the tail: a 200-500 MB scan is past
/// the whole budget, is never preloaded, and costs its decode the moment it
/// is looked at instead. Notes: the current image takes no part in the
/// budget, so the budget never removes the picture the reader sees; and a
/// skip is not a hole — smaller neighbours behind a large one are still
/// planned, by distance first.
pub(crate) const PRELOAD_BUDGET_BYTES: u64 = 256 * 1024 * 1024;

/// How long to wait for a follow-up frame after the display size changed, to
/// decide whether a drag has stopped. One frame is enough — the test is whether
/// the size is unchanged since the previous frame, not how long it has been.
pub(crate) const RESIZE_SETTLE_REPAINT: std::time::Duration = std::time::Duration::from_millis(16);

/// Source size for the window icon, in pixels. winit scales this to whatever the
/// shell asks for, so this only sets the ceiling: 256 is the largest size
/// `icon::render` produces for the `.ico`, and a taskbar button wants 24-32 even
/// on a HiDPI display, so there is nothing to gain from generating larger.
pub(crate) const WINDOW_ICON_PX: u32 = 256;

/// Max cached preview textures (each is at most ~8192 px wide, typically ~4K).
pub(crate) const PREVIEW_CACHE_CAP: usize = 16;
/// Full-res tile edge in source pixels.
pub(crate) const TILE: u32 = 1024;
/// Max tiles converted+uploaded per frame (keeps frames smooth while zooming).
pub(crate) const TILES_PER_FRAME: usize = 8;
/// Max full-res tile textures kept on the GPU (64 x 4 MB = 256 MB).
pub(crate) const MAX_TILES: usize = 64;
/// Wait this long on an image before starting its full-res decode.
pub(crate) const FULL_DWELL: Duration = Duration::from_millis(120);
/// Minimum time between two external process spawns (user commands,
/// Explorer). The second layer under the repeat-ignoring chords: a held
/// Ctrl+1 once spawned one Paint per frame until the machine needed a hard
/// reset, and a rate limit bounds the damage to one window a second if any
/// spawn path ever fires repeatedly again. Legitimate use never notices —
/// nobody runs two external programs within a second on purpose.
pub(crate) const SPAWN_COOLDOWN: Duration = Duration::from_secs(1);
/// The **fallback** folder poll: used only when the OS folder watch could not
/// be started (an archive's temp dir, a network share that refuses
/// notifications). With a watch running, notifications are the only source and
/// this timer does not tick at all — see `maybe_rescan`.
pub(crate) const RESCAN_INTERVAL: Duration = Duration::from_secs(2);
/// Minimum time between two notification-driven rescans. One bulk copy emits
/// one event per file; they are coalesced per frame, but a folder being written
/// continuously would still ask for a scan every frame, which is worse than the
/// poll this replaced. A rescan that is owed is never dropped - it is deferred
/// and a repaint is scheduled for it - so the listing cannot end up stale.
pub(crate) const RESCAN_MIN_GAP: Duration = Duration::from_secs(1);
/// How long a file has to stop changing before it is re-decoded. A save is
/// often several writes, and the OS reports each one: acting on the first would
/// start a decode of a half-written file, then throw it away for the next
/// chunk, repeatedly. The clock restarts on every event for the path, so the
/// decode happens once, when the writer has finished - the same reasoning as
/// `FULL_DWELL`, and the same cost, which is a reload a fifth of a second after
/// the save rather than during it.
pub(crate) const CONTENT_SETTLE: Duration = Duration::from_millis(200);
/// Highest zoom, in screen points per source pixel.
pub(crate) const MAX_SCALE: f32 = 32.0;

// Job priorities (lower runs first) live in the library, next to `plan_jobs`,
// so the planner and its priorities cannot drift apart.

// Single-instance guard: `Config::single_instance` plus the mutex claim in
// `image_viewer::claim_single_instance` (tested there). The name is
// session-local: `Global\` needs a privilege a normal login does not have,
// and a `\\.\` device-path prefix is not an object name at all — creation
// failed on every launch and read as "already running".
pub(crate) const SINGLE_INSTANCE_MUTEX_NAME: &str = "Local\\image-viewer-single-instance-v1";

/// Returns `true` when this launch is the single allowed instance
/// (mutex created fresh). Returns `false` when a second instance
/// is already running — in which case this process should exit
/// rather than open another window (two instances fighting over an
/// archive's shared temp dir is one of the bugs this gate prevents).
pub(crate) fn hold_single_instance() -> bool {
    image_viewer::claim_single_instance(SINGLE_INSTANCE_MUTEX_NAME)
}

