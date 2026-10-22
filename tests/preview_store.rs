//! The preview store: a picture kept on disk so that showing it again does not
//! decode the file again.
//!
//! The cell cache in `thumb_cache.rs` and this one answer different questions,
//! so they cannot be one cache: a cell is 256 px and 13-80 KB, a preview is the
//! size of the panel and 1.8-3.2 MB. What they share is the shape of the
//! failure — a cache that serves the *wrong* picture is worse than no cache —
//! and so this suite has one test per way that can go wrong, each of them
//! red-verified by breaking the guard it covers.
//!
//! The store's own rule is the interesting part, and it is not "cache every
//! preview": a stored preview has to be **exactly what the decode produced**,
//! which is only true of the previews that come without a full-resolution
//! buffer beside them. A hit that could not answer the questions the app asks
//! of a preview — the source's size, the EXIF tag, the crop grid, the colour —
//! would be a different picture wearing the same pixels.
//!
//! Fixtures are built in a `tempdir` and the store root is passed explicitly,
//! so nothing here writes to the real `%LOCALAPPDATA%`.

use std::path::{Path, PathBuf};

use image_viewer::{
    read_cached_preview, write_cached_preview, ColorInfo, ColorSpace, ImageData, PreviewMeta,
};

/// Grid-cell size is 256; a preview is the size of the panel.
const TARGET: u32 = 957;

/// A preview's worth of pixels, made here rather than decoded, so the test does
/// not need a 12 MP file on disk to exercise the store.
fn pixels(w: u32, h: u32) -> ImageData {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x % 256) as u8,
                (y % 256) as u8,
                ((x ^ y) % 256) as u8,
                255,
            ]);
        }
    }
    ImageData {
        width: w,
        height: h,
        rgba,
        load_time: std::time::Duration::ZERO,
    }
}

/// The metadata a decode would have reported for a large, turned, wide-gamut
/// scan: every field of it is something the app asks of a preview.
fn meta() -> PreviewMeta {
    PreviewMeta {
        decoded_for: TARGET,
        orig: (4096, 3072),
        exif_tag: 6,
        crop_grid: (16, 16),
        colour: ColorInfo {
            space: ColorSpace::WideGamut,
            description: "Display P3".to_string(),
            profile_bytes: 512,
            data_space: "RGB ".to_string(),
            red_primaries: Some([33759, 15807, -69]),
        },
    }
}

/// A store root and a file to store, in one temp directory.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Fixture {
            dir: tempfile::tempdir().unwrap(),
        }
    }
    fn root(&self) -> PathBuf {
        self.dir.path().join("previews")
    }
    /// A file that exists, so the store has a stamp to compare against. Its
    /// bytes are not a picture: the store never looks at them.
    fn source(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, b"not really a picture").unwrap();
        p
    }
}

/// **A hit is the decode it replaced, pixel for pixel and answer for answer.**
///
/// This is the property the whole feature rests on: a stored preview is used
/// *instead of* a decode, so if it is not the same picture the app is showing
/// something it did not decode. The metadata matters as much as the pixels —
/// the crop and save paths compose with the EXIF tag and the crop grid, and the
/// metadata panel prints the colour.
#[test]
fn a_stored_preview_comes_back_byte_for_byte() {
    let f = Fixture::new();
    let src = f.source("photo.jpg");
    let data = pixels(256, 192);
    write_cached_preview(&f.root(), &src, 0, meta(), &data);

    let hit = read_cached_preview(&f.root(), &src, 0, TARGET).expect("a hit");
    assert_eq!(hit.preview.rgba, data.rgba, "the pixels are the decode's");
    assert_eq!((hit.preview.width, hit.preview.height), (256, 192));
    // The shape a decode of a large source produces: a preview and nothing else.
    assert!(hit.full.is_none(), "a stored preview carries no full buffer");
    assert!(hit.below_target);
    assert_eq!(hit.orig, Some((4096, 3072)));
    assert_eq!(hit.exif_tag, 6);
    assert_eq!(hit.crop_grid, (16, 16));
    assert_eq!(hit.colour.space, ColorSpace::WideGamut);
    assert_eq!(hit.colour.description, "Display P3");
    assert_eq!(hit.colour.profile_bytes, 512);
    assert_eq!(hit.colour.data_space, "RGB ");
    assert_eq!(hit.colour.red_primaries, Some([33759, 15807, -69]));
}

