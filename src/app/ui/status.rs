//! `status` panel, extracted from `MyApp::ui` when `src/app/ui.rs` passed the
//! 1000-line target.

use crate::*;

impl MyApp {
    pub(crate) fn ui_status(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, focus_search: bool) {
        if self.show_panels {
            egui::Panel::top("status").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(self.status_text(ui.ctx().pixels_per_point()));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Clear button first so it sits at the right edge.
                        if !self.filter.is_empty() && ui.small_button("x").clicked() {
                            self.filter.clear();
                            self.on_filter_changed(ctx);
                        }
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.filter)
                                .hint_text("filter (/)")
                                .desired_width(160.0),
                        );
                        if focus_search {
                            resp.request_focus();
                        }
                        if resp.changed() {
                            self.on_filter_changed(ctx);
                        }
                    });
                });
                if !self.filter.is_empty() {
                    ui.label(format!("{} of {} shown", self.gallery.len(), self.all_files.len()));
                }
            });
        }
    }
}
