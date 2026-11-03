//! Module `thumb_cache`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use thumb_cache::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Grid thumbnails: the on-disk cache
// ---------------------------------------------------------------------------
//
// **Why this exists at all, because the interesting number is a negative one.**
// A 95 MP baseline JPEG takes ~700-800 ms to decode at 1/8 scale on this box, and
// there is no cheaper way to get the whole picture out of it: every pixel needs
// every DCT coefficient, so the entropy decode is the floor for *any* conformant
// decoder. Measured, interleaved, min of 4:
//
//   0_booklet_inside.jpg  95.7 MP baseline 4:2:0, 27 MB
//     fs::read 13 ms | DCT 1/8 833 ms | DCT 1/1 1242 ms | cell texture 5851 KB
//
// The DCT path saves RAM, not time — which is why the embedded EXIF thumbnail
// (`decode_embedded_thumbnail`, 15-40x) is the only real answer for the files that
// have one, and why the files that do not were the residue with nowhere to go.
//
// So the fix is not a faster decode. It is *not decoding it again*: a folder of
// large scans is opened over and over, and the second open should cost a file
// read. That is a cache, and the design of one is almost entirely about the ways
// it can be **wrong**, which is what the rest of this section is about:
//
// * A stale entry must be a **miss**, never the wrong picture. The stamp
//   (mtime + length) lives in the entry and is checked on every read, so a file
//   edited on disk is re-decoded even if nothing remembered to invalidate it.
// * A hash collision must be a **miss**, not a neighbour's thumbnail. The
//   source path is stored in the entry and compared, so 64 bits of name is not
//   the only thing standing between two files and each other's pictures.
// * A half-written entry must be **unreadable**. Entries are written to a
//   temporary name and renamed into place, so a reader — including a second
//   instance of the app — sees either the old entry or the new one.
// * Every failure is a **miss**. An unwritable cache directory, a full disk, a
//   corrupt PNG: each one costs a decode, and none of them may fail the decode.
//
// What this does *not* do is make the first visit to a folder of large scans
// fast. It cannot, and the honest number for a cold folder of 24 baseline scans
// is still ~24 x 700 ms / 4 workers. See `tests/bench_browse.rs`
// (`thumb_cache_ab`) for that A/B.

/// Every entry starts with this, so a stray file, a directory, or a truncated
/// write from a killed process is a miss and not a decode of whatever is there.
pub(crate) const THUMB_MAGIC: [u8; 4] = *b"IVTC";

/// Bumped whenever the header layout changes, which makes every existing entry a
/// miss instead of being misread.
pub(crate) const THUMB_HEADER_VERSION: u8 = 1;

/// magic(4) + version(1) + reserved(3) + width(4) + height(4) + mtime(8)
/// + len(8) + target(4) + path_len(2)
pub(crate) const THUMB_HEADER_FIXED: usize = 4 + 1 + 3 + 4 + 4 + 8 + 8 + 4 + 2;

/// The last 12 bytes of every complete PNG: a zero-length `IEND` chunk and its
/// CRC. See [`read_cached_thumbnail`] for why this is load-bearing.
pub(crate) const PNG_IEND: [u8; 12] = [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82];

/// Cap on the whole cache directory.
///
/// 256 MB is 3 000-19 000 cells at the 13-80 KB an entry measures (see
/// [`encode_png`]), so it holds a large scan library rather than a photo archive,
/// and it is trimmed oldest-first when it is reached. The number is stated in the
/// settings panel next to the toggle, because a cache that silently takes a
/// quarter of a disk is not something to enable quietly.
pub const THUMB_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;

/// How many writes happen between trims. A trim walks the whole cache, so doing
/// it on every write would be absurd; doing it never would let the cache grow
/// without bound, which is the failure mode a cache has to be designed against.
pub(crate) const THUMB_TRIM_EVERY: u64 = 64;

/// Fraction of the cap a trim aims back down to, so it is not repeated on the
/// very next write.
pub(crate) const CACHE_TRIM_TO_NUM: u64 = 4;
pub(crate) const CACHE_TRIM_TO_DEN: u64 = 5;

/// Root of the on-disk thumbnail cache.
///
/// **Local, not roaming, and deliberately not next to the config.** These are
/// this machine's decodes of files that may well be on a network share, they are
/// worthless on another machine, and a profile folder is the wrong place for
/// something this size.
pub fn thumb_cache_root() -> PathBuf {
    if let Some(dir) = portable_dir() {
        return dir.join("thumbs");
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local).join("image-viewer").join("thumbs")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".cache").join("image-viewer").join("thumbs")
    } else {
        PathBuf::from("image-viewer-thumbs")
    }
}

/// On/off for the whole cache, so a user who wants the viewer to write nothing
/// outside its config folder can say so. Mirrors [`set_linear_resample`]: a
/// global the workers read, set once from config at start-up.
pub(crate) static THUMB_CACHE_ON: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

