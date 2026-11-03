//! Reading a page's dimensions, and the cache that remembers them.
//!
//! Split out of `tests/spreads.rs`; shared fixtures are in
//! `tests/common/spreads_fixtures.rs`.
//!
//! The last three tests are source scans of `src/`, for the reason `AGENTS.md`
//! gives: the wiring they protect (`invalidate_path`, the `Msg::Preview` arm) is in
//! the binary, which no integration test can reach.

use image_viewer::{
    exif_orientation, next_anchor, page_dims, pairable, shape_of, turn_len, Gallery, ImgKey,
    PageCtx, PagePos, Shape, PROBE_PREFIX_BYTES,
};

use Shape::{Double, Single};

#[path = "common/spreads_fixtures.rs"]
mod spreads_fixtures;

use spreads_fixtures::*;

/// A JPEG of `w` x `h` pixels carrying a real APP1/Exif block whose IFD0 has the
/// single Orientation entry `tag`.
///
/// Hand-built for the same reason as everywhere else in this tree
/// (`tests/rotate_save.rs`, `tests/unit.rs`): `image`'s encoder writes no EXIF, so
/// there is nothing to configure and the only way to get a tagged JPEG is to
/// splice the segment in. Big-endian so the bytes are readable.
fn write_jpeg_with_orientation(path: &std::path::Path, w: u32, h: u32, tag: u16) {
    let mut jpeg = Vec::new();
    image::DynamicImage::ImageLuma8(image::GrayImage::from_pixel(w, h, image::Luma([128])))
        .write_to(&mut std::io::Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
        .unwrap();

    // APP1 payload: "Exif\0\0", then a TIFF header, then IFD0 with one SHORT.
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\x00\x2a"); // big-endian, version 42
    tiff.extend_from_slice(&8u32.to_be_bytes()); // IFD0 at offset 8
    tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
    tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
    tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
    tiff.extend_from_slice(&1u32.to_be_bytes()); // one value
                                                 // A SHORT's value lives in the first 2 bytes of its 4-byte field.
    tiff.extend_from_slice(&tag.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes()); // no IFD1

    let mut app1 = Vec::new();
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend_from_slice(&tiff);
    let mut seg = Vec::new();
    seg.extend_from_slice(&[0xFF, 0xE1]);
    seg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    seg.extend_from_slice(&app1);

    // Straight after SOI, where a camera puts it.
    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[0..2]);
    out.extend_from_slice(&seg);
    out.extend_from_slice(&jpeg[2..]);
    std::fs::write(path, out).unwrap();
}

/// `page_dims` reads a header, not pixels.
///
/// The control matters as much as the assertion: the file is **provably
/// undecodable** (its compressed image data is scrambled) and `image::open`
/// refuses it, so an answer here cannot have come from decoding it. `image`'s
/// `into_dimensions` stops at the header, which is the whole point - the pairing
/// query has to be cheap enough to run on arrival rather than on arrival *plus*
/// a decode.
///
/// An aspect of the header, so the value is not a shape the reader could get by
/// luck: 800x1200 is portrait and 0.667, well under [`SPREAD_ASPECT`], so it
/// would also be judged `Single` - which is what makes the *rotation* test's
/// failure mode below legible rather than a coincidence.
///
/// **The fixture is shorter than the 64 KB prefix, so this also pins the prefix
/// read's tolerance of a short file** - and the test checks that premise rather
/// than asserting it in a comment. `read_exact` into the preallocated buffer
/// would fail here; `read_prefix` truncates to what actually arrived. A uniform
/// 800x1200 grey PNG compresses to about 8 KB, which is not an accident of the
/// fixture: it is what a *small* image is, and a small image is the common case.
#[test]
fn page_dims_reads_stored_dimensions_without_decoding() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 800, 1200);

    // The premise of the short-file half of this test, checked rather than
    // asserted in the doc comment.
    let len = std::fs::metadata(&p).unwrap().len() as usize;
    assert!(
        len < PROBE_PREFIX_BYTES,
        "this fixture must be shorter than the prefix ({len} bytes), or the \
         short-file tolerance is not being exercised"
    );
    assert_eq!(page_dims(&p, 0), Some((800, 1200)));

    // Now break the pixels. IHDR is at a fixed offset and stays intact; the
    // entropy-coded data after it does not.
    let mut bytes = std::fs::read(&p).unwrap();
    let idat = bytes.windows(4).position(|w| w == b"IDAT").expect("the fixture has no IDAT chunk");
    for b in bytes[idat + 4..].iter_mut() {
        *b ^= 0xA5;
    }
    std::fs::write(&p, &bytes).unwrap();

    assert!(image::open(&p).is_err(), "the control: this file must not decode");
    assert_eq!(
        page_dims(&p, 0),
        Some((800, 1200)),
        "the header is still readable from a file whose pixels are nonsense"
    );
}

