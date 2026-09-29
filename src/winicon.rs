//! Put the application icon on the window, not just in the executable.
//!
//! **Why this file exists.** Embedding the icon in the exe is necessary but not
//! sufficient, and the symptom looks like the embed failed. It had not: the
//! exe's `RT_GROUP_ICON` was verified byte-for-byte identical to
//! `assets/app.ico` at 32 px. The problem is that the taskbar asks the *window*
//! for an icon, and asks for the big one:
//!
//! | what | value, measured on a running window |
//! |---|---|
//! | `WM_GETICON(ICON_BIG)` | 0 |
//! | `WM_GETICON(ICON_SMALL)` | 0 |
//! | `GetClassLongPtr(GCLP_HICON)` | 0 |
//! | `GetClassLongPtr(GCLP_HICONSM)` | set (by winit, not us) |
//!
//! With no big icon anywhere in that chain the shell draws its own default,
//! and an `.ico` sitting in a `.rs` file's build output cannot fix it. The
//! only reliable answer is to set it explicitly.
//!
//! **eframe cannot do this for us.** `CreationContext` exposes no window handle
//! — `IntegrationInfo` carries only `cpu_usage` — and `egui::ViewportCommand` has
//! no icon variant. So the handle is found by enumerating this process's own
//! top-level windows, which is safe because we have exactly one.

#![cfg(windows)]

use std::ffi::c_void;

/// `RT_GROUP_ICON`. A group, not an `RT_ICON`: the shell wants the group so it
/// can pick the size it needs.
const RT_GROUP_ICON: u16 = 14;
const IMAGE_ICON: u32 = 1;
const LR_DEFAULTCOLOR: u32 = 0x0000_0000;
const SM_CXICON: i32 = 11;
const SM_CXSMICON: i32 = 49;
const SM_CXSMICON2: i32 = 50;
const WM_SETICON: u32 = 0x0080_0002;
const WM_SETFRAMEICON: u32 = 0x0460;
const RDW_INVALIDATE: u32 = 0x0001;
const RDW_FRAME: u32 = 0x0400;
const RDW_ALLCHILDREN: u32 = 0x0080;
const RDW_UPDATENOW: u32 = 0x0100;
const ICON_SMALL: usize = 0;
const ICON_BIG: usize = 1;
const ICON_SMALL2: usize = 2;
const GCLP_HICON: i32 = -14;
const GCLP_HICONSM: i32 = -13;
const GCLP_HICONSM2: i32 = -15;
const GWL_HICON: i32 = -14;
const GWL_HICONSM: i32 = -12;

#[repr(C)]
struct ICONINFO {
    f_icon: i32,
    x_hotspot: i32,
    y_hotspot: i32,
    hbm_mask: *mut c_void,
    hbm_color: *mut c_void,
}

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(cb: extern "system" fn(*mut c_void, *mut c_void) -> i32, l: *mut c_void) -> i32;
    fn EnumResourceNamesW(
        inst: *mut c_void,
        ty: u16,
        cb: extern "system" fn(*mut c_void, *mut u16, *mut u16, *mut c_void) -> i32,
        l: *mut c_void,
    ) -> i32;
    fn GetWindowThreadProcessId(h: *mut c_void, pid: *mut u32) -> u32;
    fn IsWindowVisible(h: *mut c_void) -> i32;
    fn GetWindowTextLengthW(h: *mut c_void) -> i32;
    fn SendMessageW(h: *mut c_void, m: u32, w: usize, l: *mut c_void) -> usize;
    #[link_name = "GetWindowLongPtrW"]
    fn GetWindowLongPtrW(h: *mut c_void, i: i32) -> *mut c_void;
    #[link_name = "SetWindowLongPtrW"]
    fn SetWindowLongPtrW(h: *mut c_void, i: i32, v: *mut c_void) -> *mut c_void;
    fn GetClassLongPtrW(h: *mut c_void, i: i32) -> *mut c_void;
    fn SetClassLongPtrW(h: *mut c_void, i: i32, v: *mut c_void) -> *mut c_void;
    fn LoadImageW(
        inst: *mut c_void,
        name: *const u16,
        kind: u32,
        w: i32,
        h: i32,
        flags: u32,
    ) -> *mut c_void;
    fn GetIconInfo(i: *mut c_void, info: *mut ICONINFO) -> i32;
    fn GetSystemMetrics(i: i32) -> i32;
    fn RedrawWindow(
        h: *mut c_void,
        rect: *mut c_void,
        rgn: *mut c_void,
        flags: u32,
    ) -> i32;
}

#[link(name = "gdi32")]
extern "system" {
    /// `GetObjectW`, not `GetObject`: gdi32 exports only the A and W spellings,
    /// and `<wingdi.h>` aliases them with a macro rather than exporting an
    /// undecorated name.
    #[link_name = "GetObjectW"]
    fn GetObject(h: *mut c_void, n: i32, buf: *mut c_void) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
}

