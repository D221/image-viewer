//! Minimal eframe 0.36 app: a window and one label, nothing else.
//!
//! This exists to answer one question with a measurement instead of an argument:
//! when the viewer sits at ~189 MB before it has looked at a single photo, how
//! much of that is *this viewer's* doing and how much is eframe's glow renderer
//! on this machine? It uses the same eframe, the same renderer and the same
//! window size as the real app, and nothing else - no loader, no cache, no
//! gallery, no decode.
//!
//! It is a **control**, and the control is the point: it has no config file, so
//! it cannot suffer the mistake that made a renderer comparison wrong once
//! already (a renderer choice that silently did not take effect, so the memory
//! figure reported belonged to the other backend - see `AGENTS.md`).
//!
//! Measured, sampled 2 s after the window appears, 1280x800: **84 MB** working
//! set, **21 MB** private. The rest of the viewer's footprint is the viewer.
//!
//! Run it, then sample from outside:
//!   cargo build --release --example bare_eframe
//!   & target\release\examples\bare_eframe.exe
//!   (Get-Process bare_eframe).WorkingSet64
//!
//! The wgpu-versus-glow numbers this example produced are why the app renders on
//! glow at all; `AGENTS.md` records them. wgpu is no longer built (eframe's
//! default features are off), so there is nothing left here to compare against -
//! what remains useful is the baseline itself.

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
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
