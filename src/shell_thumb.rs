//! Module `shell_thumb`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use shell_thumb::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Grid cells from Explorer's thumbnail cache
// ---------------------------------------------------------------------------
//
// Explorer keeps a thumbnail of every picture it has ever shown, and a folder
// the user has browsed in File Explorer therefore already holds most of the
// cells this grid is about to decode. `IShellItemImageFactory::GetImage` with
// `SIIGBF_INCACHEONLY` reads that cache and **never causes an extraction**: a
// miss is a miss, so this cannot make the cold case slower, cannot start a
// decode behind our back, and cannot write anything into Explorer's cache.
//
// What it is for is the *first* visit to a folder of large scans: the decode's
// floor is 700-900 ms per cell and nothing in this crate can move it (a baseline
// JPEG's
// entropy decode is the floor for any conformant decoder). If Explorer already
// has the cell, that wait is not paid at all. A folder *we* have visited is
// served by our own cache first, so the shell is only asked on a miss — see
// `load_grid_thumbnail`.
//
// **A shell cell is never written into our thumbnail cache.** An entry there has
// to be the decode it claims to be (see `tests/thumb_cache.rs`), and the shell's
// picture is a different decode by a different decoder; a "cache hit" that is
// sometimes the shell's bytes and sometimes ours would make that claim false.

/// Convert the 32-bit DIB `GetImage` hands back into the RGBA this crate speaks.
///
/// The buffer is BGRA in memory order. **Its alpha channel is not trustworthy**:
/// a shell thumbnail is often drawn into a 32-bit bitmap whose fourth byte is
/// left at zero, and honouring that paints the whole cell invisible. An image
/// whose alpha is *entirely* zero is treated as opaque; one with any real alpha
/// keeps it.
///
/// Whether the shell's alpha is straight or premultiplied is **not settled here**
/// — it makes no difference to an opaque photograph, which is what a grid cell
/// is, and un-premultiplying on a guess would darken or blow out the transparent
/// edge of the few cells that are not.
pub fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    let mut any_alpha = false;
    for px in bgra.chunks_exact(4) {
        if px[3] != 0 {
            any_alpha = true;
            break;
        }
    }
    let mut out = Vec::with_capacity(bgra.len());
    for px in bgra.chunks_exact(4) {
        out.push(px[2]);
        out.push(px[1]);
        out.push(px[0]);
        out.push(if any_alpha { px[3] } else { 255 });
    }
    out
}

/// The path form the shell accepts, from one that may carry the verbatim prefix.
///
/// Two things this is *not*. It is not `canonicalize`, which is the obvious way
/// to make a path absolute and the wrong one here: it returns a verbatim
/// (`\\?\C:\...`) path, and `SHCreateItemFromParsingName` rejects those with
/// `E_INVALIDARG`, so every file misses, which reads exactly like "Explorer has
/// never thumbnailed anything". And it is not a string round-trip: the prefix is
/// matched as a path *prefix*, so a file name that is not valid UTF-8 survives.
///
/// Verbatim paths exist to say "do not parse this"; the shell is a component that
/// parses it anyway, so the prefix is dropped to the ordinary form: `\\?\C:\a`
/// becomes `C:\a`, and `\\?\UNC\srv\share\a` becomes `\\srv\share\a`.
pub fn shell_api_path(path: &Path) -> Option<PathBuf> {
    // The prefix is reduced to an owned description first: `PrefixComponent`
    // borrows the path it came from, and the arms below return that path.
    enum Verbatim {
        Disk(char),
        Unc(std::ffi::OsString, std::ffi::OsString),
    }
    let abs = std::path::absolute(path).ok()?;
    let verbatim = match abs.components().next() {
        Some(std::path::Component::Prefix(p)) => match p.kind() {
            std::path::Prefix::VerbatimDisk(letter) => Some(Verbatim::Disk(letter as char)),
            std::path::Prefix::VerbatimUNC(server, share) => {
                Some(Verbatim::Unc(server.to_os_string(), share.to_os_string()))
            }
            // `Disk`, `UNC` and `DeviceNS` are already what the shell wants, and
            // `Verbatim` (`\\?\Volume{...}`) has no shell-friendly form at all:
            // both are left alone, and a device path fails into a decode.
            _ => None,
        },
        _ => None,
    };
    let mut out = match verbatim {
        None => return Some(abs),
        Some(Verbatim::Disk(letter)) => PathBuf::from(format!("{letter}:\\")),
        Some(Verbatim::Unc(server, share)) => {
            let mut root = PathBuf::from(r"\\");
            root.push(server);
            root.push(share);
            root
        }
    };
    // Everything that remains is an ordinary name. The old prefix and root are
    // replaced by the one just built, and a verbatim path cannot contain `..`.
    for c in abs.components() {
        if let std::path::Component::Normal(name) = c {
            out.push(name);
        }
    }
    Some(out)
}

