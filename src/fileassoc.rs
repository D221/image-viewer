//! Windows file-type registration (HKCU — per-user, no admin rights).
//!
//! Creates a ProgID describing this exe and registers it under
//! `OpenWithProgids` for each supported image extension. That makes the app
//! appear in Explorer's "Open with…" list; "Set default" must be confirmed
//! by the user in the Windows dialog (by design since Windows 8 — apps are
//! not allowed to silently claim the default association).

type R<T> = Result<T, String>;
fn io<T, E: std::fmt::Display>(r: Result<T, E>) -> R<T> {
    r.map_err(|e| e.to_string())
}

/// ProgID for this viewer: `ImageViewer.<exedir>` so parallel copies don't collide.
pub fn progid() -> String {
    let n = std::env::current_exe()
        .ok()
        .and_then(|p| {
            p.parent()
                .and_then(|d| d.file_name().map(|s| s.to_string_lossy().into_owned()))
        })
        .unwrap_or_else(|| "app".into());
    format!("ImageViewer.{}", sanitize(&n))
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

fn exts() -> &'static [&'static str] {
    &[
        "jpg", "jpeg", "png", "webp", "gif", "jxl", "avif", "heic", "heif",
    "tif", "tiff", "bmp", "dib", "ico", "cur", "qoi",
    ]
}

fn open_key(sub: &str, writable: bool) -> R<winreg::RegKey> {
    use winreg::enums::*;
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let access = if writable {
        KEY_SET_VALUE | KEY_CREATE_SUB_KEY
    } else {
        KEY_READ
    };
    io(hkcu.open_subkey_with_flags(sub, access))
}

/// The `DefaultIcon` value to register, and where it came from.
///
/// A ProgID with no icon is what makes Explorer show the *generic application
/// icon* for every file it handles: the shell resolves the icon through the
/// default handler, finds nothing, and falls back to the executable. This is
/// worth fixing rather than arguing about, because a file list is mostly icons.
///
/// Two sources, in order of preference:
///  * the icon embedded in our own exe, which `build.rs` puts there, and
///  * a system picture icon, so an un-embedded build still shows a picture
///    rather than a generic application.
///
/// Note what is *not* registered: a `ShellEx\ThumbnailHandler`. The Windows
/// Imaging Component already provides real photo thumbnails for image types, and
/// claiming that key would displace WIC and replace actual thumbnails with our
/// icon — strictly worse. Leaving it alone is what keeps the preview pane
/// working.
fn icon_value(exe: &std::path::Path) -> (String, &'static str) {
    if exe_has_icon(exe) {
        (
            format!("\"{}\",0", exe.to_string_lossy()),
            "the icon in the exe",
        )
    } else {
        (
            format!(r"%SystemRoot%\System32\imageres.dll,-102"),
            "a system picture icon (this build has no embedded icon)",
        )
    }
}

/// True if the exe carries an icon resource at all.
///
/// Checked with `ExtractIconExW` rather than assumed, so the fallback is used
/// only when it is actually needed, and so a build that loses the embedded
/// resource degrades to a sensible icon instead of the generic one.
fn exe_has_icon(exe: &std::path::Path) -> bool {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "shell32")]
    extern "system" {
        fn ExtractIconExW(
            file: *const u16,
            index: i32,
            large: *mut *mut core::ffi::c_void,
            small: *mut *mut core::ffi::c_void,
            count: u32,
        ) -> u32;
        fn DestroyIcon(icon: *mut core::ffi::c_void) -> i32;
    }
    let mut wide: Vec<u16> = exe.as_os_str().encode_wide().collect();
    wide.push(0);
    // A negative index asks "how many?", which needs no icon out.
    let n = unsafe { ExtractIconExW(wide.as_ptr(), -1, core::ptr::null_mut(), core::ptr::null_mut(), 0) };
    if n == 0 {
        return false;
    }
    // Now actually extract one, so a malformed resource cannot pass the count.
    let mut large: *mut core::ffi::c_void = core::ptr::null_mut();
    let mut small: *mut core::ffi::c_void = core::ptr::null_mut();
    let got = unsafe { ExtractIconExW(wide.as_ptr(), 0, &mut large, &mut small, 1) };
    if !large.is_null() {
        unsafe { DestroyIcon(large) };
    }
    if !small.is_null() {
        unsafe { DestroyIcon(small) };
    }
    got > 0
}

