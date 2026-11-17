//! `settings` panel, extracted from `MyApp::ui` when `src/app/ui.rs` passed the
//! 1000-line target.
//!
//! Rows here are `horizontal_wrapped`, never plain `horizontal`. The pane is
//! pinned to 300 in fit-window mode, and a row that does not fit grows the
//! panel's content rect past that pin; egui clamps the *size* but re-anchors
//! the rect at the grown right edge, so the pane is reported shifted by the
//! overflow — drawn off the window, with the central panel painting over its
//! left end. That was the fit-window "only the right side of Settings is
//! drawn" bug (the Back/Forward row needs ~401 pt against the 300 pt pin).
//! Wrapping ends the rows at the pane edge at any width, in and out of the
//! mode. A text edit sharing its row with a button is sized by [`edit_width`]
//! for the same reason.

use crate::*;

impl MyApp {
    pub(crate) fn ui_settings(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        _first: bool,
        _toggle: bool,
    ) -> (bool, bool) {
        let mut resort = false;
        let mut refilter = false;
        if self.show_panels && self.show_right {
            let mut pane = egui::Panel::right("settings");
            if self.config.fit_window_mode {
                pane = pane.exact_size(300.0);
            }
            pane.show(ui, |ui| {
                ui.heading("Settings");
                // Rows wrap — see the module note above the impl.
                // Scrollable so expanded sections don't run off the bottom of
                // short windows; no frame so it doesn't look like a "window
                // inside window" — each section is its own collapsible group.
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        egui::CollapsingHeader::new("Graphics & window")
                    .default_open(true)
                    .show(ui, |ui| {
                    if ui
                        .checkbox(&mut self.config.esc_closes_app, "Esc closes the app")
                        .on_hover_text("Off: Esc still leaves crop mode, the grid and fullscreen, but never quits.")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Worker threads (0 = auto):")
                            .on_hover_text("0 picks 2-4 from your CPU. Applies on restart.");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.workers)
                                    .range(0..=8),
                            )
                            .changed()
                        {
                            // The pool is built once at startup, so this takes
                            // effect on restart rather than rebuilding it live.
                            self.config.save(&self.config_path);
                        }
                    });
                    if ui
                        .checkbox(&mut self.config.single_instance, "Allow only one window")
                        .changed()
                    {
                        // Takes effect on next launch: the running process already
                        // holds the mutex, so flipping this cannot open or close one.
                        self.config.save(&self.config_path);
                        self.notice = Some(if self.config.single_instance {
                            "Single-instance on — takes effect on restart"
                        } else {
                            "Single-instance off — multiple windows allowed"
                        }.into());
                    }
                    // Window chrome, applied live: decorations have a viewport
                    // command, and always-on-top goes straight to Win32
                    // (`set_topmost`) because egui 0.36 only offers a builder for
                    // it. The launch loop reads the same flags, so a restart
                    // keeps whatever is set here.
                    if ui
                        .checkbox(&mut self.config.always_on_top, "Always on top")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        if !image_viewer::set_topmost(self.config.always_on_top) {
                            self.error = Some("Could not change window order".into());
                        }
                    }
                    if ui
                        .checkbox(&mut self.config.hide_title_bar, "Hide title bar")
                        .on_hover_text("Without a title bar there is no drag handle — Win+arrows still move the window.")
                        .changed()
                    {
                        // Left alone, winit keeps a decorations change's outer
                        // rect and lets the client area absorb (hide) or give
                        // up (show) the title strip: the picture jumps and
                        // re-fits, and in fit-window mode the window's hug
                        // breaks — "the option doesn't reset the window."
                        // Keep the client instead: it is re-asserted at its
                        // pre-toggle size, and the outer frame is put where
                        // [`image_viewer::outer_top_for_decoration_toggle`]
                        // computes, so only the chrome arrives or leaves.
                        let hiding = self.config.hide_title_bar;
                        let vp = ui.input(|i| {
                            let v = i.viewport();
                            v.inner_rect.zip(v.outer_rect).map(|(inner, outer)| {
                                (
                                    inner,
                                    outer,
                                    v.maximized.unwrap_or(false),
                                    v.fullscreen.unwrap_or(false),
                                )
                            })
                        });
                        match vp {
                            Some((inner, outer, maximized, _fullscreen)) => {
                                // The strip is measurable only while the
                                // window is decorated, so a hide is the one
                                // moment to remember it; the reverse toggle
                                // reads it back. Measured even maximized — the
                                // clamped work area shifts `outer.top` by
                                // little more than the strip either way.
                                let strip = (inner.top() - outer.top()).max(0.0);
                                if hiding {
                                    self.config.title_strip = strip;
                                }
                                self.config.save(&self.config_path);
                                // `if maximized || fullscreen {` — maximized windows keep the OS
                                // clamp; fullscreen does too, and a cover experiment holds the
                                // monitor rect without `ViewportCommand`, so `is_fullscreen`
                                // reads both the viewport flag and the cover flag.
                                if maximized || self.is_fullscreen(ctx) {
                                    ctx.send_viewport_cmd(
                                        egui::ViewportCommand::Decorations(!hiding),
                                    );
                                } else {
                                    let strip_after =
                                        if hiding { 0.0 } else { self.config.title_strip };
                                    let top = image_viewer::outer_top_for_decoration_toggle(
                                        inner.top(),
                                        strip_after,
                                    );
                                    // Enqueue order is settle order, and both
                                    // application orders converge: the size
                                    // re-assert anchors at the top-left it
                                    // sees, so whichever arrives first — the
                                    // frame already moved there, or the frame
                                    // moved after — ends with the frame at
                                    // `top` and the client's size and screen
                                    // position unchanged.
                                    ctx.send_viewport_cmd(
                                        egui::ViewportCommand::Decorations(!hiding),
                                    );
                                    ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                                        egui::Pos2::new(inner.left(), top),
                                    ));
                                    ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                                        inner.size(),
                                    ));
                                }
                            }
                            // No geometry measured (the rects here are filled
                            // by eframe on Windows, but never say never): the
                            // toggle still applies, at yesterday's cost.
                            None => ctx.send_viewport_cmd(egui::ViewportCommand::Decorations(!hiding)),
                        }
                    }

                    self.ui_fullscreen_mode(ui);
                });

                egui::CollapsingHeader::new("Folders & files")
                    .default_open(true)
                    .show(ui, |ui| {
                    if ui
                        .checkbox(
                            &mut self.config.load_all_on_startup,
                            "Load all images in folder",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        if self.config.load_all_on_startup {
                            Self::load_folder_into(
                                &self.folder,
                                self.config.recursive_folders && self.zip_archive.is_none(),
                                &mut self.gallery,
                            );
                        }
                        self.goto(ctx);
                    }
                    if ui
                        .checkbox(
                            &mut self.config.recursive_folders,
                            "Include subfolders",
                        )
                        .on_hover_text("One gallery over the whole tree. The 2 s watcher walks it every poll, so this costs a walk per level.")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        // New files, possibly a new position: rescan rather than
                        // patching the list in place (`set_files` re-`goto`s when
                        // the cursor moved, so no explicit one here).
                        self.rescan(ctx, true);
                    }
                    if ui
                        .checkbox(&mut self.config.show_archives, "Show archives (zip, cbz, 7z, rar)")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        // The library reads this through `is_browsable`, so nothing is
                        // rescanned until it is told — and a listing rebuilt before
                        // the setter ran would keep the old answer.
                        image_viewer::set_show_archives(self.config.show_archives);
                        self.rescan(ctx, true);
                    }
                    if ui
                        .checkbox(
                            &mut self.config.thumbnail_cache,
                            "Cache grid thumbnails on disk",
                        )
                        .on_hover_text(format!(
                            "Up to {} of cells, so a folder of large scans opens once at full \
                             speed. Stored in {}.",
                            human_bytes(image_viewer::THUMB_CACHE_MAX_BYTES),
                            thumb_cache_root().display()
                        ))
                        .changed()
                    {
                        // Nothing to invalidate. The cells in `thumbs` are decoded
                        // pictures and stay correct whichever way this goes; only
                        // whether *new* ones are written changes. Dropping them here
                        // would cost a re-decode of every visible cell — up to 50 of
                        // them at ~700 ms each on a folder of large scans — to buy
                        // nothing at all.
                        set_thumbnail_cache(self.config.thumbnail_cache);
                        self.config.save(&self.config_path);
                    }
                    if ui
                        .checkbox(
                            &mut self.config.preview_store,
                            "Cache previews of large pictures on disk",
                        )
                        .on_hover_text(format!(
                            "Up to {} of previews, so showing a large scan again does not \
                             decode it again (690 ms to decode, 22-50 ms to read back, \
                             measured). Stored in {}.",
                            human_bytes(image_viewer::PREVIEW_STORE_MAX_BYTES),
                            image_viewer::preview_store_root().display()
                        ))
                        .changed()
                    {
                        // Nothing to invalidate, for the same reason the cell cache
                        // has nothing to invalidate: a stored preview is the
                        // decode's own pixels, so it stays correct whichever way
                        // this goes, and dropping them would cost a re-decode of
                        // every large picture for nothing.
                        image_viewer::set_preview_store(self.config.preview_store);
                        if self.config.preview_store {
                            // Turning it **on** has to start the thread that does
                            // the encoding: the store is written by a thread of
                            // its own so the encode never lands in front of the
                            // first pixels.
                            image_viewer::start_preview_store();
                        } else {
                            // Turning it **off** means off, disk included: a
                            // setting that stops the writing but leaves 512 MB
                            // behind is a promise kept in letter only.
                            image_viewer::clear_preview_store();
                        }
                        self.config.save(&self.config_path);
                    }
                    let mut new_sort = self.config.sort;
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Sort:");
                        egui::ComboBox::from_id_salt("sort_mode")
                            .selected_text(format!("{new_sort:?}"))
                            .show_ui(ui, |ui| {
                                for m in [
                                    SortMode::Name,
                                    SortMode::Date,
                                    SortMode::Size,
                                    SortMode::Taken,
                                    SortMode::Random,
                                ] {
                                    ui.selectable_value(&mut new_sort, m, format!("{m:?}"));
                                }
                            });
                    });
                    if new_sort != self.config.sort {
                        self.config.sort = new_sort;
                        self.config.save(&self.config_path);
                        resort = true;
                    }
                    if ui
                        .checkbox(&mut self.config.sort_descending, "Descending sort order")
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        resort = true;
                    }
                    if ui
                        .checkbox(&mut self.config.loop_back, "Loop back at folder ends")
                        .changed()
                    {
                        // Apply straight away; the gallery owns the wrap behaviour,
                        // so it has to be told.
                        self.gallery.loop_back = self.config.loop_back;
                        self.config.save(&self.config_path);
                    }

                    // ---- back/forward history ----
                    ui.separator();
                    ui.label("Back/forward history");
                    ui.horizontal_wrapped(|ui| {
                        let back_enabled = !self.history_back.is_empty();
                        let fwd_enabled = !self.history_forward.is_empty();
                        if ui
                            .add_enabled(
                                back_enabled,
                                egui::Button::new("← Back").shortcut_text("Alt+Left"),
                            )
                            .on_hover_text(if back_enabled {
                                "Return to the previously viewed image"
                            } else {
                                "Nothing to go back to"
                            })
                            .clicked()
                        {
                            self.go_back(ctx);
                        }
                        if ui
                            .add_enabled(
                                fwd_enabled,
                                egui::Button::new("Forward →").shortcut_text("Alt+Right"),
                            )
                            .on_hover_text(if fwd_enabled {
                                "Return to the image you went back from"
                            } else {
                                "Nothing to go forward to"
                            })
                            .clicked()
                        {
                            self.go_forward(ctx);
                        }
                        ui.label(format!(
                            "{} back / {} forward",
                            self.history_back.len(),
                            self.history_forward.len()
                        ));
                    });

                    // ---- favourite folders ----
                    ui.separator();
                    ui.label("Favourite folders");
                    for fav in self.config.favourite_folders.clone() {
                        let exists = fav.is_dir();
                        let label = format!(
                            "{} {}",
                            fav.display(),
                            if exists { "" } else { " (missing)" }
                        );
                        if ui
                            .add_enabled(
                                exists,
                                egui::Button::new(label).shortcut_text(""),
                            )
                            .on_hover_text("Open this folder")
                            .clicked()
                        {
                            self.close_transients();
                            self.open_folder(ctx, fav.clone());
                        }
                        if ui.button("✕").on_hover_text("Remove favourite").clicked()
                            && let Some(pos) = self
                                .config
                                .favourite_folders
                                .iter()
                                .position(|p| p == &fav)
                            {
                                self.config.favourite_folders.remove(pos);
                                self.config.save(&self.config_path);
                            }
                    }
                    if ui.button("Add current folder").clicked() {
                        let already = self
                            .config
                            .favourite_folders
                            .iter()
                            .any(|p| p == &self.folder);
                        if !already {
                            self.config.favourite_folders.push(self.folder.clone());
                            self.config.save(&self.config_path);
                        }
                    }
                });

                egui::CollapsingHeader::new("View & zoom")
                    .default_open(true)
                    .show(ui, |ui| {
                    if ui
                        .checkbox(
                            &mut self.config.linear_resample,
                            "Linear-light downsampling",
                        )
                        .changed()
                    {
                        set_linear_resample(self.config.linear_resample);
                        self.config.save(&self.config_path);
                    }
                    // Off by default: this changes how photos look, so it is opt-in
                    // rather than a correction. Like the toggle above it applies at
                    // decode time and does not re-decode what is already cached — move
                    // to another image, or restart, to see it everywhere.
                    if ui
                        .checkbox(
                            &mut self.config.convert_wide_gamut,
                            "Convert Display P3 photos to sRGB",
                        )
                        .on_hover_text("Colours outside sRGB are clipped to fit, which is lossy.")
                        .changed()
                    {
                        image_viewer::set_wide_gamut_conversion(self.config.convert_wide_gamut);
                        self.config.save(&self.config_path);
                    }
                    // The same surface as the thumbnail cache — what a grid cell
                    // looks like. Applies live — the grid reads
                    // the value every frame, so no invalidation or re-decode.
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Grid cell size:");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.grid_cell)
                                    .range(64.0..=400.0)
                                    .speed(1.0),
                            )
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                        ui.label("pt");
                    });
                    let mut new_filter = self.config.downscale_filter;
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Filter:");
                        egui::ComboBox::from_id_salt("ds_filter")
                            .selected_text(format!("{:?}", self.config.downscale_filter))
                            .show_ui(ui, |ui| {
                                for f in [
                                    DownscaleFilter::Hermite,
                                    DownscaleFilter::Catrom,
                                    DownscaleFilter::Lanczos3,
                                ] {
                                    ui.selectable_value(&mut new_filter, f, format!("{f:?}"));
                                }
                            });
                    });
                    if new_filter != self.config.downscale_filter {
                        self.config.downscale_filter = new_filter;
                        set_downscale_filter(new_filter);
                        self.config.save(&self.config_path);
                        refilter = true;
                    }
                    // Sits with the shape dropdown rather than under it because it is
                    // the same decision — how a downscaled page is resampled — and
                    // the two axes have to be read together to make sense of either.
                    // The label says what it *does*, not that a `filter_scale` is
                    // multiplied: the user is choosing between a clean tone field
                    // and a crisp line, and neither word describes the mechanism.
                    //
                    // **Labelled for the effect, because the effect is what the user
                    // can see.** "Low-pass strength" is a true description and a
                    // useless one; nobody looking at a page with a lattice in it is
                    // going to find a checkbox that says "low-pass strength".
                    if ui
                        .checkbox(
                            &mut self.config.smooth_downscale,
                            "Smooth: soften the downscale to hide a dot grid",
                        )
                        .on_hover_text(
                            "Off by default. A downscaled page's fine detail can fold back \
                             on itself into a grid of dark and light. Smooth low-passes \
                             harder: it hides that grid on a page screened at a single \
                             pitch, and can bring one out on a page whose tones use more \
                             than one, and edges come out noticeably softer. Fast is \
                             crisper.",
                        )
                        .changed()
                    {
                        // Apply straight away. Nothing re-queues a decode on its own,
                        // so the alternative is "looks like it worked after a
                        // restart", which is the same failure as not applying it.
                        set_downscale_smoothing(self.config.smooth_downscale);
                        self.config.save(&self.config_path);
                        // **Invalidate, or this reads as a broken setting.** The
                        // previews already in `previews` were decoded by the *other*
                        // kernel, and nothing else would ever re-queue them: the
                        // filter dropdown has carried `refilter` for exactly this
                        // reason, and `linear_resample` a few rows up is the
                        // documented counter-example of what happens without it.
                        refilter = true;
                    }
                    // **Says the cost is real, and says it is not a clear win.** The
                    // label used to claim Smooth keeps a grid from forming, which is
                    // only half of what it does: on pages whose greys are screened at
                    // more than one pitch it can bring a grid *out* instead. It is
                    // off by default for exactly that reason, so the hover text is
                    // what a reader has to go on when they turn it on, and promising a
                    // one-sided win here would be the same defect as hiding the cost.
                    // The 1.71x figure is the measured step-edge softening and is
                    // still true; it is quoted so the trade's size is on the screen.

                    // Pixel art is a **sampling** choice, made when each content
                    // texture is uploaded: nearest on magnify, linear on minify.
                    // Nearest is for the *magnification* axis only, because
                    // minifying nearest is what drops pixel rows and reads as a
                    // broken picture — so the shipped linear minification stays
                    // for every fit and zoomed-out view, photo or sprite.
                    if ui
                        .checkbox(
                            &mut self.config.pixel_art,
                            "Pixel art: sharp pixels when zoomed in",
                        )
                        .on_hover_text(
                            "Off by default. For pixel drawings: the magnifying filter samples \
                             nearest, so a sprite zoomed past 100% stays squares of its own \
                             pixels instead of smearing; zoomed out on anything, the averaging \
                             filter still runs, so a downscaled picture does not lose rows. \
                             Applies to the picture, its zoom tiles and animation frames, not \
                             the grid. Flipping it re-requests the previews; the old picture \
                             stays until the replacement arrives.",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        // **Invalidate the same frame.** The sampling was baked
                        // into the textures already in `previews` and `full`'s
                        // tiles, and nothing else would re-ask: the box flips and
                        // the picture keeps the samplers it arrived with, which is
                        // exactly the broken-feature shape the refilter walk below
                        // (and `the_smooth_toggle_is_wired...` in tests/unit.rs)
                        // documents for a toggle whose work is not re-done. The
                        // replacements arrive into the same slot, so the old
                        // picture stays up until then — no blank flash.
                        refilter = true;
                    }
                    if ui
                        .checkbox(
                            &mut self.config.block_while_loading,
                            "Wait for image when holding arrows",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    // Fixed zoom ratios on `1`–`4`, as source-pixel multiples
                    // (`1` is 100%). `zoom_preset` clamps on read, so anything
                    // typed here stays sane.
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Zoom keys 1-4:");
                        let mut changed = false;
                        for (n, z) in self.config.zoom_presets.iter_mut().enumerate() {
                            ui.label(format!("{}:", n + 1));
                            changed |= ui
                                .add(egui::DragValue::new(z).range(0.1..=32.0).speed(0.1))
                                .changed();
                        }
                        if changed {
                            self.config.save(&self.config_path);
                        }
                    });
                    if ui
                        .checkbox(
                            &mut self.config.wheel_navigation,
                            "Mouse wheel flips images (Ctrl+wheel zooms)",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    // Session view state — memory that exists nowhere but this
                    // run — so the flip needs no invalidation: it changes what a
                    // future arrival restores, not anything on screen or in a
                    // decode. The false arm is exactly the shipped behaviour,
                    // and both arms are `arriving_view`'s recorded answer.
                    if ui
                        .checkbox(
                            &mut self.config.remember_view,
                            "Remember zoom and pan per image",
                        )
                        .on_hover_text(
                            "An image you zoomed or panned comes back the way you left it, while \
                             you are browsing; others keep the zoom carried over. Per page of a \
                             document. Lost when the app closes.",
                        )
                        .changed()
                    {
                        self.config.save(&self.config_path);
                    }
                    // A property of the *gesture*, not of the app: it changes what
                    // a drag does, and nothing about it applies until somebody drags.
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Crop shape:")
                            .on_hover_text(
                                "Frames a new crop to this shape; Shift forces a square for \
                                 one drag. A frame already drawn keeps the shape it has.",
                            );
                        let mut choice = self.config.crop_aspect;
                        egui::ComboBox::from_id_salt("crop_aspect")
                            .selected_text(choice.label())
                            .show_ui(ui, |ui| {
                                for a in image_viewer::CropAspect::ALL {
                                    ui.selectable_value(&mut choice, a, a.label());
                                }
                            });
                        if choice != self.config.crop_aspect {
                            self.config.crop_aspect = choice;
                            self.config.save(&self.config_path);
                        }
                    });
                });

                ui.collapsing("Slideshow & animation", |ui| {
                    // Slideshow: one picture per interval until toggled off, or
                    // until the end with looping off. The pace persists; running
                    // does not survive a restart.
                    ui.horizontal_wrapped(|ui| {
                        let label = if !self.slideshow {
                            "Start slideshow"
                        } else if self.slideshow_paused {
                            "Resume slideshow"
                        } else {
                            "Pause slideshow"
                        };
                        if ui.button(label).clicked() {
                            if !self.slideshow {
                                self.start_slideshow();
                            } else {
                                self.toggle_slideshow_pause();
                            }
                        }
                        ui.label("every");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.slideshow_secs)
                                    .range(1..=60),
                            )
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                        ui.label("s");
                        // Beside the pace because it is the same decision — how a
                        // show walks — and a shuffle buried in a submenu is a
                        // setting nobody finds. Session-only like the show itself:
                        // reshuffled every start, folder order restored on stop.
                        if ui
                            .checkbox(&mut self.config.slideshow_shuffle, "Shuffle")
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                    });
                    // Animation playback speed, next to the slideshow for the
                    // same reason it sits there: both are time-based playback,
                    // and both apply live with no re-decode.
                    ui.horizontal_wrapped(|ui| {
                        ui.label("Animation speed (1 = file timing):");
                        if ui
                            .add(
                                egui::DragValue::new(&mut self.config.anim_speed)
                                    .range(0.1..=8.0)
                                    .speed(0.25),
                            )
                            .changed()
                        {
                            self.config.save(&self.config_path);
                        }
                    });
                });

                // ---- keys ---------------------------------------------------
                self.ui_settings_keys(ui, ctx);

                ui.collapsing("User commands", |ui| {
                    // ---- user commands ------------------------------------------
                    //
                    // External programs run against the current file (Tier 1 #2):
                    // Ctrl+1..9 in list order. `{file}`, `{dir}` and `{name}`
                    // expand in the program and in every arg, so
                    // `mspaint.exe "{file}"` opens the picture on screen in Paint.
                    // Spawned detached — the viewer never waits — so this is an
                    // "open with", not scripting with results.
                    ui.horizontal_wrapped(|ui| {
                        ui.label("User commands (Ctrl+1..9):");
                        ui.colored_label(ui.visuals().warn_fg_color, "(beta)");
                    });
                    let mut cmds_changed = false;
                    let mut remove_at: Option<usize> = None;
                    for (n, cmd) in self.config.user_commands.iter_mut().enumerate() {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(format!("{}:", n + 1));
                            let w = edit_width(ui);
                            if ui
                                .add(egui::TextEdit::singleline(&mut cmd.name).desired_width(w))
                                .changed()
                            {
                                cmds_changed = true;
                            }
                            if ui.small_button("x").clicked() {
                                remove_at = Some(n);
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("program");
                            if ui.text_edit_singleline(&mut cmd.program).changed() {
                                cmds_changed = true;
                            }
                        });
                        ui.horizontal_wrapped(|ui| {
                            ui.label("args");
                            let mut joined = cmd.args.join(" ");
                            if ui.text_edit_singleline(&mut joined).changed() {
                                // Space-separated; quote nothing. Args with real
                                // spaces in them need their program to take one
                                // `{file}` and split it itself — documented below.
                                cmd.args = joined
                                    .split_whitespace()
                                    .map(|s| s.to_owned())
                                    .collect();
                                cmds_changed = true;
                            }
                        });
                    }
                    if let Some(n) = remove_at {
                        self.config.user_commands.remove(n);
                        cmds_changed = true;
                    }
                    if ui.button("Add command").clicked() {
                        self.config.user_commands.push(image_viewer::UserCommand {
                            name: String::new(),
                            program: String::new(),
                            args: Vec::new(),
                        });
                        cmds_changed = true;
                    }
                    if cmds_changed {
                        // Saved on edit, not per frame: the flag above is only set
                        // by a field that reported `changed()`. Empty rows are
                        // kept, not pruned: pruning while typing deletes the row
                        // being typed in.
                        self.config.save(&self.config_path);
                    }
                    ui.small("One arg per space. {file} {dir} {name} expand. Empty args just passes {file}.");
                });

                ui.collapsing("Manga", |ui| {
                    // ---- manga mode -------------------------------------------
                    //
                    // `M` and `D` reach these same two fields, so this panel and the
                    // keyboard cannot drift: both flip the bool and save.
                    //
                    // **`Manga mode` re-`goto`s and `Right-to-left` does not**, the same
                    // asymmetry the key handlers have and for the same reason - see
                    // `toggle_manga_mode` and `toggle_manga_rtl`. With the mode off the
                    // second box is a setting for nothing, so it says so rather than
                    // sitting there looking live.
                    //
                    // Both beta features carry a red marker, the same one: this is
                    // the promise that the rough edges are known and written down
                    // (README's beta sections), not that the setting is dangerous.
                    let mut mode_changed = false;
                    let manga_label = action_button_text(&self.config, "Double page mode", "manga");
                    let rtl_label = action_button_text(&self.config, "Right-to-left pages", "rtl");
                    ui.horizontal_wrapped(|ui| {
                        mode_changed = ui
                            .checkbox(&mut self.config.manga_mode, manga_label)
                            .changed();
                        ui.colored_label(ui.visuals().warn_fg_color, "(beta)");
                    });
                    if mode_changed {
                        self.config.save(&self.config_path);
                        self.goto(ctx);
                    }
                    if ui
                        .checkbox(&mut self.config.manga_rtl, rtl_label)
                        .changed()
                    {
                        self.config.save(&self.config_path);
                        ctx.request_repaint();
                    }
                    if !self.config.manga_mode {
                        ui.small("Both are for double page mode, which is off.");
                    } else {
                        ui.small("A turn opens on the right, so Left reads forwards. \
                                  Right-to-left off makes it match Left-to-right.");
                        // The disclosure `AGENTS.md` requires beside any cache: a file
                        // that silently re-pairs a library must not be enabled quietly.
                        // Deleting it costs nothing but the decisions in it.
                        match spreads_root() {
                            Some(store) => {
                                ui.small(format!(
                                    "Pairing overrides live in {}. Delete that file to clear \
                                     every override and go back to the detected shapes.",
                                    store.display()
                                ));
                            }
                            None => {
                                ui.small(
                                    "No local app-data folder, so pairing overrides are kept in \
                                     memory only and are forgotten at exit.",
                                );
                            }
                        }
                    }
                });

                ui.collapsing("File associations", |ui| {
                    let registered = fileassoc::is_registered();
                    if registered {
                        ui.label(format!("Registered as '{}'", fileassoc::progid()));
                        // A missing icon is invisible here and only shows up as a
                        // generic application icon in a file list, so say so.
                        match fileassoc::default_icon() {
                            Some(icon) => {
                                ui.small(format!("Icon: {icon}"));
                            }
                            None => {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    "No icon registered — files will show a generic icon",
                                );
                            }
                        }
                        if ui.button("Unregister file types").clicked() {
                            match fileassoc::unregister() {
                                Ok(_) => self.notice = Some("File associations removed".into()),
                                Err(e) => self.error = Some(format!("Unregister failed: {e}")),
                            }
                        }
                    } else if ui.button("Register as image viewer…").clicked() {
                        match fileassoc::register() {
                            Ok(_) => self.notice = Some("Registered — set default via Explorer's 'Open with'".into()),
                            Err(e) => self.error = Some(format!("Register failed: {e}")),
                        }
                    }
                    ui.small("Registers in the user registry (no admin). To make it the default, right-click an image > Open with > Choose another app > Image Viewer > Always.");
                });
                ui.label(format!("Config: {}", self.config_path.display()));
                // Which checkout this binary came from, so a copy sitting in
                // Downloads can be told apart from the current build.
                ui.small(format!("Build {}", env!("IV_BUILD_ID")));
                });
            });
        }
        (resort, refilter)
    }
}

/// Width for a text edit that shares its row with a trailing button.
///
/// A `TextEdit` defaults to the whole remaining row, which pushes the "x"
/// past the pane's edge — and an over-wide row shifts the whole pinned pane
/// (see the module note above the impl). Reserve the button and the spacing
/// before it; in a wrapping row `available_width` is the whole row, so the
/// width still left on this line comes from `available_rect_before_wrap`.
fn edit_width(ui: &egui::Ui) -> f32 {
    (ui.available_rect_before_wrap().width() - 32.0).max(0.0)
}