/// **A file that changed is a miss**, however small the change.
///
/// The stamp is mtime **and** length: a save that lands inside one filesystem
/// timestamp tick changes the bytes without changing the time, and that is the
/// case that would otherwise show a stale picture.
#[test]
fn a_changed_file_is_a_miss() {
    let f = Fixture::new();
    let src = f.source("photo.jpg");
    write_cached_preview(&f.root(), &src, 0, meta(), &pixels(256, 192));
    assert!(read_cached_preview(&f.root(), &src, 0, TARGET).is_some());

    // A different length: the file was re-exported.
    std::fs::write(&src, b"not really a picture, but longer now").unwrap();
    assert!(
        read_cached_preview(&f.root(), &src, 0, TARGET).is_none(),
        "a re-exported file must not be served from the store"
    );
}

/// **A different file, and a different page, are misses.**
///
/// The page is part of the key: a multi-page document's pages are different
/// pictures, and an entry keyed by the path alone would hand page 1 back for
/// page 3.
#[test]
fn another_path_and_another_page_are_misses() {
    let f = Fixture::new();
    let src = f.source("photo.jpg");
    let other = f.source("other.jpg");
    write_cached_preview(&f.root(), &src, 0, meta(), &pixels(256, 192));

    // A different file is a different entry, so there is nothing to hit.
    assert!(read_cached_preview(&f.root(), &other, 0, TARGET).is_none());
    // And so is a different page.
    assert!(read_cached_preview(&f.root(), &src, 1, TARGET).is_none());

    // **The collision case**, which is what the path in the header is for: the
    // name is a hash of the path and the page, so two files that hash the same
    // would share one entry. Put this entry's bytes under another name and the
    // header has to notice.
    let bytes = std::fs::read(image_viewer::preview_store_entry(&f.root(), &src, 0)).unwrap();
    // Another path is another fanout directory, so the copy needs one.
    let other_entry = image_viewer::preview_store_entry(&f.root(), &other, 0);
    std::fs::create_dir_all(other_entry.parent().unwrap()).unwrap();
    std::fs::write(&other_entry, &bytes).unwrap();
    assert!(
        read_cached_preview(&f.root(), &other, 0, TARGET).is_none(),
        "an entry made for another file must not be served for this one"
    );
    let page_one = image_viewer::preview_store_entry(&f.root(), &src, 1);
    std::fs::create_dir_all(page_one.parent().unwrap()).unwrap();
    std::fs::write(&page_one, &bytes).unwrap();
    assert!(
        read_cached_preview(&f.root(), &src, 1, TARGET).is_none(),
        "an entry made for another page must not be served for this one"
    );
}

/// **A half-written entry is a miss, not a picture with a black band.**
///
/// `image`'s PNG decoder accepts a stream missing its last 64 bytes and hands
/// back a full-size image with the tail zeroed. That was a live bug in the
/// cell cache, and it is the same decoder here — so the cut has to be the
/// **terminator** (one byte is enough), not the data: a cut that also breaks
/// the compressed stream is caught by the decoder and proves nothing about the
/// completeness check.
#[test]
fn a_truncated_entry_is_a_miss() {
    let f = Fixture::new();
    let src = f.source("photo.jpg");
    write_cached_preview(&f.root(), &src, 0, meta(), &pixels(256, 192));
    let entry = image_viewer::preview_store_entry(&f.root(), &src, 0);
    let bytes = std::fs::read(&entry).unwrap();
    for cut in [1usize, 12, 64] {
        let mut short = bytes.clone();
        short.truncate(bytes.len() - cut);
        std::fs::write(&entry, &short).unwrap();
        assert!(
            read_cached_preview(&f.root(), &src, 0, TARGET).is_none(),
            "an entry missing its last {cut} bytes must not be decoded"
        );
    }
    // And the intact entry still works, so the cuts above were not simply
    // deleting the file and the assertions were not vacuous.
    std::fs::write(&entry, &bytes).unwrap();
    assert!(read_cached_preview(&f.root(), &src, 0, TARGET).is_some());
}

/// **An entry this version did not write is a miss.**
///
/// The magic and the version are what make a format change safe: an entry from
/// an older build is "no entry here", not a wrong picture.
#[test]
fn a_damaged_header_is_a_miss() {
    let f = Fixture::new();
    let src = f.source("photo.jpg");
    write_cached_preview(&f.root(), &src, 0, meta(), &pixels(256, 192));
    let entry = image_viewer::preview_store_entry(&f.root(), &src, 0);
    let mut bytes = std::fs::read(&entry).unwrap();
    bytes[0] = b'X';
    std::fs::write(&entry, &bytes).unwrap();
    assert!(read_cached_preview(&f.root(), &src, 0, TARGET).is_none());

    // And a version from the future.
    write_cached_preview(&f.root(), &src, 0, meta(), &pixels(256, 192));
    let mut bytes = std::fs::read(&entry).unwrap();
    bytes[4] = 99;
    std::fs::write(&entry, &bytes).unwrap();
    assert!(read_cached_preview(&f.root(), &src, 0, TARGET).is_none());
}