/// How much larger than its own pixels a shell thumbnail may be painted.
///
/// **The shell returns what its cache holds, not what was asked for.** Measured on
/// this box: after an extract at 32, an ask for 256 comes back 32x24 — the size
/// argument is a hint about which entry to look for, not a scale. Painted into a
/// 150-point cell that is a smear, and a smear is worse than the decode it saved,
/// so a cell smaller than half the target is refused and the decode runs.
///
/// Half is the line and not a guess: Explorer's 256 entry is exact, 96 gives a
/// 1.6x upscale and is under the line, and 32/48 are the entries a folder browsed
/// in Small-icons view has — a folder whose cache cannot serve a grid cell anyway.
pub const SHELL_MIN_CELL_UPSCALE: u32 = 2;

/// Is a shell thumbnail of `dims` worth painting into a `target`-sized cell?
///
/// Pure, and tested without the shell in the loop: the interesting inputs are the
/// sizes Explorer's cache actually holds (32, 48, 96, 256) against a 256 target,
/// and a rule that only ever saw its own 256 would pass on any threshold at all.
pub fn shell_cell_is_usable(dims: (u32, u32), target: u32) -> bool {
    dims.0.max(dims.1).saturating_mul(SHELL_MIN_CELL_UPSCALE) >= target
}

/// Read the thumbnail Explorer already has for `path`, at most `target` pixels
/// on the long side.
///
/// **Cache-only, always.** The app never asks the shell to make a thumbnail:
/// that is an extraction, it would run a decoder we do not control on the paint
/// path's behalf, and it is the cold case's job to decode for itself.
#[cfg(windows)]
pub fn shell_thumbnail(path: &Path, target: u32) -> Option<ImageData> {
    shell_thumbnail_flagged(path, target, true)
}

/// The same call with the cache-only flag under the caller's control.
///
/// `cache_only: false` **causes an extraction**, which is the one thing the app
/// must never do — it exists for a test that has to put a file into Explorer's
/// cache before it can prove the cache-only read works. A test that could not
/// make an entry could only assert that the function returns `None`, which is
/// also true of a function that is broken in every possible way.
#[cfg(windows)]
pub fn shell_thumbnail_allowing_extraction(path: &Path, target: u32) -> Option<ImageData> {
    shell_thumbnail_flagged(path, target, false)
}