/// Resolved pointers for this module's icon group: (id, instance).
struct Group {
    id: *const u16,
    inst: *mut c_void,
}

thread_local! {
    static LAST_ERROR: std::cell::Cell<Option<&'static str>> =
        const { std::cell::Cell::new(None) };
}

extern "system" fn enum_icon_group(
    module: *mut c_void,
    _ty: *mut u16,
    name: *mut u16,
    lparam: *mut c_void,
) -> i32 {
    let out = unsafe { &mut *(lparam as *mut Option<Group>) };
    // First one wins, then stop: a multi-group exe is not something we produce,
    // and the first group is the one `DefaultIcon` points at.
    if out.is_none() {
        *out = Some(Group { id: name as *const u16, inst: module });
    }
    1 // TRUE = stop enumerating
}

/// Find this module's icon group resource, so the id is read from the binary
/// rather than assumed. `winresource` chooses it, and hard-coding "1" would be
/// a silent break the first time that changed.
fn find_icon_group() -> Option<Group> {
    let inst = unsafe { GetModuleHandleW(std::ptr::null()) };
    if inst.is_null() {
        return None;
    }
    let mut found: Option<Group> = None;
    unsafe {
        EnumResourceNamesW(
            inst,
            RT_GROUP_ICON,
            enum_icon_group,
            &mut found as *mut Option<Group> as *mut c_void,
        );
    }
    found
}

/// Load one size of the application icon from our own embedded resources.
fn load_icon(group: &Group, size: i32) -> Option<*mut c_void> {
    if size <= 0 {
        return None;
    }
    let h = unsafe {
        LoadImageW(
            group.inst,
            group.id,
            IMAGE_ICON,
            size,
            size,
            LR_DEFAULTCOLOR,
        )
    };
    if h.is_null() {
        None
    } else {
        Some(h)
    }
}

/// This process's main visible window, or `None`.
///
/// `EnumWindows` hands the callback only a handle and an LPARAM, and the LPARAM
/// is already carrying our pid, so the found handle goes through a thread-local
/// rather than being smuggled through the same pointer.
fn our_window() -> Option<*mut c_void> {
    use std::cell::Cell;
    thread_local! {
        static HANDLE: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
    }

    extern "system" fn cb(hwnd: *mut c_void, lparam: *mut c_void) -> i32 {
        let me = unsafe { &*(lparam as *const u32) };
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        if pid != *me {
            return 1;
        }
        // Visible, and actually a window with a title: winit creates
        // invisible and message-only helper windows that are not the one to
        // icon, and putting the icon on one of those does nothing visible.
        if unsafe { IsWindowVisible(hwnd) } != 0 && unsafe { GetWindowTextLengthW(hwnd) } > 0 {
            HANDLE.with(|h| h.set(hwnd));
            return 0; // stop
        }
        1
    }

    let me = std::process::id();
    HANDLE.with(|h| h.set(std::ptr::null_mut()));
    unsafe { EnumWindows(cb, &me as *const u32 as *mut c_void) };
    let h = HANDLE.with(|h| h.get());
    if h.is_null() {
        None
    } else {
        Some(h)
    }
}

