//! Module `app_actions`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    /// Show the current file's folder in the system's default file browser
    /// (respects folder associations, e.g. Directory Opus instead of Explorer).
    pub(crate) fn explore_current(&mut self) {
        let cur = self.gallery.current().map(|p| p.to_path_buf());
        if let Some(p) = cur {
            if !self.spawn_guard("Explore") {
                return;
            }
            let folder = p
                .parent()
                .map(|d| d.to_path_buf())
                .unwrap_or_else(|| p.to_path_buf());
            if let Err(e) = open::that_detached(&folder) {
                self.error = Some(format!("Open folder failed: {e}"));
            }
        }
    }

    /// True when an external spawn is allowed now; records it. False — with a
    /// notice naming what was held — when the last spawn was less than
    /// `SPAWN_COOLDOWN` ago. Global across all spawn sites on purpose: per
    /// command would let cycling Ctrl+1..9 evade it.
    pub(crate) fn spawn_guard(&mut self, what: &str) -> bool {
        if self
            .last_external_spawn
            .is_some_and(|t| t.elapsed() < SPAWN_COOLDOWN)
        {
            self.notice = Some(format!("{what} ignored — ran a moment ago"));
            return false;
        }
        self.last_external_spawn = Some(Instant::now());
        true
    }

    /// Fit-view as a mode (`F`): every image fitted to the window.
    /// Mutually exclusive with fit-window mode — turning one on switches
    /// the other off. On applies now; off leaves the zoom where it is,
    /// absolute from here.
    pub(crate) fn toggle_fit_mode(&mut self) {
        self.config.fit_mode = !self.config.fit_mode;
        if self.config.fit_mode {
            self.config.fit_window_mode = false;
            self.config.fit_width_mode = false;
            self.view.fit = true;
        } else {
            self.view.fit = false;
        }
        self.config.save(&self.config_path);
        self.notice = Some(
            if self.config.fit_mode {
                "Fit view: on"
            } else {
                "Fit view: off"
            }
            .into(),
        );
    }

    /// Fit-width as a mode: every image scaled so its width fills the window,
    /// the height overflows and you pan vertically. Mutually exclusive with
    /// fit mode — turning one on switches the other off.
    pub(crate) fn toggle_fit_width_mode(&mut self) {
        self.config.fit_width_mode = !self.config.fit_width_mode;
        if self.config.fit_width_mode {
            self.config.fit_mode = false;
            self.view.fit = true;
        } else {
            self.view.fit = false;
        }
        self.config.save(&self.config_path);
        self.notice = Some(
            if self.config.fit_width_mode {
                "Fit width: on"
            } else {
                "Fit width: off"
            }
            .into(),
        );
    }

    /// Fit window to image as a mode (`T`): every press fits now *and*
    /// toggles persistence — JPEGView's exact shape (`AdjustWindowToImage`
    /// runs unconditionally; only the sticky flag flips conditionally).
    /// Splitting those two (fit-only-when-turning-on) stranded the view the
    /// moment the mode and the key disagreed about what `T` means.
    /// While on, arrivals refit (see `goto`) and user zooms re-hug the new
    /// size (see `central`).
    /// Grid only: fitting the window to a grid of cells is a different
    /// feature, so this says so instead of doing something surprising.
    pub(crate) fn fit_window_to_image(&mut self) {
        if self.gallery_view {
            self.notice = Some("Close the grid first".into());
            return;
        }
        self.config.fit_window_mode = !self.config.fit_window_mode;
        if self.config.fit_window_mode {
            self.config.fit_mode = false;
        }
        self.config.save(&self.config_path);
        self.fit_window_pending = true;
        self.fit_iters = 6;
        self.notice = Some(
            if self.config.fit_window_mode {
                "Fit window: on"
            } else {
                "Fit window: off"
            }
            .into(),
        );
    }

    /// The side-panel master (`I`), **persisted**: chrome is a preference, the
    /// way `hide_title_bar` is — how little sits around the picture is what
    /// the user wants on every launch, not what they wanted once. Saved on
    /// the press like every other config-backed toggle; the per-pane keys
    /// (`L`/`P`) stay session-only, fine-tuning a run without an opinion
    /// about the next one. The one dispatcher, so the key and the menu row
    /// cannot disagree.
    ///
    /// Not fullscreen's: there the master is an overlay (see
    /// `panels_before_fullscreen`), and writing the config from the dance
    /// would turn a fullscreen detour into a re-formed preference.
    pub(crate) fn toggle_panels_master(&mut self) {
        self.suppress_resize_redecode = true;
        self.show_panels = !self.show_panels;
        self.config.show_panels = self.show_panels;
        self.config.save(&self.config_path);
    }

    /// Step the on-screen animation one frame (`,` back, `.` forward,
    /// YouTube-style) and pause on it. Wraps at both ends — animations loop,
    /// so stepping never gets stuck. Without an animation on screen this is
    /// nothing, which is also what the grid does with it.
    pub(crate) fn step_animation(&mut self, forward: bool) {
        let shown_path = self.shown.as_ref().map(|k| k.path.clone());
        let Some(av) = self
            .anim
            .as_mut()
            .filter(|a| Some(&a.path) == shown_path.as_ref())
        else {
            return;
        };
        let n = av.anim.frames.len();
        let speed = image_viewer::clamp_anim_speed(self.config.anim_speed);
        let cur = av
            .paused
            .unwrap_or_else(|| image_viewer::anim_frame_state(&av.anim, av.start.elapsed(), speed).0);
        av.paused = Some(image_viewer::step_frame(n, cur.min(n.saturating_sub(1)), forward));
    }

    /// Pause or resume the on-screen animation (`K`). Resuming rewinds the
    /// clock to the held frame's start, so playback continues mid-loop
    /// instead of restarting.
    pub(crate) fn toggle_animation_play(&mut self) {
        let now = Instant::now();
        let shown_path = self.shown.as_ref().map(|k| k.path.clone());
        let Some(av) = self
            .anim
            .as_mut()
            .filter(|a| Some(&a.path) == shown_path.as_ref())
        else {
            return;
        };
        if let Some(held) = av.paused.take() {
            // Scaled back down: the stored offset is in animation time, but
            // `start` ticks wall time. Same clamp the frame clock uses, or
            // the two disagree about which frame is on screen.
            av.start = now
                - image_viewer::anim_frame_start(&av.anim, held)
                    .div_f32(image_viewer::clamp_anim_speed(self.config.anim_speed));
        } else {
            let idx = image_viewer::anim_frame_state(
                &av.anim,
                av.start.elapsed(),
                self.config.anim_speed,
            )
            .0;
            av.paused = Some(idx);
        }
    }

    /// Print the file on screen through its registered handler
    /// ([`image_viewer::print_file`], the OS `print` verb — usually the
    /// Photos print view). The *file*, not the view: crops and turns are not
    /// applied, because the handler receives a path rather than pixels.
    /// Through `spawn_guard` like every external action, so a repeating
    /// chord is one dialog a second rather than one per frame.
    pub(crate) fn print_current(&mut self) {
        let Some(path) = self
            .shown
            .as_ref()
            .map(|k| k.path.clone())
            .or_else(|| self.cur.as_ref().map(|k| k.path.clone()))
        else {
            self.notice = Some("No image is open".into());
            return;
        };
        if !self.spawn_guard("Print") {
            return;
        }
        match image_viewer::print_file(&path) {
            Ok(()) => {
                self.notice = Some(
                    format!(
                        "Sent {} to print",
                        path.file_name().unwrap_or_default().to_string_lossy()
                    ),
                );
            }
            Err(e) => self.error = Some(format!("Print failed: {e}")),
        }
    }
}