/// Unreadable is `None`, not a guess. A shape judged from dimensions nobody could
/// read is a shape nobody can correct, and the pairing is built to survive
/// `dims` being `None` (see `an_unknown_page_pairs_and_is_a_turn_of_two_until_it_is_measured`),
/// so there is nothing to gain by inventing a pair.
#[test]
fn page_dims_returns_none_for_a_file_that_is_not_an_image() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("notes.txt");
    std::fs::write(&p, b"this folder is not an image folder, it is a folder").unwrap();
    assert_eq!(page_dims(&p, 0), None);

    // And a file that is not there at all is the same answer, not a panic: this
    // is called on arrival, and a file can be deleted between being listed and
    // being opened.
    assert_eq!(page_dims(&dir.path().join("gone.png"), 0), None);

    // And so is a file too short to hold any header, which is the short-file case
    // the prefix read has to tolerate rather than treat as an error.
    let stub = dir.path().join("stub.png");
    std::fs::write(&stub, b"\x89PNG").unwrap();
    assert_eq!(page_dims(&stub, 0), None);
}

/// A `PageCtx` that reports one fixed pair for every position.
///
/// Exists so a test can ask *what the detector would say about a different set of
/// dimensions* - which is how the orientation test below states its own bug as an
/// executable assertion rather than a comment.
struct FixedDims((u32, u32));

impl PageCtx for FixedDims {
    fn step(&self, _from: PagePos, _by: isize) -> Option<PagePos> {
        None
    }
    fn step_no_wrap(&self, _from: PagePos, _by: isize) -> Option<PagePos> {
        None
    }
    fn key(&self, _pos: PagePos) -> Option<ImgKey> {
        None
    }
    fn dims(&self, _pos: PagePos) -> Option<(u32, u32)> {
        Some(self.0)
    }
    fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
        None
    }
    fn flat(&self, pos: PagePos) -> Option<usize> {
        Some(pos.index)
    }
}

/// Review Focus 1, and the highest-consequence line in the whole plan: a
/// phone-scanned page is stored **landscape** and tagged to display **portrait**.
///
/// Stored 1600x900 with orientation 6, the stored pair's ratio is 1.78 - above
/// [`SPREAD_ASPECT`](image_viewer::SPREAD_ASPECT) - so judging the stored size
/// calls *every* page in the volume a spread. Every page then takes a whole turn
/// to itself, nothing ever pairs, and nothing on screen says why. It is silent,
/// and it lands on the largest category of real manga library there is.
///
/// So `page_dims` deliberately reports the **stored** size and `learn_shapes`
/// applies the tag, and this test pins both halves plus the converse: feeding the
/// stored pair to the detector is the bug, asserted directly rather than
/// described.
#[test]
fn a_rotated_phone_scan_is_judged_as_it_displays() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("scan.jpg");
    write_jpeg_with_orientation(&p, 1600, 900, 6);
    assert_eq!(exif_orientation(&p), 6, "the fixture must really carry the tag");

    assert_eq!(
        page_dims(&p, 0),
        Some((1600, 900)),
        "`page_dims` reports the *stored* size; applying the tag is a separate step"
    );

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let page = gal.page_pos();
    // Behaviour first, mechanism second: what a dropped orientation tag actually
    // breaks is the *verdict*, and the dimension is only how it gets there.
    assert_eq!(shape_of(page, &gal), Single, "a portrait page is not a spread");
    assert_eq!(
        PageCtx::dims(&gal, page),
        Some((900, 1600)),
        "the cache holds the size the page *displays* at"
    );

    // The converse, and the reason the assertion above is worth anything: the
    // stored pair is exactly what a dropped orientation tag feeds the detector,
    // and it says the opposite.
    assert_eq!(
        shape_of(page, &FixedDims((1600, 900))),
        Double,
        "the stored size is the bug this cache exists to prevent"
    );

    // The whole tag table, so the swap is pinned as a property of the tags rather
    // than of the one example. 5-8 rotate by a quarter turn and swap; 2-4 do not,
    // and a 180 rotation keeps the aspect ratio, so a landscape stored page is
    // still a spread under tag 3.
    for tag in 1u16..=8 {
        let p = dir.path().join(format!("tag{tag}.jpg"));
        write_jpeg_with_orientation(&p, 1600, 900, tag);
        let mut gal = Gallery::from_file(p.clone());
        gal.learn_shapes(PagePos { index: 0, page: 0 });
        let pos = gal.page_pos();
        let swaps = (5..=8).contains(&tag);
        assert_eq!(
            PageCtx::dims(&gal, pos),
            Some(if swaps { (900, 1600) } else { (1600, 900) }),
            "tag {tag}"
        );
        assert_eq!(shape_of(pos, &gal), if swaps { Single } else { Double }, "tag {tag}");
    }
}