pub fn set_thumbnail_cache(on: bool) {
    THUMB_CACHE_ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the on-disk cache in use? Public so a test can save and restore it around
/// assertions that only hold with it off.
pub fn thumbnail_cache_enabled() -> bool {
    THUMB_CACHE_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// What identifies the source file an entry was made from.
///
/// mtime + length, the same pair the file-change watcher compares
/// (`Stamp` in main.rs). Length is in the pair because a save that lands inside
/// one filesystem timestamp tick changes the bytes without changing the time,
/// and that is the case that would otherwise show a stale picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbStamp {
    pub mtime_secs: u64,
    pub len: u64,
}

pub fn thumb_stamp(path: &Path) -> Option<ThumbStamp> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_secs = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(ThumbStamp { mtime_secs, len: meta.len() })
}

/// Header of one cache entry: everything needed to decide whether it may be
/// used, and nothing that is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThumbHeader {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) stamp: ThumbStamp,
    pub(crate) target: u32,
    pub(crate) path: String,
}

/// Little-endian on purpose, so an entry written on one machine reads the same on
/// another and the format does not depend on this host's byte order.
pub(crate) fn encode_thumb_header(h: &ThumbHeader) -> Vec<u8> {
    let path = h.path.as_bytes();
    let path_len = u16::try_from(path.len()).unwrap_or(u16::MAX);
    let mut b = Vec::with_capacity(THUMB_HEADER_FIXED + path.len());
    b.extend_from_slice(&THUMB_MAGIC);
    b.push(THUMB_HEADER_VERSION);
    b.extend_from_slice(&[0u8; 3]);
    b.extend_from_slice(&h.width.to_le_bytes());
    b.extend_from_slice(&h.height.to_le_bytes());
    b.extend_from_slice(&h.stamp.mtime_secs.to_le_bytes());
    b.extend_from_slice(&h.stamp.len.to_le_bytes());
    b.extend_from_slice(&h.target.to_le_bytes());
    b.extend_from_slice(&path_len.to_le_bytes());
    b.extend_from_slice(path);
    b
}

/// Split an entry into its header and payload. `None` for anything that is not
/// an entry this version wrote, including a short read, a bad magic, a version
/// from the future, or a `path_len` that does not fit — all of which are simply
/// "no entry here".
pub(crate) fn decode_thumb_header(bytes: &[u8]) -> Option<(ThumbHeader, &[u8])> {
    if bytes.len() < THUMB_HEADER_FIXED || bytes[..4] != THUMB_MAGIC {
        return None;
    }
    if bytes[4] != THUMB_HEADER_VERSION {
        return None;
    }
    // Bounds are already settled by the length check above: the largest read ends
    // at 38, which is exactly `THUMB_HEADER_FIXED`. Spelled out rather than
    // `try_into().ok()?` per field because a closure cannot return `Option` here
    // and the invariant is worth stating once.
    let u32_at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let u64_at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
    let path_len = usize::from(u16::from_le_bytes(bytes[36..38].try_into().unwrap()));
    let start = THUMB_HEADER_FIXED;
    let end = start.checked_add(path_len)?;
    let path = std::str::from_utf8(bytes.get(start..end)?).ok()?.to_owned();
    Some((
        ThumbHeader {
            width: u32_at(8),
            height: u32_at(12),
            stamp: ThumbStamp { mtime_secs: u64_at(16), len: u64_at(24) },
            target: u32_at(32),
            path,
        },
        bytes.get(end..)?,
    ))
}

/// Where one source file's entry lives: `<root>/<2 hex>/<16 hex>.png`.
///
/// **The name is a hash of the path and nothing else, on purpose.** The stamp that
/// decides whether the entry is still good lives inside the entry, so a file that
/// is edited re-uses and overwrites its own slot instead of leaving one orphan
/// behind per save — which is the difference between a cache that is bounded by
/// the library size and one that is bounded by how many times you have hit save.
///
/// The two-hex fanout is not cosmetic either: a library of 30 000 files in one
/// directory is a single NTFS directory big enough to make `read_dir` and every
/// `create_dir` in it measurably slower.
/// Announce a cache directory to Windows Search.
///
/// `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED` is the one lever the other-viewers
/// survey (§9.8 in `TODO.md`) found anyone pulling for a cache directory —
/// nomacs sets it — and it matches what this directory is: potentially
/// thousands of small PNGs that no search service should spend a crawl on.
/// The attribute is per-directory, so the **root and every two-hex fanout
/// child** get it — a child does not inherit it from its parent. Existing
/// attributes are read and OR'd rather than replaced, because
/// `SetFileAttributesW` assigns the whole set and a fresh directory's
/// `FILE_ATTRIBUTE_DIRECTORY` belongs to the file system anyway; carrying it
/// through is the documented round-trip.
///
/// Best effort like everything on this path: a failure returns `false` and
/// none may affect the caller's write.
#[cfg(windows)]
pub fn mark_not_content_indexed(dir: &Path) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    use std::os::windows::fs::MetadataExt as _;
    let current = match std::fs::metadata(dir) {
        Ok(m) => m.file_attributes(),
        Err(_) => return false,
    };
    let wide = dir.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    unsafe {
        windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(
            wide.as_ptr(),
            current | windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NOT_CONTENT_INDEXED,
        ) != 0
    }
}

#[cfg(not(windows))]
pub fn mark_not_content_indexed(_dir: &Path) -> bool {
    true
}
