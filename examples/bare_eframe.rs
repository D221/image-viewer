//! Minimal eframe 0.36 app: a window and one label, nothing else.
//!
//! This exists to answer one question with a measurement instead of an argument:
//! when this viewer sits at 343 MB before it has looked at a single photo, how much
//! of that is *this viewer's* doing and how much is eframe's renderer on this
//! machine?
//!
//! It uses the same eframe, the same wgpu backend pinning and the same window size
//! as the real app, and nothing else — no loader, no cache, no gallery, no decode.
//! It is a **control**, and the control is the point: it has no config file, so it
//! cannot suffer the one mistake that made the backend look irrelevant once already
//! (a `renderer.choice` write that silently did not take effect, reporting DX12's
//! footprint for a GL run — see `AGENTS.md`).
//!
//! Measured, sampled 2 s after the window appears, 1280x800:
//!
//! | | working set | private |
//! |---|---|---|
//! | bare eframe, DX12 | 321 MB | 289 MB |
//! | bare eframe, GL (`--gl`) | **84 MB** | **21 MB** |
//! | this viewer, DX12 | 343 MB | 312 MB |
//! | this viewer, GL | 189 MB | 90 MB |
//! | Oculante 0.9.2 (glow, not wgpu) | 118 MB | 78 MB |
//!
//! So 237 MB of the footprint is eframe's and all of it is the backend, which is
//! why `PROBE_ORDER` now leads with GL. It also means Oculante is *not* the light
//! one on a like-for-like renderer: wgpu+GL alone is 84 MB against Oculante's
//! glow 118 MB.
//!
//! Run it, then sample from outside:
//!   cargo build --release --example bare_eframe
//!   & target\release\examples\bare_eframe.exe --gl
//!   (Get-Process bare_eframe).WorkingSet64

fn main() -> eframe::Result<()> {
    use eframe::egui_wgpu::{
        wgpu::{Backends, InstanceDescriptor},
        WgpuSetup, WgpuSetupCreateNew,
    };

    // Pinned to exactly one backend, which is the whole reason the real app has
    // `wgpu_options`. With eframe's default `NativeOptions` this example exits
    // 0xC0000005 before a window exists: wgpu enumerates *every* enabled backend
    // before choosing an adapter, and Vulkan is a hard access violation on this
    // machine. A nice incidental confirmation of the rule in `AGENTS.md`.
    let backend = if std::env::args().any(|a| a == "--gl") { Backends::GL } else { Backends::DX12 };
    let mut instance = InstanceDescriptor::new_without_display_handle();
    instance.backends = backend;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
        wgpu_options: eframe::egui_wgpu::WgpuConfiguration {
            wgpu_setup: WgpuSetup::CreateNew(WgpuSetupCreateNew {
                instance_descriptor: instance,
                ..WgpuSetupCreateNew::without_display_handle()
            }),
            ..Default::default()
        },
        ..Default::default()
    };
    eframe::run_native("bare eframe", options, Box::new(|_cc| Ok(Box::new(Bare))))
}

struct Bare;

impl eframe::App for Bare {
    // eframe 0.36 renamed `App::update(&mut self, ctx, frame)` to
    // `App::ui(&mut self, ui: &mut egui::Ui, frame)`. Same reason `Context::run`
    // became `Context::run_ui`: the panels own the `Ui` now.
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.label("hello");
    }
}