/// A multi-page TIFF whose pages are **different sizes**, which is legal and which
/// `tiff_page_dim` exists to see. The pages differ in aspect on purpose, so a
/// walker that answered every page with the first page's size would report a shape
/// the pairing then trusts for the session.
///
/// `write_image` is the whole page in one call and it *chains* into the IFD
/// sequence, so calling it in a loop is what produces a multi-page file.
/// `new_directory()` per page would not: an unlinked IFD is invisible to
/// `more_images()`, so the file comes out single-page however many times the loop
/// runs (`tests/multiframe.rs` records that the hard way).
#[test]
fn a_later_page_of_a_document_is_measured_as_its_own_page() {
    use tiff::encoder::{colortype, TiffEncoder};
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("doc.tiff");

    let mut bytes = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut bytes)).unwrap();
        for &(w, h) in &[(900u32, 1200u32), (1600, 900)] {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [9u8, 9, 9, 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px).expect("one chained page");
        }
    }
    std::fs::write(&p, &bytes).unwrap();

    assert_eq!(page_dims(&p, 0), Some((900, 1200)), "page 0 is portrait");
    assert_eq!(
        page_dims(&p, 1),
        Some((1600, 900)),
        "page 1 is walked to its own directory, not page 0's"
    );
    // A page that does not exist is `None` rather than the last one that does: the
    // walk is bounded by the chain, so this also says the walk terminates.
    assert_eq!(page_dims(&p, 2), None);
    assert_eq!(page_dims(&p, 99), None);

    // The cache is keyed per *page*, so page 1 keeps its own dimensions rather than
    // page 0's. This is the consequence RULING 11 exists for, and
    // `a_page_of_a_document_is_judged_by_its_own_dimensions` is the test that
    // fails if the keying ever reverts; the assertion here is the mechanism behind
    // it, one level down.
    let mut gal = Gallery::from_file(p.clone());
    gal.learn_pages(&p);
    assert_eq!(gal.pages_in(0), 2, "the fixture is really two pages");
    // **The second call is redundant since RULING 13** Ã¢â‚¬â€ learning page 0 of a
    // two-page document already learns page 1 Ã¢â‚¬â€ and it is kept anyway, as a cache
    // hit, so this test's subject stays unambiguously "the IFD walk reaches page 1"
    // rather than "the next-page learn filled it".
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    gal.learn_shapes(PagePos { index: 0, page: 1 });
    assert_eq!(PageCtx::dims(&gal, PagePos { index: 0, page: 0 }), Some((900, 1200)));
    assert_eq!(
        PageCtx::dims(&gal, PagePos { index: 0, page: 1 }),
        Some((1600, 900)),
        "page 1 keeps its own entry rather than page 0's"
    );

    // **And `forget_shapes` drops *every* page of the file, not just page 0.** The
    // entries are keyed by `ImgKey`, so a path-keyed `remove` would leave page 1's
    // old shape behind and only page 0 would ever be re-measured Ã¢â‚¬â€ the per-page
    // failure that a per-file `remove` cannot even express. `known_shapes` is the
    // only place the difference shows: a single-page query answers identically
    // either way.
    assert_eq!(gal.known_shapes(), 2, "both pages are remembered");
    gal.forget_shapes(&p);
    assert_eq!(gal.known_shapes(), 0, "not one page of the old document survives");
    assert_eq!(PageCtx::dims(&gal, PagePos { index: 0, page: 0 }), None);
    assert_eq!(PageCtx::dims(&gal, PagePos { index: 0, page: 1 }), None);
}

/// RULING 11: **the pages of one document are judged separately**, because a
/// document can hold a landscape double-page spread among portrait singles.
///
/// This is the shape the feature exists for Ã¢â‚¬â€ a manga volume scanned as one
/// multi-page TIFF, with page 3 a two-page spread Ã¢â‚¬â€ and it is exactly what a
/// path-keyed cache cannot express: one entry for the file would judge page 3 by
/// page 0, call it a portrait page, and pair it with page 4. Every spread after the
/// fold-out would then be wrong, and nothing on screen would say why.
///
/// Reverting `shapes` to a path key makes this fail and nothing else, which is why
/// it is a separate test from the `page_dims` walk above: that one pins the reader,
/// this one pins what the cache does with the answer.
///
/// Both pages are measured deliberately. Learning only page 0 would leave page 3
/// `None`, and `shape_of` reads `None` as `Single` Ã¢â‚¬â€ the *same wrong verdict* by a
/// different route, so a test that asserted only "page 3 is not Double" would pass
/// against the more likely half-regression as well.
#[test]
fn a_page_of_a_document_is_judged_by_its_own_dimensions() {
    use tiff::encoder::{colortype, TiffEncoder};
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("volume.tiff");

    // Pages 0-3: portrait singles. Page 3: a landscape two-page spread.
    let mut bytes = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut bytes)).unwrap();
        for &(w, h) in &[(900u32, 1200u32), (900, 1200), (900, 1200), (1600, 900)] {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [9u8, 9, 9, 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px).expect("one chained page");
        }
    }
    std::fs::write(&p, &bytes).unwrap();

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_pages(&p);
    assert_eq!(gal.pages_in(0), 4, "the fixture is really four pages");

    // Arrive at each page in turn, the way `goto` does. Since RULING 13 the first call
    // already learns pages 0 and 1, so the loop is mostly cache hits Ã¢â‚¬â€ kept because
    // this test's subject is "every page is judged on its own", and that should not
    // depend on how many pages the next-page learn happens to cover.
    for page in 0..4u32 {
        gal.learn_shapes(PagePos { index: 0, page });
    }

    for page in 0..3u32 {
        assert_eq!(
            shape_of(PagePos { index: 0, page }, &gal),
            Single,
            "page {page} is a portrait page"
        );
    }
    assert_eq!(
        shape_of(PagePos { index: 0, page: 3 }, &gal),
        Double,
        "the spread is judged by its own dimensions, not page 0's"
    );

    // The turn walk agrees, which is the consequence that matters: the spread takes
    // a whole turn and the pages around it pair from where they really are. A
    // path-keyed cache judges all four `Single` and tiles them as pages 0+1 then
    // 2+3, putting the spread beside a portrait page.
    //
    // One document, so a flat ordinal *is* the page number and no mapping is needed.
    // An anchor is the *first* page of its turn, so the pair is `{1,2}` and its
    // anchor is page 1.
    let page = |n: u32| PagePos { index: 0, page: n };
    let mut anchors = vec![page(0)];
    let mut cur = page(0);
    while let Some(next) = next_anchor(cur, 1, &gal) {
        if anchors.contains(&next) {
            break;
        }
        anchors.push(next);
        cur = next;
    }
    assert_eq!(
        anchors,
        vec![page(0), page(1), page(3)],
        "the cover alone, then pages 1+2 paired, then the spread alone"
    );
    assert_eq!(turn_len(page(3), 1, &gal), 1, "the spread takes a whole turn");
    assert_eq!(turn_len(page(1), 1, &gal), 2, "and page 1 pairs with page 2");
}

