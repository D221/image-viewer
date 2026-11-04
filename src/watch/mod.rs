//! Module `watcher`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use watcher::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Folder watching
// ---------------------------------------------------------------------------

// `FILE_ACTION_*` as `ReadDirectoryChangesW` reports them. Spelled out rather
// than imported so `parse_notify_buffer` and `classify_change` are testable on
// a buffer a test can hand-write; the values are the OS's, and the `windows-sys`
// constants carry the same numbers (checked by the watcher's own use of both).
pub const FILE_ACTION_ADDED: u32 = 1;
pub const FILE_ACTION_REMOVED: u32 = 2;
pub const FILE_ACTION_MODIFIED: u32 = 3;
pub const FILE_ACTION_RENAMED_OLD_NAME: u32 = 4;
pub const FILE_ACTION_RENAMED_NEW_NAME: u32 = 5;

/// Not a `FILE_ACTION_*`: the OS buffer overflowed and names were lost
/// (`ERROR_NOTIFY_ENUM_DIR`). Synthesised by the watcher so the app can answer
/// with the full pass the timer used to do, instead of silently missing the
/// change it was never told about — which is the one failure mode a
/// notification-based watcher has that a poll does not.
pub const CHANGE_OVERFLOW: u32 = 0;

/// What one OS change means to the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeKind {
    /// A file the app holds state for, whose bytes may have changed.
    Content,
    /// The folder listing changed: a file appeared, vanished, or left.
    Membership,
    /// Nothing the app can act on.
    Ignore,
    /// Events were lost; only a full comparison knows what changed.
    Overflow,
}

/// The decision table, as a pure function, because it is the one part of the
/// watcher that is easy to get subtly wrong and impossible to see: every arm
/// here is silent — a wrong `Ignore` is a file that never reloads, and a wrong
/// `Membership` is a full folder scan per keystroke of a program's save.
///
/// `known` is "this path is in the folder listing the app holds" — the
/// *unfiltered* list, so a file hidden by the filter still counts and its save
/// does not turn into a rescan. `looks_like_image` is the extension test, used
/// for names the app has never seen: it is what tells a new picture (`a.jpg`)
/// apart from a temporary write (`a.jpg.tmp`, `thumbs.db`, `~$doc.docx`).
pub fn classify_change(action: u32, known: bool, looks_like_image: bool) -> ChangeKind {
    match action {
        CHANGE_OVERFLOW => ChangeKind::Overflow,
        FILE_ACTION_ADDED => {
            if looks_like_image {
                ChangeKind::Membership
            } else {
                ChangeKind::Ignore
            }
        }
        FILE_ACTION_REMOVED | FILE_ACTION_RENAMED_OLD_NAME => {
            if known {
                ChangeKind::Membership
            } else {
                ChangeKind::Ignore
            }
        }
        // A rename **to** a name we already list is a rename-over: the bytes
        // under that path are new, so it is a content change, not a listing
        // change. That is the re-export case, and treating it as membership
        // would scan the whole folder once per save.
        FILE_ACTION_MODIFIED | FILE_ACTION_RENAMED_NEW_NAME => {
            if known {
                ChangeKind::Content
            } else if looks_like_image {
                ChangeKind::Membership
            } else {
                ChangeKind::Ignore
            }
        }
        _ => ChangeKind::Ignore,
    }
}

/// Fold one batch of OS records down to the changes worth acting on.
///
/// **A single re-export arrives as five records**, measured on this box:
/// `ADDED page.jpg.tmp`, `MODIFIED page.jpg.tmp`, `REMOVED page.jpg`,
/// `RENAMED_OLD_NAME page.jpg.tmp`, `RENAMED_NEW_NAME page.jpg`. The `REMOVED`
/// is the replace target being unlinked, not a file leaving the gallery — the
/// same name is back in the same batch. Acted on naively it is one full folder
/// rescan *per save*, which is the cost this whole feature exists to remove, so
/// a name that is removed **and** added/renamed-to within one batch keeps only
/// the record that says its bytes are new. That is also the right answer for a
/// genuine delete-then-recreate of the same name (the file is there with new
/// bytes) — the two cases are indistinguishable, and need the same response.
///
/// Exact duplicate records are dropped too; the OS is free to repeat itself and
/// each repeat would otherwise cost another `stat`.
pub fn coalesce_changes(changes: Vec<FsChange>) -> Vec<FsChange> {
    let returns: HashSet<String> = changes
        .iter()
        .filter(|c| c.action == FILE_ACTION_ADDED || c.action == FILE_ACTION_RENAMED_NEW_NAME)
        .map(|c| c.name.clone())
        .collect();
    let mut out: Vec<FsChange> = Vec::with_capacity(changes.len());
    for c in changes {
        if c.action == FILE_ACTION_REMOVED && returns.contains(&c.name) {
            continue;
        }
        if out.iter().any(|o| o.action == c.action && o.name == c.name) {
            continue;
        }
        out.push(c);
    }
    out
}

