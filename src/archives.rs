//! Module `archives`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use archives::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// The archive containers this app can open.
///
/// A member listing is the only thing every one of these promises: read the
/// names, count them, decide which are images. What comes *out* of a member is
/// the reader's business and is not uniform — see [`extract_archive`].
pub const ARCHIVE_EXTENSIONS: [&str; 4] = ["zip", "cbz", "7z", "rar"];

/// True if the path looks like an image archive this app can open.
///
/// **Listing an archive is not decoding it.** This predicate decides what a
/// folder scan yields; [`is_browsable`] adds the images to it, and neither puts
/// an archive in [`SUPPORTED_EXTENSIONS`], which is what the decoders and
/// [`extract_zip`] filter members with. A zip in there would make a container
/// something the image decoders get handed.
pub fn is_archive_path(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| ARCHIVE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Turn one archive member's name into the file name it will be written as, or
/// `None` if it should not be written at all.
///
/// **Shared by all three readers**, which is the point: the gallery inside an
/// archive is the same whichever container it came from, so a `.rar` and a
/// `.cbz` of the same pages produce the same files in the same order.
///
/// Three filters, each load-bearing:
///  * **directories** — an archive member may name a folder.
///  * **non-images** — by [`SUPPORTED_EXTENSIONS`], which is deliberately *not*
///    the listing predicate. A `.zip` inside a `.zip` is not extracted, so an
///    archive can never contain a browsable archive and the gallery is one level
///    deep by construction.
///  * **illegal and reserved names** — entries may carry characters NTFS
///    refuses, plus the trailing dot or space it silently strips, so the name is
///    compared *after* cleaning rather than before.
pub(crate) fn archive_member_name(raw: &str) -> Option<String> {
    let name = std::path::Path::new(raw).file_name()?.to_str()?;
    let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
    if !SUPPORTED_EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let safe: String = name
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20 {
                '_'
            } else {
                c
            }
        })
        .collect();
    // NTFS silently drops a trailing dot or space, so `page.png ` would collide
    // with `page.png` after cleaning - compare what will actually be on disk.
    let trimmed = safe.trim_end_matches(['.', ' ']).to_string();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed)
}

/// Name an extracted member uniquely, case-insensitively.
///
/// Two archive members can flatten to the same file name — `ch1/page01.jpg`
/// twice, or `page01.jpg` and `PAGE01.JPG` — and Windows treats those as one
/// file, so the second would silently overwrite the first and the gallery would
/// be a page shorter than the archive with no error anywhere. Returns `false`
/// when the name is taken.
pub(crate) fn claim_member_name(seen: &mut std::collections::HashSet<String>, safe: &str) -> bool {
    seen.insert(safe.to_lowercase())
}

/// A file the app now owns a copy of, and the directory that copy lives in.
///
/// The directory is a separate value because **it is the unit that is deleted**:
/// the caller hands it back when the app moves on, and `remove_dir_all` is the
/// only cleanup there is. Two copies of this struct are never live at once -
/// `MyApp` holds one `Option` - so the ownership is a single slot, the same slot
/// discipline the archive's temp dir already uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropCopy {
    pub dir: PathBuf,
    pub file: PathBuf,
    /// **The copy was already in `dir` and no bytes were moved.**
    ///
    /// Recorded because it is the difference between a drop that costs two
    /// `stat` calls and a drop that costs a read and a write of the whole file,
    /// and because a status bar that says "copied" when nothing was copied is
    /// lying about the one thing the user just waited for.
    pub reused: bool,
}

/// Where owning a dropped file *would* put its copy, and whether it is already
/// there — the decision, with no bytes moved.
///
/// **Split from the copy so the copy can happen on a worker.** The decision is
/// two `stat` calls and a hash, which is microseconds and is safe anywhere; the
/// copy is a read and a write of a file whose size the app does not know, which
/// is hundreds of milliseconds and is safe nowhere but off the UI thread. See
/// `own_dropped`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DropCopyPlan {
    pub dir: PathBuf,
    pub file: PathBuf,
    /// A copy of exactly this source is already in `dir`.
    pub reuse: bool,
}