/// A **two-page** document whose *second* page is a landscape spread. Page 1 is the
/// shape the cursor has not reached yet, and it has to be right anyway.
fn two_page_tiff_with_a_spread_on_page_1(dir: &std::path::Path) -> std::path::PathBuf {
    use tiff::encoder::{colortype, TiffEncoder};
    let p = dir.join("volume.tiff");
    let mut bytes = Vec::new();
    {
        let mut enc = TiffEncoder::new(std::io::Cursor::new(&mut bytes)).unwrap();
        for &(w, h) in &[(900u32, 1200u32), (1600, 900)] {
            let px: Vec<u8> = (0..(w * h)).flat_map(|_| [9u8, 9, 9, 255]).collect();
            enc.write_image::<colortype::RGBA8>(w, h, &px).expect("one chained page");
        }
    }
    std::fs::write(&p, &bytes).unwrap();
    p
}

/// RULING 13: **the page after the cursor is already measured**, because the pairing
/// query reads one page ahead.
///
/// `lone(anchor)` asks about `anchor + 1`, and an unknown `+1` reads as `Single`. So
/// learning only the page under the cursor gets the turn wrong in the worst way
/// available: arriving at page 1 changes page 1's shape, which changes `lone`, which
/// **re-flows the turn the reader is already looking at**. A spread that changes
/// under the reader is worse than one that was wrong from the start.
///
/// The cursor is asserted to be on page 0 throughout, because that is the whole
/// claim: page 1 is judged without having been visited.
///
/// This is the regression the next-page learn exists to prevent, and nothing else
/// pins it: removing the learn leaves page 1 at `None`, `shape_of` calls that
/// `Single`, and the turn silently pairs the spread with the page after it.
#[test]
fn a_spread_on_the_next_page_is_known_before_the_cursor_reaches_it() {
    let dir = tempfile::tempdir().unwrap();
    let p = two_page_tiff_with_a_spread_on_page_1(dir.path());
    let at = |page: u32| PagePos { index: 0, page };

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_pages(&p);
    assert_eq!(gal.pages_in(0), 2, "the fixture is really two pages");

    // The cursor arrives at page 0 and stops there.
    gal.goto_index(0);
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(gal.page_pos(), at(0), "the cursor has not moved to page 1");

    assert_eq!(
        PageCtx::dims(&gal, at(1)),
        Some((1600, 900)),
        "page 1 was measured without being visited"
    );
    assert_eq!(
        shape_of(at(1), &gal),
        Double,
        "so the spread is detected before the reader reaches it"
    );

    // The consequence, in turns: page 0 does not pair with the spread, because a
    // spread never shares a turn. Learning page 1 late would have made page 0 pair
    // with it and then, on arrival, break that pair.
    assert!(!pairable(at(0), at(1), &gal), "a wide page never shares a turn");
    assert_eq!(turn_len(at(0), 0, &gal), 1, "so page 0 is alone in its turn");
    assert_eq!(turn_len(at(1), 0, &gal), 1, "and so is the spread");
}