#[cfg(windows)]
pub(crate) fn shell_thumbnail_flagged(path: &Path, target: u32, cache_only: bool) -> Option<ImageData> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::SIZE;
    use windows_sys::Win32::UI::Shell::{
        SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY,
    };

    if !grid_target(target) {
        return None;
    }
    let start = std::time::Instant::now();
    // The shell resolves names against *its* current directory, which is not
    // necessarily ours, so the path has to be absolute — and **not**
    // `canonicalize`'d: that returns a verbatim (`\\?\C:\...`) path, which
    // `SHCreateItemFromParsingName` rejects with `E_INVALIDARG` (0x80070057).
    // Every file then "misses", which is indistinguishable from Explorer having
    // thumbnailed nothing at all — measured, and the first version of this code
    // did exactly that. `std::path::absolute` makes the path absolute without
    // the prefix and without touching the file system, so it is also cheaper
    // (no syscall per cell).
    let abs = shell_api_path(path)?;
    let wide: Vec<u16> = abs
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let _com = ComApartment::init()?;
    let mut factory: *mut std::ffi::c_void = std::ptr::null_mut();
    let hr = unsafe {
        SHCreateItemFromParsingName(
            wide.as_ptr(),
            std::ptr::null_mut(),
            &IID_SHELL_ITEM_IMAGE_FACTORY,
            &mut factory,
        )
    };
    if hr < 0 || factory.is_null() {
        return None;
    }

    // `THUMBNAILONLY` matters: without it the shell may hand back the file
    // type's *icon*, which is a picture of the wrong thing. A miss must be a
    // miss, so the caller falls back to its own decode.
    let mut flags = SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK;
    if cache_only {
        flags |= SIIGBF_INCACHEONLY;
    }
    let mut hbm: HBITMAP = std::ptr::null_mut();
    let hr = unsafe {
        get_image(
            factory,
            SIZE {
                cx: target as i32,
                cy: target as i32,
            },
            flags,
            &mut hbm,
        )
    };
    unsafe { release(factory) };
    if hr < 0 || hbm.is_null() {
        return None;
    }
    let data = unsafe { shell_bitmap_to_image(hbm, target, start) }?;
    // The size argument is a hint about which cache entry to look for, not a
    // scale: what comes back is what Explorer holds. See
    // `SHELL_MIN_CELL_UPSCALE`.
    if !shell_cell_is_usable((data.width, data.height), target) {
        return None;
    }
    Some(data)
}

/// Non-Windows stub: there is no shell cache to read, so the caller decodes.
#[cfg(not(windows))]
pub fn shell_thumbnail(_path: &Path, _target: u32) -> Option<ImageData> {
    None
}

#[cfg(not(windows))]
pub fn shell_thumbnail_allowing_extraction(_path: &Path, _target: u32) -> Option<ImageData> {
    None
}

/// COM, initialised for the calling thread.
///
/// The shell's image factory lives in an apartment, and the worker threads that
/// call this are plain `std::thread`s with none. `CoInitializeEx` is refcounted
/// per thread, so the guard releases exactly the reference it took.
///
/// `RPC_E_CHANGED_MODE` is **not** a failure: it means the thread is already in
/// an apartment (a host that initialised MTA for itself), the call works through
/// COM's proxy from there, and the reference is not ours to release.
#[cfg(windows)]
pub(crate) struct ComApartment {
    uninit: bool,
}

#[cfg(windows)]
impl ComApartment {
    fn init() -> Option<Self> {
        use windows_sys::Win32::Foundation::RPC_E_CHANGED_MODE;
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let hr = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if hr >= 0 {
            Some(ComApartment { uninit: true })
        } else if hr == RPC_E_CHANGED_MODE {
            Some(ComApartment { uninit: false })
        } else {
            None
        }
    }
}

#[cfg(windows)]
impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.uninit {
            unsafe { windows_sys::Win32::System::Com::CoUninitialize() };
        }
    }
}

/// `IShellItemImageFactory`'s ABI, declared here instead of taking on the
/// `windows` crate's COM wrappers for one call.
///
/// `windows-sys` — already a dependency — has every function and type this
/// needs; the one thing it does not carry is this interface, which is three
/// `IUnknown` slots and one `GetImage`. The IID and the slot order are the SDK's
/// and not ours, and **the way to be sure they are right is a test that asks the
/// shell for a real thumbnail and gets one**, not a comment: a wrong IID is
/// `E_NOINTERFACE` and a wrong slot order is a crash, and both are visible the
/// first time `tests/thumb_cache.rs` runs. Do not "simplify" this into a
/// hand-counted pointer offset.
#[cfg(windows)]
pub(crate) const IID_SHELL_ITEM_IMAGE_FACTORY: windows_sys::core::GUID = windows_sys::core::GUID {
    data1: 0xbcc1_8b79,
    data2: 0xba16,
    data3: 0x442f,
    data4: [0x80, 0xc4, 0x8a, 0x59, 0xc3, 0x0c, 0x46, 0x3b],
};

