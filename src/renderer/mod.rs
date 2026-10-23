//! Module `renderer`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use renderer::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Which wgpu backends to enable at instance-creation time.
///
/// wgpu has no "try this, else that": `WgpuSetup::native_adapter_selector`
/// still filters enumeration by `InstanceDescriptor::backends` first, so every
/// enabled backend is *probed* before one is chosen, and the probe is a hard
/// crash (`0xC0000005`) when wgpu's Vulkan backend cannot cope with a driver.
///
/// That crash is why this enum exists at all, and it is also why `Auto` cannot
/// be smarter. Measured on this machine: `vulkaninfo` reports a healthy
/// Intel UHD 620 device at API 1.2.187, and wgpu + Vulkan still access-violates
/// every single time. So "is Vulkan installed" is the wrong question — Vulkan
/// working does not imply wgpu-on-Vulkan working, and the only way to find out
/// is to crash.
/// `rename_all = "lowercase"` is load-bearing, not tidiness. Without it serde
/// writes and expects `"Dx12"`, while [`WgpuBackend::name`] and
/// [`WgpuBackend::parse`] — the two functions everything else goes through —
/// use `"dx12"`. The result is a config file that this program cannot read back
/// if a person edits it to the name the documentation says, and two spellings of
/// the same setting in the same file. This was caught by reading a config the
/// app had just written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WgpuBackend {
    /// Probe, and use the first backend that survives. See [`PROBE_ORDER`].
    Auto,
    /// DirectX 12. The Windows native backend, and the only combination verified
    /// to work here.
    Dx12,
    /// OpenGL, through wgpu's GLES backend.
    Gl,
    /// Vulkan.
    Vulkan,
}

impl WgpuBackend {
    /// Every backend, in the order a human would pick them off the list. Excludes
    /// `Auto`, which is not a backend.
    pub const CHOICES: [WgpuBackend; 3] = [Self::Dx12, Self::Gl, Self::Vulkan];

    /// Parse a backend choice, case-insensitively. `None` for anything
    /// unrecognised, so a typo falls back to the default rather than picking
    /// something surprising.
    ///
    /// **There is no DX11 option and there cannot be one.** wgpu removed its
    /// Direct3D 11 backend upstream (it is absent from `Backends` entirely by
    /// wgpu 22), so `dx11` here is a typo that correctly resolves to nothing.
    /// Windows exposes DX12 and OpenGL to wgpu, and those are the choices.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(Self::Auto),
            "vulkan" | "vk" => Some(Self::Vulkan),
            "dx12" | "d3d12" => Some(Self::Dx12),
            "gl" | "opengl" | "gles" => Some(Self::Gl),
            _ => None,
        }
    }

    /// The stable name used in the config file and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dx12 => "dx12",
            Self::Gl => "gl",
            Self::Vulkan => "vulkan",
        }
    }

    /// The label shown in the settings dropdown.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatic (probe for a working one)",
            Self::Dx12 => "DirectX 12",
            Self::Gl => "OpenGL",
            Self::Vulkan => "Vulkan",
        }
    }
}

/// The order `Auto` probes in, most-likely-to-work first.
///
/// GL leads because it is measurably the cheaper backend on this machine.
///
/// The order was DX12, then GL, on the reasonable grounds that DX12 is the native
/// Windows backend. Then it was measured, with `examples/bare_eframe.rs` as the
/// control — an eframe window and a label, no viewer code at all, so the number is
/// the renderer's and not ours:
///
/// | | working set | private |
/// |---|---|---|
/// | bare eframe 0.36, DX12 | 321 MB | 289 MB |
/// | bare eframe 0.36, GL | **84 MB** | **21 MB** |
/// | this viewer, DX12 | 343 MB | 312 MB |
/// | this viewer, GL | **189 MB** | 90 MB |
/// | Oculante 0.9.2 (glow, not wgpu) | 118 MB | 78 MB |
///
/// So 237 MB of the footprint is eframe's, and all of it is the backend. Adapter
/// plus device creation alone measures 455 ms on DX12 against 186 ms on GL.
///
/// **Frame rate is equal, which is what this reversal was missing.** It could not
/// be measured for two commits — frame rate over RDP is meaningless, and this
/// project has drawn two wrong conclusions from that before — and DX12 might well
/// have been faster. It is not: on a console session, interleaved A/B, four 20 s
/// rounds on an animation, **76.8 fps on both**, interval p50 16.6 ms on both, GL
/// 0.03% slower. Same speed, 154 MB less, 269 ms cheaper to create. See
/// `tests/bench_framerate.ps1`, which refuses to run over RDP.
///
/// **DX12 is second**, kept because it goes through a different driver stack and so
/// is the one most likely to work where GL does not — which is the entire reason
/// the other two still exist at all.
///
/// Vulkan is last *because it is the one that crashes*: on the development machine
/// wgpu access-violates during adapter enumeration 3 times out of 3 even though
/// `vulkaninfo` reports a healthy device. That is survivable here only because each
/// probe runs in its own process — see [`pick_probed`].
pub const PROBE_ORDER: [WgpuBackend; 3] =
    [WgpuBackend::Gl, WgpuBackend::Dx12, WgpuBackend::Vulkan];

