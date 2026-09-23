use egui::{ColorImage, Context, TextureHandle};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use image_viewer::{Gallery, ZoomPan, load_image, ImageData};

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

/// Cache of decoded images keyed by file path.
type ImageCache = Arc<Mutex<HashMap<PathBuf, Arc<ImageData>>>>;

struct MyApp {
    folder: PathBuf,
    gallery: Gallery,
    texture: Option<TextureHandle>,
    zoom_pan: ZoomPan,
    drag_start: Option<egui::Pos2>,
    load_time_ms: u64,
    pending_scroll: f32,
    image_cache: ImageCache,
    needs_fit: bool,
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
            pending_scroll: 0.0,
            image_cache: Arc::new(Mutex::new(HashMap::new())),
            needs_fit: true,
        };
        app.load_current(&_cc.egui_ctx);
        app.preload_neighbors();
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

        // Check cache first — skip decode if already loaded
        let cached = {
            let cache = self.image_cache.lock().unwrap();
            cache.get(path).cloned()
        };

        let data = if let Some(cached) = cached {
            cached
        } else {
            match load_image(path, 1600) {
                Ok(data) => {
                    let arc = Arc::new(data);
                    let mut cache = self.image_cache.lock().unwrap();
                    cache.insert(path.to_path_buf(), arc.clone());
                    arc
                }
                Err(e) => {
                    eprintln!("Failed to load {}: {}", path.display(), e);
                    self.texture = None;
                    return;
                }
            }
        };

        let rgba = data.rgba.clone();
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
        self.needs_fit = true;
    }

    /// Preload next and previous images in background threads so navigation is instant.
    fn preload_neighbors(&self) {
        let cache = self.image_cache.clone();
        let files = self.gallery.files.clone();
        let index = self.gallery.index;

        for &neighbor_idx in &[index.saturating_sub(1), (index + 1).min(files.len().saturating_sub(1))] {
            if neighbor_idx >= files.len() {
                continue;
            }
            let path = files[neighbor_idx].clone();
            let cache = cache.clone();
            // Skip if already cached
            {
                let c = cache.lock().unwrap();
                if c.contains_key(&path) {
                    continue;
                }
            }
            std::thread::spawn(move || {
                if let Ok(data) = load_image(&path, 1600) {
                    let mut c = cache.lock().unwrap();
                    c.insert(path, Arc::new(data));
                }
            });
        }
    }

    fn next(&mut self, ctx: &Context) {
        self.gallery.next();
        self.load_current(ctx);
        self.preload_neighbors();
    }

    fn prev(&mut self, ctx: &Context) {
        self.gallery.prev();
        self.load_current(ctx);
        self.preload_neighbors();
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
            self.preload_neighbors();
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
                self.preload_neighbors();
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
        self.gallery.load_all_from_folder(&self.folder);
        if !self.gallery.is_empty() {
            self.load_current(ctx);
        }
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

        // Capture scroll for later use in panel (where we have access to avail)
        let scroll = ctx.input(|i| i.raw_scroll_delta.y);
        if scroll.abs() > 0.01 {
            self.pending_scroll = scroll;
        }

        // Pan drag (firm, incremental)
        if ctx.input(|i| i.pointer.button_down(egui::PointerButton::Primary)) {
            if let Some(pos) = pointer {
                if let Some(last) = self.drag_start {
                    let delta = pos - last;
                    self.zoom_pan.pan_x += delta.x;
                    self.zoom_pan.pan_y += delta.y;
                }
                self.drag_start = Some(pos); // update for next frame
            }
        } else {
            self.drag_start = None;
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
                let pointer = ctx.pointer_hover_pos();

                // Apply pending scroll with cursor-centering zoom
                if self.pending_scroll.abs() > 0.01 {
                    // Store old values
                    let old_zoom = self.zoom_pan.zoom;
                    let old_pan_x = self.zoom_pan.pan_x;
                    let old_pan_y = self.zoom_pan.pan_y;

                    // Apply zoom change
                    self.zoom_pan.zoom_by(self.pending_scroll);

                    // If we have a pointer position, adjust pan to zoom toward cursor
                    if let Some(cursor_pos) = pointer {
                        // Calculate current center of image in screen space
                        let center_x = avail.x * 0.5 + old_pan_x;
                        let center_y = avail.y * 0.5 + old_pan_y;

                        // New center so that cursor position stays fixed
                        // Formula: new_center = cursor - (cursor - old_center) * new_zoom / old_zoom
                        let new_center_x = cursor_pos.x - (cursor_pos.x - center_x) * self.zoom_pan.zoom / old_zoom;
                        let new_center_y = cursor_pos.y - (cursor_pos.y - center_y) * self.zoom_pan.zoom / old_zoom;

                        // Update pan to achieve new center
                        self.zoom_pan.pan_x = new_center_x - avail.x * 0.5;
                        self.zoom_pan.pan_y = new_center_y - avail.y * 0.5;
                    }

                    // Clear pending scroll
                    self.pending_scroll = 0.0;
                }

                let tex_w = tex.size()[0] as f32 * self.zoom_pan.zoom;
                let tex_h = tex.size()[1] as f32 * self.zoom_pan.zoom;

                // Auto-fit to window on first display
                if self.needs_fit {
                    self.zoom_pan.fit_to_window(
                        tex.size()[0] as f32,
                        tex.size()[1] as f32,
                        avail.x,
                        avail.y,
                    );
                    self.needs_fit = false;
                }

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
                if ui.button("Fit to Window").clicked() {
                    if let Some(tex) = &self.texture {
                        let avail = ui.available_size();
                        let tex_w = tex.size()[0] as f32;
                        let tex_h = tex.size()[1] as f32;
                        self.zoom_pan.fit_to_window(tex_w, tex_h, avail.x, avail.y);
                    }
                }
                ui.label(format!("Zoom: {:.1}x", self.zoom_pan.zoom));
            });
        });
    }
}