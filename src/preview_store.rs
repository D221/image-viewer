// The preview store on disk.
//
// Moved out of lib.rs because the file was over the 1000-line target and
// growing; see AGENTS.md ("Keep a source file under a thousand lines") and
// 	ests/source_size.rs, which holds the sizes.
//
// The items are re-exported by the crate root, so image_viewer::read_cached_preview
// and the rest keep working exactly as they did.

use std::path::{Path, PathBuf};

use crate::{
    encode_png, needs_refine, normalized, portable_dir, prune_cache, thumb_stamp, Animation, ColorInfo,
    ColorSpace, ImageData, PreviewAndFull, ThumbStamp, FAST_FULL_MAX_PIXELS, PNG_IEND,
    RESIZE_PX_FLOOR,
};
// ---------------------------------------------------------------------------
// The preview store on disk
// ---------------------------------------------------------------------------
//
// A preview of a picture, kept on disk so that showing it again does not decode
// the file again. This is the tier the cell cache is not: a cell is 256 px and
// costs 13-80 KB, while a preview is the size of the panel and costs megabytes,
// so the two caches answer different questions and cannot be one.
//
// **What it is for.** The decode of a large scan is the floor of this crate: a
// 28 MB JPEG spends **690 ms** here before the first pixels, and no amount of
// cleverness moves that (a baseline JPEG's entropy decode is the floor for any
// conformant decoder). A *revisit* — a slideshow, a back-flip, a folder opened
// again — pays it again for nothing. Measured here, the same picture from this
// store is on screen in **22-50 ms**, which is 15-30x sooner.
//
// **Which pictures qualify, and why it is not "all of them".** Only a source
// over [`FAST_FULL_PIXELS`] (12 MP) is stored, and that is the shape of the
// whole feature: such a source is decoded DCT-scaled and yields **no
// full-resolution buffer**, so a stored copy of its preview is *exactly* the
// thing the decode produced — same pixels, same flags, same "no full" — and a
// hit is indistinguishable from a miss except for the time. A smaller source is
// decoded in one pass that yields the preview **and** the full together, so a
// stored preview would be the preview alone: the app would have to decode the
// full a second time to get what it gets for free today, and the store would
// make that file *slower*, not faster.
//
// **The store holds the source's pixels, not a rendering of them.** The
// wide-gamut conversion is applied by the wrapper around the decode, so an
// entry written before the conversion is converted on the way out exactly as a
// decode would be, and toggling the setting changes the stored picture the way
// it changes a decoded one. (The cells' cache has the other shape, because a
// cell is written *after* its conversion — see `tests/thumb_cache.rs`.)
//
// **The encode is paid after the picture is on screen.** A preview encode is
// 14-33 ms on this box and it must not land in front of the first pixels: the
// cold floor is the one thing this feature is not allowed to make worse. So the
// decode hands the pixels to a store thread and returns, and the encode happens
// there, off the critical path.

/// Cap on the preview store, in bytes.
///
/// **512 MB**, twice the cell cache's 256 MB, and the ratio is the entries' sizes
/// rather than a preference: a preview of a large scan is **1.8-3.2 MB** on disk
/// (measured, 1.5-2 bytes per pixel — deflate is no good at photographs), so
/// this holds about 170 of them. The cell cache holds 512 cells in the same
/// space because a cell is 13-80 KB.
pub const PREVIEW_STORE_MAX_BYTES: u64 = 512 * 1024 * 1024;

/// How many previews may be waiting to be encoded at once.
///
/// The store thread is faster than the decoders that feed it (14-33 ms against
/// 690 ms), so this is a safety valve rather than a working limit: when it is
/// full the preview is **dropped**, which costs nothing but a later decode.
const PREVIEW_STORE_QUEUE: usize = 16;

/// How often the store is trimmed, in writes.
const PREVIEW_TRIM_EVERY: u64 = 16;

/// The header's magic and version. The version is what lets a future format
/// change make every old entry a miss rather than a wrong picture.
const PREVIEW_MAGIC: [u8; 4] = *b"IVPV";
const PREVIEW_HEADER_VERSION: u8 = 1;

