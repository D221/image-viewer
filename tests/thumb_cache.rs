//! Grid thumbnails: the cell-shaped decode, and the on-disk cache in front of it.
//!
//! Two separable claims, tested separately because they have different failure
//! modes and different evidence:
//!
//! * **A cell is a different request from a preview.** It must come back at the
//!   cell's size, must not carry a full-resolution image, and must show the same
//!   pixels the main view would. The old code asked for the same thing with a
//!   smaller number and got a 100 MB `full` and a 5851 KB texture for a 150 pt
//!   cell.
//! * **A cache hit is the same picture, or it is nothing.** Every way this can go
//!   wrong produces a grid cell showing the wrong image, which is worse than a
//!   blank one and is not something a user can diagnose — so each of them gets a
//!   test that fails if the guard is removed: a stale stamp, a hash collision, a
//!   half-written entry, a foreign file in the cache directory.
//!
//! Fixtures are built in a `tempdir` and the cache root is passed explicitly, so
//! nothing here reads `test-images/` (gitignored) or writes to the real
//! `%LOCALAPPDATA%`. Deliberately no wall-clock assertion except the one in
//! `an_animated_file_gives_a_cell_one_frame_not_the_whole_loop`, which compares
//! two operations in the same process rather than against a threshold.

#[path = "common/exif_fixtures.rs"]
mod exif_fixtures;
use exif_fixtures::{jpeg, jpeg_with_thumbnail, Endian};
use image_viewer::{
    load_image_preview_full, load_thumbnail, prune_thumb_cache, read_cached_thumbnail,
    set_thumbnail_cache, thumb_cache_entry, thumb_stamp, write_cached_thumbnail, ImageData,
};
use std::path::{Path, PathBuf};

/// Grid cell size, matching `THUMB_DIM` in main.rs.
const CELL: u32 = 256;

/// A source file plus the directory holding it and the cache root.
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
        self.dir.path().join("cache")
    }
    /// Write `bytes` as `name` and return the path.
    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }
    /// A JPEG big enough that the cell must be smaller than the source.
    fn jpeg(&self, name: &str, w: u32, h: u32) -> PathBuf {
        self.file(name, &jpeg(w, h))
    }
    /// The bytes of a cell's entry, so a test can damage them.
    fn entry_bytes(&self, path: &Path) -> Vec<u8> {
        std::fs::read(thumb_cache_entry(&self.root(), path)).unwrap()
    }
    fn put_entry_bytes(&self, path: &Path, bytes: &[u8]) {
        let entry = thumb_cache_entry(&self.root(), path);
        std::fs::create_dir_all(entry.parent().unwrap()).unwrap();
        std::fs::write(entry, bytes).unwrap();
    }
}

fn self_consistent(d: &ImageData, what: &str) {
    assert_eq!(
        d.rgba.len(),
        (d.width as usize) * (d.height as usize) * 4,
        "{what}: {}x{} but {} bytes",
        d.width,
        d.height,
        d.rgba.len()
    );
    assert!(d.width > 0 && d.height > 0, "{what}: zero dimension");
}

/// Force a file's modification time, so a test can change *only* the timestamp.
///
/// This is the case the stamp exists for and it cannot be reached by writing the
/// file: a rewrite that keeps the length and lands inside the same filesystem
/// timestamp tick leaves mtime and size both unchanged, and a cache that trusted
/// either one alone would serve the old picture. Setting the time directly also
/// keeps the test from having to sleep for the clock to tick.
fn set_mtime(path: &Path, secs: u64) {
    let f = std::fs::File::options().write(true).open(path).unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
    f.set_times(std::fs::FileTimes::new().set_modified(t)).unwrap();
}

// ---------------------------------------------------------------------------
// The cell is a different request from a preview
// ---------------------------------------------------------------------------

#[test]
fn a_cell_is_the_cell_sized_picture_and_nothing_else() {
    let f = Fixture::new();
    // 1600x1200, so the source is well over the cell in both directions and the
    // JPEG has no embedded thumbnail to take a shortcut.
    let p = f.jpeg("t.jpg", 1600, 1200);
    let cell = load_thumbnail(&p, CELL).expect("cell must load");

    self_consistent(&cell, "cell");
    assert!(
        cell.width.max(cell.height) <= CELL,
        "cell is {}x{}, asked for {CELL}",
        cell.width,
        cell.height
    );
}