/// **A preview decoded for a target the display has outgrown is a miss.**
///
/// The rule is [`image_viewer::needs_refine`], the same one the in-memory cache
/// uses: a preview decoded for a much larger target is being minified by more
/// than the pipeline filters for, and a source with more to give should be
/// decoded again. The other arm matters too: a preview decoded for a *small*
/// target cannot serve a much larger display.
#[test]
fn a_preview_for_the_wrong_target_is_refused() {
    let f = Fixture::new();
    let src = f.source("photo.jpg");
    let mut m = meta();
    m.decoded_for = TARGET;
    write_cached_preview(&f.root(), &src, 0, m, &pixels(256, 192));

    // The same target: a hit.
    assert!(read_cached_preview(&f.root(), &src, 0, TARGET).is_some());

    // A display far larger than the one it was decoded for: the source has more
    // to give, so the stored pixels are not what the display can show.
    assert!(
        read_cached_preview(&f.root(), &src, 0, 8000).is_none(),
        "a preview decoded for 957 must not serve an 8000 px display"
    );

    // And the other way round: a preview decoded for a huge target is minified
    // by more than the pipeline filters for.
    let mut big = meta();
    big.decoded_for = 8000;
    write_cached_preview(&f.root(), &src, 0, big, &pixels(256, 192));
    assert!(
        read_cached_preview(&f.root(), &src, 0, TARGET).is_none(),
        "a preview decoded for 8000 must not be minified into a 957 panel"
    );
}

/// **The entry is renamed into place, so a reader never sees half of one.**
///
/// A second instance of the app reads these too, and so does the reader that
/// arrives while the writer is still going.
#[test]
fn the_entry_is_written_atomically() {
    let f = Fixture::new();
    let src = f.source("photo.jpg");
    write_cached_preview(&f.root(), &src, 0, meta(), &pixels(256, 192));
    let entry = image_viewer::preview_store_entry(&f.root(), &src, 0);
    assert!(entry.exists());
    // Nothing left over from the write: no half-written file beside it.
    let leftovers: Vec<PathBuf> = std::fs::read_dir(entry.parent().unwrap())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "tmp").unwrap_or(false))
        .collect();
    assert!(leftovers.is_empty(), "left over: {leftovers:?}");
}

/// **The store is trimmed to its cap, oldest first.**
///
/// The entries are megabytes, so an untrimmed store would be a disk problem
/// rather than a cache.
#[test]
fn the_store_is_trimmed_to_its_cap() {
    let f = Fixture::new();
    for i in 0..4 {
        let p = f.source(&format!("photo{i}.jpg"));
        write_cached_preview(&f.root(), &p, 0, meta(), &pixels(256, 192));
    }
    let before = total_bytes(&f.root());
    assert!(before > 0);
    // Trim to a quarter of what is there. The *order* the trim uses (oldest
    // first) is not asserted here: the file system's timestamp granularity
    // decides which of four entries written in the same millisecond is older,
    // and a test that depends on that is a test that fails on another disk.
    let left = image_viewer::prune_cache(&f.root(), before / 4);
    assert!(
        left <= before / 4,
        "left {left} bytes of {before} after trimming to a quarter"
    );
    assert!(left < before, "something was trimmed");
    // And the store still works afterwards: a trim that leaves broken entries
    // would be worse than no trim.
    let src = f.source("after.jpg");
    write_cached_preview(&f.root(), &src, 0, meta(), &pixels(256, 192));
    assert!(read_cached_preview(&f.root(), &src, 0, TARGET).is_some());
}

/// The bytes in a store, by walking it.
fn total_bytes(root: &Path) -> u64 {
    let mut total = 0;
    let Ok(fanouts) = std::fs::read_dir(root) else {
        return 0;
    };
    for fanout in fanouts.flatten() {
        let Ok(files) = std::fs::read_dir(fanout.path()) else {
            continue;
        };
        for f in files.flatten() {
            if let Ok(meta) = f.metadata() {
                total += meta.len();
            }
        }
    }
    total
}

