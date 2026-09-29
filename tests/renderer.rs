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
fn auto_probes_dx12_first_and_vulkan_last() {
    // DX12 is the only combination verified to work on the development machine,
    // so it leads. Vulkan goes last because it is the one that access-violates.
    assert_eq!(PROBE_ORDER, [WgpuBackend::Dx12, WgpuBackend::Gl, WgpuBackend::Vulkan]);
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
    assert_eq!(pick_probed(&all_ok), WgpuBackend::Dx12);

    // DX12 unavailable: GL is the next in order, even though Vulkan also works.
    let no_dx12 = vec![
        (WgpuBackend::Dx12, false),
        (WgpuBackend::Gl, true),
        (WgpuBackend::Vulkan, true),
    ];
    assert_eq!(pick_probed(&no_dx12), WgpuBackend::Gl);

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
    assert_eq!(pick_probed(&reversed), WgpuBackend::Dx12);

    // Incomplete: DX12 was never tried, GL and Vulkan both report fine.
    let partial = vec![(WgpuBackend::Gl, true), (WgpuBackend::Vulkan, true)];
    assert_eq!(pick_probed(&partial), WgpuBackend::Gl);
}

#[test]
fn if_nothing_works_we_still_have_to_pick_something() {
    // A probe that fails for every backend means the probing itself is
    // untrustworthy — most likely the exe cannot be re-launched. Picking nothing
    // would leave `Backends` empty and fail in a far more confusing way, so the
    // least-bad answer is the one that has never been seen to fail.
    let none = vec![
        (WgpuBackend::Dx12, false),
        (WgpuBackend::Gl, false),
        (WgpuBackend::Vulkan, false),
    ];
    assert_eq!(pick_probed(&none), WgpuBackend::Dx12);
    assert_eq!(pick_probed(&[]), WgpuBackend::Dx12);
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
    // And the bit values must be the ones wgpu actually defines, or the
    // `debug_assert` in main.rs is the only thing standing between us and a
    // silent "no adapter found".
    assert_eq!(image_viewer::wgpu_backend_set(WgpuBackend::Vulkan), 1 << 0);
    assert_eq!(image_viewer::wgpu_backend_set(WgpuBackend::Gl), 1 << 1);
    assert_eq!(image_viewer::wgpu_backend_set(WgpuBackend::Dx12), 1 << 3);
    // `Auto` is resolved before it gets here, but must not produce an empty set.
    assert_eq!(image_viewer::wgpu_backend_set(WgpuBackend::Auto), 1 << 3);
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
