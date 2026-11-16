//! Module `scan`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use scan::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// Supported image extensions for gallery scanning.
///
/// Purely a *listing* filter — decoding is chosen from the file's magic bytes
/// (`is_jpeg_bytes` and friends), so a misnamed file still opens whatever it
/// really is. Anything missed here simply never appears in a folder scan; it
/// can still be opened by passing the path on the command line.
pub const SUPPORTED_EXTENSIONS: [&str; 17] = [
    "jpg", "jpeg", "png", "webp", "gif", "jxl", "avif", "heic", "heif", // 9
    "tif", "tiff", // TIFF
    "bmp", "dib", // Windows bitmap
    "ico", "cur", // Windows icon / cursor (one container format)
    "qoi", "svg", // QOI and SVG vector image
];

/// Decoders the `image` crate offers that this app does not use, each with the
/// reason.
///
/// Not a wish list, and not a claim about what the app *cannot* open — `avif` is
/// in that list and is very much supported, just not by this crate. The reasons
/// are not all the same kind:
///
///  * **Targa (`tga`)** — has *no magic bytes*. Decoding goes through
///    `image::load_from_memory`, which identifies by content, so a Targa cannot
///    be recognised at all. Making it work would mean passing the file extension
///    into the decode path, which is exactly the extension-based dispatch this
///    codebase is built to avoid — and the reason a PNG saved as `.jpg` keeps
///    working. `a_targa_cannot_be_identified_from_its_content` demonstrates this
///    with a real hand-built Targa rather than asserting it.
///  * **DDS** — decode-only in the crate, so a fixture cannot be generated with
///    the crate that decodes it, and it is a game-texture container rather than
///    something anyone keeps photographs in.
///  * **OpenEXR and Radiance HDR** — linear-light floating point. Rendered
///    through an 8-bit sRGB surface with no tone mapping they come out washed
///    out and blown out, which is worse than refusing them: the user cannot tell
///    the file is fine and the viewer is wrong. Belongs with the P3 → sRGB work
///    in `TODO.md` §1, as part of a real colour-management story.
///  * **PNM (pbm/pgm/ppm/pam)** and **farbfeld** — four more extensions of a
///    bitmap-mask family and a format that exists mostly inside Rust demos. They
///    would pad the file-association list for close to no real use.
///  * **AVIF** — already decoded, and better, by the `heic` crate, which also
///    reads the container's `colr` box. The `image` crate's AVIF support would be
///    a second, weaker decoder for a format already handled.
///
/// The line is not "can the crate decode it" but "can we show it *correctly*,
/// and can we still test it". QOI clears both; the rest do not.
pub const IMAGE_CRATE_DECODERS_WE_SKIP: &[(&str, &str)] = &[
    ("tga", "no magic bytes; content-based detection cannot see it"),
    ("dds", "decode-only in the crate, so untestable with it"),
    ("exr", "linear float; would display wrong without tone mapping"),
    ("hdr", "high dynamic range; same, and would look plausible but be wrong"),
    ("pnm", "bitmap-mask family; no real use in a photo viewer"),
    ("ff", "farbfeld; a demo format"),
    ("avif", "already handled, and better, by the heic crate"),
];

