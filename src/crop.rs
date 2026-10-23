//! Module `crop`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use crop::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Cropping
// ---------------------------------------------------------------------------

/// A crop rectangle in **stored** pixel coordinates — that is, before the EXIF
/// orientation is applied.
///
/// Stored, not display, and that is the whole subtlety. TurboJPEG's crop region
/// is expressed against the compressed source, so the rectangle has to be
/// converted from the picture the user dragged on. It is safe to do it in that
/// order — crop the stored pixels, *then* turn them — because a lattice symmetry
/// maps a rectangle to a rectangle: `T(crop(src, R'))` is exactly
/// `crop(T(src), R)` when `R' = T⁻¹(R)`. So the result is the region the user
/// actually framed, not the same region somewhere else in the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Crop {
    /// A validated crop, or `None` for an empty or inverted rectangle.
    pub fn new(x: u32, y: u32, w: u32, h: u32) -> Option<Crop> {
        (w > 0 && h > 0).then_some(Crop { x, y, w, h })
    }

    pub fn right(&self) -> u32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> u32 {
        self.y + self.h
    }

    /// Does this crop keep every pixel? Then there is nothing to do, and the save
    /// should behave exactly as it did before cropping existed.
    pub fn is_full(&self, w: u32, h: u32) -> bool {
        self.x == 0 && self.y == 0 && self.w >= w && self.h >= h
    }

    /// Pull the rectangle inside a `w x h` image, shrinking rather than moving,
    /// and refusing to become empty.
    pub fn clamped_to(&self, w: u32, h: u32) -> Option<Crop> {
        if self.x >= w || self.y >= h {
            return None;
        }
        Some(Crop {
            x: self.x,
            y: self.y,
            w: self.w.min(w - self.x),
            h: self.h.min(h - self.y),
        })
    }

    /// Grow the rectangle onto the MCU grid, and clamp it to the image.
    ///
    /// TurboJPEG requires a crop's *origin* to sit on an MCU boundary — 8 px for
    /// 4:4:4, 16 px for the common 4:2:0 — and a size that stops part-way through
    /// a block leaves a partial block that `trim` then discards. Either way the
    /// boundary lands somewhere other than where the user asked.
    ///
    /// Grown, not shrunk, so nothing inside the selection is ever lost: you get
    /// up to one block of *extra* picture rather than up to one block cut off the
    /// edge you framed. The UI draws this rectangle, not the raw drag, so what is
    /// on screen is what gets saved — the same bargain the rotate trim already
    /// makes, and the only honest one available at this granularity.
    pub fn snapped_to_mcu(&self, mcu_w: u32, mcu_h: u32, w: u32, h: u32) -> Option<Crop> {
        let mcu_w = mcu_w.max(1);
        let mcu_h = mcu_h.max(1);
        // The origin moves *down* to the grid, which pushes the far edge out, so
        // the rectangle only ever grows before the clamp.
        let snapped = Crop {
            x: self.x - self.x % mcu_w,
            y: self.y - self.y % mcu_h,
            w: self.w + self.x % mcu_w,
            h: self.h + self.y % mcu_h,
        };
        snapped.clamped_to(w, h)
    }

    /// The rectangle the user framed on the **displayed** picture, expressed in
    /// stored coordinates.
    ///
    /// `rect` is `(x, y, w, h)` in the displayed image's own pixels — not screen
    /// points; the caller does the scaling. `stored` is the file's `(w, h)`.
    ///
    /// The four corners go through [`Xform::inverse`], and the answer is their
    /// bounding box. A lattice symmetry sends a rectangle to a rectangle, so the
    /// box is exact rather than an approximation, and going through `map` means
    /// this cannot disagree with what is drawn on screen.
    pub fn from_display_rect(
        exif_tag: u16,
        user: Xform,
        rect: (u32, u32, u32, u32),
        stored: (u32, u32),
    ) -> Option<Crop> {
        let (dx, dy, dw, dh) = rect;
        let (dw, dh) = (dw.checked_sub(1)?, dh.checked_sub(1)?); // to inclusive corners
        let (sw, sh) = stored;
        let display = display_xform(exif_tag, user);

        // `Xform::map` takes *output* coordinates plus the *input* dimensions: it
        // answers "this output pixel came from which input pixel". The displayed
        // picture is the output here and the stored pixels are the input, so this
        // is `display.map(..)` with the stored dimensions — no inverse needed.
        // Passing the inverse here instead transposes the rectangle, which for a
        // quarter turn is the difference between cropping the top-left of the
        // photo and the bottom-left.
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (cx, cy) in [(dx, dy), (dx + dw, dy), (dx, dy + dh), (dx + dw, dy + dh)] {
            let (sx, sy) = display.map(cx, cy, sw, sh);
            min_x = min_x.min(sx);
            min_y = min_y.min(sy);
            max_x = max_x.max(sx);
            max_y = max_y.max(sy);
        }
        Crop::new(min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)?.clamped_to(sw, sh)
    }

    /// This stored rectangle expressed in the **displayed** picture's pixels.
    ///
    /// The forward direction, and the exact inverse of [`Self::from_display_rect`].
    /// The UI draws the result of this, so what is framed on screen is what the
    /// save will cut — and
    /// `a_display_rect_survives_the_round_trip_through_stored_space` checks the
    /// two against each other over every transform and a grid of rectangles.
    ///
    /// `display` is the full stored -> display transform, i.e. the one from
    /// [`display_xform`], not the user's part alone. `stored` is the file's
    /// `(w, h)`, which is what the display's dimensions are derived from.
    pub fn to_display_rect(&self, display: Xform, stored: (u32, u32)) -> (u32, u32, u32, u32) {
        let (dw, dh) = display.out_dims(stored.0, stored.1);
        let inv = display.inverse();
        let x1 = self.right().saturating_sub(1);
        let y1 = self.bottom().saturating_sub(1);
        let mut min_x = u32::MAX;
        let mut min_y = u32::MAX;
        let mut max_x = 0u32;
        let mut max_y = 0u32;
        for (cx, cy) in [
            (self.x, self.y),
            (x1, self.y),
            (self.x, y1),
            (x1, y1),
        ] {
            let (dx, dy) = inv.map(cx, cy, dw, dh);
            min_x = min_x.min(dx);
            min_y = min_y.min(dy);
            max_x = max_x.max(dx);
            max_y = max_y.max(dy);
        }
        (min_x, min_y, max_x - min_x + 1, max_y - min_y + 1)
    }

    /// Cut this rectangle out of an RGBA buffer in stored coordinates.
    pub fn apply_rgba(&self, w: u32, h: u32, rgba: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let c = self.clamped_to(w, h)?;
        let stride = w as usize * 4;
        let row = c.w as usize * 4;
        let mut out = Vec::with_capacity(c.w as usize * c.h as usize * 4);
        for y in c.y..c.bottom() {
            let start = y as usize * stride + c.x as usize * 4;
            out.extend_from_slice(rgba.get(start..start + row)?);
        }
        Some((c.w, c.h, out))
    }

    /// A short description for the status line and the save confirmation.
    pub fn label(&self) -> String {
        format!("{}×{}", self.w, self.h)
    }
}

