//! Core logic for the image viewer, kept separate from the GUI so it can be
//! unit-tested, benchmarked, and fuzz-tested.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

// Import GenericImageView to access dimensions() method
use image::{DynamicImage, GenericImageView, RgbaImage};

// Progressively load images with low-res preview first
pub fn load_image_preview(path: &Path, preview_max_dim: u32) -> Result<ImageData, LoadError> {
    let start = std::time::Instant::now();
    // Read bytes first so format is detected from magic numbers, not file extension.
    let bytes = std::fs::read(path)?;
    let img = image::load_from_memory(&bytes)?;
    let (w, h) = img.dimensions();

    // Calculate preview scale (aggressive downscale for fast first display)
    let max_side = preview_max_dim.min(256); // Preview at 256px max for speed
    let scale = (max_side as f32 / w as f32)
        .min(max_side as f32 / h as f32)
        .min(1.0);
    let (resized_w, resized_h) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
    let resized = if scale < 1.0 {
        img.resize(resized_w, resized_h, image::imageops::FilterType::Nearest)
    } else {
        img
    };

    let rgba = resized.to_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    let pixels = rgba.into_raw();

    Ok(ImageData {
        width,
        height,
        rgba: pixels,
        load_time: start.elapsed(),
    })
}

/// Configuration settings for the image viewer.
#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    /// Whether to load all images from the current folder on startup.
    pub load_all_on_startup: bool,

    /// Whether to show images before applying auto-fit (fit_to_window).
    pub show_image_before_auto_fit: bool,

    /// Optional folder to load images from on startup.
    pub folder: Option<String>,
}

impl Config {
    /// Load configuration from a JSON file.
    pub fn load(path: &Path) -> Self {
        let default = Self {
            load_all_on_startup: true,
            show_image_before_auto_fit: true,
            folder: None,
        };

        let Ok(data) = std::fs::read_to_string(path) else {
            return default;
        };

        match serde_json::from_str::<Self>(&data) {
            Ok(config) => config,
            Err(err) => {
                eprintln!("Failed to load config {}: {}", path.display(), err);
                default
            }
        }
    }

    /// Save configuration to a JSON file.
    pub fn save(&self, path: &Path) {
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                eprintln!(
                    "Failed to create config directory {}: {}",
                    parent.display(),
                    err
                );
                return;
            }
        }

        let Ok(data) = serde_json::to_string_pretty(self) else {
            eprintln!("Failed to serialize config");
            return;
        };

        if let Err(err) = std::fs::write(path, data) {
            eprintln!("Failed to save config {}: {}", path.display(), err);
        }
    }

    /// Get the default configuration path.
    pub fn default_path() -> PathBuf {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            PathBuf::from(appdata)
                .join("image-viewer")
                .join("config.json")
        } else if let Some(home) = std::env::var_os("HOME") {
            PathBuf::from(home)
                .join(".config")
                .join("image-viewer")
                .join("config.json")
        } else {
            PathBuf::from("config.json")
        }
    }
}

/// Supported image extensions for gallery scanning.
pub const SUPPORTED_EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];

/// Scan a directory for supported image files, sorted alphabetically.
pub fn scan_folder(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                let ext = p
                    .extension()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                if SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
                    files.push(p);
                }
            }
        }
    }
    files.sort();
    files
}

/// Decoded image data plus load timing metadata.
#[derive(Debug, Clone)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub load_time: std::time::Duration,
}

/// Errors that can occur while loading an image.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("decode error: {0}")]
    Decode(#[from] image::ImageError),
}

/// Load and decode an image file (format detected from content, not extension).
///
/// For very large images, the result is downscaled to `max_dim` on the longest
/// side to keep memory and GPU upload bounded. The original resolution is
/// preserved in `width`/`height` only when no downscaling was needed.
pub fn load_image(path: &Path, max_dim: u32) -> Result<ImageData, LoadError> {
    let start = std::time::Instant::now();
    // Read bytes first so format is detected from magic numbers, not file extension.
    let bytes = std::fs::read(path)?;
    let img = image::load_from_memory(&bytes)?;
    let (w, h) = img.dimensions();

    // Clamp dimensions to GPU texture limit (16384) to prevent memory errors.
    // Use min (not max) so the larger dimension drives the scale down.
    let max_side = max_dim.min(16384);
    let scale = (max_side as f32 / w as f32)
        .min(max_side as f32 / h as f32)
        .min(1.0);
    let (resized_w, resized_h) = ((w as f32 * scale) as u32, (h as f32 * scale) as u32);
    let resized = if scale < 1.0 {
        img.resize(resized_w, resized_h, image::imageops::FilterType::Triangle)
    } else {
        img
    };

    let rgba = resized.to_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    let pixels = rgba.into_raw();

    Ok(ImageData {
        width,
        height,
        rgba: pixels,
        load_time: start.elapsed(),
    })
}