/// The fixed part of the header, before the three variable-length strings.
const PREVIEW_HEADER_FIXED: usize = 4 + 1 + 3 + 4 * 2 + 8 * 2 + 4 + 4 * 2 + 2 + 4 * 2 + 4 + 1 + 1 + 2 + 4 + 12 + 2 * 3;

/// How much of an entry is read to decide whether it may be used.
///
/// The header is at the front of the file and the path is the only variable
/// part of it, so a kilobyte is enough for any path Windows will hand us (the
/// limit is 260 characters) with room to spare. Reading only this much is what
/// keeps a stale entry from costing a whole payload read.
const PREVIEW_HEAD_BYTES: usize = 1024;

/// Counts the writes, for the trim's cadence.
static PREVIEW_PUTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Root of the preview store.
///
/// Local, not roaming, and deliberately not next to the config: like the cell
/// cache, these are this machine's decodes of files that may be on a network
/// share, and they are worthless on another machine.
pub fn preview_store_root() -> PathBuf {
    if let Some(dir) = portable_dir() {
        return dir.join("previews");
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local)
            .join("image-viewer")
            .join("previews")
    } else if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home)
            .join(".cache")
            .join("image-viewer")
            .join("previews")
    } else {
        PathBuf::from("image-viewer-previews")
    }
}

/// On/off for the whole store, so a user who wants the viewer to write nothing
/// outside its config folder can say so. Mirrors the cell cache's switch.
static PREVIEW_STORE_ON: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

pub fn set_preview_store(on: bool) {
    PREVIEW_STORE_ON.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Is the preview store in use? Public so a test can save and restore it around
/// assertions that only hold with it off.
pub fn preview_store_enabled() -> bool {
    PREVIEW_STORE_ON.load(std::sync::atomic::Ordering::Relaxed)
}

/// What a stored preview has to remember besides its pixels.
///
/// **Every field here is load-bearing, and the list is the answer to "what
/// makes a hit the decode it replaced".** The pixels are the preview;
/// `decoded_for` and the source's dimensions are what [`needs_refine`] judges
/// sharpness with; `orig`, `exif_tag` and `crop_grid` are what the crop and
/// save paths compose with; and `colour` is the classification the metadata
/// panel prints and the wide-gamut conversion gates on. A hit that could not
/// answer all of them would be a different picture wearing the same pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct PreviewMeta {
    /// The target this preview was decoded **for**, not its pixel size: the
    /// resize rule compares requests, because comparing pixels re-decodes forever
    /// on a source smaller than the display.
    pub decoded_for: u32,
    /// The source's dimensions, orientation applied — the same numbers the
    /// decode reported.
    pub orig: (u32, u32),
    pub exif_tag: u16,
    pub crop_grid: (u32, u32),
    pub colour: ColorInfo,
}

/// The header of one entry: the metadata, plus what identifies the file it was
/// made from.
#[derive(Debug, Clone, PartialEq)]
struct PreviewHeader {
    width: u32,
    height: u32,
    stamp: ThumbStamp,
    page: u32,
    meta: PreviewMeta,
    path: String,
}