#[test]
fn a_cell_of_a_source_too_big_for_one_eighth_still_comes_back_cell_sized() {
    // The case that needs the downscale in `load_thumbnail` rather than inside
    // `decode_page`. `decode_jpeg_scaled` can only pick 1/8, 1/4, 1/2 or 1, so the
    // *smallest* reduction that still covers a 256 px cell is 1/8 — and 1/8 of a
    // 2400 px scan is 300 px. That is a 240 KB texture for a 150 pt cell, and it is
    // what the grid used to upload (5851 KB for the 17000 px scan in `test-images`,
    // which is 44x worse).
    //
    // 2400 rather than the 6000 a real scan would be, because the fixture has to be
    // *encoded* by the test and this suite is also run in debug: 2400 px is the
    // smallest size where 1/8 still lands above the cell, which is the property
    // being tested. It exercises the same wrapper either way — that wrapper is
    // exactly what makes one downscale cover `decode_page`'s DCT return point
    // (files over `FAST_FULL_MAX_PIXELS`) and its decode-and-resize one.
    let f = Fixture::new();
    let p = f.jpeg("big.jpg", 2400, 1600);
    let cell = load_thumbnail(&p, CELL).expect("cell must load");
    self_consistent(&cell, "cell");
    assert!(
        cell.width.max(cell.height) <= CELL,
        "cell is {}x{} ({:.0} KB of texture) for a {CELL} px cell",
        cell.width,
        cell.height,
        cell.rgba.len() as f64 / 1024.0
    );
}

#[test]
fn a_cell_of_a_non_jpeg_and_the_main_view_agree_on_the_pixels() {
    // For every format whose only reduced-size read is "decode it all, then
    // resize", the cell and the main view go through the same code at the same
    // target and must produce *identical* buffers — so a cell can never show a
    // differently-resampled version of the image it opens.
    //
    // **JPEG is deliberately not in this list, and the asymmetry is real.** The
    // cell takes the DCT-scaled read (`decode_jpeg_scaled` at 1/8), while the main
    // view of a file under `FAST_FULL_MAX_PIXELS` decodes at 1/1 and resizes. Those
    // are two different reconstructions of the same picture and they are *not*
    // equal — which is the point: the cell is a cheaper read, not a worse copy of
    // the same read. What a JPEG cell must satisfy is only that it is at most the
    // target, which `a_cell_is_the_cell_sized_picture_and_nothing_else` pins.
    for (w, h) in [(1200u32, 900u32), (900, 1600), (100, 70)] {
        let f = Fixture::new();
        let p = f.file(&format!("t{w}x{h}.png"), &png(w, h));
        let cell = load_thumbnail(&p, CELL).unwrap();
        let view = load_image_preview_full(&p, CELL).unwrap();
        assert_eq!(
            (cell.width, cell.height),
            (view.preview.width, view.preview.height),
            "{w}x{h}: cell and view disagree on size"
        );
        assert_eq!(
            cell.rgba, view.preview.rgba,
            "{w}x{h}: cell and view disagree on pixels"
        );
    }
}