/// Register the ProgID and add it to OpenWithProgids for all extensions.
/// Returns the ProgID on success.
pub fn register() -> R<String> {
    use winreg::enums::*;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if !exe.exists() {
        return Err(format!("exe not found: {}", exe.display()));
    }
    let id = progid();
    let exe_str = exe.to_string_lossy().to_string();
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);

    // ProgID: friendly name + shell open command (create_subkey makes all
    // intermediate keys).
    let (key, _) = io(hkcu.create_subkey(format!(r"Software\Classes\{id}")))?;
    io(key.set_value("", &"Image Viewer"))?;
    io(key.set_value("FriendlyTypeName", &"Image Viewer"))?;
    // Without this the shell has no icon for the type and shows the generic
    // application icon instead of a picture.
    let (icon, _source) = icon_value(&exe);
    io(key.set_value("DefaultIcon", &icon))?;
    let (shell, _) = io(key.create_subkey(r"shell\open\command"))?;
    io(shell.set_value("", &format!("\"{exe_str}\" \"%1\"")))?;

    // Application entry so the "Open with" dialog shows a nice name.
    let app_path = format!(r"Applications\{}", exe_str.replace('\\', "/"));
    let (app_root, _) = io(hkcu.create_subkey(format!(r"Software\Classes\{app_path}")))?;
    io(app_root.set_value("FriendlyAppName", &"Image Viewer"))?;
    let (app_cmd, _) = io(app_root.create_subkey(r"shell\open\command"))?;
    io(app_cmd.set_value("", &format!("\"{exe_str}\" \"%1\"")))?;

    // Associate with every extension (non-destructive: OpenWithProgids only
    // adds an entry to the "Open with" menu).
    for ext in exts() {
        let (ext_key, _) =
            io(hkcu.create_subkey(format!(r"Software\Classes\.{ext}\OpenWithProgids")))?;
        io(ext_key.set_value(&id, &""))?;
    }
    Ok(id)
}

/// The registered `DefaultIcon` value, or `None`.
///
/// `DefaultIcon` is a *value on the ProgID key*, not a subkey — which is the
/// easy mistake here, and it fails silently: the write succeeds, the read finds
/// nothing, and the conclusion drawn is that registration is broken when it is
/// only the reader.
pub fn default_icon() -> Option<String> {
    open_key(&format!(r"Software\Classes\{}", progid()), false)
        .ok()
        .and_then(|k| io(k.get_value::<String, _>("DefaultIcon")).ok())
        .filter(|v| !v.trim().is_empty())
}

/// Remove the ProgID and its OpenWithProgids entries. Returns true if the
/// ProgID existed.
pub fn unregister() -> R<bool> {
    use winreg::enums::*;
    let id = progid();
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let (classes, _) = io(hkcu.create_subkey(r"Software\Classes"))?;

    let existed = classes.delete_subkey_all(&id).is_ok();

    for ext in exts() {
        if let Ok(owp) = hkcu.open_subkey_with_flags(
            format!(r"Software\Classes\.{ext}\OpenWithProgids"),
            KEY_SET_VALUE,
        ) {
            let _ = owp.delete_value(&id);
        }
    }
    Ok(existed)
}

/// True if the ProgID for this exe exists in HKCU.
pub fn is_registered() -> bool {
    open_key(&format!(r"Software\Classes\{}", progid()), false)
        .ok()
        .and_then(|k| io(k.get_value::<String, _>(r"shell\open\command")).ok())
        .is_some()
}

/// The taskbar's name for this app, as an AppUserModelID.
///
/// A **stable** string, deliberately not derived from the exe folder the way
/// `progid()` is. The taskbar caches a window's icon against this identity, so
/// a value that changes between builds (or between `debug` and `release`) makes
/// a fresh cache entry every time and the button flickers between icons. Two
/// copies of the viewer grouping under one taskbar entry is the correct outcome
/// anyway; the ProgID is where collision-avoidance actually matters, and it
/// already handles that.
///
/// The dotted shape is Microsoft's: `Company.Product.SubProduct.AppID`.
pub const APP_USER_MODEL_ID: &str = "ImageViewer.Desktop.Photo";