/// Little-endian on purpose, so an entry written on one machine reads the same
/// on another and the format does not depend on this host's byte order.
fn encode_preview_header(h: &PreviewHeader) -> Vec<u8> {
    let path = h.path.as_bytes();
    let desc = h.meta.colour.description.as_bytes();
    let data_space = h.meta.colour.data_space.as_bytes();
    let path_len = u16::try_from(path.len()).unwrap_or(u16::MAX);
    let desc_len = u16::try_from(desc.len()).unwrap_or(u16::MAX);
    let space_len = u16::try_from(data_space.len()).unwrap_or(u16::MAX);
    let primaries = h.meta.colour.red_primaries.unwrap_or([0, 0, 0]);
    let has_primaries = u8::from(h.meta.colour.red_primaries.is_some());
    let mut b = Vec::with_capacity(PREVIEW_HEADER_FIXED + path.len() + desc.len() + data_space.len());
    b.extend_from_slice(&PREVIEW_MAGIC);
    b.push(PREVIEW_HEADER_VERSION);
    b.extend_from_slice(&[0u8; 3]);
    b.extend_from_slice(&h.width.to_le_bytes());
    b.extend_from_slice(&h.height.to_le_bytes());
    b.extend_from_slice(&h.stamp.mtime_secs.to_le_bytes());
    b.extend_from_slice(&h.stamp.len.to_le_bytes());
    b.extend_from_slice(&h.meta.decoded_for.to_le_bytes());
    b.extend_from_slice(&h.meta.orig.0.to_le_bytes());
    b.extend_from_slice(&h.meta.orig.1.to_le_bytes());
    b.extend_from_slice(&h.meta.exif_tag.to_le_bytes());
    b.extend_from_slice(&h.meta.crop_grid.0.to_le_bytes());
    b.extend_from_slice(&h.meta.crop_grid.1.to_le_bytes());
    b.extend_from_slice(&h.page.to_le_bytes());
    b.push(colour_space_code(h.meta.colour.space));
    b.push(has_primaries);
    b.extend_from_slice(&[0u8; 2]);
    b.extend_from_slice(&(h.meta.colour.profile_bytes as u32).to_le_bytes());
    for v in primaries {
        b.extend_from_slice(&v.to_le_bytes());
    }
    b.extend_from_slice(&path_len.to_le_bytes());
    b.extend_from_slice(&desc_len.to_le_bytes());
    b.extend_from_slice(&space_len.to_le_bytes());
    b.extend_from_slice(path);
    b.extend_from_slice(desc);
    b.extend_from_slice(data_space);
    b
}

/// Split an entry into its header and payload. `None` for anything that is not
/// an entry this version wrote — a short read, a bad magic, a version from the
/// future, a length that does not fit — all of which are simply "no entry here".
fn decode_preview_header(bytes: &[u8]) -> Option<(PreviewHeader, &[u8])> {
    if bytes.len() < PREVIEW_HEADER_FIXED || bytes[..4] != PREVIEW_MAGIC {
        return None;
    }
    if bytes[4] != PREVIEW_HEADER_VERSION {
        return None;
    }
    // The magic, the version and the three reserved bytes.
    let mut at = 8usize;
    let u32_at = |at: &mut usize| -> Option<u32> {
        let s = bytes.get(*at..*at + 4)?;
        *at += 4;
        Some(u32::from_le_bytes(s.try_into().ok()?))
    };
    let u64_at = |at: &mut usize| -> Option<u64> {
        let s = bytes.get(*at..*at + 8)?;
        *at += 8;
        Some(u64::from_le_bytes(s.try_into().ok()?))
    };
    let u16_at = |at: &mut usize| -> Option<u16> {
        let s = bytes.get(*at..*at + 2)?;
        *at += 2;
        Some(u16::from_le_bytes(s.try_into().ok()?))
    };
    let width = u32_at(&mut at)?;
    let height = u32_at(&mut at)?;
    let mtime_secs = u64_at(&mut at)?;
    let len = u64_at(&mut at)?;
    let decoded_for = u32_at(&mut at)?;
    let orig_w = u32_at(&mut at)?;
    let orig_h = u32_at(&mut at)?;
    let exif_tag = u16_at(&mut at)?;
    let crop_w = u32_at(&mut at)?;
    let crop_h = u32_at(&mut at)?;
    let page = u32_at(&mut at)?;
    let space = colour_space_from_code(*bytes.get(at)?)?;
    at += 1;
    let has_primaries = *bytes.get(at)?;
    at += 1;
    at += 2;
    let profile_bytes = u32_at(&mut at)?;
    let mut primaries = [0i32; 3];
    for p in primaries.iter_mut() {
        let s = bytes.get(at..at + 4)?;
        at += 4;
        *p = i32::from_le_bytes(s.try_into().ok()?);
    }
    let path_len = u16_at(&mut at)? as usize;
    let desc_len = u16_at(&mut at)? as usize;
    let space_len = u16_at(&mut at)? as usize;
    let path = std::str::from_utf8(bytes.get(at..at + path_len)?).ok()?.to_string();
    at += path_len;
    let desc = std::str::from_utf8(bytes.get(at..at + desc_len)?).ok()?.to_string();
    at += desc_len;
    let data_space = std::str::from_utf8(bytes.get(at..at + space_len)?).ok()?.to_string();
    at += space_len;
    let payload = bytes.get(at..)?;
    Some((
        PreviewHeader {
            width,
            height,
            stamp: ThumbStamp { mtime_secs, len },
            page,
            meta: PreviewMeta {
                decoded_for,
                orig: (orig_w, orig_h),
                exif_tag,
                crop_grid: (crop_w, crop_h),
                colour: ColorInfo {
                    space,
                    description: desc,
                    profile_bytes: profile_bytes as usize,
                    data_space,
                    red_primaries: if has_primaries == 1 {
                        Some(primaries)
                    } else {
                        None
                    },
                },
            },
            path,
        },
        payload,
    ))
}

