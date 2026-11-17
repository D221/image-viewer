//! The `Keys` section of the settings pane: every action is rebindable.
//!
//! Extracted from `src/app/ui/settings.rs` when that file reached the
//! 1000-line target, so the pane kept room for the row-wrapping fix below.

use crate::*;

impl MyApp {
    /// Capture a new chord, then draw the key list.
    ///
    /// Every action is rebindable: click a chord, press keys. Meanings never
    /// change (navigation still asks `nav_is_forward`), only the chords. While
    /// one waits, the dispatch stands down so the capture press cannot act —
    /// deleting a photo by rebinding Delete is exactly the accident this gate
    /// exists to prevent. Capture: fresh (non-repeat) key presses become the
    /// chord. Bare Escape cancels (so Escape is not bindable — it already owns
    /// the typing/grid/fullscreen/quit chain, which is structural rather than a
    /// binding).
    pub(crate) fn ui_settings_keys(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        if let Some(action) = self.capturing.clone() {
            // Fresh non-modifier presses only: the modifier keys themselves
            // also arrive as Key events, and committing the first event would
            // turn "hold Ctrl, press G" into "Ctrl+?" — the capture waits for
            // the real key, whose event already carries the held modifiers.
            let pressed: Vec<(egui::Key, egui::Modifiers)> = ctx.input(|i| {
                i.events
                    .iter()
                    .filter_map(|e| match e {
                        egui::Event::Key {
                            key, pressed: true, repeat: false, modifiers, ..
                        } if !is_modifier_key(*key) => Some((*key, *modifiers)),
                        _ => None,
                    })
                    .collect()
            });
            if pressed.iter().any(|(k, m)| *k == egui::Key::Escape && !m.any()) {
                self.capturing = None;
            } else if let Some((key, mods)) = pressed.first() {
                let mut raw = String::new();
                if mods.ctrl {
                    raw.push_str("Ctrl+");
                }
                if mods.shift {
                    raw.push_str("Shift+");
                }
                if mods.alt {
                    raw.push_str("Alt+");
                }
                raw.push_str(key_name(*key));
                match image_viewer::canonical_chord(&raw) {
                    Some(chord)
                        if image_viewer::split_chord(&chord)
                            .is_some_and(|(_, _, _, name)| parse_key_full(&name).is_some()) =>
                    {
                        // Duplicates are allowed and shown, not refused:
                        // blocking second-guesses the reader (chords can be
                        // intentional — one key stepping two views is sometimes
                        // exactly the point), while a silent double-fire is the
                        // failure mode. So the commit always lands, the notice
                        // names the sharers, and every row below marks its own
                        // conflicts in red.
                        if chord == image_viewer::default_binding(&action) {
                            self.config.keybindings.remove(&action);
                        } else {
                            self.config.keybindings.insert(action.clone(), chord.clone());
                        }
                        set_nav_overrides(&self.config.keybindings);
                        self.config.save(&self.config_path);
                        let sharers: Vec<&str> = image_viewer::KEY_ACTIONS
                            .iter()
                            .filter(|(id, _, _)| {
                                *id != action
                                    && image_viewer::resolve_binding(&self.config.keybindings, id)
                                        == chord
                            })
                            .map(|(_, _, label)| *label)
                            .collect();
                        self.notice = Some(if sharers.is_empty() {
                            format!("{label} is now {chord}", label = action_label(&action))
                        } else {
                            format!(
                                "{label} is now {chord} (also: {sharers})",
                                label = action_label(&action),
                                sharers = sharers.join(", ")
                            )
                        });
                        self.capturing = None;
                    }
                    _ => {
                        self.notice = Some(format!("{raw} is not a bindable key"));
                        self.capturing = None;
                    }
                }
            }
        }
        ui.collapsing("Keys", |ui| {
            if ui.button("Reset keys").clicked() {
                self.config.keybindings.clear();
                set_nav_overrides(&self.config.keybindings);
                self.config.save(&self.config_path);
                self.notice = Some("Keys reset to defaults".into());
            }
            egui::ScrollArea::vertical()
                .id_salt("key_list")
                .max_height(300.0)
                .show(ui, |ui| {
                    for (id, _, label) in image_viewer::KEY_ACTIONS {
                        ui.horizontal_wrapped(|ui| {
                            ui.label(*label);
                            let current = image_viewer::resolve_binding(&self.config.keybindings, id);
                            let text = if self.capturing.as_deref() == Some(*id) {
                                "press keys… (Esc cancels)".to_owned()
                            } else if current.is_empty() {
                                // The one blank shipped row (`next_alt`):
                                // deliberately unbound, so say so rather than
                                // drawing a button with no name.
                                "(unbound)".to_owned()
                            } else {
                                current.clone()
                            };
                            if ui.button(text).clicked() {
                                self.capturing = Some((*id).to_owned());
                            }
                            // Conflict marker: every other action on this exact
                            // chord, in red. Duplicates fire together — this row
                            // is where that reads as intentional rather than
                            // mysterious.
                            let others: Vec<&str> = image_viewer::KEY_ACTIONS
                                .iter()
                                .filter(|(other, _, _)| {
                                    *other != *id
                                        && image_viewer::resolve_binding(
                                            &self.config.keybindings,
                                            other,
                                        ) == current
                                })
                                .map(|(_, _, other_label)| *other_label)
                                .collect();
                            if !others.is_empty() {
                                ui.colored_label(
                                    ui.visuals().warn_fg_color,
                                    format!("also: {}", others.join(", ")),
                                );
                            }
                        });
                    }
                });
            ui.small("Esc quits the app chain and is not bindable; Ctrl+C keeps its platform meaning alongside any alternative you set.");
        });
    }
}
