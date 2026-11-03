//! Shell thumbnail cache: the cells Explorer writes, and the rules our grid
//! follows when reading them.
//!
//! Shell thumbnails live in `%LOCALAPPDATA%` outside this suite's temp dirs, so
//! the extraction-reading half is exercised by writing entries the way Explorer
//! would and reading them back through `shell_thumbnail`. The miss/give-up
//! policy (`ShellAsk`) is pure and tested without touching the shell at all.
#![cfg(windows)]

#[path = "common/exif_fixtures.rs"]
mod exif_fixtures;

use exif_fixtures::jpeg;
use image_viewer::{
    SHELL_MISS_GIVEUP, ShellAsk, bgra_to_rgba, load_thumbnail, shell_api_path,
    shell_cell_is_usable, shell_thumbnail, shell_thumbnail_allowing_extraction,
};
use std::path::{Path, PathBuf};

/// Grid cell size, matching `THUMB_DIM` in main.rs.
const CELL: u32 = 256;

/// A source file plus the directory holding it.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Fixture { dir: tempfile::tempdir().unwrap() }
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
}

/// Mean absolute difference per colour channel, at 64x64 so two decodes of
/// different sizes can be compared at all.
fn mean_abs_diff(a: &image_viewer::ImageData, b: &image_viewer::ImageData) -> f64 {
    let at = |d: &image_viewer::ImageData| {
        let img = image::RgbaImage::from_raw(d.width, d.height, d.rgba.clone()).unwrap();
        image::imageops::resize(&img, 64, 64, image::imageops::FilterType::Triangle)
    };
    let (a, b) = (at(a), at(b));
    let mut sum = 0f64;
    for (pa, pb) in a.pixels().zip(b.pixels()) {
        for c in 0..3 {
            sum += (pa.0[c] as f64 - pb.0[c] as f64).abs();
        }
    }
    sum / (64.0 * 64.0 * 3.0)
}