/// The colour space as one byte. The enum's own discriminant would do, but
/// naming the cases keeps the file readable and the order free to change.
fn colour_space_code(space: ColorSpace) -> u8 {
    match space {
        ColorSpace::UntaggedSrgb => 0,
        ColorSpace::Srgb => 1,
        ColorSpace::WideGamut => 2,
        ColorSpace::Unknown => 3,
        ColorSpace::NonRgb => 4,
        ColorSpace::NotInspected => 5,
    }
}

fn colour_space_from_code(code: u8) -> Option<ColorSpace> {
    Some(match code {
        0 => ColorSpace::UntaggedSrgb,
        1 => ColorSpace::Srgb,
        2 => ColorSpace::WideGamut,
        3 => ColorSpace::Unknown,
        4 => ColorSpace::NonRgb,
        5 => ColorSpace::NotInspected,
        _ => return None,
    })
}

/// Where one picture's entry lives.
///
/// The key is the **path and the page**: a multi-page document's pages are
/// different pictures, and an entry keyed by the path alone would hand page 1
/// back for page 3. The name is 64 bits of hash of both, and the path is in the
/// header as well, so a collision is caught rather than served.
pub fn preview_store_entry(root: &Path, path: &Path, page: u32) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    page.hash(&mut h);
    let name = format!("{:016x}", h.finish());
    root.join(&name[..2]).join(format!("{name}.png"))
}

/// Read a stored preview, or `None` for a miss of any kind.
///
/// Takes the root rather than resolving [`preview_store_root`] so a test can
/// point it at a temp directory.
pub fn read_cached_preview(
    root: &Path,
    path: &Path,
    page: u32,
    target: u32,
) -> Option<PreviewAndFull> {
    let start = std::time::Instant::now();
    let entry = preview_store_entry(root, path, page);
    // **The head first.** A stale entry must not cost a whole payload read: the
    // header is at the front of the file, and everything that could make the
    // entry unusable is in it.
    let head = read_file_head(&entry)?;
    let (header, _) = decode_preview_header(&head)?;
    if header.path != path.to_string_lossy() || header.page != page {
        return None;
    }
    // The stamp is mtime **and** length, the same pair the watcher compares:
    // a save that lands inside one filesystem timestamp tick changes the bytes
    // without changing the time.
    if Some(header.stamp) != thumb_stamp(path) {
        return None;
    }
    // The same rule the in-memory cache uses, on the same inputs: a preview
    // decoded for a target the display has outgrown is not what the display can
    // show, and a source smaller than the display is never re-decoded for it.
    let preview_long = header.width.max(header.height);
    let source_long = header.meta.orig.0.max(header.meta.orig.1);
    if needs_refine(
        preview_long,
        source_long,
        header.meta.decoded_for,
        target,
        RESIZE_PX_FLOOR,
    ) {
        return None;
    }
    // Now the payload, and only now.
    let bytes = std::fs::read(&entry).ok()?;
    let (header, payload) = decode_preview_header(&bytes)?;
    // **The payload must be complete before it is decoded**: `image`'s PNG
    // decoder accepts a stream missing its last 64 bytes and hands back a
    // full-size image with the tail zeroed, so a truncated entry would be a
    // *hit* with a black band across it. PNG's own terminator is the check.
    if !payload.ends_with(&PNG_IEND) {
        return None;
    }
    let img = image::load_from_memory_with_format(payload, image::ImageFormat::Png).ok()?;
    let rgba = img.to_rgba8();
    let data = normalized(ImageData {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
        load_time: start.elapsed(),
    });
    if (data.width, data.height) != (header.width, header.height) {
        return None;
    }
    Some(PreviewAndFull {
        preview: data,
        // A stored preview is a preview: there is no full-resolution buffer
        // behind it, exactly as there is none behind the DCT-scaled preview it
        // replaces. The app asks for the full separately, as it already does for
        // a big JPEG.
        full: None,
        anim: None,
        orig: Some(header.meta.orig),
        below_target: true,
        exif_tag: header.meta.exif_tag,
        crop_grid: header.meta.crop_grid,
        colour: header.meta.colour,
    })
}

