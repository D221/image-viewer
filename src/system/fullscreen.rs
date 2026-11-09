//! "Cover" fullscreen: the same picture as F11, without telling the shell the
//! window is fullscreen.
//!
//! `ViewportCommand::Fullscreen(true)` reaches winit as
//! `Fullscreen::Borderless`, and that path — not the exclusive one — is what
//! swaps the window to `WS_POPUP` and calls
//! `ITaskbarList2::MarkFullscreenWindow` (see
//! `winit-0.30.13/src/platform_impl/windows/window.rs`, `set_fullscreen`;
//! `ChangeDisplaySettingsExW` sits behind `Fullscreen::Exclusive` and is never
//! reached from here). Windows then treats the window as owning the display,
//! and on some GPU/driver/monitor combinations taking the output over — and
//! giving it back — costs several seconds of black with the cursor gone. That
//! is the output itself not scanning, so nothing this app paints can cover it.
//!
//! The experiments here draw the same rectangle by other means, so the three
//! candidate triggers can be told apart on the machine that blanks:
//!
//! * [`FsMode::Cover`]  — undecorated window, exactly the monitor rect, no
//!   fullscreen request. Is the shell's fullscreen *state* the trigger?
//! * [`FsMode::Inset`]  — the same, one pixel short on every edge. Is it the
//!   rectangle reaching every monitor edge that Windows' detection keys on?
//! * [`FsMode::Resize`] — decorated window, exactly the monitor rect, style
//!   untouched. Is it a window covering the display at all?
//!
//! All three keep the shell out of it: `MarkFullscreenWindow` is never called,
//! and the window is a normal window that just happens to be monitor-sized.
//! [`FsMode::Real`] is the shipped behaviour and the control.
//!
//! Chosen in **Settings → Graphics & window → Fullscreen mode** and stored in
//! `Config::fullscreen_mode`, so it survives restarts. `Inset` is the default:
//! the same borderless fullscreen as `real`, pulled in by one pixel per monitor
//! edge, which avoids the display re-sync that blanks some GPU/driver/monitor
//! combinations. The inset is tunable with `IMAGE_VIEWER_FULLSCREEN_INSET`
//! pixels (default 1) for experimentation only.
//!
//! `Safe` is the fallback: decorated, maximized, panels hidden — nothing
//! blanks, but the title bar and taskbar remain.
//!
//! The whole window's frame is saved with `GetWindowPlacement` before any of
//! it happens, so leaving restores the user's own window — size, position and
//! maximized state — rather than a rect this module invented.

use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// How F11 fullscreen is realised. See the module doc for the experiment each
/// variant runs. Stored in `Config::fullscreen_mode` and serialized with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FsMode {
    /// Undecorated window sized to the monitor minus one pixel per edge — true
    /// borderless fullscreen without the display re-sync that blanks some
    /// GPU/driver/monitor combinations. The default.
    Inset,
    /// `ViewportCommand::Fullscreen` - the shipped behaviour.
    Real,
    /// Undecorated window sized to the monitor, no fullscreen request.
    Cover,
    /// Decorated window sized to the monitor; the window style is untouched.
    Resize,
    /// Decorated, maximized, panels hidden: the fullscreen *feel* without an
    /// undecorated window. Nothing blanks, but the title bar and taskbar
    /// remain.
    Safe,
}

impl Default for FsMode {
    fn default() -> Self {
        Self::Inset
    }
}

impl FsMode {
    /// Short name for the status line and the settings dropdown, so the
    /// machine under test says which mode it is running without anyone having
    /// to read the config file.
    pub fn label(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Cover => "cover",
            Self::Inset => "inset",
            Self::Resize => "resize",
            Self::Safe => "safe",
        }
    }

    /// Pixels to pull the window in from each monitor edge.
    pub fn inset(self) -> i32 {
        if self != Self::Inset {
            return 0;
        }
        std::env::var("IMAGE_VIEWER_FULLSCREEN_INSET")
            .ok()
            .and_then(|v| v.trim().parse::<i32>().ok())
            .filter(|px| (0..64).contains(px))
            .unwrap_or(1)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: i32,
    y: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// `WINDOWPLACEMENT`. `rc_device` is part of the struct since NT 4 and is what
/// makes `length` 60 on x64 — `GetWindowPlacement` rejects anything else.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct WindowPlacement {
    length: u32,
    flags: u32,
    show_cmd: u32,
    min_position: Point,
    max_position: Point,
    normal_position: Rect,
    device: Rect,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct MonitorInfo {
    cb_size: u32,
    monitor: Rect,
    work: Rect,
    flags: u32,
}

unsafe extern "system" {
    fn EnumWindows(
        callback: Option<unsafe extern "system" fn(isize, isize) -> i32>,
        param: isize,
    ) -> i32;
    fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
    fn GetCurrentProcessId() -> u32;
    fn GetWindowLongW(hwnd: isize, index: i32) -> i32;
    fn SetWindowLongW(hwnd: isize, index: i32, value: i32) -> i32;
    fn GetWindowPlacement(hwnd: isize, placement: *mut WindowPlacement) -> i32;
    fn SetWindowPlacement(hwnd: isize, placement: *const WindowPlacement) -> i32;
    fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
    fn SetWindowPos(hwnd: isize, after: isize, x: i32, y: i32, cx: i32, cy: i32, flags: u32)
    -> i32;
    fn MonitorFromWindow(hwnd: isize, flags: u32) -> isize;
    fn GetMonitorInfoW(monitor: isize, info: *mut MonitorInfo) -> i32;
}

const GWL_STYLE: i32 = -16;
const SWP_NOSIZE: u32 = 0x0001;
const SWP_NOMOVE: u32 = 0x0002;
const SWP_NOZORDER: u32 = 0x0004;
const SWP_FRAMECHANGED: u32 = 0x0020;
const SW_RESTORE: i32 = 9;
const SW_SHOWMAXIMIZED: u32 = 3;
const MONITOR_DEFAULTTONEAREST: u32 = 2;
const WS_CAPTION: i32 = 0x00C0_0000;
const WS_BORDER: i32 = 0x0080_0000;
const WS_SYSMENU: i32 = 0x0008_0000;
const WS_THICKFRAME: i32 = 0x0004_0000;
const WS_MINIMIZEBOX: i32 = 0x0002_0000;
const WS_MAXIMIZEBOX: i32 = 0x0001_0000;
const WS_POPUP: i32 = 0x8000_0000u32 as i32;
const WS_VISIBLE: i32 = 0x1000_0000;

/// The window as it was before the cover went on. Held here rather than in the
/// app so the restore cannot be half-remembered: one place saves it and the
/// same place reads it back.
#[derive(Clone, Copy)]
struct Saved {
    placement: WindowPlacement,
    style: i32,
}

static SAVED: Mutex<Option<Saved>> = Mutex::new(None);

/// The first top-level window owned by this process — eframe makes exactly one,
/// and it is found by owning PID rather than title because the title names the
/// current file and changes constantly.
unsafe extern "system" fn find_own_window(hwnd: isize, out: isize) -> i32 {
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut pid);
        if pid == GetCurrentProcessId() {
            *(out as *mut isize) = hwnd;
            return 0;
        }
    }
    1
}