/// The backend to use, given which probes survived.
///
/// `results` is `(backend, survived)` in probe order. The first survivor wins.
/// If nothing survived, GL — the same reasoning as [`PROBE_ORDER`]: it is the
/// cheapest to create and the lightest, so when the probes were themselves
/// untrustworthy the least-bad answer is the one that costs least if it is wrong.
///
/// This is a pure function of the probe results, which is the only reason it can
/// be tested — the probing itself cannot, because a failure is a crash.
pub fn pick_probed(results: &[(WgpuBackend, bool)]) -> WgpuBackend {
    for &want in &PROBE_ORDER {
        if results.iter().any(|(b, ok)| *b == want && *ok) {
            return want;
        }
    }
    WgpuBackend::Gl
}

/// How the user chose the renderer, and what the last probe concluded.
///
/// Kept as two fields so `Auto` does not re-probe on every launch: probing
/// spawns a child process per candidate, which is a poor thing to do on every
/// start. `probed` is only a *cache of a probe* — an explicit choice ignores it
/// entirely, and if the machine's drivers change the user can clear it by
/// choosing a backend and switching back to Automatic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RendererPref {
    /// What the user picked.
    pub choice: WgpuBackend,
    /// The last backend a probe found working, reused to skip probing.
    pub probed: Option<WgpuBackend>,
}

impl Default for RendererPref {
    fn default() -> Self {
        Self {
            choice: WgpuBackend::Auto,
            probed: None,
        }
    }
}

impl RendererPref {
    /// The backend to actually use, given what a fresh probe found (or `None`
    /// if no probe was run).
    pub fn resolve(&self, fresh: Option<WgpuBackend>) -> WgpuBackend {
        match self.choice {
            WgpuBackend::Auto => fresh
                .or(self.probed)
                .unwrap_or(WgpuBackend::Dx12),
            other => other,
        }
    }

    /// Whether launching with this preference needs to probe, i.e. whether
    /// `Auto` is set and no usable answer is cached.
    pub fn needs_probe(&self) -> bool {
        self.choice == WgpuBackend::Auto && self.probed.is_none()
    }
}

/// The wgpu backend bits to enable for a resolved backend.
///
/// **Exactly one, always.** This is the whole reason the probe design exists:
/// wgpu enumerates every backend in the set *before* choosing an adapter, and on
/// some drivers that enumeration is a hard access violation before any window
/// exists. So a "try the others if this fails" set is not a fallback, it is just
/// a way to crash. One backend per process, and let the probe decide which.
///
/// `Auto` never reaches here — it is resolved first — but it maps to DX12 rather
/// than to an empty set, because an empty `Backends` would fail in a much more
/// confusing way.
pub fn wgpu_backend_set(requested: WgpuBackend) -> u32 {
    match requested {
        // Bit values are wgpu's `Backends` constants; `main.rs` converts to the
        // real type so this module does not have to depend on wgpu.
        //
        // **These are not `1 << n` in order of this enum, and the numbering is
        // wgpu's, not ours.** `Backends` is a bitflag over `wgpu::Backend`, which
        // in wgpu 30 is `Noop=0, Vulkan=1, Metal=2, Dx12=3, Gl=4,
        // BrowserWebGpu=5` — so `Gl` is bit 4, not bit 1. The old values were
        // written from an older wgpu and left as bare literals, and
        // `exactly_one_backend_bit_is_ever_set` asserted those same literals
        // rather than wgpu's, so the pair agreed with each other while both were
        // wrong and `cargo run` panicked on the `debug_assert_eq!` in
        // `wgpu_options`. `tests/renderer.rs` now compares against
        // `Backends::VULKAN/GL/DX12` directly; do not hand-transcribe these.
        WgpuBackend::Auto | WgpuBackend::Dx12 => 1 << 3, // DX12
        WgpuBackend::Gl => 1 << 4,                        // GL
        WgpuBackend::Vulkan => 1 << 1,                   // VULKAN
    }
}

/// The file or folder to open, taken from the command line: the first argument
/// that is not a flag.
///
/// This exists because taking `args().nth(1)` is wrong the moment a flag can
/// come first, and it fails *silently and expensively*: `--perf` is not a path,
/// so it fell through to the persisted/default folder and the app quietly
/// measured or displayed the wrong thing. That cost a wrong performance
/// conclusion before it was caught, because `--perf <folder>` is the exact
/// invocation AGENTS.md documents.
///
/// An argument is treated as a flag when it starts with `-`. On Windows that is
/// safe for real paths; if it ever is not, `.\-name.jpg` still works.
pub fn path_arg<I, S>(args: I) -> Option<std::path::PathBuf>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    // Flags that consume the *next* argument. Their value does not start with a
    // dash, so a plain "not a flag" test hands the value back as the folder —
    // which is precisely how `--perf <folder>` once silently opened the wrong
    // directory. Any new flag that takes a value has to be listed here.
    const FLAGS_WITH_VALUE: [&str; 1] = ["--probe-renderer"];

    let mut skip_next = false;
    args.into_iter()
        .map(|s| s.as_ref().to_string())
        .filter(|s| {
            if skip_next {
                skip_next = false;
                return false;
            }
            if FLAGS_WITH_VALUE.contains(&s.as_str()) {
                skip_next = true;
                return false;
            }
            true
        })
        .find(|s| !s.starts_with('-') && !s.is_empty())
        .map(std::path::PathBuf::from)
}