/// One parsed `FILE_NOTIFY_INFORMATION` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsChange {
    /// A `FILE_ACTION_*` above, or [`CHANGE_OVERFLOW`].
    pub action: u32,
    /// Name relative to the watched directory (with subdirectories, when the
    /// watch is recursive). UTF-16 lossily decoded: a name Windows cannot
    /// represent in UTF-8 is still a name, and dropping the whole event over
    /// one is worse than a name that does not round-trip.
    pub name: String,
}

/// Parse the buffer `ReadDirectoryChangesW` filled.
///
/// The records are `NextEntryOffset: u32, Action: u32, FileNameLength: u32`
/// followed by that many bytes of UTF-16LE, chained by the offset. Every read
/// is bounds-checked: this parses what the OS wrote into a fixed-size buffer,
/// so a truncated or malformed tail is a decoding condition, not a panic — the
/// same rule `find_embedded_thumbnail` follows on untrusted input, applied
/// here because a worker panic costs a restart.
pub fn parse_notify_buffer(buf: &[u8]) -> Vec<FsChange> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while let Some(header) = buf.get(off..off + 12) {
        let u32_at =
            |i: usize| u32::from_le_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
        let next = u32_at(0) as usize;
        let action = u32_at(4);
        let name_len = u32_at(8) as usize;
        let Some(name_bytes) = buf.get(off + 12..off + 12 + name_len) else {
            break;
        };
        let units: Vec<u16> = {
            // `chunks_exact` is used here because `as_chunks` does not resolve as an
            // iterator on this slice type.
            #[allow(clippy::chunks_exact_to_as_chunks)]
            name_bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect()
        };
        out.push(FsChange { action, name: String::from_utf16_lossy(&units) });
        // `NextEntryOffset == 0` marks the last record; anything that cannot
        // advance the cursor ends the walk rather than looping forever.
        if next == 0 || next < 12 {
            break;
        }
        off += next;
    }
    out
}