impl DropCopyPlan {
    /// Carry out the plan: copy `src` into `file` **unless a valid copy is
    /// already there**.
    ///
    /// **The validity check is made here rather than trusted from the plan.**
    /// `plan.reuse` was decided at plan time and the bytes move later, so
    /// re-deriving the source's length here is what makes the answer a property
    /// of the file rather than of a value latched one message ago — the same
    /// "derive it, do not remember it" rule the rest of this crate is built on.
    /// Trusting the flag would mean a plan made before a 500 MB copy cannot be
    /// wrong about it; re-checking cannot.
    pub fn materialise(&self, src: &Path) -> Result<DropCopy, String> {
        let want = std::fs::metadata(src)
            .map_err(|e| format!("{}: {e}", src.display()))?
            .len();
        let reused = copy_is_current(&self.file, want);
        if !reused {
            std::fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
            // Re-owning must be idempotent, and `copy` writes through an existing
            // destination rather than refusing it. Removing first is what makes a
            // second drop of the same file a clean overwrite instead of leaving a
            // stale tail. Only on the copy path: a reused copy is not touched at
            // all, which is the whole point of `reuse`.
            let _ = std::fs::remove_file(&self.file);
            std::fs::copy(src, &self.file).map_err(|e| format!("{}: {e}", self.file.display()))?;
        }
        Ok(DropCopy {
            dir: self.dir.clone(),
            file: self.file.clone(),
            reused,
        })
    }
}

/// Is there already a copy of exactly `want` bytes at `dst`?
///
/// **A length check, and the length is half the identity.** `drop_copy_dir`
/// hashes the source's **path, mtime and length**, so reaching this function
/// with `dst` inside the planned directory already means "same path, same
/// mtime, same length" — the same `(mtime, len)` stamp the folder watcher calls
/// a change, so the reuse check is exactly as strong as change detection
/// anywhere else in this app. A rewrite that preserves *both* mtime and length
/// is invisible here, and is invisible to the watcher too; there is no stamp
/// that catches it.
///
/// A length check and not an existence check, because a half-written copy from a
/// killed session is a file that exists and is the wrong thing to show.
pub(crate) fn copy_is_current(dst: &Path, want: u64) -> bool {
    std::fs::metadata(dst).is_ok_and(|m| m.is_file() && m.len() == want)
}

/// Decide what owning `src` would do, without moving any bytes.
///
/// Pure apart from the two `stat` calls it needs to answer the question, so the
/// app can ask it on the UI thread and do the copy on a worker — or ask it and
/// the copy in the same place, which is what `own_dropped` is.
pub fn plan_drop_copy(src: &Path) -> Result<DropCopyPlan, String> {
    let meta = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", src.display()));
    }
    let name = src
        .file_name()
        .ok_or_else(|| format!("{} has no file name", src.display()))?;
    let dir = drop_copy_dir(src, &meta);
    let file = dir.join(name);
    Ok(DropCopyPlan {
        reuse: copy_is_current(&file, meta.len()),
        dir,
        file,
    })
}

