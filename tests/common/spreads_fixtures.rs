//! Shared fixtures for the spread test targets.
//!
//! `spreads.rs` was split by subject (see `tests/source_size.rs`); this module holds
//! what more than one of those targets needs, so the test gallery, the source-text
//! helpers and the navigation fixtures exist once rather than in every file. Each
//! target compiles its own copy through `#[path = "common/spreads_fixtures.rs"]`,
//! the pattern `tests/common/` already uses.
#![allow(dead_code)]

use image_viewer::{
    Gallery, ImgKey, PageCtx, PagePos, Rect4, Shape, next_anchor, next_turn, prev_turn, turn_len,
};

use Shape::{Double, Single};

/// The panel every layout, zoom and pan test uses. Its seam is `x + width / 2` = 400.
pub const PANEL: Rect4 = [0.0, 0.0, 800.0, 600.0];

pub fn main_src() -> String {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut texts: Vec<String> = Vec::new();
    collect_src(&root, &mut texts);
    texts.sort();
    texts.join("\n")
}

fn collect_src(dir: &std::path::Path, texts: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_src(&path, texts);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            texts.push(std::fs::read_to_string(&path).expect("read"));
        }
    }
}

/// A gallery of documents, each a list of page shapes. One entry per file, so a
/// multi-page TIFF is a document with more than one page - which is what the
/// straddle tests need.
#[derive(Debug, Clone)]
pub struct TestGallery {
    pub docs: Vec<Vec<Shape>>,
    pub loop_back: bool,
}

impl TestGallery {
    /// One file whose pages have these shapes. Most tests are about the walk, and
    /// a file boundary is not one of the things they are testing.
    pub fn one_doc(shapes: Vec<Shape>) -> Self {
        Self::docs(vec![shapes])
    }

    /// One entry per file.
    pub fn docs(docs: Vec<Vec<Shape>>) -> Self {
        TestGallery { docs, loop_back: false }
    }

    /// How many pages the whole gallery has.
    pub fn total_pages(&self) -> usize {
        self.docs.iter().map(|d| d.len()).sum()
    }

    /// The shape of one page, or `None` for a position that does not exist.
    pub fn shape_at(&self, pos: PagePos) -> Option<Shape> {
        self.docs.get(pos.index)?.get(pos.page as usize).copied()
    }

    /// The `n`th page of the flat list, so a test can name a position without
    /// knowing which document it falls in. This is the same ordering the walk
    /// sees: every page of a document, then the next document's first page.
    pub fn page_at(&self, n: usize) -> Option<PagePos> {
        let mut seen = 0usize;
        for (index, doc) in self.docs.iter().enumerate() {
            if n < seen + doc.len() {
                return Some(PagePos { index, page: (n - seen) as u32 });
            }
            seen += doc.len();
        }
        None
    }

    /// The flat ordinals of a list of positions, dropping any that do not exist.
    /// Tests assert on ordinals rather than on positions because an ordinal is
    /// what a reader can count, and `PagePos` makes them spell out the file
    /// layout every time.
    pub fn flat_of(&self, positions: &[PagePos]) -> Vec<usize> {
        positions.iter().filter_map(|p| self.flat(*p)).collect()
    }

    /// The whole anchor list, by repeated `next_anchor`. Stops when the walk
    /// stops moving: with `loop_back` off it runs out of pages, and the visited
    /// check is what keeps a looping gallery from spinning here forever.
    ///
    /// The stop condition is "a position we have already seen". With looping on,
    /// `next_anchor` from the last anchor wraps to page 0, so a walk that only
    /// stopped on `None` would spin forever; and it cannot stop merely on
    /// returning to the start, because a one-page looping gallery returns to the
    /// start on the first step. A helper that assumed either would hang rather
    /// than fail, which is the worst way for a test to end.
    pub fn anchors(&self, lead_lone: u32) -> Vec<PagePos> {
        let Some(mut cur) = self.page_at(0) else {
            return Vec::new();
        };
        let mut out = vec![cur];
        while let Some(next) = next_anchor(cur, lead_lone, self) {
            if out.contains(&next) {
                break;
            }
            out.push(next);
            cur = next;
        }
        out
    }

    /// The turn anchored at `a`: one position, or the anchor and the page after
    /// it. There is deliberately no `Turn` type in the library for this to
    /// return - a turn is `[anchor] + maybe [anchor+1]`, which is already
    /// expressible, and a struct would only add a conversion.
    ///
    /// `step_no_wrap`, because `turn_len` looks for a partner the same way: a
    /// turn must not name a page twice, and across the wrap seam it would.
    pub fn turn_at(&self, a: PagePos, lead_lone: u32) -> Vec<PagePos> {
        let mut turn = vec![a];
        if turn_len(a, lead_lone, self) == 2
            && let Some(partner) = self.step_no_wrap(a, 1)
        {
            turn.push(partner);
        }
        turn
    }