/// A PNG of `w x h` with a gradient, written as a real PNG (so the crate that
/// writes it also has to read it back).
fn png(w: u32, h: u32) -> Vec<u8> {
    let mut img = image::RgbaImage::from_pixel(w, h, image::Rgba([90, 120, 200, 255]));
    for (x, y, px) in img.enumerate_pixels_mut() {
        *px = image::Rgba([
            (x % 251) as u8,
            (y % 241) as u8,
            (x.wrapping_mul(y) % 200) as u8,
            255,
        ]);
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn a_cell_of_a_non_jpeg_keeps_nothing_at_source_size() {
    // PNG is the case the old code got most wrong: there is no reduced-size read
    // for it at all, so the preview path decoded at MAX_TEXTURE_DIM, kept the
    // result as `full`, and a 256 px cell shipped 100 MB across the channel to be
    // dropped. What is pinned here is the difference between the two requests — the
    // cell is small, and the main view still rides on the full decode. The
    // transient full-size buffer inside `dynamic_to_image_data` is a separate piece
    // of work (a row-streaming decode) and is not what this claims.
    let f = Fixture::new();
    let p = f.file("big.png", &png(1200, 900));

    let cell = load_thumbnail(&p, CELL).unwrap();
    self_consistent(&cell, "png cell");
    assert!(cell.width.max(cell.height) <= CELL, "{}x{}", cell.width, cell.height);

    let view = load_image_preview_full(&p, CELL).unwrap();
    assert!(
        view.full.is_some(),
        "the main view must still ride on the full decode"
    );
}

#[test]
fn a_cell_of_a_rotated_file_is_the_oriented_picture() {
    // The cache stores what the cell paints, so a cell of a quarter-turned file
    // has to be turned too — and the test for that is that a cache *hit* equals
    // this decode, not that the decode is turned (which `embedded_thumbs.rs`
    // already covers).
    let f = Fixture::new();
    let bytes = jpeg_with_thumbnail(1200, 900, 6, &jpeg(120, 160), Endian::Little, None);
    let p = f.file("rot.jpg", &bytes);
    let cell = load_thumbnail(&p, CELL).unwrap();
    self_consistent(&cell, "rotated cell");
    let cached = read_cached_thumbnail(&f.root(), &p, CELL);
    // Not cached yet (nothing wrote it) — so assert via the writer, below.
    assert!(cached.is_none(), "read must not invent an entry");
    write_cached_thumbnail(&f.root(), &p, CELL, &cell);
    let hit = read_cached_thumbnail(&f.root(), &p, CELL).expect("must hit");
    assert_eq!((hit.width, hit.height), (cell.width, cell.height));
    assert_eq!(hit.rgba, cell.rgba);
}

#[test]
fn a_cell_of_an_animation_shows_the_frame_the_main_view_would() {
    // A cell is one picture, and the picture it has to match is the first frame —
    // the same one the main view shows before it starts animating. A grid cell that
    // is a slideshow would be worse than useless, and a cell that decoded the whole
    // loop to get that first frame is doing 16x the work for it (measured in
    // release: 7.9 ms against 344 ms for the 900x318 GIF in `test-images`; see
    // `tests/bench_browse.rs` for the A/B).
    //
    // **No timing assertion here, and the reason is worth recording.** This started
    // as one — a ratio between a 16-frame cell and a 1-frame cell with the same
    // first frame, which is the shape AGENTS.md asks for. It was wrong twice: the
    // arm that ran first absorbed every one-time cost in the process (550 ms
    // against 53 ms, which looked exactly like a 10x regression and was pure
    // warm-up), and after interleaving and min-of-two it was rayon's global thread
    // pool being built by the first `linear_resize` call, which cost ~450 ms in a
    // debug build and landed on whichever arm went first. Both artefacts are the
    // kind AGENTS.md says this project has already been burned by four times. The
    // speed claim belongs in a benchmark on a quiet machine, not in a unit test on
    // a shared one; what is pinned here is the part that is deterministic.
    let f = Fixture::new();
    let (multi, single) = animated_gifs(4);
    let p_multi = f.file("multi.gif", &multi);
    let p_single = f.file("single.gif", &single);

    let a = load_thumbnail(&p_multi, CELL).expect("cell must load");
    self_consistent(&a, "multi-frame cell");
    // The 1-frame file has the identical first frame, so the cells must be
    // identical — which is only true if the multi-frame cell really is frame 0.
    let b = load_thumbnail(&p_single, CELL).unwrap();
    assert_eq!((a.width, a.height), (b.width, b.height));
    assert_eq!(a.rgba, b.rgba, "a cell must show the first frame, not a later one");

    // And it must be the frame the main view would show. The view's frames are at
    // up to `PREVIEW_TEXTURE_DIM`, not cell size — a main view is drawn at display
    // density and a cell is not — so the comparison has to put them on the same
    // scale, using the same resampler the app uses. Then it is exact.
    let view = load_image_preview_full(&p_multi, CELL).unwrap();
    let anim = view.anim.as_ref().expect("the main view decodes the whole loop");
    assert_eq!(anim.frames.len(), 4, "sanity: the fixture really has 4 frames");
    assert_eq!(view.preview.rgba, anim.frames[0].rgba, "the view shows frame 0");
    let at_cell_size = image_viewer::downscale_image(&anim.frames[0], CELL);
    assert_eq!(
        (at_cell_size.width, at_cell_size.height),
        (a.width, a.height),
        "cell size is not frame 0 at cell size"
    );
    assert_eq!(
        at_cell_size.rgba, a.rgba,
        "a cell must show the same picture the main view opens on"
    );
}

#[test]
fn a_cell_of_an_animation_is_not_capped_by_the_animation_limits() {
    // `load_animation` stops at `MAX_ANIM_FRAMES` (240) and `MAX_ANIM_BYTES`
    // (256 MB). A cell decodes one frame and is subject to neither, so a long
    // animation still gets a cell. 300 frames is over the cap on purpose: if the
    // cell went through the loop path it would come back truncated or empty rather
    // than showing frame 0.
    let f = Fixture::new();
    let (many, _) = animated_gifs_small(300);
    let p = f.file("long.gif", &many);
    let cell = load_thumbnail(&p, CELL).expect("a 300-frame animation still gets a cell");
    self_consistent(&cell, "long-animation cell");
    let view = load_image_preview_full(&p, CELL).unwrap();
    let anim = view.anim.as_ref().expect("the view decodes the loop");
    assert!(
        anim.frames.len() <= 240,
        "the fixture is meant to exceed the frame cap, got {}",
        anim.frames.len()
    );
}

/// A GIF of `frames` frames plus a 1-frame GIF with the same first frame.
///
/// Four frames of 240x180. The count only has to exceed one (the claim is about
/// which frame a cell shows, not how many there are) and the *pixels* are what the
/// comparison reads, so the frames are kept small: this fixture's other half is a
/// full `load_image_preview_full`, which decodes the whole loop, and at 16 frames
/// of 400x300 that was 29 s of a debug test suite.
fn animated_gifs(frames: u32) -> (Vec<u8>, Vec<u8>) {
    let mut multi = Vec::new();
    let mut enc = image::codecs::gif::GifEncoder::new(&mut multi);
    enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
    for i in 0..frames {
        enc.encode_frame(gif_frame(i as u8)).unwrap();
    }
    drop(enc);
    let mut single = Vec::new();
    let mut enc = image::codecs::gif::GifEncoder::new(&mut single);
    enc.encode_frame(gif_frame(0)).unwrap();
    drop(enc);
    (multi, single)
}

/// One opaque frame whose colour identifies it, so "which frame did the cell show"
/// is answerable from the pixels.
fn gif_frame(i: u8) -> image::Frame {
    let mut img = image::RgbaImage::from_pixel(240, 180, image::Rgba([i, 255 - i, 40, 255]));
    // A little structure, so the resizer has something to do and the frames are
    // not a flat block that every filter reproduces identically.
    for x in 0..240u32 {
        img.put_pixel(x, x % 180, image::Rgba([x as u8, 0, i, 255]));
    }
    image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(100, 1))
}

/// A GIF of `frames` tiny frames, plus a 1-frame GIF with the same first frame.
///
/// Small frames because this fixture exists to cross `MAX_ANIM_FRAMES` (240), not
/// to be looked at: 300 frames of 400x300 would be 43 MB of pixels to encode in a
/// debug build in order to assert something about a *count*.
fn animated_gifs_small(frames: u32) -> (Vec<u8>, Vec<u8>) {
    let tiny = |i: u8| {
        let mut img = image::RgbaImage::from_pixel(64, 48, image::Rgba([i, 255 - i, 40, 255]));
        for x in 0..64u32 {
            img.put_pixel(x, x % 48, image::Rgba([x as u8, 0, i, 255]));
        }
        image::Frame::from_parts(img, 0, 0, image::Delay::from_numer_denom_ms(100, 1))
    };
    let mut many = Vec::new();
    let mut enc = image::codecs::gif::GifEncoder::new(&mut many);
    enc.set_repeat(image::codecs::gif::Repeat::Infinite).unwrap();
    for i in 0..frames {
        enc.encode_frame(tiny(i as u8)).unwrap();
    }
    drop(enc);
    let mut one = Vec::new();
    let mut enc = image::codecs::gif::GifEncoder::new(&mut one);
    enc.encode_frame(tiny(0)).unwrap();
    drop(enc);
    (many, one)
}

// ---------------------------------------------------------------------------
// A cache hit is the same picture, or it is nothing
// ---------------------------------------------------------------------------

#[test]
fn a_cached_cell_is_byte_for_byte_the_pixels_the_decode_produced() {
    // The property the whole cache rests on. Not "looks the same": the *same
    // bytes*, so a hit can never show something the main view cannot reproduce,
    // and so this test would fail if the stored form ever became lossy.
    let f = Fixture::new();
    let p = f.jpeg("t.jpg", 1200, 800);
    let decoded = load_thumbnail(&p, CELL).unwrap();
    assert!(read_cached_thumbnail(&f.root(), &p, CELL).is_none(), "must start empty");
    write_cached_thumbnail(&f.root(), &p, CELL, &decoded);
    let hit = read_cached_thumbnail(&f.root(), &p, CELL).expect("must hit");
    assert_eq!((hit.width, hit.height), (decoded.width, decoded.height));
    assert_eq!(hit.rgba, decoded.rgba);
}

#[test]
fn a_file_edited_on_disk_is_a_miss_and_not_a_stale_picture() {
    // Three ways a file can change under a cache that only looked at one of these.
    // The dangerous one is the middle test: same length, different time.
    let f = Fixture::new();

    // (a) different length
    let p = f.jpeg("len.jpg", 800, 600);
    let d = load_thumbnail(&p, CELL).unwrap();
    write_cached_thumbnail(&f.root(), &p, CELL, &d);
    std::fs::write(&p, jpeg(900, 700)).unwrap();
    assert!(
        read_cached_thumbnail(&f.root(), &p, CELL).is_none(),
        "a longer file must not reuse the old cell"
    );

    // (b) same length, different mtime — a re-save of equal size
    let p = f.jpeg("time.jpg", 800, 600);
    let d = load_thumbnail(&p, CELL).unwrap();
    write_cached_thumbnail(&f.root(), &p, CELL, &d);
    let before = thumb_stamp(&p).unwrap();
    set_mtime(&p, before.mtime_secs + 120);
    let after = thumb_stamp(&p).unwrap();
    assert_eq!(before.len, after.len, "the length must be unchanged for this test");
    assert_ne!(before.mtime_secs, after.mtime_secs, "the mtime must have moved");
    assert!(
        read_cached_thumbnail(&f.root(), &p, CELL).is_none(),
        "a re-saved file of the same length must not reuse the old cell"
    );
}

#[test]
fn a_different_target_is_a_miss() {
    // `THUMB_DIM` is a constant today, so this cannot happen yet. It is here
    // because the alternative is a cache whose entries are only valid for the size
    // that happened to be compiled in, discovered by a blurry grid after someone
    // changed a constant.
    let f = Fixture::new();
    let p = f.jpeg("t.jpg", 1200, 800);
    let d = load_thumbnail(&p, CELL).unwrap();
    write_cached_thumbnail(&f.root(), &p, CELL, &d);
    assert!(read_cached_thumbnail(&f.root(), &p, 512).is_none());
    assert!(read_cached_thumbnail(&f.root(), &p, CELL).is_some());
}

#[test]
fn a_hash_collision_is_a_miss_and_not_a_neighbours_thumbnail() {
    // The cache name is 64 bits of hash, so two files *can* land on the same entry.
    // What stops that from being two cells showing each other's pictures is the
    // path stored in the entry — so this plants one file's entry at another file's
    // address with every other field matching, and requires a miss.
    let f = Fixture::new();
    let a = f.jpeg("a.jpg", 400, 300);
    let b = f.jpeg("b.jpg", 400, 300);
    // Same length (both 400x300) and same mtime, so the stamp cannot tell them
    // apart either. Only the path can.
    set_mtime(&b, thumb_stamp(&a).unwrap().mtime_secs);
    assert_eq!(thumb_stamp(&a).unwrap().len, thumb_stamp(&b).unwrap().len);

    let da = load_thumbnail(&a, CELL).unwrap();
    write_cached_thumbnail(&f.root(), &a, CELL, &da);
    assert!(read_cached_thumbnail(&f.root(), &a, CELL).is_some(), "a must hit");

    // Plant A's entry at B's address.
    f.put_entry_bytes(&b, &f.entry_bytes(&a));
    assert_ne!(
        thumb_cache_entry(&f.root(), &a),
        thumb_cache_entry(&f.root(), &b),
        "the two paths must have different addresses for this test to mean anything"
    );
    assert!(
        read_cached_thumbnail(&f.root(), &b, CELL).is_none(),
        "an entry for another file must be a miss, not that file's cell"
    );
}

#[test]
fn a_damaged_entry_is_a_miss_rather_than_a_panic() {
    // The cache directory is ours, but it is also writable by anything running as
    // the user, and a process killed mid-write can leave a `.tmp` behind. Every one
    // of these must be a miss: a cell showing garbage from a corrupt file is worse
    // than a blank cell, and a panic here is a dead worker.
    let f = Fixture::new();
    let p = f.jpeg("t.jpg", 900, 700);
    let d = load_thumbnail(&p, CELL).unwrap();
    write_cached_thumbnail(&f.root(), &p, CELL, &d);
    let good = f.entry_bytes(&p);
    assert!(read_cached_thumbnail(&f.root(), &p, CELL).is_some());

    let mut damaged: Vec<(String, Vec<u8>)> = Vec::new();
    damaged.push(("empty".into(), Vec::new()));
    for cut in [1usize, 4, 5, 8, 20, 37, 38, good.len() - 1] {
        damaged.push((format!("truncated to {cut}"), good[..cut].to_vec()));
    }
    let mut wrong_magic = good.clone();
    wrong_magic[..4].copy_from_slice(b"XXXX");
    damaged.push(("bad magic".into(), wrong_magic));
    let mut wrong_version = good.clone();
    wrong_version[4] = 99;
    damaged.push(("future version".into(), wrong_version));
    // A path_len that runs off the end of the file.
    let mut long_path = good.clone();
    long_path[36..38].copy_from_slice(&u16::MAX.to_le_bytes());
    damaged.push(("path_len past the end".into(), long_path));
    // A valid header, a payload that is cut short. **This is the one that found a
    // real bug**: without an explicit completeness check, `image`'s PNG decoder
    // accepts a stream missing its last 64 bytes and returns a full-size image
    // with the tail zeroed, so this was a *hit* — a grid cell with a black band
    // across it and no error anywhere.
    for cut in [1usize, 12, 64, 1000] {
        let mut short = good.clone();
        short.truncate(good.len() - cut);
        damaged.push((format!("payload missing its last {cut} bytes"), short));
    }
    // Bytes appended after a complete PNG: a header that is fine and a payload
    // that is not exactly what was written.
    let mut extended = good.clone();
    extended.extend_from_slice(b"trailing junk");
    damaged.push(("payload extended".into(), extended));
    // Header dims that disagree with the payload: the shape of a bug in whatever
    // wrote the entry, and the reason `normalized` is not optional here.
    let mut lying = good.clone();
    lying[8..12].copy_from_slice(&7u32.to_le_bytes());
    damaged.push(("header lies about its size".into(), lying));

    for (what, bytes) in damaged {
        f.put_entry_bytes(&p, &bytes);
        assert!(
            read_cached_thumbnail(&f.root(), &p, CELL).is_none(),
            "{what} must be a miss"
        );
    }

    // And the good entry still works, so the damage above was not simply deleting
    // the file and the assertions were not vacuous.
    f.put_entry_bytes(&p, &good);
    assert!(read_cached_thumbnail(&f.root(), &p, CELL).is_some());
}

#[test]
fn a_cache_that_cannot_be_written_still_returns_the_picture() {
    // The root is a *file*, so every create_dir_all under it fails. Nothing here
    // may panic, and a read must be a miss rather than an error.
    let f = Fixture::new();
    let p = f.jpeg("t.jpg", 600, 400);
    let root = f.dir.path().join("not-a-directory");
    std::fs::write(&root, b"a file, not a cache").unwrap();
    let d = load_thumbnail(&p, CELL).unwrap();
    write_cached_thumbnail(&root, &p, CELL, &d);
    assert!(read_cached_thumbnail(&root, &p, CELL).is_none());
    // A root that does not exist at all is the ordinary cold case.
    let fresh = f.dir.path().join("never-created");
    assert!(read_cached_thumbnail(&fresh, &p, CELL).is_none());
    write_cached_thumbnail(&fresh, &p, CELL, &d);
    assert!(read_cached_thumbnail(&fresh, &p, CELL).is_some());
}

#[test]
fn a_missing_source_file_is_a_miss() {
    let f = Fixture::new();
    let p = f.jpeg("t.jpg", 400, 300);
    let d = load_thumbnail(&p, CELL).unwrap();
    write_cached_thumbnail(&f.root(), &p, CELL, &d);
    std::fs::remove_file(&p).unwrap();
    assert!(
        read_cached_thumbnail(&f.root(), &p, CELL).is_none(),
        "a file that is gone must not be served from the cache"
    );
}

// ---------------------------------------------------------------------------
// The key
// ---------------------------------------------------------------------------

#[test]
fn the_entry_name_is_a_pure_function_of_the_path() {
    let root = Path::new("C:\\cache");
    let a = Path::new("C:\\photos\\one.jpg");
    let b = Path::new("C:\\photos\\two.jpg");
    assert_eq!(thumb_cache_entry(root, a), thumb_cache_entry(root, a));
    assert_ne!(thumb_cache_entry(root, a), thumb_cache_entry(root, b));

    // The name must be usable as a file name and the fanout must exist, because a
    // 30 000 file library in one directory is a directory big enough to make every
    // operation on it measurably slower.
    let entry = thumb_cache_entry(root, a);
    let fanout = entry.parent().unwrap().file_name().unwrap().to_str().unwrap();
    let name = entry.file_name().unwrap().to_str().unwrap();
    assert_eq!(fanout.len(), 2, "fanout should be two hex chars, got {fanout}");
    assert!(fanout.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(name.ends_with(".png"));
    assert_eq!(name.len(), 16 + 4, "16 hex chars plus .png, got {name}");
    assert!(name[..16].chars().all(|c| c.is_ascii_hexdigit()));
    // Nothing from the source path may leak in: a drive letter, a separator or a
    // non-ASCII byte would either be illegal or would make the name unbounded.
    assert!(!name.contains(':') && !name.contains('\\') && !name.contains('/'));
}

#[test]
fn paths_that_are_awkward_to_hash_still_work() {
    // Long names, non-ASCII, spaces, a `#` and a `~`: all legal on NTFS, and all
    // of them must produce one usable entry rather than a decode every time.
    // (`?` and `*` are left out on purpose — they are illegal in an NTFS file
    // name, so a test asserting they work would be asserting something untrue.)
    let f = Fixture::new();
    let names = [
        "a".repeat(200),
        "صورة كبيرة جدا مع اسم طويل جدا.jpg".to_string(),
        "with spaces and #hash and ~tilde.jpg".to_string(),
        "UPPERCASE.JPG".to_string(),
    ];
    for (i, name) in names.iter().enumerate() {
        let p = f.file(name, &jpeg(400 + i as u32 * 10, 300));
        let d = load_thumbnail(&p, CELL).unwrap();
        write_cached_thumbnail(&f.root(), &p, CELL, &d);
        let hit = read_cached_thumbnail(&f.root(), &p, CELL)
            .unwrap_or_else(|| panic!("{name} did not hit"));
        assert_eq!(hit.rgba, d.rgba, "{name}");
    }
}

// ---------------------------------------------------------------------------
// Staying inside the cap
// ---------------------------------------------------------------------------

#[test]
fn a_trim_removes_the_oldest_entries_and_stops_under_the_cap() {
    let f = Fixture::new();
    let root = f.root();
    // 20 entries, oldest first. Their sizes are whatever a real thumbnail encodes
    // to (measured at 13-80 KB) rather than a round number, and the assertion below
    // compares the reported total against the files actually left, so a made-up
    // size would make that comparison meaningless.
    let mut total = 0u64;
    let mut made: Vec<(PathBuf, u64)> = Vec::new();
    for i in 0..20u64 {
        let p = f.file(&format!("f{i:02}.jpg"), &jpeg(200, 200));
        let d = load_thumbnail(&p, CELL).unwrap();
        write_cached_thumbnail(&root, &p, CELL, &d);
        let entry = thumb_cache_entry(&root, &p);
        let len = std::fs::metadata(&entry).unwrap().len();
        total += len;
        // One second apart, oldest first, so the order is unambiguous.
        set_mtime(&entry, 1_700_000_000 + i);
        made.push((entry, len));
    }
    assert!(total > 0);
    let all = prune_thumb_cache(&root, u64::MAX);
    assert_eq!(all, total, "a cap that is never reached must delete nothing");

    // A cap whose target (4/5 of it) is comfortably under half of what is there.
    let left = prune_thumb_cache(&root, total / 4);
    assert!(left <= total / 4, "left {left} of {total}, cap was {}", total / 4);
    assert!(left < total, "a trim under its cap must delete something");
    let survivors: u64 = made.iter().filter(|(p, _)| p.exists()).map(|(_, l)| *l).sum();
    assert_eq!(
        survivors, left,
        "reported bytes must match the files actually left"
    );
    assert!(
        made[19].0.exists() && !made[0].0.exists(),
        "the newest entry must survive and the oldest must not"
    );
    // Every survivor is newer than every victim.
    let newest_victim = made
        .iter()
        .position(|(p, _)| !p.exists())
        .expect("some entries were removed");
    let oldest_survivor = made
        .iter()
        .rposition(|(p, _)| p.exists())
        .expect("some entries survived");
    assert!(
        oldest_survivor > newest_victim,
        "survivors up to {oldest_survivor} must all be newer than the last victim {newest_victim}"
    );
}

#[test]
fn a_trim_of_a_cache_that_does_not_exist_is_a_no_op() {
    let f = Fixture::new();
    assert_eq!(prune_thumb_cache(&f.dir.path().join("nope"), 1024), 0);
}

// ---------------------------------------------------------------------------
// The kill switch
// ---------------------------------------------------------------------------

#[test]
fn turning_the_cache_off_leaves_the_decode_exactly_as_it_was() {
    // Global state, so save and restore it: the rest of the suite runs in the same
    // process and must not inherit it. This is the same dance
    // `linear_resample_enabled` exists for.
    let was = image_viewer::thumbnail_cache_enabled();
    let f = Fixture::new();
    let p = f.jpeg("t.jpg", 1000, 800);
    set_thumbnail_cache(false);
    let a = load_thumbnail(&p, CELL).unwrap();
    let b = image_viewer::load_grid_thumbnail(&p, CELL).unwrap();
    set_thumbnail_cache(was);
    assert_eq!(a.rgba, b.rgba, "with the cache off, a cell is just a decode");
}

#[test]
fn a_config_written_before_the_cache_existed_still_loads_with_it_on() {
    // `thumbnail_cache` is the newest `Config` field, and the rule every new field
    // has to follow is `#[serde(default)]` — a config file written by an older build
    // has no such key, and if that stopped it loading, the upgrade would silently
    // reset every other setting the user had. Defaulting to *on* is the same choice
    // `renderer.rs` documents for `probed`.
    let old = r#"{
        "load_all_on_startup": true,
        "folder": "C:\\photos",
        "sort": "name",
        "linear_resample": true,
        "downscale_filter": "Hermite",
        "block_while_loading": false,
        "wheel_navigation": false,
        "loop_back": true,
        "last_by_folder": {}
    }"#;
    let c: image_viewer::Config = serde_json::from_str(old).expect("an old config must load");
    assert!(
        c.thumbnail_cache,
        "a config with no thumbnail_cache key must get the default, which is on"
    );
    // And a config that says `false` is honoured, so the setting round-trips.
    let off = r#"{"load_all_on_startup": true, "thumbnail_cache": false}"#;
    let c2: image_viewer::Config = serde_json::from_str(off).unwrap();
    assert!(!c2.thumbnail_cache, "an explicit false must not be overridden");
}