/// Take a copy of a file that the app cannot expect to still be there, and say
/// where the copy is.
///
/// **A drag-and-drop is a hand-off, not a reference.** Explorer does not pass a
/// path into a zip's member and leave it there: it extracts the member to a
/// transient temp file, hands *that* path to the drop target, and deletes it a
/// moment later. So the source of a drop is transient **by construction**, and an
/// app that opens one in place is holding a path to a file with an expiry date on
/// it. What that cost, measured (`tests/drops.rs` and the trace in
/// `.superpowers/sdd/2026-10-02-manga-spread-mode/dropped-temp-file-vanishes.md`):
/// on the app's own 2 s poll the vanished file was a *changed* file, so
/// `invalidate_path` dropped the texture, `rescan` removed it from the gallery and
/// the re-requested decode failed into a `failed` entry that is permanent for the
/// session - shown for about a second, then gone with no way back.
///
/// Copying is the same shape the archive path already uses (`%TEMP%\image-viewer`
/// keyed by content), for the same reason: a member of an archive and a member
/// dragged out of one are the same kind of hand-off, and only one of them used to
/// be handled.
///
/// **A real `copy`, not a hard link** - and the reason is a measurement, because
/// the link was the obvious cheaper answer and it is wrong.
///
/// A hard link survives the *deletion* this function exists for: unlinking one
/// name leaves the data reachable through the other. So it passes the property
/// under test and still fails the thing that property is for. A link **shares the
/// file**, so any in-place write to the source mutates the "copy" too - verified
/// on this box, both ways a writer can do it:
///
/// ```text
/// dst after in-place rewrite: [82, 69, 76, ...]   // "REPLACED, longer contents"
/// dst after rename-over:      [82, 69, 76, ...]   // same
/// ```
///
/// The app would then be showing - and `Ctrl+S` would write - bytes under a path
/// it believes it owns, which is precisely the stale-or-aliasing class this
/// codebase treats as worse than slow (`AGENTS.md` on the watcher's stale
/// texture: "keeping old pixels for a re-exported file is the one outcome that
/// looks like a bug"). A snapshot is the whole point; an alias is not a snapshot.
///
/// **What it costs, honestly: a read and a write of the dropped file, once per
/// drop — and not on the UI thread.** `own_dropped` is the *blocking* form: the
/// plan, then the copy. The app does not call it from the drop handler. It calls
/// [`plan_drop_copy`] on the UI thread — two `stat` calls and a hash — hands the
/// copy to a worker thread, and opens the copy when the worker reports in. The
/// reason is the first line of `AGENTS.md`: this app never does I/O of
/// unbounded size on the thread that paints. Measured on this box, the
/// synchronous form cost **3.8 ms at 5 MB, 26 ms at 50 MB and 279 ms at 500 MB**
/// (`bench_browse drop_copy_ab`, release, warm cache, NVMe) — the 500 MB figure
/// is eighteen frames at 60 Hz, and it is linear in a size the app has no way to
/// know. A user should be told either way, which is why `MyApp::start_drop_copy`
/// puts "Copying…" in the status bar.
///
/// **A repeated drop of an unchanged file moves no bytes at all**, which is what
/// `reuse` is for and what the same benchmark pins: before the check existed the
/// repeat cost 3.2 / 24 / 244 ms against the cold arm's 3.8 / 26 / 279 ms —
/// the same work, because the directory was reused and the *copy* was remade.
/// Now it is two `stat` calls. The directory name is a hash of the source's
/// **path, mtime and length** (`drop_copy_dir`), which is what makes that
/// statement true rather than hopeful: reaching the reuse check at all means the
/// source has the same path, mtime and length as the copy's.
///
/// **What it does not fix.** If Explorer is still finalising the extraction when
/// the drop lands, the copy captures a partial file. That was a hypothesis
/// considered here and **not observed**: the trace of the real failure shows the
/// file decoded and painted correctly at frame 2 and was blanked at frame 14
/// because its *stamp changed when it vanished*. Mid-write is not a mechanism
/// this fix claims to address.
///
/// The name is a hash of the source's **path, mtime and length**, like
/// `MyApp::zip_temp_for`: two drops of the same unchanged file land in the same
/// directory, so the second one **reuses** the copy rather than remaking it (see
/// `copy_is_current`), and a file that changed under us gets a new directory
/// rather than silently overwriting a copy something else may be reading.
pub fn own_dropped(src: &Path) -> Result<DropCopy, String> {
    plan_drop_copy(src)?.materialise(src)
}

/// Where a dropped file's copy goes: `%TEMP%\image-viewer\drop\<16 hex>`.
///
/// **Under the OS temp directory and not under `%APPDATA%`**, for the reason
/// every cache in this app lives there: these are this machine's copies of files
/// that may be anywhere, they are worthless elsewhere, and a re-drop overwrites
/// rather than accumulating.
pub fn drop_copy_dir(src: &Path, meta: &std::fs::Metadata) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    meta.modified().ok().map(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).hash(&mut h);
    meta.len().hash(&mut h);
    std::env::temp_dir()
        .join("image-viewer")
        .join("drop")
        .join(format!("{:016x}", h.finish()))
}

/// Drop the copies this app owns. **Every failure is swallowed on purpose**:
/// a leftover directory in `%TEMP%` is harmless, and the alternative is an error
/// on a path that is already shutting down.
pub fn forget_drop_copy(copy: &DropCopy) {
    let _ = std::fs::remove_dir_all(&copy.dir);
}

/// Extract supported image entries from a zip/cbz archive into `dest`,
/// flattening nested paths into the destination directory. Encrypted entries
/// are skipped and counted. Returns (extracted, encrypted_skipped).
pub fn extract_zip(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut z = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut extracted = 0;
    let mut encrypted = 0;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).map_err(|e| e.to_string())?;
        if f.is_dir() {
            continue;
        }
        if f.encrypted() {
            encrypted += 1;
            continue;
        }
        let Some(safe) = archive_member_name(f.name()) else {
            continue;
        };
        if !claim_member_name(&mut seen, &safe) {
            continue;
        }
        let mut out = std::fs::File::create(dest.join(&safe)).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
        extracted += 1;
    }
    Ok((extracted, encrypted))
}