    /// Every turn in the gallery, as flat ordinals, in reading order.
    pub fn turns(&self, lead_lone: u32) -> Vec<Vec<usize>> {
        self.anchors(lead_lone).iter().map(|&a| self.flat_of(&self.turn_at(a, lead_lone))).collect()
    }
}
impl PageCtx for TestGallery {
    /// Step the flat page list by `by` pages. `None` past either end with
    /// `loop_back` off, wrapped otherwise - the same contract `Gallery::next`
    /// and `Gallery::prev` honour, including `prev` from page 0 landing on the
    /// *last* page of the previous document rather than its first.
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
        let total = self.total_pages() as isize;
        if total == 0 {
            return None;
        }
        let target = self.flat(from)? as isize + by;
        let flat = if self.loop_back {
            target.rem_euclid(total) as usize
        } else if target < 0 || target >= total {
            return None;
        } else {
            target as usize
        };
        self.page_at(flat)
    }

    /// The same walk with the wrap taken out, so a gallery's last turn cannot
    /// straddle the seam however `loop_back` is set.
    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
        let total = self.total_pages() as isize;
        if total == 0 {
            return None;
        }
        let target = self.flat(from)? as isize + by;
        if target < 0 || target >= total {
            return None;
        }
        self.page_at(target as usize)
    }

    /// A real key per position, because [`turn_partner`] hands one back and the
    /// planner cannot be shown right without it. `path` names the document and
    /// `page` is the position's own page, which is what makes page 2
    /// distinguishable from page 0 of the same file.
    ///
    /// Nothing else in the pairing reads this: `shape_of`, `pairable` and `lone`
    /// go through `dims`, `override_shape` and `flat`, so giving it a real answer
    /// cannot perturb a test that never asks for a key. `None` for a position
    /// that does not exist, like every other `PageCtx` here.
    fn key(&self, pos: PagePos) -> Option<ImgKey> {
        self.shape_at(pos)?;
        Some(key_of(pos.index, pos.page))
    }

    /// The dimensions a detected shape implies: portrait for a single page, and
    /// a real spread's 1.414 for a double, so the tolerance check in `pairable`
    /// has two genuinely different ratios to disagree about.
    fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
        match self.shape_at(pos)? {
            Single => Some((1000, 1400)),
            Double => Some((2000, 1400)),
        }
    }

    fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
        None
    }

    /// The page's ordinal in the whole gallery. Everything here is known, so this
    /// is `Some` for every position that exists; `Gallery` returns `None` when a
    /// preceding file's page count has not been learned yet.
    fn flat(&self, pos: PagePos) -> Option<usize> {
        let doc = self.docs.get(pos.index)?;
        let page = pos.page as usize;
        if page >= doc.len() {
            return None;
        }
        Some(self.docs[..pos.index].iter().map(|d| d.len()).sum::<usize>() + page)
    }
}

/// A key the way `turn_partner` must build it: the document's path and the
/// position's **own page number**, which is the half `ImgKey::first` throws away.
pub fn key_of(index: usize, page: u32) -> ImgKey {
    ImgKey { path: std::path::PathBuf::from(format!("/vol/doc{index}.tiff")), page }
}

/// A gallery of `len` pages in one document, for the tests that only care about
/// length. `Single` throughout, so the only thing that decides the turns is the
/// leading-lone offset and the end of the gallery.
pub fn singles(len: usize) -> TestGallery {
    TestGallery::one_doc(vec![Single; len])
}

/// A file of `w` x `h` grey pixels, written as a PNG.
///
/// `image` is the crate the whole tree already decodes PNGs with, so the fixture
/// is built the same way a real file is rather than hand-assembled.
pub fn write_png(path: &std::path::Path, w: u32, h: u32) {
    let mut bytes = Vec::new();
    image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(w, h, image::Luma([128])))
        .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// The pages of `paths`, as a `Gallery` with `counts` pages each. The files on
/// disk are deliberately not real images: these tests are about the arithmetic,
/// and `set_page_counts` exists so a navigation test does not also depend on the
/// IFD walker being right.
pub fn gallery_of(paths: &[std::path::PathBuf], counts: &[u32]) -> Gallery {
    let mut gal = Gallery::from_file(paths[0].clone());
    gal.files = paths.to_vec();
    gal.set_page_counts(counts.iter().copied());
    gal.goto_index(0);
    gal
}

