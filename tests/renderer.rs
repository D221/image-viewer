//! The renderer's decision logic.
//!
//! What can be tested here is the *policy*, not the probing: a probe is a child
//! process that either starts or crashes, and a crash cannot be unit-tested. So
//! the probing is reduced to a list of `(backend, survived)` pairs and the
//! interesting function is which one gets picked from it.

use image_viewer::{pick_probed, path_arg, RendererPref, WgpuBackend, PROBE_ORDER};
use std::path::PathBuf;

fn pref(choice: WgpuBackend, probed: Option<WgpuBackend>) -> RendererPref {
    RendererPref { choice, probed }
}

#[test]
fn there_is_no_dx11_option_and_the_reason_is_recorded() {
    // The honest answer to "dx11(?)": wgpu removed its Direct3D 11 backend
    // upstream, so it is not an option we declined to implement — it does not
    // exist in the API. `parse` must therefore not invent one, and the doc must
    // say why rather than leaving the question open again next time.
    assert_eq!(WgpuBackend::parse("dx11"), None);
    assert_eq!(WgpuBackend::parse("d3d11"), None);
    // And it is absent from the list the UI offers, not merely unparseable.
    assert_eq!(WgpuBackend::CHOICES.len(), 3, "a fourth backend appeared");
    assert!(
        !WgpuBackend::CHOICES.iter().any(|b| b.name().contains("11")),
        "a Direct3D 11 backend was added, which wgpu does not have"
    );
}

#[test]
fn every_backend_round_trips_through_its_name() {
    // The name is what goes in the config file and on the command line, so a
    // backend that does not survive the round trip cannot be persisted.
    for b in WgpuBackend::CHOICES {
        assert_eq!(WgpuBackend::parse(b.name()), Some(b), "{} did not round trip", b.name());
    }
    // And the aliases people will actually type.
    assert_eq!(WgpuBackend::parse("DX12"), Some(WgpuBackend::Dx12));
    assert_eq!(WgpuBackend::parse("d3d12"), Some(WgpuBackend::Dx12));
    assert_eq!(WgpuBackend::parse("OpenGL"), Some(WgpuBackend::Gl));
    assert_eq!(WgpuBackend::parse(" vk "), Some(WgpuBackend::Vulkan));
    assert_eq!(WgpuBackend::parse("nonsense"), None);
}

#[test]
fn auto_probes_gl_first_and_vulkan_last() {
    // GL leads because it is measurably the cheapest backend on this machine: a
    // bare eframe window is 321 MB on DX12 against 84 MB on GL, and adapter +
    // device creation is 455 ms against 186 ms. See `PROBE_ORDER` for the table and
    // for the caveat that frame rate was *not* measured — this is a one-line
    // reversal if a console session says otherwise.
    //
    // Vulkan goes last because it is the one that access-violates.
    assert_eq!(PROBE_ORDER, [WgpuBackend::Gl, WgpuBackend::Dx12, WgpuBackend::Vulkan]);
    // The probe order and the choice list must not drift apart, or "Automatic"
    // would quietly stop offering something the user can pick by hand.
    for b in WgpuBackend::CHOICES {
        assert!(
            PROBE_ORDER.contains(&b),
            "{} is offered in the dropdown but never probed",
            b.name()
        );
    }
    assert_eq!(PROBE_ORDER.len(), WgpuBackend::CHOICES.len());
}

#[test]
fn the_first_surviving_probe_wins() {
    let all_ok = vec![
        (WgpuBackend::Dx12, true),
        (WgpuBackend::Gl, true),
        (WgpuBackend::Vulkan, true),
    ];
    assert_eq!(pick_probed(&all_ok), WgpuBackend::Gl, "GL leads: it is the cheapest to create");

    // GL unavailable: DX12 is next in order, even though Vulkan also works.
    let no_gl = vec![
        (WgpuBackend::Dx12, true),
        (WgpuBackend::Gl, false),
        (WgpuBackend::Vulkan, true),
    ];
    assert_eq!(pick_probed(&no_gl), WgpuBackend::Dx12);

    // Only Vulkan works. This is the case the whole design exists for.
    let only_vulkan = vec![
        (WgpuBackend::Dx12, false),
        (WgpuBackend::Gl, false),
        (WgpuBackend::Vulkan, true),
    ];
    assert_eq!(pick_probed(&only_vulkan), WgpuBackend::Vulkan);
}