fn own_window() -> isize {
    let mut found: isize = 0;
    unsafe {
        EnumWindows(Some(find_own_window), &mut found as *mut isize as isize);
    }
    found
}

/// Put the window over the monitor the way `mode` asks for, without a
/// fullscreen request. Returns whether the window is (now) covering.
pub fn cover_monitor(mode: FsMode) -> bool {
    let mut saved = SAVED.lock().unwrap();
    if saved.is_some() {
        return true;
    }
    if mode == FsMode::Real || mode == FsMode::Safe {
        return false;
    }
    unsafe {
        let hwnd = own_window();
        if hwnd == 0 {
            return false;
        }
        let mut placement = WindowPlacement {
            length: std::mem::size_of::<WindowPlacement>() as u32,
            ..WindowPlacement::default()
        };
        if GetWindowPlacement(hwnd, &mut placement) == 0 {
            return false;
        }
        let style = GetWindowLongW(hwnd, GWL_STYLE);
        // **A maximized window ignores `SetWindowPos`**: Windows clamps it back
        // to the work area, which stops short of the taskbar. Restore first;
        // the saved placement still carries `SW_SHOWMAXIMIZED`, so leaving
        // gives the maximized window back.
        if placement.show_cmd == SW_SHOWMAXIMIZED {
            ShowWindow(hwnd, SW_RESTORE);
        }
        let mut monitor = MonitorInfo {
            cb_size: std::mem::size_of::<MonitorInfo>() as u32,
            ..MonitorInfo::default()
        };
        if GetMonitorInfoW(MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST), &mut monitor) == 0 {
            return false;
        }
        let inset = mode.inset();
        let left = monitor.monitor.left + inset;
        let top = monitor.monitor.top + inset;
        let width = (monitor.monitor.right - inset - left).max(1);
        let height = (monitor.monitor.bottom - inset - top).max(1);
        // `Resize` keeps the frame: the point of that arm is that only the
        // window's rectangle changes, so the style is not touched at all.
        if mode != FsMode::Resize {
            let undecorated = (style
                & !(WS_CAPTION
                    | WS_BORDER
                    | WS_THICKFRAME
                    | WS_MINIMIZEBOX
                    | WS_MAXIMIZEBOX
                    | WS_SYSMENU))
                | WS_POPUP
                | WS_VISIBLE;
            SetWindowLongW(hwnd, GWL_STYLE, undecorated);
        }
        if SetWindowPos(hwnd, 0, left, top, width, height, SWP_NOZORDER | SWP_FRAMECHANGED) == 0 {
            if mode != FsMode::Resize {
                SetWindowLongW(hwnd, GWL_STYLE, style);
            }
            return false;
        }
        *saved = Some(Saved { placement, style });
        true
    }
}

/// Give the window back exactly as it was: same frame, same size, same
/// position, same maximized state.
pub fn uncover_monitor() -> bool {
    let mut saved = SAVED.lock().unwrap();
    let Some(entry) = saved.take() else {
        return false;
    };
    unsafe {
        let hwnd = own_window();
        if hwnd == 0 {
            *saved = Some(entry);
            return false;
        }
        SetWindowLongW(hwnd, GWL_STYLE, entry.style);
        let placement = &entry.placement;
        SetWindowPlacement(hwnd, placement);
        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
        true
    }
}

/// Whether a cover is holding the window, for callers that need to know
/// without trusting their own bookkeeping.
pub fn monitor_covered() -> bool {
    SAVED.lock().unwrap().is_some()
}