/// Extract image entries from a 7z archive. Same contract as [`extract_zip`].
///
/// **Encrypted entries cannot be counted the way zip counts them**: 7z reports
/// one archive-level flag, so a password-protected `.7z` arrives here as an
/// error from the reader rather than as a per-member skip. That is the honest
/// shape for it — a whole-archive refusal rather than a partial gallery.
pub fn extract_7z(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    use sevenz_rust2::ArchiveReader;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    let mut reader = ArchiveReader::open(archive, sevenz_rust2::Password::empty())
        .map_err(|e| e.to_string())?;
    let mut extracted = 0;
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut names: Vec<(String, bool)> = Vec::new();
    reader
        .for_each_entries(|entry, _| {
            names.push((entry.name().to_string(), entry.is_directory()));
            Ok(true)
        })
        .map_err(|e| e.to_string())?;
    for (name, is_dir) in names {
        if is_dir {
            continue;
        }
        let Some(safe) = archive_member_name(&name) else {
            continue;
        };
        if !claim_member_name(&mut seen, &safe) {
            continue;
        }
        // `read_file` decodes the whole member into memory. 7z members are
        // solid by default, so a reader that streams would decode the members
        // before it anyway; the difference is only the transient buffer, and a
        // comic page is a few MB.
        let bytes = reader.read_file(&name).map_err(|e| e.to_string())?;
        std::fs::write(dest.join(&safe), &bytes).map_err(|e| e.to_string())?;
        extracted += 1;
    }
    Ok((extracted, 0))
}

/// Extract image entries from a RAR archive. Same contract as [`extract_zip`].
///
/// **Only stored (uncompressed) members come out, and that is a limit of the
/// `rar` crate rather than a choice.** Its extractor has an unpack step marked
/// `todo`, so a *compressed* member's stored bytes would be written out as if
/// they were the page — a file that is not a JPEG, which the decoders then
/// reject with an error the user has no way to interpret.
///
/// `rar` 0.4 exposes exactly one entry point, [`rar::Archive::extract_all`],
/// which writes every member of the archive into a directory and hands back the
/// parsed structure. So this extracts to a scratch directory, then applies the
/// same member-name and content rules the other two readers apply *before* the
/// bytes reach the gallery. The scratch directory is deleted either way.
///
/// A RAR4 file fails inside `extract_all` (it parses RAR5 headers), which
/// surfaces as an error rather than as an empty gallery — the honest outcome
/// for a format this crate does not read.
pub fn extract_rar(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    // A sibling of `dest`, never inside it: `extract_all` writes member names
    // verbatim, so a member called `sub/page.jpg` creates directories, and a
    // scratch dir under `dest` would be scanned by the gallery.
    let scratch = dest.with_extension("rar-scratch");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let parsed = rar::Archive::extract_all(
        archive.to_string_lossy().as_ref(),
        scratch.to_string_lossy().as_ref(),
        "",
    );
    let archive = match parsed {
        Ok(a) => a,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return Err(format!("Cannot read this RAR ({e})"));
        }
    };
    // The name list is authoritative over the directory walk: a member whose
    // name is not a safe flat file name must not become a gallery entry, and a
    // compressed one must not become one either.
    let mut kept: Vec<(String, PathBuf)> = Vec::new();
    for block in &archive.files {
        let name = block.name.clone();
        let Some(safe) = archive_member_name(&name) else {
            continue;
        };
        let path = scratch.join(Path::new(&name).file_name().unwrap_or_default());
        if path.is_file() {
            kept.push((safe, path));
        }
    }
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut extracted = 0;
    let mut skipped = 0;
    for (safe, path) in kept {
        if !claim_member_name(&mut seen, &safe) {
            continue;
        }
        match verify_extracted_image(&path) {
            Ok(true) => {
                std::fs::copy(&path, dest.join(&safe)).map_err(|e| e.to_string())?;
                extracted += 1;
            }
            // **The compression case.** The bytes are not an image, so the
            // member is counted as skipped and the status line says why. Left in
            // place it would become a gallery entry that fails to decode with a
            // message about a corrupt file, which is both untrue and useless.
            Ok(false) => skipped += 1,
            Err(_) => skipped += 1,
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    Ok((extracted, skipped))
}

/// True if these leading bytes are an image this app can decode.
///
/// **Content, never a file name**, which is the codebase's rule for choosing a
/// decoder and is what makes it worth having here too: a member can arrive under
/// a `.png` name carrying bytes that are not a picture. It exists as a public
/// function because the archive readers need it *after* extracting, and that
/// check is what stops an undecompressed member from becoming a gallery entry
/// that later fails to decode with a message about a corrupt file.
pub fn looks_like_supported_image(head: &[u8]) -> bool {
    is_jpeg_bytes(head)
        || head.starts_with(b"\x89PNG\r\n\x1a\n")
        || is_gif_bytes(head)
        // `RIFF` alone is not WebP: WAV and AVI start the same way, so the form
        // type four bytes in is the part that decides it.
        || (head.starts_with(b"RIFF") && head.get(8..12) == Some(b"WEBP"))
        || is_qoi_bytes(head)
        || is_tiff_bytes(head)
        || is_bmp_bytes(head)
        || is_ico_bytes(head)
}

/// True if a just-extracted file really is an image, read from its own bytes.
pub(crate) fn verify_extracted_image(path: &Path) -> Result<bool, std::io::Error> {
    use std::io::Read;
    let mut head = [0u8; 16];
    let mut f = std::fs::File::open(path)?;
    let n = f.read(&mut head)?;
    Ok(looks_like_supported_image(&head[..n]))
}

/// Extract every image member of any supported archive into `dest`.
///
/// **The three readers are not equally capable, and this function does not
/// pretend otherwise.** Returns `(extracted, skipped)` where `skipped` counts
/// members the reader could not produce — encrypted for zip, compressed for
/// rar — so the caller can say how many and why rather than showing a short
/// gallery with no explanation.
pub fn extract_archive(archive: &Path, dest: &Path) -> Result<(usize, usize), String> {
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "zip" | "cbz" => extract_zip(archive, dest),
        "7z" => extract_7z(archive, dest),
        "rar" => extract_rar(archive, dest),
        other => Err(format!("Unsupported archive: .{other}")),
    }
}