#[test]
fn the_probe_order_beats_the_order_the_results_arrive_in() {
    // The parent runs the probes in `PROBE_ORDER` anyway, but a crash makes the
    // parent *skip* entries, so the list it ends up with can be out of order or
    // incomplete. Preference must come from PROBE_ORDER, not from the input.
    let reversed = vec![
        (WgpuBackend::Vulkan, true),
        (WgpuBackend::Gl, true),
        (WgpuBackend::Dx12, true),
    ];
    assert_eq!(pick_probed(&reversed), WgpuBackend::Gl);

    // Incomplete: GL was never tried, DX12 and Vulkan both report fine.
    let partial = vec![(WgpuBackend::Dx12, true), (WgpuBackend::Vulkan, true)];
    assert_eq!(pick_probed(&partial), WgpuBackend::Dx12);
}

#[test]
fn if_nothing_works_we_still_have_to_pick_something() {
    // A probe that fails for every backend means the probing itself is
    // untrustworthy — most likely the exe cannot be re-launched. Picking nothing
    // would leave `Backends` empty and fail in a far more confusing way, so the
    // least-bad answer is the cheapest one: same reasoning as `PROBE_ORDER`.
    let none = vec![
        (WgpuBackend::Dx12, false),
        (WgpuBackend::Gl, false),
        (WgpuBackend::Vulkan, false),
    ];
    assert_eq!(pick_probed(&none), WgpuBackend::Gl);
    assert_eq!(pick_probed(&[]), WgpuBackend::Gl);
}

/// The justification for the probe stopping at the first backend that works.
///
/// Probing all three cost ~1456 ms of the first launch, because adapter + device
/// creation is 186 ms on GL, 455 ms on DX12 and 474 ms on Vulkan, run one after
/// another — and the two after the winner are work whose result is thrown away.
/// So `probe_backends` shortens the list, and this checks that doing so cannot
/// change the answer, for **every** combination of which backends work rather than
/// a couple of hand-picked cases.
///
/// What this deliberately does *not* check, having been tried: that the decision is
/// insensitive to the order results arrive in. A shortened list contains at most
/// one `true` — everything before the first success is a failure, by construction —
/// so there is no ordering for `pick_probed` to get wrong, and the check passed
/// even with `pick_probed` rewritten to trust arrival order. It was decoration, and
/// proving that is why it is not here. Arrival order is covered where it can
/// actually fail, by `the_probe_order_beats_the_order_the_results_arrive_in`.
///
/// The invariant is "the list ends **at** the first success", not "any prefix
/// works". A prefix of nothing but failures is a list the probe can never produce,
/// and truncating to one *does* change the answer — that was the first version of
/// this test, and its failure looked like a bug in `pick_probed` and was not.
///
/// `probe_backends` itself is a binary-side function and cannot be reached from
/// here, so the early exit itself is pinned against the source text by
/// `the_probe_stops_at_the_first_backend_that_works`.
#[test]
fn a_probe_that_stopped_at_the_first_success_decides_the_same_way() {
    for mask in 0u8..8 {
        let full: Vec<(WgpuBackend, bool)> = PROBE_ORDER
            .iter()
            .map(|&b| (b, mask & bit(b) != 0))
            .collect();
        let expected = pick_probed(&full);

        let stop = full
            .iter()
            .position(|(_, ok)| *ok)
            .map(|i| i + 1)
            .unwrap_or(full.len());
        let short = full[..stop].to_vec();
        assert_eq!(
            pick_probed(&short),
            expected,
            "shortened {short:?} decided differently from full {full:?}"
        );
    }
}