/// Declare this process's taskbar identity, and give the shell an icon to use.
///
/// **This is the fix for the taskbar showing a generic icon**, and neither of the
/// obvious approaches is:
///
///  * Embedding the icon in the exe fixes *Explorer* — that path goes through
///    the registered `DefaultIcon` — and does nothing for the taskbar.
///  * Setting `WM_SETICON` on the window does not stick here, because the window
///    procedure is winit's and does not forward it to `DefWindowProc`.
///    (Measured: `GWL_HICON` stays 0 through both `WM_SETICON` and
///    `SetWindowLongPtrW`.)
///
/// What is left is that the taskbar keys its icon cache on the AppUserModelID.
/// With none set, Windows derives one from the exe *path* and caches whatever it
/// found the first time — so a taskbar button keeps showing "no icon" from
/// before the exe had an icon resource, and no amount of setting the window's
/// icon will dislodge it. A brand-new ID is a cache miss, and `IconUri` says
/// where the icon is rather than leaving it to guess.
///
/// Also what makes the taskbar group windows correctly and what "Pin to taskbar"
/// uses. It must be called before the first window exists.
///
/// Idempotent and cheap: the registry key is only written when it is missing or
/// stale, because a write on every launch is a surprising thing for a viewer to
/// do. Errors are non-fatal — without this the app still runs, it just shows the
/// default taskbar icon, which is the behaviour we are trying to improve on.
pub fn declare_taskbar_identity() -> R<()> {
    use winreg::enums::*;

    // The registry half first: the shell reads it when it sees the ID.
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe_str = exe.to_string_lossy().to_string();
    let hkcu = winreg::RegKey::predef(HKEY_CURRENT_USER);
    let sub = format!(r"Software\Classes\AppUserModelId\{APP_USER_MODEL_ID}");

    // Write once, then leave it alone. `IconUri` necessarily comes from
    // `current_exe()`, and the debug and release builds live at different paths —
    // so rewriting it on every launch would make the taskbar flip between the
    // two whenever both are used, which is the exact instability a fixed
    // AppUserModelID exists to prevent. The icon is the same artwork either way.
    //
    // The one exception is a path that no longer exists, so moving the exe
    // self-heals instead of leaving the shell pointing at nothing.
    let stored_icon = hkcu
        .open_subkey_with_flags(&sub, KEY_READ)
        .ok()
        .and_then(|k| k.get_value::<String, _>("IconUri").ok());
    let stale = match stored_icon.as_deref() {
        None => true,
        Some(uri) => {
            let path = uri.trim_matches('"');
            !std::path::Path::new(path).exists()
        }
    };

    if stale {
        let (k, _) = io(hkcu.create_subkey(&sub))?;
        io(k.set_value("DisplayName", &"Image Viewer"))?;
        // A bare exe path is a valid IconUri: the shell extracts the icon from
        // the file. Quoted, because the path very often contains spaces.
        io(k.set_value("IconUri", &format!("\"{exe_str}\"")))?;
    }

    // The process half, and it only sticks if it happens before the first window.
    #[link(name = "shell32")]
    extern "system" {
        fn SetCurrentProcessExplicitAppUserModelID(id: *const u16) -> i32;
    }
    let mut wide: Vec<u16> = APP_USER_MODEL_ID.encode_utf16().collect();
    wide.push(0);
    let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(wide.as_ptr()) };
    if hr != 0 {
        // S_OK is 0. Anything else is cosmetic, so report it rather than failing
        // a launch over it.
        eprintln!("note: taskbar identity was not accepted (hr=0x{hr:08X})");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deliberately does *not* touch the registry. A test that registers and
    /// unregisters would rewrite the user's real file associations, and a test
    /// that fails halfway through would leave them broken. So this covers the
    /// decision, and the registry effect is checked by running `--register` and
    /// reading the key back.
    #[test]
    fn the_icon_value_is_always_something_the_shell_can_resolve() {
        let exe = std::env::current_exe().unwrap();
        let (value, source) = icon_value(&exe);
        assert!(!value.trim().is_empty(), "empty DefaultIcon");
        let resolves = value.contains("imageres.dll") || value.contains(".exe");
        assert!(resolves, "unrecognised DefaultIcon {value:?} ({source})");
    }

    /// The whole reason this code exists. Run under `cargo test --bin
    /// image-viewer`; `build.rs` embeds the icon, so this binary has one and the
    /// registered value must point at it rather than at the fallback.
    #[test]
    fn an_embedded_icon_is_detected_and_preferred() {
        let exe = std::env::current_exe().unwrap();
        if !cfg!(windows) {
            return;
        }
        assert!(
            exe_has_icon(&exe),
            "no icon resource in the exe - build.rs should have embedded assets/app.ico"
        );
        let (value, source) = icon_value(&exe);
        assert!(
            value.contains(".exe"),
            "should have used the exe's own icon, got {value:?} ({source})"
        );
    }

    /// A file that cannot have an icon must be reported as having none, or the
    /// fallback never gets used.
    #[test]
    fn a_file_with_no_icon_is_reported_as_such() {
        if !cfg!(windows) {
            return;
        }
        let tmp = std::env::temp_dir().join("image-viewer-not-an-exe.txt");
        std::fs::write(&tmp, b"hello").unwrap();
        assert!(!exe_has_icon(&tmp), "a text file reported an icon");
        let _ = std::fs::remove_file(&tmp);
    }
}