/// The next-page learn is bounded, and both bounds are pinned here.
///
/// **A one-page file pays nothing extra**, which is the ordinary case and the whole
/// of a photo folder: the learn is guarded on the page count being greater than one,
/// so a single-page image produces exactly one entry and no read of a page that does
/// not exist. Dropping the guard is invisible in every other test here, because the
/// fixtures that *have* a next page legitimately learn it.
///
/// **And the hop stays inside the document.** On the last page of a multi-page file,
/// `step_no_wrap` points at the *next file's* first page; that is not this function's
/// business, and nothing is lost by skipping it Ã¢â‚¬â€ the next file's first page is
/// reported by its own preview landing, or measured when the cursor gets there.
///
/// `known_shapes` is what makes the second half visible. `dims` cannot: a page nobody
/// learned answers `None` either way, which is exactly what an unmeasured page is
/// supposed to look like.
///
/// **What this test does *not* establish, stated plainly:** the two bounds are not
/// independently load-bearing, and only one of them can be caught here. Removing the
/// page-count bound is a **no-op mutation** Ã¢â‚¬â€ a one-page file's next page is by
/// definition in a *different* file, so the same-document bound always catches it too.
/// Both mutations were run; that one leaves all 28 tests green. The page-count bound
/// is kept because it states the read-ahead's scope at the point where it is decided
/// and skips one in-memory hop, not because anything depends on it.
#[test]
fn the_next_page_learn_stops_at_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let doc = two_page_tiff_with_a_spread_on_page_1(dir.path());
    let after = dir.path().join("after.tiff");
    write_png(&after, 700, 1000);

    // Two files: the document, then an ordinary image.
    let mut gal = Gallery::from_file(doc.clone());
    gal.files = vec![doc.clone(), after.clone()];
    gal.learn_pages(&doc);
    gal.learn_pages(&after);
    assert_eq!(gal.pages_in(0), 2);

    // **Order matters here, and getting it wrong hides the cross-file hop.** The
    // document's last page is learned *first*, while the next file's page is still
    // unknown. Learn it second instead and the cache-first check absorbs the hop Ã¢â‚¬â€
    // the entry is already there, so `known_shapes` cannot tell "did not learn it"
    // from "learned it and it was a no-op", and a guard that was doing nothing looks
    // exactly like a guard that is working. That is a no-op mutation wearing a green
    // tick, and it is the reason this ordering is spelled out.
    gal.learn_shapes(PagePos { index: 0, page: 1 });
    assert_eq!(
        gal.known_shapes(),
        1,
        "the document's last page was learned and it did not reach into the next file"
    );
    assert_eq!(
        PageCtx::dims(&gal, PagePos { index: 1, page: 0 }),
        None,
        "the next file's first page is still unmeasured"
    );

    // Now the ordinary file. It has one page, so there is no next page of it to
    // learn, and it is the last file, so there is no next file either.
    gal.goto_index(1);
    gal.learn_shapes(PagePos { index: 1, page: 0 });
    assert_eq!(gal.known_shapes(), 2);
    assert_eq!(
        PageCtx::dims(&gal, PagePos { index: 1, page: 0 }),
        Some((700, 1000)),
        "learned directly this time"
    );

    // And a one-page file has no page 1 to invent.
    gal.learn_shapes(PagePos { index: 1, page: 1 });
    assert_eq!(gal.known_shapes(), 2, "a one-page file has no page 1 to learn");
    assert_eq!(PageCtx::dims(&gal, PagePos { index: 1, page: 1 }), None);
}

/// `note_shapes` **fills a gap and never replaces a measurement**, so a decode
/// cannot displace what `learn_shapes` read out of the source header.
///
/// The two numbers here are ratio-equivalent Ã¢â‚¬â€ 900x1200 and 450x600 Ã¢â‚¬â€ so no verdict
/// is at stake either way, and that is exactly the point: the guard is not there to
/// stop a wrong shape, it is there to stop a *source measurement* being replaced by a
/// *downscaled approximation*. `learn_shapes` short-circuits on the entry existing, so
/// once the approximation landed the better number could never come back.
///
/// Both halves are asserted. The first says the learned value survives; the second
/// says `note_shapes` is not merely inert Ã¢â‚¬â€ a key with no entry is still filled, or
/// this test would pass against a `note_shapes` that did nothing at all.
#[test]
fn a_decode_never_displaces_a_learned_dimension() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 900, 1200);

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let at = |page: u32| PagePos { index: 0, page };
    assert_eq!(gal.known_shapes(), 1, "the control: one page is known");
    assert_eq!(PageCtx::dims(&gal, at(0)), Some((900, 1200)), "learned from the header");

    // A decode of the same page reports a half-size version - what an animation over
    // 4096, or a TIFF page past the first, would arrive with.
    gal.note_shapes(ImgKey::first(&p), (450, 600));
    assert_eq!(
        PageCtx::dims(&gal, at(0)),
        Some((900, 1200)),
        "the source measurement survives the decode"
    );
    assert_eq!(gal.known_shapes(), 1, "and the decode added no second entry");

    // And a page with no entry is still filled, so the guard above is a guard and
    // not a no-op.
    gal.note_shapes(ImgKey { path: p.clone(), page: 4 }, (300, 400));
    assert_eq!(PageCtx::dims(&gal, at(4)), Some((300, 400)), "a gap is still filled");
    assert_eq!(gal.known_shapes(), 2);
}