/// The first [`PREVIEW_HEAD_BYTES`] of a file, or the whole of it when it is
/// shorter. `None` when the file cannot be read.
fn read_file_head(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; PREVIEW_HEAD_BYTES];
    let mut filled = 0usize;
    while filled < buf.len() {
        let n = f.read(&mut buf[filled..]).ok()?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    if buf.is_empty() {
        return None;
    }
    Some(buf)
}

/// Write a preview into the store at `root`. Best effort: every failure is
/// silent, and none of them may affect the decode the caller already has.
///
/// Takes the root for the same reason [`read_cached_preview`] does: a test can
/// point it at a temp directory, and the app's own root is [`preview_store_root`].
pub fn write_cached_preview(
    root: &Path,
    path: &Path,
    page: u32,
    meta: PreviewMeta,
    data: &ImageData,
) {
    let Some(stamp) = thumb_stamp(path) else {
        return;
    };
    let header = PreviewHeader {
        width: data.width,
        height: data.height,
        stamp,
        page,
        meta,
        path: path.to_string_lossy().into_owned(),
    };
    let Ok(png) = encode_png(&data.rgba, data.width, data.height) else {
        return;
    };
    let entry = preview_store_entry(root, path, page);
    let Some(parent) = entry.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    // The cache announces itself to Windows Search, for the same reason the
    // cell cache's does: these are megabytes of PNGs, and a content indexer
    // walking them is a cost nobody asked for.
    let _ = crate::mark_not_content_indexed(parent);
    if let Some(root) = parent.parent() {
        let _ = crate::mark_not_content_indexed(root);
    }
    let mut bytes = encode_preview_header(&header);
    bytes.extend_from_slice(&png);
    // Write beside the entry and rename into place: `fs::rename` replaces on
    // Windows, and a reader therefore sees either the previous entry or this
    // one, never a half-written one.
    let tmp = entry.with_extension("png.tmp");
    if std::fs::write(&tmp, &bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if std::fs::rename(&tmp, &entry).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    maybe_trim_preview_store(root);
}

/// Trim the store off a background thread, every [`PREVIEW_TRIM_EVERY`] writes.
///
/// Off-thread because a trim walks the whole directory: on a full store that is
/// hundreds of `read_dir` and `metadata` calls, and doing it inline would add
/// that to one unlucky preview's latency. Detached rather than joined because
/// nothing waits on it — at worst the process exits mid-trim, which leaves the
/// store slightly over its cap and perfectly valid.
fn maybe_trim_preview_store(root: &Path) {
    let n = PREVIEW_PUTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    if n % PREVIEW_TRIM_EVERY != 0 {
        return;
    }
    let root = root.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("iv-preview-trim".into())
        .spawn(move || {
            prune_cache(&root, PREVIEW_STORE_MAX_BYTES);
        });
}

/// A preview waiting to be encoded and stored.
pub(crate) struct PreviewJob {
    path: PathBuf,
    page: u32,
    meta: PreviewMeta,
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

/// The store's queue, once [`start_preview_store`] has run.
static PREVIEW_STORE_TX: std::sync::OnceLock<std::sync::mpsc::SyncSender<PreviewJob>> =
    std::sync::OnceLock::new();

/// Start the store thread.
///
/// **One** thread, one queue, one store: the encode is 14-33 ms and the decode
/// that feeds it is 690 ms, so a single thread keeps up easily and the queue is
/// a safety valve rather than a working limit. Starting it is idempotent, so
/// the app can call it at start-up without asking whether it already has.
///
/// The thread is detached: nothing waits on it, and at worst the process exits
/// mid-encode, which leaves the store without that entry and perfectly valid.
pub fn start_preview_store() {
    let _ = PREVIEW_STORE_TX.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::sync_channel::<PreviewJob>(PREVIEW_STORE_QUEUE);
        let root = preview_store_root();
        let _ = std::thread::Builder::new()
            .name("iv-preview-store".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    let data = ImageData {
                        width: job.width,
                        height: job.height,
                        rgba: job.rgba,
                        load_time: std::time::Duration::ZERO,
                    };
                    write_cached_preview(&root, &job.path, job.page, job.meta, &data);
                }
            });
        tx
    });
}