/// Downscale a full ImageData (RGBA) to a smaller preview size quickly.
pub fn downscale_image(data: &ImageData, max_dim: u32) -> ImageData {
    let img = DynamicImage::ImageRgba8(
        RgbaImage::from_raw(data.width, data.height, data.rgba.clone()).unwrap(),
    );
    let scale = (max_dim as f32 / data.width as f32)
        .min(max_dim as f32 / data.height as f32)
        .min(1.0);
    let (new_w, new_h) = (
        (data.width as f32 * scale) as u32,
        (data.height as f32 * scale) as u32,
    );
    let resized = if scale < 1.0 {
        img.resize(new_w, new_h, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = resized.to_rgba8();
    ImageData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        load_time: data.load_time, // keep original decode time
    }
}

/// A simple gallery state: a list of files and a current index.
#[derive(Debug, Clone)]
pub struct Gallery {
    pub files: Vec<PathBuf>,
    pub index: usize,
}

impl Gallery {
    pub fn new(folder: &Path) -> Self {
        Self {
            files: scan_folder(folder),
            index: 0,
        }
    }

    /// Create a gallery containing a single file.
    pub fn from_file(path: PathBuf) -> Self {
        Self {
            files: vec![path],
            index: 0,
        }
    }

    pub fn current(&self) -> Option<&Path> {
        self.files.get(self.index).map(|p| p.as_path())
    }

    pub fn next(&mut self) {
        if !self.files.is_empty() {
            self.index = (self.index + 1) % self.files.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.files.is_empty() {
            self.index = if self.index == 0 {
                self.files.len() - 1
            } else {
                self.index - 1
            };
        }
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Load all images from the folder, preserving the current image's position.
    /// Sorts alphabetically (like Explorer) and maintains the current image index.
    pub fn load_all_from_folder(&mut self, folder: &Path) {
        // Remember current image path
        let current_path = match self.current() {
            Some(p) => p.to_path_buf(),
            None => PathBuf::new(),
        };

        // Scan and sort
        let mut new_files = scan_folder(folder);
        new_files.sort();

        // Find position of current image in sorted list
        let mut new_index = 0;
        if !current_path.as_os_str().is_empty() && !new_files.is_empty() {
            for (i, file) in new_files.iter().enumerate() {
                if file == &current_path {
                    new_index = i;
                    break;
                }
            }
        }

        self.files = new_files;
        self.index = new_index;
    }
}

/// Zoom and pan state for the image viewport.
#[derive(Debug, Clone)]
pub struct ZoomPan {
    pub zoom: f32,
    pub pan_x: f32,
    pub pan_y: f32,
}

impl Default for ZoomPan {
    fn default() -> Self {
        Self::new()
    }
}

impl ZoomPan {
    pub fn new() -> Self {
        Self {
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
        }
    }

    /// Apply a scroll delta to zoom, clamped to [0.1, 8.0].
    pub fn zoom_by(&mut self, delta: f32) {
        self.zoom = (self.zoom + delta * 0.01).clamp(0.1, 8.0);
    }

    /// Reset to fit-to-screen defaults.
    pub fn reset(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Fit the image to the available window area, maintaining aspect ratio.
    /// Always applies the fit scale (downscales large images, upscales small ones).
    pub fn fit_to_window(&mut self, tex_w: f32, tex_h: f32, avail_w: f32, avail_h: f32) {
        let scale_x = avail_w / tex_w;
        let scale_y = avail_h / tex_h;
        self.zoom = scale_x.min(scale_y).clamp(0.1, 8.0);
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }
}