/// How many images an archive holds, without extracting it.
///
/// **A header read, not a decode.** zip and 7z read their central directory
/// and RAR walks its file headers; no member payload is touched, so this is
/// bounded by the header rather than by the archive. It is what lets the status
/// line say `vol1.cbz - 42 images` and a grid cell draw a label, and it runs on
/// the UI thread, so it must stay this cheap.
///
/// How many images an archive holds, without extracting it.
///
/// **A header read, not a decode.** zip and 7z read their central directory
/// and RAR walks its file headers; no member payload is touched, so this is
/// bounded by the header rather than by the archive. It is what lets the status
/// line say `vol1.cbz - 42 images` and a grid cell draw a label, and it runs on
/// the UI thread, so it must stay this cheap.
///
/// **It counts what extraction would produce, not how many members the archive
/// holds.** Two members can flatten to the same file name — `ch1/01.png` and
/// `ch2/01.PNG` both become `01.png`, and Windows has one file there — so a
/// plain member tally says 3 for a 2-page archive. The number on screen would
/// then disagree with the gallery beside it, and the caller has no way to tell
/// which one is lying. Hence the same [`claim_member_name`] the extractors use.
///
/// `None` for anything that cannot be read: the caller has a path, not a
/// problem to solve, and the error belongs at open time.
pub fn archive_image_count(archive: &Path) -> Option<usize> {
    let ext = archive
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "zip" | "cbz" => {
            let f = std::fs::File::open(archive).ok()?;
            let mut z = zip::ZipArchive::new(f).ok()?;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut n = 0usize;
            for i in 0..z.len() {
                if let Ok(entry) = z.by_index(i) {
                    if entry.is_dir() {
                        continue;
                    }
                    if let Some(safe) = archive_member_name(entry.name()) {
                        if claim_member_name(&mut seen, &safe) {
                            n += 1;
                        }
                    }
                }
            }
            Some(n)
        }
        "7z" => {
            use sevenz_rust2::{ArchiveReader, Password};
            let mut reader = ArchiveReader::open(archive, Password::empty()).ok()?;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut n = 0usize;
            reader
                .for_each_entries(|entry, _| {
                    if !entry.is_directory() {
                        if let Some(safe) = archive_member_name(entry.name()) {
                            if claim_member_name(&mut seen, &safe) {
                                n += 1;
                            }
                        }
                    }
                    Ok(true)
                })
                .ok()?;
            Some(n)
        }
        "rar" => {
            // The one public entry point extracts to a directory, so counting
            // without writing is not offered. This is a header read plus a
            // scratch directory, which is still bounded by the member list and
            // not by the payload.
            let scratch = std::env::temp_dir().join(format!(
                "iv-rar-count-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&scratch);
            let archive = rar::Archive::extract_all(
                archive.to_string_lossy().as_ref(),
                scratch.to_string_lossy().as_ref(),
                "",
            )
            .ok()?;
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let n = archive
                .files
                .iter()
                .filter(|b| {
                    let written = scratch
                        .join(std::path::Path::new(&b.name).file_name().unwrap_or_default())
                        .is_file();
                    match (archive_member_name(&b.name), written) {
                        (Some(safe), true) => claim_member_name(&mut seen, &safe),
                        _ => false,
                    }
                })
                .count();
            let _ = std::fs::remove_dir_all(&scratch);
            Some(n)
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Manga spreads: a page's shape, and the walk over turns
// ---------------------------------------------------------------------------
//
// A *turn* is what the screen shows at once: one page, or two pages facing each
// other. A page that is itself a scan of two printed pages takes a whole turn
// alone and **re-pairs everything after it**, which is the whole reason the walk
// below cannot be a pairing table computed once and stored - one wide page in
// the middle of a book moves every turn behind it.
//
// ## The invariant: the cursor is always a turn anchor
//
// An *anchor* is a page a turn opens on. Every function in this section is
// written assuming the position handed to it is an anchor, and that assumption
// is the design rather than a convenience: it is what makes "is this page
// alone?" one local test instead of a question about everything behind it.
//
// The tempting formulation is "can this page pair with the next one?", asked at
// whatever page the cursor happens to be on. It is wrong, and wrong in the one
// case a reader notices:
//
//     page 4 is portrait and page 5 is a fold-out, so 4 "cannot pair with 5",
//     so 4 is alone.  False: 4 is the second half of {3, 4}.
//
// Nothing local to a page can tell "the first half of a pair" from "alone",
// because that depends on what happened *behind* it. So the question is only
// ever asked at an anchor, where by construction nothing paired from behind,
// and `next_anchor`/`prev_anchor` are what keep the cursor at one. `lone`
// carries this in its doc comment; that comment is load-bearing.
//
// ## No page map, and why that is affordable
//
// Mihon keeps a `TreeSet` of isolated page indices, because it looks up an
// arbitrary prefix of a chapter of unknown length. Aidoku keeps a set *and* an
// `shiftIsolation(after:, by:)` to repair its indices when it physically inserts
// split pages. Kavita precomputes the whole pairing table server-side and
// invalidates it when a file changes. None of that is needed here, and each of
// those costs is a cost this design would really pay: a precomputed table has
// to be rebuilt whenever a dimension is learned, a file is re-exported, or the
// filter changes, and a stale entry is a *wrong pairing*, which is invisible
// until you are three pages into the wrong book.
//
// Instead every query is relative to a cursor that is always an anchor.
// Forward is O(1): one `turn_len`. **Backward is not, and cannot be** - it
// rebuilds the tiling forward from page 0, so it costs the distance from the
// front in in-memory reads over cached dimensions. See `prev_anchor` for why
// there is no local rule for it, which is the one place this design's
// "no page map" argument does not hold. The questions those need - step, key,
// dimensions, override - are exactly `PageCtx`.
//
// ## The irreducible ambiguity, and why it is survivable
//
// A single landscape page (a splash page, a fold-out insert) and a scan of two
// printed pages are geometrically identical, both about 1.414, and no
// aspect-ratio detector can separate them. Every reader in the field has this
// bug: Paperback #251, "the double page always start on the left panel"; Aidoku
// #939, wide images "forced into double-page layout, resulting in black screen".
//
// Two things contain it. `pairable`'s tolerance check means a landscape page
// will not pair with a portrait one, so it takes its own turn rather than being
// squashed into a half. And a turn's halves are drawn at one shared scale, so a
// partner that turns out to be wrong shows with a gap under it rather than
// distorted. The detector's only job is **whether to look for a partner**,
// never which side; a wrong guess costs a pairing, not a broken picture. A
// user override beats the detector outright, which is why `PageCtx` asks for one
// separately instead of folding it into the dimensions.

