//! Module `app_frame`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

impl MyApp {
    pub(crate) fn frame_prelude(&mut self, ctx: &Context) {
        // The previous frame has been presented by the time the next one
        // starts, so this is where "first visible pixels" is honestly known.
        self.lat.commit_frame();
        perf::count(perf::C_FRAMES, 1);
        // Frames that arrived carrying winit events. Compared against
        // `C_FRAMES` this separates "the user is driving" from "something is
        // asking for frames by itself", which is the question you have to ask
        // before concluding the app is burning CPU.
        if ctx.input(|i| !i.events.is_empty()) {
            perf::count(perf::C_FRAMES_WITH_EVENTS, 1);
        }
        if let Some(prev) = self.t_prev_frame.replace(Instant::now()) {
            perf::record_since(perf::FRAME_INTERVAL_US, prev);
        }
        if let Some(t) = self.t_boot.take() {
            perf::record_since(perf::STARTUP_TO_FIRST_FRAME, t);
        }
        self.check_for_stall(ctx);
        // LRU clock for the preview/thumbnail caches. Lives here (not in
        // `central`) so it also advances while the gallery grid is open.
        self.clock += 1;

        // Consume a queued advance from "wait for load" mode — the image it
        // belongs to was painted during the previous frame.
        //
        // The *key* it re-reads is only a check that the reader is still pushing
        // something; the direction is the one `pump` already resolved through
        // `nav_is_forward`, so a queued advance cannot come back with the opposite
        // sense to the press that queued it.
        if let Some(dir) = self.pending_advance.take() {
            let still_held = ctx.input(|i| {
                nav_direction(
                    |key, c, s, a| {
                        i.key_down(key)
                            && i.modifiers.ctrl == c
                            && i.modifiers.shift == s
                            && i.modifiers.alt == a
                    },
                    |_| false,
                    self.config.manga_rtl,
                ) == Some(dir)
            });
            if still_held {
                self.step(ctx, dir);
            }
        }
        // ---- end of the hold-to-browse pair ----
        self.pump(ctx);
        self.sync_shown(ctx);
        self.maybe_rescan(ctx);

        // Start the full-res decode once the user has dwelled on an image.
        if let Some(due) = self.full_due {
            let now = Instant::now();
            if now >= due {
                self.full_due = None;
                self.schedule();
            } else {
                ctx.request_repaint_after(due - now);
            }
        }
    }
}