/// A real two-page scan's proportions, from the volume this was reported on: 1.41,
/// comfortably above [`image_viewer::SPREAD_ASPECT`] so the detector agrees it is a
/// spread. 2700x1920 rather than a round pair so the fit is not an exact division and
/// a rect cannot land on a nice number by accident.
pub const WIDE_SCAN: (f32, f32) = (2700.0, 1920.0);

/// A gallery of `counts.len()` files named in order, with those page counts and
/// nothing measured.
///
/// `gallery_of` does the three things needed and no more: install the list, set the
/// counts, put the cursor on the first page. Nothing here reads a file, so the
/// names need not exist - which is why a test about navigation cannot also fail
/// because a fixture was not written.
pub fn turns_gallery(counts: &[u32]) -> Gallery {
    let names: Vec<std::path::PathBuf> = (0..counts.len())
        .map(|i| std::path::PathBuf::from(format!("/vol/page{i:02}.jpg")))
        .collect();
    gallery_of(&names, counts)
}

/// Record that one page *displays* at a spread's proportions.
///
/// Through [`Gallery::note_shapes`] rather than by writing a file: that is the route
/// a real decode's dimensions take, and it is the only one that needs no bytes on
/// disk. It is fill-only, so it cannot overwrite a measurement - harmless here,
/// because these galleries have none.
///
/// 2000x1400 is 1.43, over [`SPREAD_ASPECT`](image_viewer::SPREAD_ASPECT) and a
/// long way from a real page's 0.71, so "wide" and "portrait" are not in the same
/// class by a hair.
pub fn mark_wide(gal: &mut Gallery, index: usize, page: u32) {
    gal.note_shapes(ImgKey { path: gal.files[index].clone(), page }, (2000, 1400));
}

/// One step forwards, as `MyApp::turn_forward` performs it.
///
/// Two lines and no rule: which anchor the next turn opens on is [`next_turn`], and
/// the move is `Gallery::goto_page`. The app's copies of both are in the binary, and
/// the mode gate that chooses between this and `Gallery::next` is the source scan at
/// the end of this section.
pub fn step_forward(gal: &mut Gallery, lead_lone: u32) -> bool {
    let Some(to) = next_turn(gal.page_pos(), lead_lone, &*gal) else {
        return false;
    };
    gal.goto_page(to);
    true
}

/// One step backwards: [`prev_turn`] rather than [`prev_anchor`], for the same
/// reason `step_forward` uses [`next_anchor`] rather than `Gallery::next` - and
/// with a difference that matters, because `prev_anchor` stops at the first turn
/// whatever `loop_back` says while a backward *step* on a looping gallery wraps.
pub fn step_backward(gal: &mut Gallery, lead_lone: u32) -> bool {
    let Some(to) = prev_turn(gal.page_pos(), lead_lone, &*gal) else {
        return false;
    };
    gal.goto_page(to);
    true
}

/// Every position a turn-step lands on is a page that exists, in both directions.
///
/// The premise for the two claims made about this walk elsewhere: that a turn-step
/// cannot put the cursor on a page its file does not have, and that
/// [`Gallery::goto_page`] does not have to defend against one - it refuses a file
/// index past the end but trusts the page, so the walk's correctness *is* the page's
/// safety.
///
/// `tests/multiframe.rs` asserts the same property of `next`/`prev`; this is the
/// turn walk's version of it, over galleries that actually hold documents.
///
/// `forward` picks the direction, and the walk ends when a step returns `false` or
/// when a position repeats: with `loop_back` on a full round trip returns to the
/// anchor it started from, which is the end of the walk and not a failure. A walk
/// that stopped only on `false` would spin forever on a looping gallery, and a test
/// that hangs is worse than one that fails.
pub fn assert_turn_step_landings_exist(gal: &mut Gallery, lead_lone: u32, forward: bool) {
    for from in walk_start_positions(gal) {
        let mut gal = gal.clone();
        gal.goto_page(from);
        let mut seen = vec![gal.page_pos()];
        while if forward {
            step_forward(&mut gal, lead_lone)
        } else {
            step_backward(&mut gal, lead_lone)
        } {
            let at = gal.page_pos();
            if seen.contains(&at) {
                break;
            }
            assert!(
                at.index < gal.len(),
                "a step landed on file {} of a gallery of {} (from {from:?})",
                at.index,
                gal.len()
            );
            assert!(
                at.page < gal.pages_in_current(),
                "a step landed on page {} of a {}-page file: {at:?} (from {from:?})",
                at.page,
                gal.pages_in_current()
            );
            seen.push(at);
        }
    }
}