/// A learned dimension is **frozen** until it is dropped.
///
/// This is the same rule the on-disk thumbnail cache follows and for the same
/// reason: correctness must not depend on something remembering to invalidate.
/// Here the thing that would have to remember is the 2 s folder watcher, and a
/// pairing that reads a stale shape mis-pairs every turn *after* the changed
/// file with nothing on screen to say so.
#[test]
fn a_learned_dimension_never_changes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 800, 1200);

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let pos = gal.page_pos();
    assert_eq!(PageCtx::dims(&gal, pos), Some((800, 1200)));

    // Same path, different picture. `learn_shapes` runs on *every* navigation, so
    // re-reading here would be a file open on the critical path of every
    // keypress to re-learn something already known.
    write_png(&p, 300, 200);
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(
        PageCtx::dims(&gal, pos),
        Some((800, 1200)),
        "a learned dimension is not re-read behind its own back"
    );
}

/// ...and `forget_shapes` is what makes it move again. The file's *contents*
/// changed, which is the only signal anything has: `invalidate_path` sees a
/// changed stamp and knows nothing about which dimension it implies.
#[test]
fn forget_shapes_makes_the_dimension_change_again() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("page.png");
    write_png(&p, 800, 1200);

    let mut gal = Gallery::from_file(p.clone());
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    let pos = gal.page_pos();
    assert_eq!(PageCtx::dims(&gal, pos), Some((800, 1200)));

    write_png(&p, 300, 200);
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(PageCtx::dims(&gal, pos), Some((800, 1200)), "still the old one");

    gal.forget_shapes(&p);
    assert_eq!(PageCtx::dims(&gal, pos), None, "forgotten means unknown, not stale");
    gal.learn_shapes(PagePos { index: 0, page: 0 });
    assert_eq!(PageCtx::dims(&gal, pos), Some((300, 200)), "re-learned from the new bytes");
}

/// `PageCtx::step` on a real `Gallery` is the same walk `Gallery::next` and
/// `Gallery::prev` do, so the two cannot disagree about where a page is.
///
/// It is `next`/`prev` on a copy rather than a reimplementation, because the rules
/// that are easy to get wrong are all in them: from page 0 `prev` lands on the
/// **last** page of the *previous* file, not its first; the file boundary is not
/// a turn boundary; and `loop_back` decides what happens at either end. Page
/// counts are deliberately uneven (2, 1, 3) so a step that crossed a file
/// boundary and a step that stayed inside a document are both exercised, and the
/// cursor is parked *after* the step in each direction.
///
/// Asserted in both directions over the whole gallery rather than at one position:
/// `prev`'s page-before-file rule only shows up somewhere in the middle, and the
/// second assertion is what proves `step` is not a `next` that happens to look
/// right forwards.
#[test]
fn a_gallery_walks_pages_the_same_way_the_planner_sees_them() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<std::path::PathBuf> =
        ["a.jpg", "b.jpg", "c.jpg"].iter().map(|n| dir.path().join(n)).collect();
    for p in &paths {
        std::fs::write(p, b"not really a jpeg").unwrap();
    }
    let mut gal = gallery_of(&paths, &[2, 1, 3]);
    gal.set_loop_back(false);

    let start = gal.page_pos();
    assert_eq!(start, PagePos { index: 0, page: 0 });
    let mut forwards = vec![start];
    while gal.next() {
        forwards.push(gal.page_pos());
    }
    // 2 + 1 + 3 pages, in reading order, with no page named twice.
    assert_eq!(
        forwards,
        vec![
            PagePos { index: 0, page: 0 },
            PagePos { index: 0, page: 1 },
            PagePos { index: 1, page: 0 },
            PagePos { index: 2, page: 0 },
            PagePos { index: 2, page: 1 },
            PagePos { index: 2, page: 2 },
        ]
    );

    for pair in forwards.windows(2) {
        assert_eq!(PageCtx::step(&gal, pair[0], 1), Some(pair[1]), "step forwards from {pair:?}");
        assert_eq!(PageCtx::step(&gal, pair[1], -1), Some(pair[0]), "step back from {pair:?}");
    }

    // And `prev` retraces the entire walk, which is the half `step` could have
    // been wrong about in a way the loop above would not show: from page 0 of
    // file 1 it goes to the *last* page of file 0, not to file 0's first.
    for &expected in forwards[..forwards.len() - 1].iter().rev() {
        assert!(gal.prev(), "there is a previous page before {expected:?}");
        assert_eq!(gal.page_pos(), expected);
    }
    assert!(!gal.prev(), "nothing before the first page");
    assert_eq!(gal.page_pos(), start, "a refused `prev` does not move the cursor");

    // Multi-page steps, because `prev_anchor`'s walk steps by a whole turn and a
    // turn is 1 or 2. Same reason `step_no_wrap` is a loop rather than a `match`.
    assert_eq!(
        PageCtx::step(&gal, PagePos { index: 0, page: 0 }, 2),
        Some(PagePos { index: 1, page: 0 })
    );
    assert_eq!(
        PageCtx::step(&gal, PagePos { index: 2, page: 2 }, -2),
        Some(PagePos { index: 2, page: 0 })
    );
    // Off either end is `None`, not a wrap, with looping off.
    assert_eq!(PageCtx::step(&gal, PagePos { index: 2, page: 2 }, 1), None);
    assert_eq!(PageCtx::step(&gal, PagePos { index: 0, page: 0 }, -1), None);
    // An index that does not exist is `None` rather than a wrap to page 0, which
    // is what the raw `next`/`prev` arithmetic would do on its own: `next` from
    // an out-of-range index finds no next file and loops.
    assert_eq!(PageCtx::step(&gal, PagePos { index: 9, page: 0 }, 1), None);

    // With `loop_back` on, `step` wraps where `step_no_wrap` does not - the whole
    // reason there are two methods, so both are asserted rather than one.
    gal.set_loop_back(true);
    let last = *forwards.last().unwrap();
    assert_eq!(PageCtx::step(&gal, last, 1), Some(start), "forward wraps");
    assert_eq!(
        PageCtx::step(&gal, start, -1),
        Some(last),
        "and backwards wraps to the last page, not to file 0's first"
    );
    assert_eq!(
        PageCtx::step_no_wrap(&gal, last, 1),
        None,
        "while the partner test must not see across the seam"
    );
}