/// `fn name` body from a source file, brace-matched.
fn fn_body(src: &str, name: &str) -> String {
    let start = src
        .find(&format!("fn {name}("))
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

#[test]
fn a_shell_dib_becomes_rgba() {
    // One opaque pixel and one semi-transparent: the channel swap and the
    // alpha rule are both visible at once.
    assert_eq!(
        bgra_to_rgba(&[10, 20, 30, 255, 40, 50, 60, 128]),
        [30, 20, 10, 255, 60, 50, 40, 128]
    );
}

#[test]
fn an_all_zero_alpha_is_opaque_not_invisible() {
    // The case that matters: the shell often draws into a 32-bit bitmap whose
    // fourth byte is left at zero, and honouring it paints the cell blank -
    // which reads as a decode that failed, not as this.
    assert_eq!(bgra_to_rgba(&[1, 2, 3, 0, 4, 5, 6, 0]), [3, 2, 1, 255, 6, 5, 4, 255]);
}

#[test]
fn the_verbatim_prefix_is_dropped_for_the_shell() {
    // The bug that made every file a miss. `canonicalize` hands back
    // `\\?\C:\...` and the shell answers `E_INVALIDARG`.
    assert_eq!(
        shell_api_path(Path::new(r"\\?\C:\pics\a.jpg")).unwrap(),
        Path::new(r"C:\pics\a.jpg")
    );
    assert_eq!(
        shell_api_path(Path::new(r"\\?\UNC\srv\share\a b\c.png")).unwrap(),
        Path::new(r"\\srv\share\a b\c.png")
    );
    // The forms the shell already wants come back untouched.
    for plain in [r"C:\Windows\notepad.exe", r"\\srv\share\a.png"] {
        assert_eq!(shell_api_path(Path::new(plain)).unwrap(), Path::new(plain));
    }
    // A relative path is made absolute - the other half of what the shell
    // needs - and does not pick the prefix up on the way.
    let rel = shell_api_path(Path::new("relative/pic.png")).unwrap();
    assert!(rel.is_absolute(), "{rel:?}");
    assert!(!rel.to_string_lossy().starts_with(r"\\?\"), "{rel:?}");
}

#[test]
fn a_shell_cell_too_small_to_be_a_cell_is_refused() {
    // The sizes Explorer's cache actually holds, against a 256 cell. This is
    // the measured behaviour, not a guess: the shell returns what its cache
    // holds, so a 32px entry comes back 32x24 for a 256px ask.
    for smear in [(32, 24), (48, 36), (96, 72)] {
        assert!(!shell_cell_is_usable(smear, CELL), "{smear:?} is a smear at {CELL}");
    }
    for fine in [(128, 96), (256, 192), (1024, 768)] {
        assert!(shell_cell_is_usable(fine, CELL), "{fine:?} is usable at {CELL}");
    }
    // It scales with the target rather than being one magic number: half is
    // the line, and exactly half is on the usable side of it.
    assert!(!shell_cell_is_usable((32, 24), 96));
    assert!(shell_cell_is_usable((48, 36), 96), "exactly half is usable");
    assert!(!shell_cell_is_usable((47, 35), 96), "just under half is not");
}

#[test]
fn the_ask_gives_up_on_a_folder_that_never_answers() {
    let mut ask = ShellAsk::new();
    let a = Path::new(r"C:\pics\a.jpg");
    assert!(ask.wants(a), "nothing asked yet");
    for i in 1..SHELL_MISS_GIVEUP {
        ask.record(a, false);
        assert!(ask.wants(a), "still worth asking after {i} misses");
    }
    ask.record(a, false);
    assert!(!ask.wants(a), "{SHELL_MISS_GIVEUP} misses must stop the asking");

    // The record is per *folder*, which is the granularity Explorer's cache
    // works at: one folder's misses may not cost another folder its chance.
    assert!(ask.wants(Path::new(r"C:\other\b.jpg")));
    assert!(ask.wants(Path::new(r"C:\pics\album\c.jpg")));
    // ...and every file in the folder shares it, or the tax is one ask per
    // cell instead of one per four.
    assert!(!ask.wants(Path::new(r"C:\pics\z.jpg")));
}

#[test]
fn a_folder_that_answers_is_asked_again() {
    let mut ask = ShellAsk::new();
    let a = Path::new(r"C:\pics\a.jpg");
    for _ in 0..SHELL_MISS_GIVEUP {
        ask.record(a, false);
    }
    assert!(!ask.wants(a));
    // One hit says this folder *is* thumbnailed: a folder whose first files
    // are not in the cache must not be abandoned for the rest of the grid.
    ask.record(a, true);
    assert!(ask.wants(a));
}

#[test]
fn the_folder_record_is_bounded() {
    let mut ask = ShellAsk::new();
    let folder = |i: usize| format!(r"C:\f{i}\a.jpg");
    for i in 0..ShellAsk::MAX_FOLDERS {
        for _ in 0..SHELL_MISS_GIVEUP {
            ask.record(Path::new(&folder(i)), false);
        }
    }
    assert!(!ask.wants(Path::new(&folder(0))));
    // One more folder evicts the oldest record instead of growing the map.
    // The cost of being wrong is one more ask on a folder that will miss
    // anyway, which is why a coarse bound is enough.
    for _ in 0..SHELL_MISS_GIVEUP {
        ask.record(Path::new(&folder(ShellAsk::MAX_FOLDERS)), false);
    }
    assert!(ask.wants(Path::new(&folder(0))), "the oldest record is dropped");
    assert!(!ask.wants(Path::new(&folder(ShellAsk::MAX_FOLDERS))));
}

#[test]
fn a_cache_only_miss_stays_a_miss() {
    let f = Fixture::new();
    let p = f.jpeg("fresh.jpg", 900, 600);
    // Three asks, and none of them may leave an entry behind. This is the
    // property the whole feature rests on: the call in front of the decode
    // may never *cause* an extraction.
    for round in 0..3 {
        assert!(shell_thumbnail(&p, CELL).is_none(), "round {round}: a miss must stay a miss");
    }
    // And the entry appears only when something extracts, which is what makes
    // the three misses above evidence about the flag rather than about the
    // file being unthumbnailable.
    assert!(shell_thumbnail_allowing_extraction(&p, CELL).is_some());
    assert!(shell_thumbnail(&p, CELL).is_some(), "the entry just made must hit");
}

#[test]
fn a_thumbnailed_file_comes_back_as_the_same_picture() {
    let f = Fixture::new();
    let p = f.jpeg("photo.jpg", 1200, 800);
    let ours = load_thumbnail(&p, CELL).expect("our own decode");

    // Make the entry the way Explorer would. The app never does this - see
    // the module comment - so this is the only way to have something to read.
    let extracted =
        shell_thumbnail_allowing_extraction(&p, CELL).expect("the shell can thumbnail a JPEG");
    let cell = shell_thumbnail(&p, CELL).expect("the entry just made must hit");
    let cell_w = cell.width;
    let cell_h = cell.height;
    assert_eq!(
        (cell_w, cell_h),
        (extracted.width, extracted.height),
        "a hit is the entry, not a rescale of it"
    );
    assert!(cell_w.max(cell_h) >= CELL / 2, "{}x{}", cell_w, cell_h);

    // The picture, with a scale: a *different* file of a different shape
    // scores far worse, so a small number here means something. A folder icon
    // or a blank cell would land in the wrong-file range.
    let same = mean_abs_diff(&cell, &ours);
    let other = f.jpeg("other.jpg", 900, 700);
    let different = mean_abs_diff(&cell, &load_thumbnail(&other, CELL).unwrap());
    eprintln!("shell cell vs our decode: {same:.2} codes (a different file: {different:.2})");
    assert!(same < 24.0, "the shell's picture is {same:.2} codes from ours");
    assert!(
        different > same * 3.0,
        "a different file scored {different:.2} against {same:.2}, so the comparison has no scale"
    );
}

#[test]
fn a_cached_entry_too_small_is_refused_rather_than_painted() {
    let f = Fixture::new();
    let p = f.jpeg("small-entry.jpg", 1200, 800);
    // Fill the cache the way a Small-icons folder would have.
    let tiny = shell_thumbnail_allowing_extraction(&p, 32).expect("a 32px extraction");
    assert!(tiny.width.max(tiny.height) <= 32, "{tiny:?}");
    assert!(
        shell_thumbnail(&p, CELL).is_none(),
        "32x24 must not be painted into a {CELL} cell; it is a smear, and the decode is better"
    );
}

#[test]
fn a_missing_file_or_a_target_that_is_not_a_cell_is_a_clean_none() {
    let f = Fixture::new();
    assert!(shell_thumbnail(&f.dir.path().join("never-existed.jpg"), CELL).is_none());

    let p = f.jpeg("gate.jpg", 900, 600);
    // A cache entry has to exist first, or "None" would prove nothing: the
    // shell answers `None` for a target it has nothing for anyway. This is the
    // *measured* behaviour on the other side of the gate - the shell returns
    // what its cache holds, whatever size was asked for - so without the gate
    // a 256px picture would be handed to the main view as a full-screen image.
    assert!(shell_thumbnail_allowing_extraction(&p, CELL).is_some());
    assert!(shell_thumbnail(&p, 4096).is_none(), "the main view must not get a cell");
    assert!(shell_thumbnail(&p, 0).is_none());

    // Not an image at all: the shell has no picture to give and this must be
    // a `None` on a worker, never a panic or an icon.
    let junk = f.file("junk.jpg", &[0xFF, 0xD8, 0x00, 0x11, 0x22, 0x33]);
    assert!(shell_thumbnail(&junk, CELL).is_none());
    assert!(shell_thumbnail_allowing_extraction(&junk, CELL).is_none());
}

#[test]
fn a_grid_cell_asks_our_cache_then_the_shell_then_decodes() {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/cache/shell_thumb.rs"),
    )
    .unwrap();
    let body = fn_body(&src, "load_grid_thumbnail");

    let cache = body.find("read_cached_thumbnail").expect("our cache is asked");
    // The *last* ask is the one inside the cache-miss arm; the first is the
    // cache-off arm, which is reached only when our cache is switched off.
    let shell = body.rfind("shell_cell").expect("the shell is asked");
    let decode = body.find("let data = load_thumbnail(path, target)?").expect("the decode");
    let write = body.find("write_cached_thumbnail").expect("the write");
    assert!(cache < shell, "our own cache is asked first");
    assert!(shell < decode, "the shell is asked before the decode");
    assert_eq!(
        body.matches("shell_cell(").count(),
        2,
        "both arms ask the shell: the cache-off arm and the cache-miss arm"
    );

    // Every shell arm hands its cell back rather than caching it...
    for (i, _) in body.match_indices("shell_cell(") {
        let arm = &body[i..(i + 120).min(body.len())];
        assert!(arm.contains("Ok(data)"), "the shell's cell is what the cell becomes: {arm:?}");
        assert!(
            !arm.contains("write_cached_thumbnail"),
            "a shell cell is never written into our own cache: {arm:?}"
        );
    }
    // ...and exactly one write exists, after the decode, with none before it,
    // so there is no path from the shell's cell to the cache.
    assert_eq!(body.matches("write_cached_thumbnail").count(), 1);
    assert!(decode < write, "the only write follows the only decode");
    assert!(
        !body[..decode].contains("write_cached_thumbnail"),
        "nothing may be cached before the decode it claims to be"
    );

    // The counters are what let a probe tell a shell-served grid from a
    // decoded one; both the hit and the miss are counted, and in one place.
    let cell = fn_body(&src, "shell_cell");
    assert!(cell.contains("C_SHELL_THUMB"), "a shell cell is counted");
    assert!(cell.contains("C_SHELL_MISS"), "and so is the miss that paid for it");
}