/// `HBITMAP` is `windows-sys`'s own type, not one written here: the one thing
/// this interface needs that the crate does not carry is the *interface*, and
/// everything else — the bitmap handle, `SIZE`, the flags — comes from the
/// crate so there is nothing to transcribe.
#[cfg(windows)]
use windows_sys::Win32::Graphics::Gdi::HBITMAP;

#[cfg(windows)]
#[repr(C)]
pub(crate) struct ImageFactoryVtbl {
    query_interface: unsafe extern "system" fn(
        *mut std::ffi::c_void,
        *const windows_sys::core::GUID,
        *mut *mut std::ffi::c_void,
    ) -> i32,
    add_ref: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    release: unsafe extern "system" fn(*mut std::ffi::c_void) -> u32,
    get_image: unsafe extern "system" fn(
        *mut std::ffi::c_void,
        windows_sys::Win32::Foundation::SIZE,
        i32,
        *mut HBITMAP,
    ) -> i32,
}

#[cfg(windows)]
pub(crate) unsafe fn get_image(
    factory: *mut std::ffi::c_void,
    size: windows_sys::Win32::Foundation::SIZE,
    flags: i32,
    out: *mut HBITMAP,
) -> i32 {
    let vtbl = *(factory as *mut *const ImageFactoryVtbl);
    ((*vtbl).get_image)(factory, size, flags, out)
}

#[cfg(windows)]
pub(crate) unsafe fn release(factory: *mut std::ffi::c_void) {
    let vtbl = *(factory as *mut *const ImageFactoryVtbl);
    ((*vtbl).release)(factory);
}

/// Turn the shell's `HBITMAP` into pixels, and destroy it.
///
/// The bitmap is read as a top-down 32bpp DIB. A *negative* `biHeight` is what
/// makes it top-down: with a positive one the rows arrive bottom-up and the cell
/// is upside down — a bug that shows only on the cells, which nobody compared
/// against the file.
#[cfg(windows)]
pub(crate) unsafe fn shell_bitmap_to_image(
    hbm: HBITMAP,
    target: u32,
    start: std::time::Instant,
) -> Option<ImageData> {
    use windows_sys::Win32::Graphics::Gdi::{
        DeleteObject, GetDC, GetDIBits, GetObjectW, ReleaseDC, BITMAP, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HGDIOBJ,
    };

    let mut bm: BITMAP = std::mem::zeroed();
    let got = GetObjectW(
        hbm as HGDIOBJ,
        std::mem::size_of::<BITMAP>() as i32,
        &mut bm as *mut BITMAP as *mut std::ffi::c_void,
    );
    // The size cap is hygiene, not paranoia: `SIIGBF_BIGGERSIZEOK` lets the
    // shell return any cached size, and a corrupt or hostile cache entry must
    // not become a multi-gigabyte allocation on a worker.
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
        DeleteObject(hbm as HGDIOBJ);
        return None;
    }
    let (w, h) = (bm.bmWidth as u32, bm.bmHeight as u32);
    if u64::from(w) * u64::from(h) > 16 * 1024 * 1024 {
        DeleteObject(hbm as HGDIOBJ);
        return None;
    }

    let mut bi: BITMAPINFO = std::mem::zeroed();
    bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bi.bmiHeader.biWidth = w as i32;
    bi.bmiHeader.biHeight = -(h as i32);
    bi.bmiHeader.biPlanes = 1;
    bi.bmiHeader.biBitCount = 32;
    bi.bmiHeader.biCompression = BI_RGB;
    let mut bgra = vec![0u8; w as usize * h as usize * 4];
    let dc = GetDC(std::ptr::null_mut());
    let lines = GetDIBits(
        dc,
        hbm,
        0,
        h,
        bgra.as_mut_ptr() as *mut std::ffi::c_void,
        &mut bi,
        DIB_RGB_COLORS,
    );
    ReleaseDC(std::ptr::null_mut(), dc);
    DeleteObject(hbm as HGDIOBJ);
    if lines == 0 {
        return None;
    }

    let mut data = ImageData {
        width: w,
        height: h,
        rgba: bgra_to_rgba(&bgra),
        load_time: start.elapsed(),
    };
    // The same rule the decode path follows: whatever the decoder produced, the
    // cell is never larger than its target. A cached 1024px thumbnail would
    // otherwise cost 4 MB of texture per cell against the 512-entry cap.
    if data.width.max(data.height) > target {
        data = downscale_image(&data, target);
    }
    Some(data)
}

