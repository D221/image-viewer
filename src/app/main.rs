//! Module `app_main`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Where a running app leaves "restart me with this backend" on its way out.
///
/// A `run_native` loop cannot see inside the `App` it is handed, and a hard
/// crash is not something to recover from in-process, so the only two channels
/// available are a process exit code (too coarse) and shared state. This is the
/// shared state.
pub(crate) type RestartSlot = std::sync::Arc<std::sync::Mutex<Option<image_viewer::WgpuBackend>>>;

/// wgpu configuration, pinned to exactly one backend.
///
/// **One backend per process, and that is not a simplification — it is the whole
/// design.** wgpu enumerates every backend in the set during adapter discovery,
/// *before* it has chosen anything, and on some drivers that enumeration is a
/// hard access violation (`0xC0000005`) before any window exists. Measured, not
/// assumed, on the development machine:
///
/// - `vulkaninfo` reports a healthy device — Intel UHD 620, API 1.2.187. So
///   **Vulkan itself works.**
/// - wgpu with `VULKAN | DX12` access-violates, 3/3 trials, nothing on stderr.
/// - wgpu with `DX12` alone starts every time.
///
/// So "try DX12, and fall back to Vulkan if it fails" cannot be written in one
/// process: enabling Vulkan is what kills it. Hence the probe in a *child*
/// process, which is the only place a hard crash can be survived. See
/// `probe_backends`.
///
/// A registry probe for `Khronos\Vulkan\Drivers` was tried and is a false signal
/// in both directions — the key is *absent* on this machine while Vulkan works
/// (the loader finds the Intel ICD with no `Drivers` key and no `VK_*` variables
/// set), and a key being present does not mean wgpu can use the driver behind it.
///
/// The chosen backend is printed so a crash is never a mystery.
#[allow(dead_code)]
pub(crate) fn wgpu_options(
    backend: image_viewer::WgpuBackend,
) -> eframe::egui_wgpu::WgpuConfiguration {
    use eframe::egui_wgpu::{
        WgpuSetup, WgpuSetupCreateNew,
        wgpu::{Backends, InstanceDescriptor},
    };

    // The bit values live in `lib.rs` so the policy is testable without pulling
    // wgpu into the library; the conversion to the real flags happens here.
    let bits = image_viewer::wgpu_backend_set(backend);
    let backends = match backend {
        image_viewer::WgpuBackend::Gl => Backends::GL,
        image_viewer::WgpuBackend::Vulkan => Backends::VULKAN,
        // `Auto` is resolved before it gets here, and falls back to DX12.
        _ => Backends::DX12,
    };
    debug_assert_eq!(bits, backends.bits(), "backend bit value drifted from lib.rs");

    let mut instance = InstanceDescriptor::new_without_display_handle();
    instance.backends = backends;
    eprintln!("wgpu backend: {backends:?} ({} requested)", backend.name());

    eframe::egui_wgpu::WgpuConfiguration {
        // `surface` is deliberately left at the default (HIGH_THROUGHPUT, frame
        // latency 2). `SurfaceConfig::LOW_LATENCY` is documented as the right
        // choice for a GUI with little GPU work, which is this app, so it looks
        // like a free improvement — but it was tried and could not be shown to be
        // one. Don't flip it on the strength of the docs: re-measure on a *local
        // console* session first. An earlier attempt at this produced a confident
        // wrong answer by measuring over RDP.
        wgpu_setup: WgpuSetup::CreateNew(WgpuSetupCreateNew {
            instance_descriptor: instance,
            ..WgpuSetupCreateNew::without_display_handle()
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Backend probing
// ---------------------------------------------------------------------------

/// Probe one backend, in *this* process. Prints a verdict and returns the exit
/// code the parent reads.
///
/// This is the child half of [`probe_backends`]. It does the minimum that
/// actually crashes: create the instance and enumerate adapters. No window, no
/// surface, no app — because a failure later than adapter enumeration would not
/// tell us anything useful about the driver, and a window would flash.
pub(crate) fn probe_one(backend: image_viewer::WgpuBackend) -> i32 {
    use eframe::egui_wgpu::wgpu::{
        Backends, DeviceDescriptor, Instance, InstanceDescriptor, RequestAdapterOptions,
    };

    let backends = match backend {
        image_viewer::WgpuBackend::Gl => Backends::GL,
        image_viewer::WgpuBackend::Vulkan => Backends::VULKAN,
        _ => Backends::DX12,
    };
    let mut desc = InstanceDescriptor::new_without_display_handle();
    desc.backends = backends;

    // As far as the driver as possible, and no further: instance, adapter, then
    // **a device**. Enumerating adapters alone is not enough and this is not
    // hypothetical — the first version of this probe stopped at enumeration and
    // reported Vulkan as working, on a machine where starting the app on Vulkan
    // access-violates. Enumeration walks the loader's list; the crash is further
    // in, where the driver is actually initialised.
    //
    // A surface is deliberately not created: that needs a window, and a probe
    // that flashes a window is worse than one that is slightly less thorough.
    // The consequence is real and worth stating — a backend that fails only on
    // surface creation will still pass this probe.
    let verdict = pollster::block_on(async {
        let instance = Instance::new(desc);
        let adapter = match instance.request_adapter(&RequestAdapterOptions::default()).await {
            Ok(a) => a,
            Err(e) => return Err(format!("no adapter: {e}")),
        };
        let info = adapter.get_info();
        match adapter
            .request_device(&DeviceDescriptor {
                label: None,
                required_features: eframe::egui_wgpu::wgpu::Features::empty(),
                // Downlevel defaults so a weak or software adapter is not
                // rejected for limits this app never uses.
                required_limits: eframe::egui_wgpu::wgpu::Limits::downlevel_defaults(),
                experimental_features: eframe::egui_wgpu::wgpu::ExperimentalFeatures::disabled(),
                memory_hints: eframe::egui_wgpu::wgpu::MemoryHints::Performance,
                trace: eframe::egui_wgpu::wgpu::Trace::Off,
            })
            .await
        {
            Ok(_) => Ok(format!("{} / {}", info.name, info.driver)),
            Err(e) => Err(format!("no device: {e}")),
        }
    });

    match verdict {
        Ok(what) => {
            println!("ok {} {what}", backend.name());
            0
        }
        Err(why) => {
            println!("fail {} {why}", backend.name());
            1
        }
    }
}

/// Probe each candidate in its own process and return what worked.
///
/// A backend that *crashes* is contained by the child: the parent sees an exit
/// code that is not 0 and moves on. That is the only mechanism that works here,
/// because the failure is an access violation rather than an error return, and a
/// process that has access-violated cannot catch its own way out of it.
pub(crate) fn probe_backends() -> Vec<(image_viewer::WgpuBackend, bool)> {
    let Ok(exe) = std::env::current_exe() else {
        return Vec::new();
    };
    let mut results = Vec::new();
    for &backend in &image_viewer::PROBE_ORDER {
        let out = std::process::Command::new(&exe)
            .arg("--probe-renderer")
            .arg(backend.name())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // A missing exe, or a non-zero exit for any reason including a
        // crash, both count as "this one does not work".
        let ok = matches!(out, Ok(s) if s.success());
        results.push((backend, ok));
        // **Stop at the first that works.** The other backends exist only to be
        // tried when this one fails, so probing past a success is work whose
        // result is thrown away — and it is not cheap. Probing all three cost
        // ~1456 ms of the first launch, because adapter + device creation is
        // 186 ms on GL, 455 ms on DX12 and 474 ms on Vulkan, run one after
        // another, and GL leads `PROBE_ORDER`, so almost every first launch was
        // paying for two backends it was never going to use.
        //
        // This changes no behaviour that matters, and the reason is a property
        // rather than an assertion: a shortened list contains at most one
        // success — everything before the first success is a failure by
        // construction — so the entries that were never probed cannot change what
        // `pick_probed` decides. Checked exhaustively by
        // `a_probe_that_stopped_at_the_first_success_decides_the_same_way`.
        //
        // The insurance is untouched: a machine where GL is unavailable still
        // falls through to DX12, then Vulkan.
        if ok {
            break;
        }
    }
    results
}

/// Resolve the backend to use at launch, probing only if the preference needs
/// it. Returns the backend and, if a probe ran and found something, the answer
/// to cache in the config.
#[allow(dead_code)]
pub(crate) fn resolve_renderer(
    pref: &image_viewer::RendererPref,
) -> (image_viewer::WgpuBackend, Option<image_viewer::WgpuBackend>) {
    if !pref.needs_probe() {
        return (pref.resolve(None), None);
    }
    eprintln!("Probing for a working graphics backend...");
    let results = probe_backends();
    for (b, ok) in &results {
        eprintln!("  {:>7}: {}", b.name(), if *ok { "works" } else { "unavailable" });
    }
    let picked = image_viewer::pick_probed(&results);
    (picked, Some(picked))
}

// ---------------------------------------------------------------------------
// Background loader: a tiny priority queue served by a few worker threads
// ---------------------------------------------------------------------------
//
// `Kind`, `Job` and `Loader` live in the library (see `lib.rs`, "Load
// scheduling") so the queue can be tested directly. The stranding bug that
// emptied this queue lived in the interaction between the plan and the queue,
// and neither half could be reached from a test while it lived in a binary.

pub(crate) const PRIO_THUMB: u32 = PRIO_THUMB_BASE;
pub(crate) const PRIO_EXIF: u32 = PRIO_CURRENT_EXIF;

/// Long side of grid thumbnails.
pub(crate) const THUMB_DIM: u32 = 256;
/// Max grid thumbnails kept on the GPU (256 x ~0.3 MB ≈ 80 MB).
pub(crate) const THUMB_CACHE_CAP: usize = 512;
/// Decode attempts per file before a grid cell gives up (permanent
/// placeholder). Enough for transient failures, bounded so a corrupt file
/// can't keep the worker pool busy.
pub(crate) const THUMB_TRIES: u8 = 3;