/// A `ReadDirectoryChangesW` loop on its own thread.
///
/// **Notifications, not a timer**: the app used to stat every file every 2 s
/// (and repainted to do it, which is why a static photo sat at ~2 fps). One
/// blocking call replaces all of that — the OS wakes this thread when something
/// changes, the thread wakes the app, and an idle viewer costs nothing.
///
/// The wake is a **closure** (the app passes `ctx.request_repaint`) rather than
/// an `egui` type here, so this stays a library of OS plumbing. The messages
/// are batches of [`FsChange`], and the app is free to drain several at once —
/// which it must, because one `save` commonly produces three.
#[cfg(windows)]
pub struct DirWatcher {
    rx: std::sync::mpsc::Receiver<Vec<FsChange>>,
    events: std::sync::Arc<std::sync::atomic::AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl DirWatcher {
    /// Watch `dir`, recursively when `recursive`. `wake` runs on the watcher
    /// thread after every non-empty batch.
    ///
    /// **Every way this can fail is an `Err` on the calling thread**, because
    /// the app's answer to `Err` is the 2 s poll and its answer to a watch that
    /// silently never fires is nothing at all. Two of those failures are not
    /// obvious:
    ///
    /// - The directory check is a `stat` and not only the `CreateFileW` result,
    ///   because `FILE_FLAG_BACKUP_SEMANTICS` happily opens a *file* as this
    ///   handle — the call then succeeds and the first
    ///   `ReadDirectoryChangesW` on it fails, which is a watch that is dead
    ///   before it reports anything.
    /// - A backend that refuses the read (a share without change
    ///   notification) breaks the loop later, which `alive()` reports; the app
    ///   drops the watch and polls instead.
    pub fn start(
        dir: PathBuf,
        recursive: bool,
        wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<DirWatcher> {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY,
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
            FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
            ReadDirectoryChangesW,
        };

        let meta = std::fs::metadata(&dir)?;
        if !meta.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "a folder watch needs a folder",
            ));
        }

        let (tx, rx) = std::sync::mpsc::channel::<Vec<FsChange>>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
        let events = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let alive = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));

        let ev = events.clone();
        let st = stop.clone();
        let fin = finished.clone();
        let liv = alive.clone();
        let thread = std::thread::Builder::new().name("folder-watch".into()).spawn(move || {
            use std::sync::atomic::Ordering::Relaxed;
            let mut wide: Vec<u16> = dir.as_os_str().encode_wide().collect();
            wide.push(0);
            // `FILE_FLAG_BACKUP_SEMANTICS` is what lets a *directory* be
            // opened as a file handle; the share mode is everything,
            // because the app keeps reading and writing these files.
            let handle: HANDLE = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    std::ptr::null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let _ = ready_tx.send(Err(std::io::Error::last_os_error()));
                fin.store(true, Relaxed);
                liv.store(false, Relaxed);
                return;
            }
            let _ = ready_tx.send(Ok(()));
            // 32 KiB: one bulk copy can emit an event per file, and an
            // overflow is the one failure that loses information — the
            // backend is required to report it as `ERROR_NOTIFY_ENUM_DIR`,
            // which becomes `CHANGE_OVERFLOW` below.
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                if st.load(Relaxed) {
                    break;
                }
                let mut bytes = 0u32;
                let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                    | FILE_NOTIFY_CHANGE_SIZE
                    | FILE_NOTIFY_CHANGE_LAST_WRITE;
                let ok = unsafe {
                    ReadDirectoryChangesW(
                        handle,
                        buf.as_mut_ptr().cast(),
                        buf.len() as u32,
                        recursive as i32,
                        filter,
                        &mut bytes,
                        std::ptr::null_mut(),
                        None,
                    )
                };
                if st.load(Relaxed) {
                    break; // the blocking call was cancelled by `Drop`
                }
                if ok == 0 {
                    let err = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
                    if err == 1022 {
                        // ERROR_NOTIFY_ENUM_DIR: events were dropped.
                        let _ = tx
                            .send(vec![FsChange { action: CHANGE_OVERFLOW, name: String::new() }]);
                        wake();
                        continue;
                    }
                    break; // the handle or the watch is gone
                }
                let changes = parse_notify_buffer(&buf[..bytes as usize]);
                if !changes.is_empty() {
                    ev.fetch_add(changes.len() as u64, Relaxed);
                    if tx.send(changes).is_err() {
                        break; // the app dropped the receiver
                    }
                    wake();
                }
            }
            unsafe { CloseHandle(handle) };
            fin.store(true, Relaxed);
            liv.store(false, Relaxed);
        })?;

        match ready_rx.recv() {
            Ok(Ok(())) => {
                Ok(DirWatcher { rx, events, stop, finished, alive, thread: Some(thread) })
            }
            Ok(Err(e)) => {
                let _ = thread.join();
                Err(e)
            }
            Err(_) => {
                let _ = thread.join();
                Err(std::io::Error::other("folder watcher thread exited"))
            }
        }
    }

    /// One batch of changes, if the thread has finished one.
    pub fn try_recv(&self) -> Option<Vec<FsChange>> {
        self.rx.try_recv().ok()
    }

    /// How many change records the OS has delivered, ever. A probe needs this
    /// to tell "the OS sent nothing" from "the app ignored what it sent".
    pub fn events(&self) -> u64 {
        self.events.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// `false` once the watcher thread has given up — the read failed on a
    /// handle that is still open, which is how a share that does not support
    /// change notification presents. A watch that is `Some` but not alive
    /// reports nothing forever, so the app must drop it and poll instead.
    pub fn alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(windows)]