/// How many consecutive misses on one folder before Explorer is left alone.
///
/// **A miss is not free**, and this number is what bounds the damage: measured on
/// this box, a cache-only miss is ~9 ms p50 (24 cells, one thread) against the
/// 700-900 ms decode of a large scan — nothing — and against the 124 ms decode of
/// a small photograph it is 7%, which on a 5000-file folder is a minute of worker
/// time spent to be told "no". A folder Explorer has never visited misses on
/// *every* cell, so an unlimited ask is a per-cell tax on exactly the folders this
/// cannot help.
///
/// Four asks is enough to tell "not thumbnailed" from "not thumbnailed *yet*":
/// Explorer fills a folder in one pass, so a folder that has any cell in its
/// cache hits on the first ask. The lever if a real folder needs more slack is
/// this constant, not the policy.
pub const SHELL_MISS_GIVEUP: u32 = 4;

/// Per-folder record of what the shell has answered, so the asking can stop.
///
/// Keyed by **folder**, not by file, because that is the granularity Explorer's
/// cache works at: it thumbnails a folder, not a file, so a folder that missed
/// four times is a folder that will keep missing. A hit clears the record — a
/// folder with one thumbnail in it is a folder worth asking about, which is also
/// what makes a partially-cached folder behave.
///
/// Pure, and tested as a policy rather than through the shell call: "give up,
/// but not on a folder that has answered" is the kind of rule that looks right
/// while being wrong in a direction no measurement covers.
#[derive(Debug)]
pub struct ShellAsk {
    folders: Vec<(PathBuf, u32)>,
}

impl ShellAsk {
    /// Folders remembered at once. A grid works through one folder at a time and
    /// an archive is one folder, so this is slack for a folder switch that is
    /// still draining; an unbounded map of every folder ever browsed is not
    /// worth the memory for a policy this coarse.
    pub const MAX_FOLDERS: usize = 8;

    /// The empty record: nothing asked, nothing given up on.
    pub const fn new() -> Self {
        ShellAsk {
            folders: Vec::new(),
        }
    }

    /// Is the shell still worth asking about `path`?
    pub fn wants(&self, path: &Path) -> bool {
        match self.entry(path) {
            Some((_, misses)) => *misses < SHELL_MISS_GIVEUP,
            None => true,
        }
    }

    /// Record what the shell answered for `path`.
    pub fn record(&mut self, path: &Path, hit: bool) {
        let folder = folder_of(path);
        if let Some(i) = self.folders.iter().position(|(f, _)| *f == folder) {
            let misses = &mut self.folders[i].1;
            *misses = if hit { 0 } else { *misses + 1 };
            return;
        }
        if self.folders.len() >= Self::MAX_FOLDERS {
            // Oldest first: the least recently *added* folder is the one whose
            // record matters least, and this is a policy about the folder being
            // read right now.
            self.folders.remove(0);
        }
        self.folders.push((folder, u32::from(!hit)));
    }

    fn entry(&self, path: &Path) -> Option<&(PathBuf, u32)> {
        let folder = folder_of(path);
        self.folders.iter().find(|(f, _)| *f == folder)
    }
}

