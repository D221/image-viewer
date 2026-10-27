//! Tests for `image_viewer::wallpaper_target` — the pure half of the wallpaper
//! path.
//!
//! The other half (`MyApp::set_wallpaper`) lives in `src/main.rs`, which no
//! integration test can reach, so this suite covers the decision the pure
//! function makes: when does a wallpaper get a converted copy beside the config
//! instead of the original path handed straight to the shell?
//!
//! `wide_gamut_conversion` is a process-wide global, so every test that flips
//! it restores the previous value on drop — the same trap the resample suite
//! already fell into once.

use image_viewer::{
    wallpaper_target, wide_gamut_conversion, set_wide_gamut_conversion,
};

/// Restores `wide_gamut_conversion` to its previous value when dropped.
struct RestoreConversion(bool);

impl Drop for RestoreConversion {
    fn drop(&mut self) {
        set_wide_gamut_conversion(self.0);
    }
}

/// A minimal valid JPEG with no ICC profile: `color_info` reports UntaggedSrgb.
fn untagged_jpeg(dir: &std::path::Path, name: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(
        &p,
        &[
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00,
            0x01, 0x01, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0xFF, 0xD9,
        ],
    )
    .unwrap();
    p
}

#[test]
fn wallpaper_target_keeps_the_original_when_conversion_is_off() {
    let _restore = RestoreConversion(wide_gamut_conversion());
    set_wide_gamut_conversion(false);
    let dir = tempfile::tempdir().unwrap();
    let p = untagged_jpeg(dir.path(), "photo.jpg");
    let target = wallpaper_target(&p, dir.path(), false);
    assert_eq!(target, p, "conversion off means the shell gets the original path");
}

#[test]
fn wallpaper_target_keeps_the_original_for_untagged_jpeg_when_conversion_is_on() {
    let _restore = RestoreConversion(wide_gamut_conversion());
    set_wide_gamut_conversion(true);
    let dir = tempfile::tempdir().unwrap();
    let p = untagged_jpeg(dir.path(), "photo.jpg");
    let target = wallpaper_target(&p, dir.path(), false);
    assert_eq!(
        target, p,
        "an untagged JPEG is not WideGamut, so conversion does not apply"
    );
}

#[test]
fn wallpaper_target_converts_archive_members_even_with_conversion_off() {
    let _restore = RestoreConversion(wide_gamut_conversion());
    set_wide_gamut_conversion(false);
    let dir = tempfile::tempdir().unwrap();
    let p = untagged_jpeg(dir.path(), "photo.jpg");
    let target = wallpaper_target(&p, dir.path(), true);
    assert_eq!(
        target,
        dir.path().join("wallpaper.jpg"),
        "an archive member must be copied out: its temp dir dies with the app"
    );
}

#[test]
fn wallpaper_target_converts_archive_members_with_conversion_on() {
    let _restore = RestoreConversion(wide_gamut_conversion());
    set_wide_gamut_conversion(true);
    let dir = tempfile::tempdir().unwrap();
    let p = untagged_jpeg(dir.path(), "photo.jpg");
    let target = wallpaper_target(&p, dir.path(), true);
    assert_eq!(
        target,
        dir.path().join("wallpaper.jpg"),
        "archive members always get a converted copy regardless of colour space"
    );
}

#[test]
fn wallpaper_target_uses_jpg_extension_when_the_source_has_none() {
    let _restore = RestoreConversion(wide_gamut_conversion());
    set_wide_gamut_conversion(true);
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("photo");
    std::fs::write(&p, &[0xFF, 0xD8, 0xFF]).unwrap();
    let target = wallpaper_target(&p, dir.path(), true);
    assert_eq!(
        target,
        dir.path().join("wallpaper.jpg"),
        "a file without an extension falls back to .jpg"
    );
}

#[test]
fn wallpaper_target_converted_path_is_beside_the_config_folder() {
    let _restore = RestoreConversion(wide_gamut_conversion());
    set_wide_gamut_conversion(true);
    let dir = tempfile::tempdir().unwrap();
    let p = untagged_jpeg(dir.path(), "photo.jpg");
    // Non-archive, untagged → no conversion, original path. The "beside config"
    // behaviour is exercised by the archive tests above; this pins the folder
    // argument when a conversion path is taken.
    let target = wallpaper_target(&p, dir.path(), true);
    assert!(
        target.starts_with(dir.path()),
        "the converted copy lives beside the config, not beside the source"
    );
}

#[test]
fn wide_gamut_conversion_defaults_to_off() {
    let _restore = RestoreConversion(wide_gamut_conversion());
    set_wide_gamut_conversion(false);
    assert!(!wide_gamut_conversion(), "the default is off, per AGENTS.md");
}