/// **Only a preview that came without a full buffer is worth storing.**
///
/// This is the rule the whole feature turns on: a stored preview is used
/// *instead of* a decode, so it has to be exactly what the decode produced. A
/// decode that also produced the full-resolution buffer has a preview that is
/// only half of what it costs — storing that half would mean decoding the file
/// again for the other half, which is slower than never caching at all.
#[test]
fn only_a_preview_without_a_full_buffer_is_worth_storing() {
    let big = Some((4096u32, 3072u32));
    let small = Some((1000u32, 800u32));
    let full = pixels(256, 192);
    // A large source decoded without a full buffer: the DCT path. Worth storing.
    assert!(image_viewer::preview_is_storeable(big, None, None));
    // The same source with a full buffer beside it: not worth storing.
    assert!(!image_viewer::preview_is_storeable(big, Some(&full), None));
    // A small source: its decode is one pass that yields both, and caching the
    // preview alone would cost a second decode.
    assert!(!image_viewer::preview_is_storeable(small, None, None));
    // An animation's first frame is not a picture of the file.
    assert!(!image_viewer::preview_is_storeable(big, None, Some(&animation())));
    // And a source whose size could not be read is not stored: the rule is the
    // size, and a guess would put a cheap picture in a cache that exists for
    // expensive ones.
    assert!(!image_viewer::preview_is_storeable(None, None, None));
}

/// A stand-in animation, for the one arm that has to recognise one.
fn animation() -> image_viewer::Animation {
    image_viewer::Animation {
        frames: Vec::new(),
        delays: Vec::new(),
    }
}

/// The decode path's wiring, as text, because it is about *position*.
///
/// Three things have to be true and none of them is visible from outside the
/// function:
///
/// * the store is asked **before the file is read**, or a hit would still pay
///   for the read it was meant to avoid;
/// * the encode is **not** done inline — it is handed to the store thread, or
///   the 14-33 ms of encoding would land in front of the first pixels, which
///   is the one regression this feature is not allowed to cause;
/// * the write happens **after** the decode, from the decode's own pixels.
#[test]
fn the_store_is_asked_before_the_file_is_read_and_written_off_the_decode() {
    let src = include_str!("../src/lib.rs");
    let body = code_only(&fn_body(src, "decode_page"));

    let store = body.find("read_cached_preview").expect("the store is asked");
    let read = body.find("std::fs::read(path)").expect("the file is read");
    assert!(
        store < read,
        "the store must be asked before the file is read, or a hit pays for the read"
    );

    // The write is handed over, not done here.
    assert!(
        body.contains("maybe_store_preview("),
        "the decode hands its preview to the store"
    );
    let store_fn = code_only(&fn_body(src, "maybe_store_preview"));
    assert!(
        store_fn.contains("store_preview("),
        "and the store call is a hand-off to the thread, not an encode here"
    );
    let hand_off = code_only(&fn_body(src, "store_preview"));
    assert!(
        hand_off.contains("try_send"),
        "the hand-off must not block the worker that decoded the picture"
    );

    // And the writer's half of the atomicity: the entry is renamed into place,
    // so a reader sees the old one or the new one and never half of one.
    let write = code_only(&fn_body(src, "write_cached_preview"));
    let tmp = write.find("png.tmp").expect("the entry is written beside itself");
    let rename = write.find("rename").expect("and renamed into place");
    assert!(tmp < rename, "the rename is what makes the write atomic");
}

/// A body with its comments removed.
///
/// **A scan that searches for a word finds the word in a comment too**, and a
/// comment that explains the code tends to mention the very call the scan is
/// looking for — in the wrong order. The comment above this write says
/// "renamed into place: `fs::rename` replaces on Windows", which is *before*
/// the code that renames, so a scan over the raw text reads the comment's
/// order and fails on correct code.
fn code_only(body: &str) -> String {
    body.lines()
        .map(|l| match l.find("//") {
            Some(i) => &l[..i],
            None => l,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The body of `fn name` in `src`, brace-matched.
///
/// **Anchored on a line start, and that is not fussiness.** A doc comment that
/// mentions the function by name (`/// ... `fn write_cached_preview` ...`) puts
/// the same text in the file, and a naive search finds *that* first — and then
/// takes the first `{` after it, which is a brace inside the comment. The scan
/// then reads the comment and passes or fails for the wrong reason. This one
/// looks for the definition at the start of a line.
fn fn_body(src: &str, name: &str) -> String {
    let needle = format!("\nfn {name}(");
    let start = src
        .find(&needle)
        .or_else(|| src.find(&format!("\npub fn {name}(")))
        .unwrap_or_else(|| panic!("`fn {name}` is not in the source"));
    let open = start + src[start..].find('{').expect("a body");
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return src[open..=open + i].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces in `fn {name}`");
}