/// The positions a walk is started from: the first page, and **one second half from
/// the middle**.
///
/// The second half is the whole point, and it is here rather than in each test because
/// a cursor off an anchor is what three ways into the app produce - a grid pick leaves
/// `page` alone, a start-up or resume lands on page 0, and a `Del` keeps the freed
/// index. Paging forward to reach one would stop at the first anchor, so it is named as
/// a position instead: the third page of a gallery of at least three pages is a second
/// half whenever `lead_lone` is 0 (turns `{0,1} {2,3}...`) and the second page is one
/// whenever `lead_lone` is 1 (`{0} {1,2}...`), so both are covered by the two starts
/// below across the leads this section walks.
pub fn walk_start_positions(gal: &Gallery) -> Vec<PagePos> {
    let mut out = vec![PagePos { index: 0, page: 0 }];
    for n in [1usize, 2] {
        let mut seen = 0usize;
        for (index, _) in gal.files.iter().enumerate() {
            let pages = gal.pages_in(index);
            if n < seen + pages as usize {
                out.push(PagePos { index, page: (n - seen) as u32 });
                break;
            }
            seen += pages as usize;
        }
    }
    out
}

pub fn flat_position(gal: &Gallery, n: usize) -> PagePos {
    let mut seen = 0usize;
    for (index, _) in gal.files.iter().enumerate() {
        let pages = gal.pages_in(index) as usize;
        if n < seen + pages {
            return PagePos { index, page: (n - seen) as u32 };
        }
        seen += pages;
    }
    panic!("page {n} does not exist in a gallery of {seen}");
}

/// Every anchor of `gal`'s tiling at `lead_lone`, by repeated `next_anchor`.
///
/// Stops when the walk stops moving. With `loop_back` off that is `None` at the end
/// of the gallery; the visited check as well, because a tiling that wrapped would
/// otherwise run until `PREV_SCAN_LIMIT` and the test would hang rather than fail.
pub fn anchors_of(gal: &Gallery, lead_lone: u32) -> Vec<PagePos> {
    let mut out = vec![PagePos { index: 0, page: 0 }];
    let mut cur = out[0];
    while let Some(next) = next_anchor(cur, lead_lone, gal) {
        if out.contains(&next) {
            break;
        }
        out.push(next);
        cur = next;
    }
    out
}

/// The body of `fn <sig>` in a source file, found by **brace matching**.
///
/// Extracted so that a second scan of the same function is one assertion rather than
/// a second copy of this, which is the whole reason
/// [`invalidate_path_drops_the_cached_dimensions`] is not duplicated below.
///
/// **By brace matching, not by "up to the next `fn`".** The first version split
/// on `"\n    fn "`, which is wrong in the direction that matters: `split` always
/// yields at least one item, so when the delimiter did not match Ã¢â‚¬â€ `invalidate_path`
/// last in the impl, or re-indented Ã¢â‚¬â€ the "body" became the whole rest of the
/// file, the later call stood in for this one, and the scan passed
/// **vacuously**. It could not fail; that is the opposite of what a guard is for.
///
/// The depth starts at **0**, not 1, and that is load-bearing rather than
/// incidental. It is 0 because the scan starts *before* the opening brace Ã¢â‚¬â€ the
/// signature's `&mut self, p: &Path, ctx: &Context` has none Ã¢â‚¬â€ so the first `{`
/// is the function's and the matching `}` is the one that ends it. Starting at 1
/// (the shape `keybindings.rs::dispatch_region` uses, correctly, because it
/// begins *after* `if !typing {` and so has already consumed a brace) misses
/// that closing brace and runs on until the file's own braces happen to balance,
/// 60 KB later. **That was the second version of that test, and it passed with the
/// call deleted.** Verified by mutation, not reasoned about.
pub fn fn_body<'a>(src: &'a str, sig: &str) -> &'a str {
    let start = src.find(sig).unwrap_or_else(|| panic!("{sig} is gone; update this test to match"))
        + sig.len();
    let mut depth = 0i32;
    let mut end = None;
    for (i, b) in src.as_bytes()[start..].iter().enumerate() {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(start + i);
                    break;
                }
            }
            _ => {}
        }
    }
    let end = end.expect(
        "could not find the end of the function: its braces do not \
                         balance, or the signature changed shape. Update this test.",
    );
    &src[start..end]
}