impl Drop for DirWatcher {
    fn drop(&mut self) {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::IO::CancelSynchronousIo;
        std::sync::atomic::AtomicBool::store(
            &self.stop,
            true,
            std::sync::atomic::Ordering::Relaxed,
        );
        // **Cancel, don't just abandon.** The thread is blocked in a
        // `ReadDirectoryChangesW` that returns when something changes — which,
        // on a quiet folder, is never. Dropping without cancelling would leak
        // one blocked thread *and one open directory handle per folder the
        // user visits*, and `join` would hang the UI thread.
        //
        // Retried in a loop because the thread may not have entered the
        // blocking call yet when the first cancel lands (`ERROR_NOT_FOUND`);
        // one millisecond later it will have.
        if let Some(t) = &self.thread {
            let h = t.as_raw_handle();
            for _ in 0..1000 {
                if self.finished.load(std::sync::atomic::Ordering::Relaxed) {
                    break;
                }
                unsafe { CancelSynchronousIo(h) };
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        // Join only when the thread really is out: joining a thread that never
        // woke from the blocking call is the hang this whole dance avoids, so
        // the last resort is to detach it (the handle closes when it exits).
        if self.finished.load(std::sync::atomic::Ordering::Relaxed) {
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        } else {
            // Counted rather than swallowed: a detach is a leaked thread and an
            // open directory handle, one per folder the user visits, and it is
            // invisible from outside. The test asserts this stays at zero.
            WATCHERS_DETACHED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

/// How many watcher threads `Drop` had to abandon. Should be 0 always; a test
/// asserts it, because "`Drop` returned quickly" is also true of an
/// implementation that leaks every thread.
#[cfg(windows)]
pub(crate) static WATCHERS_DETACHED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// See [`WATCHERS_DETACHED`].
#[cfg(windows)]
pub fn watchers_detached() -> u64 {
    WATCHERS_DETACHED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Non-Windows stub: `Err`, so the app runs the fallback poll and the library
/// stays compilable off the one platform this viewer ships on.
#[cfg(not(windows))]
pub struct DirWatcher;

#[cfg(not(windows))]
impl DirWatcher {
    pub fn start(
        _dir: PathBuf,
        _recursive: bool,
        _wake: impl Fn() + Send + 'static,
    ) -> std::io::Result<DirWatcher> {
        Err(std::io::Error::other("folder watching is Windows-only"))
    }

    pub fn try_recv(&self) -> Option<Vec<FsChange>> {
        None
    }

    pub fn events(&self) -> u64 {
        0
    }

    pub fn alive(&self) -> bool {
        false
    }
}

pub fn thumb_cache_entry(root: &Path, path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    let name = format!("{:016x}", h.finish());
    root.join(&name[..2]).join(format!("{name}.png"))
}

/// Read a cached thumbnail from `root`, or `None` for a miss of any kind.
///
/// Takes the root rather than resolving [`thumb_cache_root`] so a test can point
/// it at a temp directory; the default-root path is [`load_grid_thumbnail`].
pub fn read_cached_thumbnail(root: &Path, path: &Path, target: u32) -> Option<ImageData> {
    let start = std::time::Instant::now();
    let entry = thumb_cache_entry(root, path);
    let bytes = std::fs::read(&entry).ok()?;
    let (header, payload) = decode_thumb_header(&bytes)?;
    // Every one of these is a miss, not a repair: a wrong picture in a grid is
    // worse than a blank cell, and the decode that replaces it is the thing this
    // cache exists to make rare.
    if header.stamp != thumb_stamp(path)? {
        return None;
    }
    if header.target != target || header.path != path.to_string_lossy() {
        return None;
    }
    // **The payload must be complete before it is decoded.** This check is here
    // because the obvious version of this function does not have it, and the test
    // that found it says why: `image`'s PNG decoder accepts a stream whose last
    // 64 bytes are missing, and hands back a full-size image with the tail zeroed.
    // So a truncated entry — a half-copied file, a backup tool mid-write, a
    // hydrated OneDrive placeholder — produced a *hit*, and a grid cell with a
    // black band across it, with no error anywhere. PNG's own terminator is the
    // exact and free way to ask "is all of this here?": a complete file ends with
    // a zero-length IEND chunk and its CRC. (Bit flips inside the compressed data
    // are a different question, and the `png` crate's per-chunk CRC answers those.)
    if !payload.ends_with(&PNG_IEND) {
        return None;
    }
    let img = image::load_from_memory_with_format(payload, image::ImageFormat::Png).ok()?;
    let rgba = img.to_rgba8();
    // The header's dims are a claim; the payload is the truth, and `normalized`
    // is what keeps a disagreement from becoming an assert in
    // `ColorImage::from_rgba_unmultiplied` on the UI thread.
    let data = normalized(ImageData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        load_time: start.elapsed(),
    });
    if (data.width, data.height) != (header.width, header.height) {
        return None;
    }
    Some(data)
}

/// Write a thumbnail into the cache at `root`. Best effort: every failure is
/// silent, and none of them may affect the decode the caller already has.
///
/// Takes the root for the same reason as [`read_cached_thumbnail`].
pub fn write_cached_thumbnail(root: &Path, path: &Path, target: u32, data: &ImageData) {
    let Some(stamp) = thumb_stamp(path) else {
        return;
    };
    let header = ThumbHeader {
        width: data.width,
        height: data.height,
        stamp,
        target,
        path: path.to_string_lossy().into_owned(),
    };
    let Ok(png) = encode_png(&data.rgba, data.width, data.height) else {
        return;
    };
    let entry = thumb_cache_entry(root, path);
    let Some(parent) = entry.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    // **The cache announces itself to Windows Search.** `create_dir_all` may
    // have made both levels just now, and the attribute is per-directory, so
    // both the fanout leaf and the root get it on every write — two
    // `SetFileAttributesW` calls beside a PNG encode, and they also bring any
    // directory created by an older build up to date on the first new write.
    let _ = mark_not_content_indexed(parent);
    if let Some(root) = parent.parent() {
        let _ = mark_not_content_indexed(root);
    }
    let mut bytes = encode_thumb_header(&header);
    bytes.extend_from_slice(&png);
    // Write beside the entry and rename into place. `fs::rename` replaces on
    // Windows, and a reader therefore sees either the previous entry or this one
    // — never a half-written one, which matters because a second instance of the
    // app reads these too.
    //
    // The temporary name is *not* made unique per writer, and that is deliberate:
    // two writers of one entry are writing the same bytes (same path, same stamp,
    // same target ⇒ the same header and the same encode), so clobbering the
    // temporary file cannot produce a wrong entry — at worst one `rename` finds its
    // source already gone and gives up, leaving the other's entry installed. A
    // per-writer name would instead leave an orphan behind whenever a process died
    // between the two calls.
    let tmp = entry.with_extension("tmp");
    if std::fs::write(&tmp, &bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &entry).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    maybe_trim_thumb_cache(root);
}

/// RGBA -> PNG, losslessly, and *without storing information the viewer does not
/// use*.
///
/// Lossless is the point and not an accident: a cache hit has to be **the same
/// pixels** as the decode it replaced, or the grid would quietly show something
/// the main view cannot reproduce — and a test could not tell the difference. That
/// is also why a lossy thumbnail format (a q90 JPEG would be ~8x smaller) was
/// rejected: it trades a property that is cheap to keep and easy to assert for one
/// that is neither.
///
/// The alpha drop is not a compromise. An opaque thumbnail stores as RGB, which
/// is 3 bytes per pixel before deflate instead of 4, and a cell draws an opaque
/// image identically either way; the reader puts the alpha back at 255, so the
/// RGBA the app receives is byte-for-byte what the decode produced. A thumbnail
/// that *does* have alpha keeps it.
///
/// What a cell costs on disk, measured: **13-80 KB, call it 1.5 bytes per pixel**.
/// Deflate is simply not good at photographic content, which is what a thumbnail
/// of a photo is, and [`THUMB_CACHE_MAX_BYTES`] is chosen with that in mind.
// `chunks_exact` is used here because `as_chunks` does not resolve as an
// iterator on this slice type.
#[allow(clippy::chunks_exact_to_as_chunks)]
pub(crate) fn encode_png(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> Result<Vec<u8>, image::ImageError> {
    use image::ExtendedColorType;
    use image::ImageEncoder;
    use image::codecs::png::{FilterType, PngEncoder};
    let mut out = Vec::new();
    {
        let enc =
            PngEncoder::new_with_quality(&mut out, THUMB_PNG_COMPRESSION, FilterType::Adaptive);
        if buffer_has_alpha(rgba) {
            enc.write_image(rgba, width, height, ExtendedColorType::Rgba8)?;
        } else {
            let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
            for px in rgba.chunks_exact(4) {
                rgb.extend_from_slice(&px[..3]);
            }
            enc.write_image(&rgb, width, height, ExtendedColorType::Rgb8)?;
        }
    }
    Ok(out)
}

/// The one tunable number in the stored form, and its table is why it is not
/// simply "the best compression available". Deflate effort, measured on real
/// thumbnails (bytes / encode time, min of 3, release):
///
/// | thumb | `Default` | `Fast` | `Best` |
/// |---|---|---|---|
/// | 256x84, wide scan | 35 734 B / 11.3 ms | 34 938 B / **0.75 ms** | 35 649 B / 26.8 ms |
/// | 107x160, embedded | 24 794 B / 6.8 ms | 32 994 B / **0.54 ms** | 24 328 B / 12.6 ms |
/// | 256x170 | 79 346 B / 21.0 ms | 79 613 B / **1.41 ms** | 79 193 B / 28.7 ms |
///
/// `Best` buys under 1% of size for 15-36x the time, and that time lands *before*
/// the cell's message is sent, so it is added to the one latency a user feels on a
/// cold open. `Default` is the same size as `Best` and just as slow. `Fast` is the
/// only one of the three that is not paying for nothing; the embedded-thumbnail row
/// is the one case where it costs size, and 8 KB on a file that takes 12 ms to
/// decode is not a trade worth optimising.
pub(crate) const THUMB_PNG_COMPRESSION: image::codecs::png::CompressionType =
    image::codecs::png::CompressionType::Fast;

pub(crate) static THUMB_PUTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Trim the cache off a background thread, every [`THUMB_TRIM_EVERY`] writes.
///
/// Off-thread because a trim walks the whole directory: on a full cache that is
/// tens of thousands of `read_dir` and `metadata` calls, and doing it inline would
/// add that to one unlucky thumbnail's latency. Detached rather than joined
/// because nothing waits on it — at worst the process exits mid-trim, which
/// leaves the cache slightly over its cap and perfectly valid.
pub(crate) fn maybe_trim_thumb_cache(root: &Path) {
    let n = THUMB_PUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if !n.is_multiple_of(THUMB_TRIM_EVERY) {
        return;
    }
    let root = root.to_path_buf();
    let _ = std::thread::Builder::new().name("iv-thumb-trim".into()).spawn(move || {
        prune_cache(&root, THUMB_CACHE_MAX_BYTES);
    });
}

/// Delete oldest-first until the cache is under `cap * THUMB_TRIM_TO_NUM/DEN`.
/// Returns the number of bytes left.
///
/// Public so a test can trim a synthetic cache and assert the result, rather than
/// trusting that a background thread eventually did it. Synchronous by design —
/// the caller decides what thread it runs on.
pub fn prune_cache(root: &Path, cap: u64) -> u64 {
    let target = cap / CACHE_TRIM_TO_DEN * CACHE_TRIM_TO_NUM;
    let mut entries: Vec<(std::time::SystemTime, u64, std::path::PathBuf)> = Vec::new();
    let mut total = 0u64;
    let Ok(fanouts) = std::fs::read_dir(root) else {
        return 0;
    };
    for fanout in fanouts.flatten() {
        let Ok(files) = std::fs::read_dir(fanout.path()) else {
            continue;
        };
        for f in files.flatten() {
            let Ok(meta) = f.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            total = total.saturating_add(meta.len());
            // An entry whose mtime cannot be read sorts as oldest, which is the
            // safe direction: it is the one most likely to be junk.
            let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
            entries.push((mtime, meta.len(), f.path()));
        }
    }
    if total > target {
        entries.sort_by_key(|(mtime, _, _)| *mtime);
        for (_, len, path) in entries {
            if total <= target {
                break;
            }
            if std::fs::remove_file(&path).is_ok() {
                total = total.saturating_sub(len);
            }
        }
        // Drop the fanout directories the trim just emptied, so the cache does not
        // slowly accumulate 256 near-empty folders. `remove_dir` refuses a
        // directory that still has anything in it, so the only casualty is a second
        // instance that created a fanout microseconds ago and has not written yet:
        // its rename fails, it caches nothing this once, and the next request for
        // that cell writes it. A cache that occasionally misses is the design.
        if let Ok(fanouts) = std::fs::read_dir(root) {
            for fanout in fanouts.flatten() {
                let _ = std::fs::remove_dir(fanout.path());
            }
        }
    }
    total
}

/// Drop `path`'s cached thumbnail, if it has one.
///
/// Called when the watcher sees the file change. Correctness does **not** depend
/// on this: the stamp in the entry already makes a changed file a miss. It is
/// here so a re-saved file does not sit on disk taking up space for a session
/// that may never open that folder again.
pub fn forget_thumbnail(path: &Path) {
    if !thumbnail_cache_enabled() {
        return;
    }
    let _ = std::fs::remove_file(thumb_cache_entry(&thumb_cache_root(), path));
}