/// The four rectangles that dim everything outside `crop` but inside `outer`.
///
/// Axis-aligned rectangles as `[min_x, min_y, max_x, max_y]`, ordered top, bottom,
/// left, right. The top and bottom bands span the **full** width of `outer`, and
/// the left and right bands span only between them. That split is what makes them
/// tile the difference *exactly* — every pixel outside the frame is covered once
/// and no pixel inside it is covered at all.
///
/// Which is the whole content of this function, and the first version got it
/// wrong: the top band ran from the picture's corner to the frame's top-*left*
/// corner and the bottom band from the frame's bottom-*right* to the picture's
/// corner, so only the corners of the top and bottom edges were dimmed while the
/// left and right bands were right. It looked like a rendering bug and was
/// actually an arithmetic one.
/// `the_dimming_bands_cover_exactly_what_is_being_cut_away` is what pins it.
///
/// Plain floats rather than `egui::Rect` so it can be tested: this module has no
/// egui dependency on purpose.
pub fn dim_bands(outer: [f32; 4], crop: [f32; 4]) -> [[f32; 4]; 4] {
    let [ox0, oy0, ox1, oy1] = outer;
    // Clip the frame to the picture. A drag that ends outside the image must not
    // dim anything beyond the edge.
    let cx0 = crop[0].max(ox0);
    let cy0 = crop[1].max(oy0);
    let cx1 = crop[2].min(ox1);
    let cy1 = crop[3].min(oy1);
    [
        [ox0, oy0, ox1, cy0], // top:    full width
        [ox0, cy1, ox1, oy1], // bottom: full width
        [ox0, cy0, cx0, cy1], // left:   only between the other two
        [cx1, cy0, ox1, cy1], // right:  only between the other two
    ]
}

