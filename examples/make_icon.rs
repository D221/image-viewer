//! Regenerate the application icon. `cargo run --release --example make_icon`.
//!
//! All the drawing is in `src/icon.rs`, where its properties can be tested; this
//! is just the part that writes files.

fn main() {
    let dir = std::path::Path::new("assets");
    if let Err(e) = image_viewer::icon::write_assets(dir) {
        eprintln!("failed to write assets: {e}");
        std::process::exit(1);
    }
    for &s in &image_viewer::icon::SIZES {
        println!("  {s:>3}px  fills {:.1}% of the bitmap", image_viewer::icon::coverage(s) * 100.0);
    }
    println!("wrote {}/app.ico and app-icon-preview.png", dir.display());
}