/// Hand a preview to the store thread. Best effort: if the store is not running
/// or its queue is full, the preview is dropped, which costs nothing but a
/// later decode.
pub(crate) fn store_preview(job: PreviewJob) {
    let Some(tx) = PREVIEW_STORE_TX.get() else {
        return;
    };
    let _ = tx.try_send(job);
}

/// Delete the whole store.
///
/// **Off means off.** The setting is a promise about the disk, not only about
/// the writing, and a store that is switched off but still holds 512 MB is a
/// promise kept in letter and broken in spirit. The cost of deleting is that
/// the pictures in it are decoded again next time — which is exactly what
/// "off" means.
///
/// Off the calling thread, because removing hundreds of megabytes is not
/// something to do on the thread that paints.
pub fn clear_preview_store() {
    let root = preview_store_root();
    let _ = std::thread::Builder::new()
        .name("iv-preview-clear".into())
        .spawn(move || {
            let _ = std::fs::remove_dir_all(&root);
        });
}

/// Whether this decode's preview is worth storing.
///
/// The rules are the **shape of the decode**, not a timer:
///
/// * **No full-resolution buffer.** A stored preview has to be exactly what the
///   decode produced, and a decode that also produced the full has a preview
///   that is only half of what it costs — storing that half would mean decoding
///   the file again for the other half, which is slower than never caching.
/// * **Not an animation.** Its preview is one frame of a loop, and the loop is
///   what the app wants; a stored frame would be a picture of something else.
/// * **The source is over [`FAST_FULL_PIXELS`].** That is where the decode is
///   expensive enough to be worth avoiding, and it is the same threshold the
///   decode path itself uses to decide that a full-resolution buffer is not
///   worth keeping.
pub fn preview_is_storeable(
    orig: Option<(u32, u32)>,
    full: Option<&ImageData>,
    anim: Option<&Animation>,
) -> bool {    if full.is_some() || anim.is_some() {
        return false;
    }
    match orig {
        Some((w, h)) => (w as u64) * (h as u64) > FAST_FULL_MAX_PIXELS,
        None => false,
    }
}

/// Hand a decoded preview to the store, if it is one worth keeping.
///
/// The pixels are **copied**, because the decode keeps its own: the store's
/// copy has to be the source's pixels, and the caller's copy is what goes on
/// screen. The copy is on a worker and costs 0.3 ms against a decode that cost
/// 690 ms.
pub(crate) fn maybe_store_preview(
    path: &Path,
    page: u32,
    preview: &ImageData,
    orig: Option<(u32, u32)>,
    full: Option<&ImageData>,
    anim: Option<&Animation>,
    meta: PreviewMeta,
) {
    if !preview_store_enabled() {
        return;
    }
    if !preview_is_storeable(orig, full, anim) {
        return;
    }
    store_preview(PreviewJob {
        path: path.to_path_buf(),
        page,
        meta,
        width: preview.width,
        height: preview.height,
        rgba: preview.rgba.clone(),
    });
}

// ---------------------------------------------------------------------------