//! Module `system`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use system::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

// Single-instance guard: a named kernel mutex. The name is session-local
// (`Local\`, which any user can always create — `Global\` needs a privilege
// a normal login does not have) and stable across runs so a second launch
// finds the first one.
unsafe extern "system" {
    fn CreateMutexW(attr: *const std::ffi::c_void, initially_owned: i32, name: *const u16)
    -> usize;
    fn GetLastError() -> u32;
    fn ShellExecuteW(
        hwnd: usize,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show: i32,
    ) -> isize;
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(isize, isize) -> i32>,
        param: isize,
    ) -> i32;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    fn GetCurrentProcessId() -> u32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32)
    -> i32;
}

/// Callback for [`set_topmost`]: stash the first top-level window owned by
/// our process and stop enumerating (return 0). One window is all eframe
/// makes here; no visibility filtering, because filtering on the wrong
/// predicate would skip the very window this exists to find.
unsafe extern "system" fn find_own_window(hwnd: isize, out: isize) -> i32 {
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);
    if pid == GetCurrentProcessId() {
        *(out as *mut isize) = hwnd;
        0
    } else {
        1
    }
}

/// Keep our window above all others, live (egui 0.36 has a builder for this
/// but no viewport command, so this goes straight to Win32 instead of
/// waiting for a restart). Finds the window by owning PID rather than title:
/// the title names the current file and changes constantly. Returns whether
/// it took effect.
pub fn set_topmost(on: bool) -> bool {
    const HWND_TOPMOST: isize = -1;
    const HWND_NOTOPMOST: isize = -2;
    const SWP_NOMOVE: u32 = 0x0002;
    const SWP_NOSIZE: u32 = 0x0001;
    unsafe {
        let mut found: isize = 0;
        EnumWindows(Some(find_own_window), &mut found as *mut isize as isize);
        if found == 0 {
            return false;
        }
        SetWindowPos(
            found,
            if on { HWND_TOPMOST } else { HWND_NOTOPMOST },
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE,
        ) != 0
    }
}

pub(crate) const ERROR_ALREADY_EXISTS: u32 = 183;

/// Handles this process created and still holds, so the mutex outlives the
/// call that claimed it. A handle dropped here would still leak (never
/// closed), which also holds it — but an explicit store says so instead of
/// relying on a leak nobody can see.
pub(crate) static HELD_INSTANCE_MUTEXES: std::sync::LazyLock<std::sync::Mutex<Vec<usize>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

/// Claim the single-instance mutex `name`.
///
/// Returns `true` when this launch may proceed: created fresh (and now held
/// for the life of the process), or the mutex could not be created at all —
/// a guard that refuses to start the app is worse than no guard, which is
/// exactly what shipped once: `\\.\`-prefixed names are device paths, not
/// object names, so creation failed on *every* launch and the `m == 0`
/// branch read it as "already running". Returns `false` only when the mutex
/// already exists, i.e. another instance holds it.
pub fn claim_single_instance(name: &str) -> bool {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let m = CreateMutexW(std::ptr::null(), 1, wide.as_ptr());
        if m == 0 {
            eprintln!(
                "note: could not create the single-instance mutex ({}); starting anyway",
                std::io::Error::last_os_error()
            );
            return true;
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            return false;
        }
        HELD_INSTANCE_MUTEXES.lock().unwrap().push(m);
        true
    }
}

/// Print `path` through the handler Windows registered for it (the Photos
/// print view for an image): the `print` verb, no program of our own.
///
/// A return of 32 or less is the documented failure: out of memory, file not
/// found, no association, and the rest of the `SE_ERR_` table. There is no
/// handle to wait on — the handler owns the dialog from here.
pub fn print_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    let verb: Vec<u16> = "print\0".encode_utf16().collect();
    let file: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    // SW_SHOWNORMAL: the handler decides the window, we only ask normally.
    let rc = unsafe {
        ShellExecuteW(0, verb.as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), 1)
    };
    if rc > 32 {
        Ok(())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("the print verb failed for {} (code {rc})", path.display()),
        ))
    }
}
