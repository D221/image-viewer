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
    "tif", "tiff", "bmp", "dib", "ico", "cur",
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