/// `flat` is the page's ordinal in the whole gallery, so it can be compared with
/// `lead_lone` - and a *sum* of page counts is only a sum if they are known.
/// A file whose count has not been learned contributes an unknown number of
/// pages, so the answer is `None` rather than a number that quietly assumes one.
///
/// Both halves are asserted, and the difference between them is the point: the
/// same position goes `None` -> `Some` as the missing count is supplied, so the
/// test cannot be passing because the position was always unanswerable.
///
/// `None` for the first page is the optimisation the walk depends on: it is only
/// ever asked about the leading pages, where nothing precedes the position, and
/// there is nothing to be uncertain about.
#[test]
fn an_unknown_preceding_page_count_makes_flat_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let paths: Vec<std::path::PathBuf> =
        ["a.jpg", "b.jpg", "c.jpg", "d.jpg"].iter().map(|n| dir.path().join(n)).collect();
    for p in &paths {
        std::fs::write(p, b"not really a jpeg").unwrap();
    }
    let mut gal = gallery_of(&paths, &[2, 1, 1, 1]);
    let at = |i: usize, page: u32| PagePos { index: i, page };

    // Every count known: the sum is real, and it counts pages rather than files.
    assert_eq!(PageCtx::flat(&gal, at(0, 0)), Some(0));
    assert_eq!(PageCtx::flat(&gal, at(0, 1)), Some(1));
    assert_eq!(PageCtx::flat(&gal, at(1, 0)), Some(2), "file 0 has two pages");
    assert_eq!(PageCtx::flat(&gal, at(3, 0)), Some(2 + 1 + 1));

    // Forget file 0's count. Nothing about file 1 changes, but its ordinal now
    // depends on a number nobody has.
    gal.forget_page_count(&paths[0]);
    assert_eq!(PageCtx::flat(&gal, at(0, 0)), Some(0), "nothing precedes the first page");
    assert_eq!(PageCtx::flat(&gal, at(0, 1)), Some(1));
    assert_eq!(PageCtx::flat(&gal, at(1, 0)), None, "file 0's count is unknown");
    assert_eq!(PageCtx::flat(&gal, at(3, 0)), None);

    // Supplying it again resolves the same positions. `learn_pages` reads four
    // bytes and this file is not a TIFF, so it learns 1 - which is why the
    // assertion is `Some(1)` and not `Some(2)`.
    gal.learn_pages(&paths[0]);
    assert_eq!(PageCtx::flat(&gal, at(1, 0)), Some(1));

    // A position that does not exist is `None`, and is not a panic.
    assert_eq!(PageCtx::flat(&gal, at(4, 0)), None);
}

