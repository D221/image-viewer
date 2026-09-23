use egui::{ColorImage, Context, TextureHandle};
use std::path::PathBuf;

use image_viewer::{Gallery, ZoomPan, load_image};

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Image Viewer",
        options,
        Box::new(|cc| Box::new(MyApp::new(cc))),
    )
}

struct MyApp {
    folder: PathBuf,
    gallery: Gallery,
    texture: Option<TextureHandle>,
    zoom_pan: ZoomPan,
    drag_start: Option<egui::Pos2>,
    load_time_ms: u64,
}

impl MyApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        let folder = PathBuf::from(r"test-images");
        let gallery = Gallery::new(&folder);
        let mut app = Self {
            folder,
            gallery,
            texture: None,
            zoom_pan: ZoomPan::new(),
            drag_start: None,
            load_time_ms: 0,
        };
        app.load_current(&_cc.egui_ctx);
        app
    }

    fn load_current(&mut self, ctx: &Context) {
        let path = match self.gallery.current() {
            Some(p) => p,
            None => {
                self.texture = None;
                return;
            }
        };

        let start = std::time::Instant::now();
        match load_image(path, 1600) {
            Ok(data) => {
                let rgba = data.rgba;
                let width = data.width as usize;
                let height = data.height as usize;
                let texture = ctx.load_texture(
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .as_ref(),
                    ColorImage::from_rgba_unmultiplied([width, height], &rgba),
                    egui::TextureOptions::LINEAR,
                );
                self.texture = Some(texture);
                self.load_time_ms = start.elapsed().as_millis() as u64;
                self.zoom_pan.reset();
            }
            Err(e) => {
                eprintln!("Failed to load {}: {}", path.display(), e);
                self.texture = None;
            }
        }
    }

    fn next(&mut self, ctx: &Context) {
        self.gallery.next();
        self.load_current(ctx);
    }

    fn prev(&mut self, ctx: &Context) {
        self.gallery.prev();
        self.load_current(ctx);
    }

    /// Open a folder picker and load images from the selected directory.
    fn pick_folder(&mut self, ctx: &Context) {
        let folder = rfd::FileDialog::new()
            .set_title("Select image folder")
            .pick_folder();

        if let Some(folder) = folder {
            self.folder = folder.clone();
            self.gallery = Gallery::new(&folder);
            self.load_current(ctx);
        }
    }

    /// Open a file picker and load the selected image file.
    fn pick_file(&mut self, ctx: &Context) {
        let files = rfd::FileDialog::new()
            .set_title("Select image file")
            .add_filter("JPEG", &["jpg", "jpeg", "png"])
            .add_filter("PNG", &["png"])
            .pick_files();

        if let Some(files) = files {
            if let Some(first) = files.first() {
                self.folder = first.parent().unwrap_or(&first).to_path_buf();
                self.gallery = Gallery::from_file(first.clone());
                self.load_current(ctx);
            }
        }
    }

    /// Load an image from a dropped file path.
    fn load_dropped_file(&mut self, ctx: &Context, path: PathBuf) {
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext != "jpg" && ext != "jpeg" && ext != "png" {
            return;
        }
        self.folder = path.parent().unwrap_or(&path).to_path_buf();
        self.gallery = Gallery::from_file(path);
        self.load_current(ctx);
    }

    /// Load all images from the current folder (sorted by name, like Explorer).
    fn load_all_from_folder(&mut self, ctx: &Context) {
        self.gallery = Gallery::new(&self.folder);
        if !self.gallery.is_empty() {
            self.gallery.index = 0;
        }
        self.load_current(ctx);
    }
}

impl eframe::App for MyApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        // Keyboard navigation
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowRight)) {
            self.next(ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::ArrowLeft)) {
            self.prev(ctx);
        }

        // Mouse interaction for zoom/pan
        let pointer = ctx.pointer_hover_pos();
        if ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if self.drag_start.is_none() {
                self.drag_start = pointer;
            }
        } else {
            self.drag_start = None;
        }

        // Zoom with scroll
        let scroll = ctx.input(|i| i.smooth_scroll_delta.y);
        if scroll.abs() > 0.01 {
            self.zoom_pan.zoom_by(scroll);
        }

        // Pan drag
        if let Some(start) = self.drag_start {
            if let Some(pos) = pointer {
                let delta = pos - start;
                self.zoom_pan.pan_x += delta.x;
                self.zoom_pan.pan_y += delta.y;
            }
        }

        // Drag-and-drop: handle dropped files
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for file in dropped.iter() {
            if let Some(path) = file.path.clone() {
                self.load_dropped_file(ctx, path);
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading(format!(
                "{} — {} / {} — load {}ms",
                self.folder.display(),
                if self.gallery.is_empty() {
                    0
                } else {
                    self.gallery.index + 1
                },
                self.gallery.len(),
                self.load_time_ms
            ));

            ui.separator();

            if let Some(tex) = &self.texture {
                let avail = ui.available_size();
                let tex_w = tex.size()[0] as f32 * self.zoom_pan.zoom;
                let tex_h = tex.size()[1] as f32 * self.zoom_pan.zoom;

                // Center of available area
                let center_x = avail.x * 0.5 + self.zoom_pan.pan_x;
                let center_y = avail.y * 0.5 + self.zoom_pan.pan_y;
                let center = egui::Pos2::new(center_x, center_y);

                let mut image_widget = egui::Image::new(tex);
                image_widget = image_widget.fit_to_exact_size(egui::Vec2::new(tex_w, tex_h));
                ui.put(
                    egui::Rect::from_center_size(center, egui::Vec2::new(tex_w, tex_h)),
                    image_widget,
                );
            } else {
                ui.label("No images loaded.");
            }

            ui.horizontal(|ui| {
                if ui.button("Browse Folder…").clicked() {
                    self.pick_folder(ctx);
                }
                if ui.button("Browse File…").clicked() {
                    self.pick_file(ctx);
                }
                if ui.button("Load All").clicked() {
                    self.load_all_from_folder(ctx);
                }
                if ui.button("Prev (←)").clicked() {
                    self.prev(ctx);
                }
                if ui.button("Next (→)").clicked() {
                    self.next(ctx);
                }
                ui.label(format!("Zoom: {:.1}x", self.zoom_pan.zoom));
            });
        });
    }
}