/// Set the application icon on this process's window.
///
/// Best-effort and safe to call repeatedly: it does nothing at all unless it
/// finds a window *and* an icon group, so calling it on the first few frames
/// (in case the window is not up yet) costs nothing.
///
/// Returns true if the window ended up with a big icon, which is what the
/// taskbar actually reads.
pub fn apply() -> bool {
    LAST_ERROR.with(|e| e.set(None));
    let Some(hwnd) = our_window() else {
        LAST_ERROR.with(|e| e.set(Some("no visible window for this process")));
        return false;
    };
    let Some(group) = find_icon_group() else {
        LAST_ERROR.with(|e| e.set(Some("no RT_GROUP_ICON in our own resources")));
        return false;
    };

    let metric = |i: i32| unsafe { GetSystemMetrics(i) };
    let big = load_icon(&group, metric(SM_CXICON));
    let small = load_icon(&group, metric(SM_CXSMICON));

    // The class icons matter as much as the window's: a window created later
    // (a dialog, a new viewport) inherits from the class, and the class is the
    // last link in the shell's fallback chain before its own default.
    if let Some(b) = big {
        unsafe { SetClassLongPtrW(hwnd, GCLP_HICON, b) };
    }
    if let Some(s) = small {
        unsafe { SetClassLongPtrW(hwnd, GCLP_HICONSM, s) };
        // HICONSM2 is the "small at 2x DPI" slot, which is the one that makes a
        // taskbar button look right on a HiDPI display.
        if let Some(s2) = load_icon(&group, metric(SM_CXSMICON2).max(metric(SM_CXSMICON))) {
            unsafe { SetClassLongPtrW(hwnd, GCLP_HICONSM2, s2) };
        }
    }

    if let Some(b) = big {
        unsafe { SendMessageW(hwnd, WM_SETICON, ICON_BIG, b) };
    }
    if let Some(s) = small {
        unsafe { SendMessageW(hwnd, WM_SETICON, ICON_SMALL, s) };
        if let Some(s2) = load_icon(&group, metric(SM_CXSMICON2).max(metric(SM_CXSMICON))) {
            unsafe { SendMessageW(hwnd, WM_SETICON, ICON_SMALL2, s2) };
        }
    }

    // `WM_SETICON` is the documented way, but it is a *message*, so it only works
    // if the window procedure passes it to `DefWindowProc` — and this window's
    // procedure is winit's, not ours. `SetWindowLongPtr` writes the same slot
    // directly and is documented as unsupported for `GWL_HICON` only on 32-bit,
    // where the value does not fit a `LONG`; on x64 it is the escape hatch. So:
    // try the message, then check, then force it.
    //
    // Whichever works, the *class* icon above is the one guaranteed to be
    // honoured, because the shell's lookup order is window icon, then class
    // icon, then its own default.
    if let Some(b) = big {
        if unsafe { GetWindowLongPtrW(hwnd, GWL_HICON) }.is_null() {
            unsafe { SetWindowLongPtrW(hwnd, GWL_HICON, b) };
            if unsafe { GetWindowLongPtrW(hwnd, GWL_HICON) }.is_null() {
                LAST_ERROR.with(|e| {
                    e.set(Some("GWL_HICON stayed null after WM_SETICON + SetWindowLongPtr"))
                });
            }
        }
    }
    if let Some(s) = small {
        if unsafe { GetWindowLongPtrW(hwnd, GWL_HICONSM) }.is_null() {
            unsafe { SetWindowLongPtrW(hwnd, GWL_HICONSM, s) };
        }
    }

    // A title bar does not re-read its icon when one arrives late. The non-client
    // area is built once, when the window is first shown, and caches what it
    // found then — which, this early in a launch, is the shell default. So the
    // icon has to be set *and* the frame has to be told to redraw, or the title
    // bar keeps drawing the old one no matter how correct the handle is.
    //
    // `WM_SETFRAMEICON` is the message specifically about the frame's icon, and
    // is what a title bar consults in preference to the plain window icon.
    if let Some(b) = big {
        unsafe { SendMessageW(hwnd, WM_SETFRAMEICON, ICON_BIG, b) };
    }
    if let Some(s) = small {
        unsafe { SendMessageW(hwnd, WM_SETFRAMEICON, ICON_SMALL, s) };
    }
    unsafe {
        RedrawWindow(
            hwnd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            RDW_FRAME | RDW_INVALIDATE | RDW_ALLCHILDREN | RDW_UPDATENOW,
        );
    }

    has_big_icon()
}

/// True if the window has a big icon — the taskbar's actual requirement.
///
/// Kept separate from `apply` so the check can be run against a live window
/// rather than trusting the write.
pub fn has_big_icon() -> bool {
    our_window()
        .map(|h| !unsafe { GetWindowLongPtrW(h, GWL_HICON) }.is_null())
        .unwrap_or(false)
}

/// Why the last [`apply`] bailed out, if it did. `None` means it did not bail.
pub fn last_error() -> Option<&'static str> {
    LAST_ERROR.with(|e| e.get())
}

/// True if the window *class* carries a big icon.
///
/// This is the fallback the shell reaches when the window has none of its own,
/// and on this build it is the one that actually sticks — see `apply`.
pub fn class_has_big_icon() -> bool {
    our_window()
        .map(|h| !unsafe { GetClassLongPtrW(h, GCLP_HICON) }.is_null())
        .unwrap_or(false)
}

/// Pixel size of the icon currently on the window, for diagnostics.
pub fn big_icon_size() -> Option<(i32, i32)> {
    let hwnd = our_window()?;
    let hicon = unsafe { GetWindowLongPtrW(hwnd, GWL_HICON) };
    if hicon.is_null() {
        return None;
    }
    let mut ii = ICONINFO {
        f_icon: 0,
        x_hotspot: 0,
        y_hotspot: 0,
        hbm_mask: std::ptr::null_mut(),
        hbm_color: std::ptr::null_mut(),
    };
    if unsafe { GetIconInfo(hicon, &mut ii) } == 0 || ii.hbm_color.is_null() {
        return None;
    }
    // BITMAPINFOHEADER is 40 bytes; width and height are the first two i32s.
    let mut hdr = [0u8; 40];
    unsafe { GetObject(ii.hbm_color, 40, hdr.as_mut_ptr() as *mut c_void) };
    let w = i32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]);
    let h = i32::from_le_bytes([hdr[8], hdr[9], hdr[10], hdr[11]]);
    Some((w, h.abs()))
}