/// The early exit in `probe_backends`, asserted against `src/main.rs` as text.
///
/// Normally a weak thing to do, and used here for the same reason
/// `tests/keybindings.rs` uses it: the function is in a binary, so the alternative
/// is that "the probe still runs all three backends" is unobservable. It cost
/// ~1456 ms of the first launch once already, and nothing about the change that
/// fixed it would fail a test otherwise — the shortened list decides the same way
/// by construction, which is what makes the optimisation invisible to the rest of
/// the suite.
///
/// Checking the text rather than the behaviour is what makes the claim checkable at
/// all: a reordering or a `.map().collect()` instead of the loop is a diff, and a
/// diff is the thing a reviewer reads.
#[test]
fn the_probe_stops_at_the_first_backend_that_works() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"),
    )
    .expect("src/main.rs");

    let body = src
        .split("fn probe_backends()")
        .nth(1)
        .expect("probe_backends is gone; this test needs updating")
        .split("\nfn ")
        .next()
        .expect("could not delimit the function body");

    assert!(
        body.contains("if ok {") && body.contains("break;"),
        "probe_backends no longer breaks on the first success.\n\
         It runs one child process per backend, and adapter + device creation is \
         186 ms (GL), 455 ms (DX12), 474 ms (Vulkan) - so probing all three costs \
         ~1456 ms of the first launch and the losers' results are discarded.\n\
         Body was:\n{body}"
    );
    // The loop form, not an iterator: an iterator cannot stop early without
    // something like `take_while`, and the rewrite would be easy to miss.
    assert!(
        body.contains("for &backend in &image_viewer::PROBE_ORDER"),
        "probe_backends no longer iterates PROBE_ORDER as a loop.\nBody was:\n{body}"
    );
}

/// The bit for a backend inside the `0u8..8` mask above.
fn bit(b: WgpuBackend) -> u8 {
    match b {
        WgpuBackend::Gl => 1,
        WgpuBackend::Dx12 => 2,
        WgpuBackend::Vulkan => 4,
        WgpuBackend::Auto => unreachable!("PROBE_ORDER must not contain Auto"),
    }
}

#[test]
fn an_explicit_choice_overrides_a_probe_and_a_cache() {
    // The user asked for a specific backend; nothing else may override that,
    // including a cached answer from before they had an opinion.
    let p = pref(WgpuBackend::Vulkan, Some(WgpuBackend::Dx12));
    assert_eq!(p.resolve(Some(WgpuBackend::Gl)), WgpuBackend::Vulkan);
    assert_eq!(p.resolve(None), WgpuBackend::Vulkan);
    assert!(!p.needs_probe());
}

#[test]
fn auto_reuses_a_cached_answer_instead_of_probing_every_launch() {
    // Probing spawns a child process per candidate. Doing that on every start
    // would be a poor trade for a question with a stable answer.
    let p = pref(WgpuBackend::Auto, Some(WgpuBackend::Gl));
    assert!(!p.needs_probe());
    assert_eq!(p.resolve(None), WgpuBackend::Gl);
    // A fresh probe still wins, so "detect again" is possible.
    assert_eq!(p.resolve(Some(WgpuBackend::Vulkan)), WgpuBackend::Vulkan);
}

#[test]
fn auto_with_no_cache_probes_and_falls_back_to_dx12() {
    let p = pref(WgpuBackend::Auto, None);
    assert!(p.needs_probe());
    assert_eq!(p.resolve(Some(WgpuBackend::Gl)), WgpuBackend::Gl);
    // Probe refused to run at all: DX12, so the app still starts.
    assert_eq!(p.resolve(None), WgpuBackend::Dx12);
    assert_eq!(RendererPref::default().choice, WgpuBackend::Auto);
    assert_eq!(RendererPref::default().probed, None);
}

#[test]
fn exactly_one_backend_bit_is_ever_set() {
    // The load-bearing property of the whole design. wgpu enumerates every
    // backend in the set before choosing an adapter, and on some drivers that
    // enumeration crashes the process — so a "set" is not a fallback list, it is
    // just a way to crash. If this ever returns more than one bit, the
    // crash-avoidance is gone even though every other test still passes.
    for b in WgpuBackend::CHOICES {
        let bits = image_viewer::wgpu_backend_set(b);
        assert_eq!(
            bits.count_ones(),
            1,
            "{} enabled {} backends, not one",
            b.name(),
            bits.count_ones()
        );
    }
    // The bit values must be the ones *this wgpu* defines, or `main.rs` builds a
    // `Backends` flag that selects a backend the app never asked for.
    //
    // These were hardcoded as `1 << 0`, `1 << 1`, `1 << 3` and asserted against
    // those same literals — which is a test that cannot fail. wgpu 30 renumbered
    // `Backend` (`Noop` was inserted, so `Gl` moved from bit 1 to bit 4), the
    // real values drifted, and `cargo run` died at startup on the
    // `debug_assert_eq!` in `wgpu_options`. The release build was fine, because
    // the assert is compiled out and `main.rs` matches on the *enum* to pick the
    // flag — so the only build that noticed was the one nobody launches.
    //
    // Compared against wgpu's own constants now, so a future renumbering is a
    // test failure rather than a stale literal that agrees with itself.
    use eframe::egui_wgpu::wgpu::Backends;
    assert_eq!(
        image_viewer::wgpu_backend_set(WgpuBackend::Vulkan),
        Backends::VULKAN.bits(),
        "Vulkan bit drifted from wgpu"
    );
    assert_eq!(
        image_viewer::wgpu_backend_set(WgpuBackend::Gl),
        Backends::GL.bits(),
        "GL bit drifted from wgpu"
    );
    assert_eq!(
        image_viewer::wgpu_backend_set(WgpuBackend::Dx12),
        Backends::DX12.bits(),
        "DX12 bit drifted from wgpu"
    );
    // `Auto` is resolved before it gets here, but must not produce an empty set.
    assert_eq!(
        image_viewer::wgpu_backend_set(WgpuBackend::Auto),
        Backends::DX12.bits()
    );
}