/// A re-exported file must be re-judged, or its old page's shape is frozen for the
/// session and **every spread after it stays mis-paired**.
///
/// What is pinned here is the **call**, and nothing else. The behaviour it calls is
/// asserted once, by [`forget_shapes_makes_the_dimension_change_again`], and repeating
/// it here would only be a second copy to keep in step Ã¢â‚¬â€ so this test is *only* the
/// source scan, and it says why.
///
/// The reason it needs one: the thing that decides a file changed is
/// `MyApp::invalidate_path`, and that lives in `src/main.rs` Ã¢â‚¬â€ a binary no
/// integration test can reach. Adding a `#[cfg(test)]` module to it for this would
/// break a documented property of the project (`AGENTS.md`, "Tests"), so the call is
/// asserted against the source as text, for the same reason `tests/renderer.rs` and
/// `tests/keybindings.rs` do.
///
/// A source scan is not behaviour, and it is weak in a specific way: it cannot tell
/// whether `invalidate_path` *reaches* `forget_shapes` on this path or on some
/// other. What it does catch is the regression that actually happens Ã¢â‚¬â€ the call being
/// deleted, or a later edit adding a cache here and forgetting this one Ã¢â‚¬â€ and neither
/// is visible at runtime. Deleting the line fails this test; that has been run.
///
/// Scoped to `invalidate_path`'s own body so a `forget_shapes` anywhere else in
/// the file cannot stand in for this one Ã¢â‚¬â€ and `remove_current_from_gallery`
/// calls it too, so that is not hypothetical.
#[test]
fn invalidate_path_drops_the_cached_dimensions() {
    let src = main_src();
    let body = fn_body(&src, "fn invalidate_path(");

    assert!(
        body.contains("forget_shapes("),
        "invalidate_path no longer forgets the cached dimensions, so a re-exported\n\
         file keeps its old page's shape and every spread after it stays mis-paired\n\
         for the session, with nothing on screen to say so.\nBody was:\n{body}"
    );
}

/// ...and the same function is the only thing that can drop a file's *overrides*, so
/// this is where the wiring for those lives too.
///
/// Two calls, and they are one feature: dropping the entry in memory without saving
/// would make "a re-export clears the override" true for one session and false
/// across a restart, because the next load would read the stale entry straight back
/// out of the file. So the `save_spreads` assertion is not a separate concern from
/// the `forget_file` one Ã¢â‚¬â€ it is the half that makes it durable.
///
/// The source scan is weak in the way [`invalidate_path_drops_the_cached_dimensions`]
/// says, and for the same reason it exists at all. It has one advantage over that
/// test, though: it reuses the brace matching already written there (see
/// [`fn_body`]) rather than adding a second copy of it.
#[test]
fn invalidate_path_drops_the_overrides_and_saves_them() {
    let src = main_src();
    let body = fn_body(&src, "fn invalidate_path(");

    assert!(
        body.contains("spreads.forget_file("),
        "invalidate_path no longer forgets the user's overrides, so a re-exported\n\
         file keeps page 3 a spread for the session whatever it now contains - and\n\
         because an override beats the detector, nothing on screen says why.\n\
         Body was:\n{body}"
    );
    assert!(
        body.contains("save_spreads("),
        "invalidate_path drops the override but does not save, so the stale entry is\n\
         still in spreads.json and comes back on the next launch.\nBody was:\n{body}"
    );
}

/// ...and the same reasoning pins the other end of the turn: a **partner's** preview
/// has to re-plan, or a spread that paired on an unmeasured page and then turned out
/// to be a wide page stands wrong with nothing on screen to say why.
///
/// The rule is asserted by
/// [`a_partners_arrival_re_plans_because_it_can_change_the_turn`], which is
/// `image_viewer::arrival_reschedules` called directly. **What is pinned here is the
/// call site**, because `MyApp::pump` is a binary and a call deleted from one is
/// invisible to every behavioural test in the tree - `resched` would simply stop being
/// set, and the app would go on looking correct.
///
/// A source scan is not behaviour, and it is weak in a specific way: **it cannot tell
/// whether `pump` ever *reaches* this line** - only that the line is in the arm. A
/// guard could fire the branch but never call this, or the call could sit behind a
/// condition nobody exercises, and both pass here. What it does catch is the
/// regression that actually happens, which is the deletion or the move out of the arm,
/// and neither is visible at runtime. Deleting the call fails this test; that has been
/// run.
///
/// Scoped to the **`Ok` half of the `Msg::Preview` arm**, not to `pump` as a whole,
/// and narrower than that sounds. Brace matching starts after the signature, whose
/// trailing `{` is the `match result`'s own, so the next brace it meets is the one
/// that opens `Ok(p) => {` Ã¢â‚¬â€ and the brace it stops on is that arm's. The span is
/// therefore the `Ok` arm's body and nothing more: the `Err` arm, the `Msg::Full`
/// and `Msg::Exif` arms, and the `turn_partner` capture above the message loop are
/// all outside it and cannot stand in for the call. (The existing `fn_body` doc
/// records that a naive "up to the next `fn`" split once made a scan of this shape
/// pass **vacuously**; the brace match is what makes this one falsifiable, and the
/// guard is the proof.)
#[test]
fn arriving_previews_re_plan_through_the_partner() {
    let src = main_src();
    let arm = fn_body(&src, "Msg::Preview { key, result } => match result {");

    assert!(
        arm.contains("image_viewer::arrival_reschedules("),
        "the Msg::Preview arm no longer asks arrival_reschedules whether to re-plan,\n\
         so a partner's decode no longer re-plans. That is the half that heals a turn:\n\
         pairable treats unknown dimensions optimistically, so a partner planned at the\n\
         current priority can turn out to be a wide page, note_shapes learns it, and the\n\
         turn collapses to one page - with no re-plan the two-page spread stays painted\n\
         until the reader moves, and nothing says why.\nBody was:\n{arm}"
    );
}