/// The images the preview cache must never evict.
///
/// The rule is "never throw away the picture you are looking at, or one of its
/// immediate neighbours". Once the cache is keyed by (file, page) that rule has to
/// be stated in those terms, and the obvious reading of it — protect every page of
/// the protected *files* — is wrong in a way that does not show up on a photo
/// folder at all:
///
/// * walking into a 20-page document, all 20 pages are the same file, so all 20
///   are protected, so nothing is evictable and the cache grows past
///   `PREVIEW_CACHE_CAP` and stays there. The eviction loop has no victim and
///   gives up, which is how a bounded cache silently becomes unbounded.
/// * it is also stricter than the rule: page 1 of the file you are on is not the
///   page you are on, and evicting it costs a re-decode the moment you press left.
///
/// So: the current *page*, plus page 0 of each neighbour (which is all the
/// preload window ever decodes), plus whatever is actually on screen — the last of
/// those because `shown` lags `cur` by design, and during that lag it is a
/// different image.
///
/// Pure, so the policy is testable. It lives here rather than in the app because
/// the app is a binary and this is the sort of thing that is only ever noticed by
/// its memory growing.
///
/// **A manga turn's second half is in this set only when `shown` names it, and that
/// is a gap, not an oversight** — an earlier design note claimed "the partner is
/// index ±1, well inside `PRELOAD_RADIUS = 3`, so this needs no change", and that is
/// **false for a turn inside a document**: the partner there is `(index, page + 1)`,
/// and the window above inserts `ImgKey::first` of each neighbour, i.e. `(index±k,
/// 0)`. So an in-document partner is not protected by position.
///
/// It is protected in practice, by the thing that cannot be evicted anyway:
/// `central` stamps the turn's second page's `used` every frame it is on screen, so
/// it is never the LRU victim while the reader is looking at it, and a turn of two
/// files is `ImgKey::first` of a neighbour and *is* in the set. **Recorded as a
/// premise to distrust rather than a guarantee**: nothing here is what makes it safe,
/// and if the eviction policy is ever changed to prefer something other than `used`,
/// this set is where the in-document partner has to be added.
pub fn protected_preview_keys(
    files: &[PathBuf],
    index: usize,
    page: u32,
    shown: Option<&ImgKey>,
    radius: usize,
) -> std::collections::HashSet<ImgKey> {
    let n = files.len();
    let mut out = std::collections::HashSet::new();
    if n == 0 {
        if let Some(s) = shown {
            out.insert(s.clone());
        }
        return out;
    }
    let i = index.min(n - 1);
    if let Some(f) = files.get(i) {
        out.insert(ImgKey {
            path: f.clone(),
            page,
        });
    }
    for k in 0..=2 * radius {
        if let Some(f) = files.get((i + n * 2 + k - radius) % n) {
            out.insert(ImgKey::first(f));
        }
    }
    if let Some(s) = shown {
        out.insert(s.clone());
    }
    out
}