/// Whether archives are listed by a folder scan. Defaults to on.
///
/// A process-wide setting rather than an argument, for the same reason
/// [`thumbnail_cache_enabled`] is one: `is_browsable` is asked from the flat
/// scan, the recursive walk and `Gallery::new`, and threading a flag through all
/// three (and through every test that builds a gallery) would be a second thing
/// to forget rather than a smaller API.
pub(crate) static SHOW_ARCHIVES: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// Set whether a folder scan lists archives. See [`show_archives_enabled`].
pub fn set_show_archives(on: bool) {
    SHOW_ARCHIVES.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Are archives listed by a folder scan?
///
/// Public so a test can save and restore it: it is process-wide, and a test that
/// changed it without restoring would silently decide the answer for every test
/// that ran after it.
pub fn show_archives_enabled() -> bool {
    SHOW_ARCHIVES.load(std::sync::atomic::Ordering::Relaxed)
}

/// True if a folder scan should list this file: an image the app can decode, or
/// an archive it can open and [`show_archives_enabled`] says to list.
///
/// **Archives are listed, and are deliberately not in
/// [`SUPPORTED_EXTENSIONS`].** That constant gates *decoding* - it is also what
/// [`extract_zip`] uses to decide which members to write out, so a `.zip` in
/// it would make an archive both a browsable item and something the decoders
/// are handed. Listing and decoding are different questions, and this
/// predicate is the listing one.
///
/// One predicate rather than an extension list per scan: the flat scan, the
/// recursive walk and `Gallery::new` all ask here, so a setting that hides
/// archives cannot be honoured by one of them and missed by the others.
///
/// **An archive is only browsable if it contains at least one image.** This
/// check reads the archive header (central directory for zip/7z, file headers
/// for RAR) but does not extract anything. An empty archive or one with no
/// supported images is not listed at all — so pressing right on the previous
/// image skips it entirely rather than opening an empty gallery.
pub fn is_browsable(p: &Path) -> bool {
    if is_archive_path(p) {
        return show_archives_enabled() && archive_image_count(p).map(|c| c > 0).unwrap_or(false);
    }
    let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("").to_lowercase();
    SUPPORTED_EXTENSIONS.contains(&ext.as_str())
}

/// Scan a directory for images, image archives, and subfolders, sorted
/// alphabetically. Subfolders are listed so the grid can descend into them;
/// they carry no pixels, so they are never handed to the decoder.
pub fn scan_folder(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(folder) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() || (p.is_file() && is_browsable(&p)) {
                files.push(p);
            }
        }
    }
    files.sort_by(|a, b| {
        let an = a.file_name().unwrap_or_default().to_string_lossy();
        let bn = b.file_name().unwrap_or_default().to_string_lossy();
        natural_cmp(&an, &bn)
    });
    files
}

/// Walk the whole subtree for images, pre-order (each folder's files in
/// natural order, then its subfolders in natural order).
///
/// Symlinked files and directories are skipped, not followed: following a
/// directory link can cycle back into its own parents and walk forever, and
/// a linked *file* would show one picture under two paths. `DirEntry::
/// file_type` is the symlink-aware query (unlike `Path::is_file`, which
/// follows links), and it comes free with the listing.
pub fn scan_folder_recursive(folder: &Path) -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by(|a, b| {
            natural_cmp(&a.file_name().to_string_lossy(), &b.file_name().to_string_lossy())
        });
        for entry in entries {
            let Ok(ft) = entry.file_type() else {
                continue;
            };
            if ft.is_symlink() {
                continue;
            }
            let p = entry.path();
            if ft.is_file() {
                if is_browsable(&p) {
                    out.push(p);
                }
            } else if ft.is_dir() {
                walk(&p, out);
            }
        }
    }

    let mut files = Vec::new();
    walk(folder, &mut files);
    // Full paths, so folders group; forward slashes so `\` never sorts
    // before or after a digit differently than `/` would.
    files.sort_by(|a, b| {
        natural_cmp(
            &a.to_string_lossy().replace('\\', "/"),
            &b.to_string_lossy().replace('\\', "/"),
        )
    });
    files
}

/// How to order the gallery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortMode {
    /// Explorer-style natural sort by file name (IMG_2 before IMG_10).
    #[default]
    Name,
    /// Newest file first.
    Date,
    /// Largest file first.
    Size,
    /// Shuffled once per folder load.
    Random,
    /// Newest capture date first, from EXIF `DateTimeOriginal`. Files without a
    /// capture date sort oldest. Reads EXIF for every file on the first sort —
    /// the persisted shape cache (`shapes.json`) makes subsequent sorts cheaper
    /// for files that have been viewed.
    Taken,
}