impl Default for ShellAsk {
    fn default() -> Self {
        Self::new()
    }
}

/// The folder a cell belongs to — what the shell's cache is keyed by.
pub(crate) fn folder_of(path: &Path) -> PathBuf {
    path.parent().unwrap_or(path).to_path_buf()
}

/// What the shell has said about each folder this session (see [`ShellAsk`]).
pub(crate) static SHELL_ASK: std::sync::Mutex<ShellAsk> = std::sync::Mutex::new(ShellAsk::new());

/// A poisoned lock is a worker that panicked with the record in hand; the policy
/// it holds is an optimisation, so the answer is "ask" (fail-open) rather than
/// "stop".
pub(crate) fn shell_ask_wants(path: &Path) -> bool {
    SHELL_ASK.lock().map(|g| g.wants(path)).unwrap_or(true)
}

pub(crate) fn shell_ask_record(path: &Path, hit: bool) {
    if let Ok(mut g) = SHELL_ASK.lock() {
        g.record(path, hit);
    }
}

/// Ask Explorer first, then decode, for a grid-sized target.
///
/// The shell is a *cache* read, so a miss costs a call and nothing else — but a
/// call is not nothing, so the ask is bounded per folder by [`ShellAsk`]. What
/// the call must never do is make the miss path more expensive than the decode it
/// stands in front of, which is why only grid-sized targets reach it at all.
///
/// The count of what happened lives here rather than in `load_grid_thumbnail`, so
/// there is one place that can say "the shell was asked" and one that says "it
/// answered".
pub(crate) fn shell_cell(path: &Path, target: u32) -> Option<ImageData> {
    if !grid_target(target) {
        return None;
    }
    if !shell_ask_wants(path) {
        return None;
    }
    let answer = shell_thumbnail(path, target);
    shell_ask_record(path, answer.is_some());
    match answer {
        Some(data) => {
            perf::count(perf::C_SHELL_THUMB, 1);
            Some(data)
        }
        None => {
            perf::count(perf::C_SHELL_MISS, 1);
            None
        }
    }
}

/// Grid-sized targets only: the same gate as the embedded EXIF thumbnail, and for
/// the same reason. A 256px picture is a cell; handing one to the main view would
/// be a soft full-screen image. Spelled once so the call and the bookkeeping
/// around it cannot disagree about which targets reach the shell.
pub(crate) fn grid_target(target: u32) -> bool {
    target > 0 && target <= EMBEDDED_THUMB_MAX_TARGET
}

/// The picture one grid cell shows, from the cache when it can be.
///
/// This is the whole grid-thumbnail entry point: a cache hit is a small file read,
/// a miss is [`load_thumbnail`] plus a write. Both are best-effort around the
/// edges — if the cache cannot be used the result is exactly what it was before
/// this existed.
///
/// Order is **our cache, then Explorer's, then our decode**. Ours first because
/// it is our own decode (the sharpest, and the one a warm folder already has);
/// Explorer's second because it is free when it hits and it is the only thing
/// that can serve a folder's *first* visit without paying the decode; decoding
/// last because it is the cold floor. A shell cell is returned without being
/// written to our cache — see the section comment above.
pub fn load_grid_thumbnail(path: &Path, target: u32) -> Result<ImageData, LoadError> {
    if !thumbnail_cache_enabled() {
        return match shell_cell(path, target) {
            Some(data) => Ok(data),
            None => load_thumbnail(path, target),
        };
    }
    let root = thumb_cache_root();
    match read_cached_thumbnail(&root, path, target) {
        Some(data) => {
            perf::count(perf::C_THUMB_CACHE_HIT, 1);
            Ok(data)
        }
        None => {
            perf::count(perf::C_THUMB_CACHE_MISS, 1);
            if let Some(data) = shell_cell(path, target) {
                return Ok(data);
            }
            let data = load_thumbnail(path, target)?;
            write_cached_thumbnail(&root, path, target, &data);
            Ok(data)
        }
    }
}