#[test]
fn a_flags_value_is_not_mistaken_for_the_folder() {
    // The `--perf <folder>` bug, which silently opened the persisted folder
    // instead of the requested one. `--probe-renderer <name>` has the same shape
    // and would do the same thing: `dx12` does not start with a dash, so a plain
    // "first non-flag argument" test hands it back as the path.
    assert_eq!(
        path_arg(["--probe-renderer", "dx12"]),
        None,
        "the probe's backend name was taken as the folder"
    );
    assert_eq!(
        path_arg(["--probe-renderer", "vulkan", "C:\\photos"]),
        Some(PathBuf::from("C:\\photos")),
        "a real path after the flag was lost"
    );
    // `--perf` takes no value, so its folder is still found.
    assert_eq!(path_arg(["--perf", "C:\\photos"]), Some(PathBuf::from("C:\\photos")));
    // Unchanged behaviour for the plain cases.
    assert_eq!(path_arg(["C:\\photos"]), Some(PathBuf::from("C:\\photos")));
    assert_eq!(path_arg(["--register"]), None);
    assert_eq!(path_arg(["--probe-renderer"]), None, "a dangling flag value slot");
    assert_eq!(path_arg(["--probe-renderer", "gl", "--perf"]), None);
}

#[test]
fn the_config_file_uses_the_same_spellings_as_parse_and_name() {
    // Caught by reading a config the app had just written: serde's derived
    // serialisation used the *variant* name ("Dx12") while `name()` and `parse()`
    // used "dx12". The file therefore could not be read back by anyone who
    // edited it to the documented spelling, and the setting had two names in one
    // place. `parse` is tested above; this tests the path that actually persists.
    let p = pref(WgpuBackend::Vulkan, Some(WgpuBackend::Gl));
    let json = serde_json::to_string(&p).unwrap();
    assert!(
        json.contains("\"vulkan\"") && json.contains("\"gl\""),
        "config file did not use the lowercase names: {json}"
    );
    assert!(
        !json.contains("Vulkan") && !json.contains("Dx12"),
        "config file leaked a Rust variant name: {json}"
    );
    // And it must read back.
    let back: RendererPref = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);

    // The specific case that would have broken: a hand-edited file using the
    // documented spelling.
    let hand = r#"{"choice":"dx12","probed":null}"#;
    assert_eq!(
        serde_json::from_str::<RendererPref>(hand).unwrap().choice,
        WgpuBackend::Dx12,
        "a hand-edited config using the documented name would not load"
    );
}

#[test]
fn an_old_config_without_a_renderer_setting_still_loads() {
    // Every other field here is `#[serde(default)]` for exactly this reason, and
    // a missing key must not stop the app starting.
    let old = r#"{
        "load_all_on_startup": true,
        "show_image_before_auto_fit": true,
        "folder": "C:\\photos",
        "sort": "name",
        "linear_resample": true,
        "downscale_filter": "Hermite",
        "block_while_loading": false,
        "wheel_navigation": false,
        "loop_back": true,
        "last_by_folder": {}
    }"#;
    let c: image_viewer::Config = serde_json::from_str(old).expect("an old config must load");
    assert_eq!(c.renderer.choice, WgpuBackend::Auto);
    assert_eq!(c.renderer.probed, None);
}
