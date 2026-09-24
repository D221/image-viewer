use egui::{ColorImage, Context, TextureHandle, TopBottomPanel};
use nom_exif::{read_exif, ExifTag};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use image_viewer::{downscale_image, load_image, Config, Gallery, ImageData, ZoomPan};

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
    show_image: bool,
    config: Config,
    config_path: PathBuf,
    exif_info: Option<String>,
    exif_loading: bool,
    current_path: Option<PathBuf>,
    last_viewport_size: Option<egui::Vec2>,
}

impl MyApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let ctx = &cc.egui_ctx;
        let config_path = Config::default_path();
        let config = Config::load(&config_path);

        let folder = config
            .folder
            .as_ref()
            .and_then(|f| {
                let p = PathBuf::from(f);
                if p.is_dir() {
                    Some(p)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| PathBuf::from("test-images"));

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
            show_image: config.show_image_before_auto_fit,
            config,
            config_path,
            exif_info: None,
            exif_loading: false,
            current_path: None,
            last_viewport_size: None,
        };

        if !app.config_path.exists() {
            app.config.save(&app.config_path);
        }
        if app.config.load_all_on_startup {
            app.gallery.load_all_from_folder(&app.folder);
        }
        app.load_current(ctx);
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

        // Store current path for background upgrade
        self.current_path = Some(path.to_path_buf());

        // Check if we already have the full image cached (clamped to GPU limit)
        let cached_full = {
            let cache = self.image_cache.lock().unwrap();
            cache.get(path).cloned()
        };

        let full_data = if let Some(data) = cached_full {
            data
        } else {
            // Decode the image once and cache the full data
            match load_image(path, 16384) {
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

        // Generate a small preview from the cached full data (fast)
        let preview_data = downscale_image(&full_data, 256);
        let rgba = preview_data.rgba.clone();
        let width = preview_data.width as usize;
        let height = preview_data.height as usize;
        let texture = ctx.load_texture(
            path.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .as_ref(),
            ColorImage::from_rgba_unmultiplied([width, height], &rgba),
            egui::TextureOptions::LINEAR,
        );
        self.texture = Some(texture);
        self.load_time_ms = preview_data.load_time.as_millis() as u64;

        self.zoom_pan.reset();
        self.needs_fit = true;
        self.show_image = self.config.show_image_before_auto_fit;

        // Update window title to current file name
        if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
                "Image Viewer — {}",
                filename
            )));
        }

        // Clear previous EXIF and start loading new one in background
        self.exif_info = None;
        self.exif_loading = true;
        let path_clone = path.to_path_buf();
        let ctx_clone = ctx.clone();
        std::thread::spawn(move || {
            let exif_data = MyApp::extract_exif(&path_clone);
            ctx_clone.data_mut(|d| {
                d.insert_persisted(egui::Id::new("exif_info"), exif_data);
            });
            ctx_clone.request_repaint();
        });
    }

    fn extract_exif(path: &std::path::Path) -> Option<String> {
        let exif = read_exif(path).ok()?;

        let mut info = Vec::new();

        // Camera make/model
        if let Some(entry) = exif.get(ExifTag::Make) {
            info.push(format!("Camera: {}", entry));
        }
        if let Some(entry) = exif.get(ExifTag::Model) {
            info.push(format!("Model: {}", entry));
        }

        // Date/time
        if let Some(entry) = exif.get(ExifTag::DateTimeOriginal) {
            info.push(format!("Date: {}", entry));
        }

        // Exposure
        if let Some(entry) = exif.get(ExifTag::ExposureTime) {
            info.push(format!("Exposure: {}", entry));
        }

        // Aperture
        if let Some(entry) = exif.get(ExifTag::FNumber) {
            info.push(format!("Aperture: {}", entry));
        }

        // ISO
        if let Some(entry) = exif.get(ExifTag::ISOSpeedRatings) {
            info.push(format!("ISO: {}", entry));
        }

        // Focal length
        if let Some(entry) = exif.get(ExifTag::FocalLength) {
            info.push(format!("Focal Length: {}", entry));
        }

        // Dimensions (from PNG or JPEG)
        if let Some(entry) = exif.get(ExifTag::ImageWidth) {
            if let Some(h_entry) = exif.get(ExifTag::ImageHeight) {
                info.push(format!(
                    "Dimensions: {}x{} px",
                    entry,
                    h_entry
                ));
            }
        }

        if info.is_empty() {
            None
        } else {
            Some(info.join("\n"))
        }
    }

    /// Preload next and previous images in background threads so navigation is instant.
    fn preload_neighbors(&self) {
        let cache = self.image_cache.clone();
        let files = self.gallery.files.clone();
        let index = self.gallery.index;

        for &neighbor_idx in &[
            index.saturating_sub(1),
            (index + 1).min(files.len().saturating_sub(1)),
        ] {
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
                if let Ok(data) = load_image(&path, 99999) {
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
            self.config.folder = Some(folder.to_string_lossy().to_string());
            self.config.save(&self.config_path);
            self.gallery = Gallery::new(&folder);
            if self.config.load_all_on_startup {
                self.gallery.load_all_from_folder(&folder);
            }
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
                self.folder = first.parent().unwrap_or(first).to_path_buf();
                self.gallery = Gallery::from_file(first.clone());
                if self.config.load_all_on_startup {
                    self.gallery.load_all_from_folder(&self.folder);
                }
                self.load_current(ctx);
                self.preload_neighbors();
            }
        }
    }

    /// Load an image from a dropped file path.
    fn load_dropped_file(&mut self, ctx: &Context, path: PathBuf) {
        if path.is_dir() {
            self.folder = path.clone();
            self.gallery = Gallery::new(&path);
            if self.config.load_all_on_startup {
                self.gallery.load_all_from_folder(&path);
            }
            self.load_current(ctx);
            self.preload_neighbors();
            return;
        }
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
        if self.config.load_all_on_startup {
            self.gallery.load_all_from_folder(&self.folder);
        }
        self.load_current(ctx);
        self.preload_neighbors();
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

        // Drag-and-drop: handle dropped files/folders
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        if let Some(file) = dropped.first() {
            if let Some(path) = file.path.clone() {
                self.load_dropped_file(ctx, path);
            }
        }

        // Settings panel
        egui::SidePanel::left("exif").show(ctx, |ui| {
            ui.heading("EXIF");
            if self.exif_loading {
                ui.label("Loading EXIF…");
            } else if let Some(info) = &self.exif_info {
                for line in info.lines() {
                    ui.label(line);
                }
            } else {
                ui.label("No EXIF available.");
            }
        });

        // Read EXIF data from persistent storage (set by background thread)
        let exif_opt =
            ctx.data_mut(|d| d.get_persisted::<Option<String>>(egui::Id::new("exif_info")));
        if let Some(exif_data) = exif_opt {
            if self.exif_info.as_ref() != exif_data.as_ref() {
                self.exif_info = exif_data.as_ref().cloned();
                self.exif_loading = false;
            }
        }

        // Settings panel
        egui::SidePanel::right("settings").show(ctx, |ui| {
            ui.heading("Settings");
            if ui
                .checkbox(
                    &mut self.config.load_all_on_startup,
                    "Load all images on startup",
                )
                .changed()
            {
                self.config.save(&self.config_path);
                if self.config.load_all_on_startup {
                    self.load_all_from_folder(ctx);
                } else {
                    self.load_current(ctx);
                }
                self.preload_neighbors();
            }
            if ui
                .checkbox(
                    &mut self.config.show_image_before_auto_fit,
                    "Show image before auto-fit",
                )
                .changed()
            {
                self.config.save(&self.config_path);
                self.show_image = self.config.show_image_before_auto_fit;
                if self.show_image {
                    self.needs_fit = true;
                }
            }
            ui.label(format!("Config: {}", self.config_path.display()));
        });

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

            // Detect viewport resize and trigger refit
            let current_size = ctx.screen_rect().size();
            if self.last_viewport_size != Some(current_size) {
                self.last_viewport_size = Some(current_size);
                self.needs_fit = true;
            }

            // Check for full image upgrade before borrowing texture
            if let Some(ref current_path) = self.current_path {
                let cached_full = {
                    let cache = self.image_cache.lock().unwrap();
                    cache.get(current_path).cloned()
                };
                if let Some(full_data) = cached_full {
                    let rgba = full_data.rgba.clone();
                    let width = full_data.width as usize;
                    let height = full_data.height as usize;
                    let new_tex = ctx.load_texture(
                        current_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .as_ref(),
                        ColorImage::from_rgba_unmultiplied([width, height], &rgba),
                        egui::TextureOptions::LINEAR,
                    );
                    self.texture = Some(new_tex);
                    self.load_time_ms = full_data.load_time.as_millis() as u64;
                    self.zoom_pan.reset();
                    self.needs_fit = true;
                    self.current_path = None;
                }
            }

            if let Some(tex) = &self.texture {
                let avail = ui.available_size();
                let pointer = ctx.pointer_hover_pos();

                // Always compute fit (even when hidden) so image fits when shown
                if self.needs_fit {
                    self.zoom_pan.fit_to_window(
                        tex.size()[0] as f32,
                        tex.size()[1] as f32,
                        avail.x,
                        avail.y,
                    );
                    self.needs_fit = false;
                    self.show_image = true; // show after fit
                }

                if self.show_image {
                    // Apply pending scroll with cursor-centering zoom
                    if self.pending_scroll.abs() > 0.01 {
                        // Store old values
                        let old_zoom = self.zoom_pan.zoom;
                        let old_pan_x = self.zoom_pan.pan_x;
                        let old_pan_y = self.zoom_pan.pan_y;

                        // Apply zoom change
                        self.zoom_pan.zoom_by(self.pending_scroll);
                        // Prevent zooming out below fit-to-window
                        let tex_size = tex.size();
                        let fit_scale = (avail.x / tex_size[0] as f32)
                            .min(avail.y / tex_size[1] as f32)
                            .clamp(0.1, 8.0);
                        self.zoom_pan.zoom = self.zoom_pan.zoom.max(fit_scale);

                        // If we have a pointer position, adjust pan to zoom toward cursor
                        if let Some(cursor_pos) = pointer {
                            // Calculate current center of image in screen space
                            let center_x = avail.x * 0.5 + old_pan_x;
                            let center_y = avail.y * 0.5 + old_pan_y;

                            // New center so that cursor position stays fixed
                            // Formula: new_center = cursor - (cursor - old_center) * new_zoom / old_zoom
                            let new_center_x = cursor_pos.x
                                - (cursor_pos.x - center_x) * self.zoom_pan.zoom / old_zoom;
                            let new_center_y = cursor_pos.y
                                - (cursor_pos.y - center_y) * self.zoom_pan.zoom / old_zoom;

                            // Update pan to achieve new center
                            self.zoom_pan.pan_x = new_center_x - avail.x * 0.5;
                            self.zoom_pan.pan_y = new_center_y - avail.y * 0.5;
                        }

                        // Clear pending scroll
                        self.pending_scroll = 0.0;
                    }

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
                    ui.label("Image hidden until fit. Use \"Fit to Window\" to display.");
                }
            } else {
                ui.label("No images loaded.");
            }

            ui.label(format!("Zoom: {:.1}x", self.zoom_pan.zoom));
        });

        // Bottom panel with menu buttons
        TopBottomPanel::bottom("menu").show(ctx, |ui| {
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
                        let fit_scale = (avail.x / tex_w).min(avail.y / tex_h).clamp(0.1, 8.0);
                        self.zoom_pan.zoom = self.zoom_pan.zoom.max(fit_scale);
                        self.needs_fit = true;
                        self.show_image = true;
                        self.config.show_image_before_auto_fit = true;
                        self.config.save(&self.config_path);
                    }
                }
            });
        });
    }
}
