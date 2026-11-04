//! Module `gallery`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use gallery::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// A simple gallery state: a list of files and a current index.
#[derive(Debug, Clone)]
pub struct Gallery {
    pub files: Vec<PathBuf>,
    pub index: usize,
    /// Whether advancing past either end wraps around to the other end.
    ///
    /// Defaults to on (the historical behaviour, and what old configs get). It
    /// lives here rather than in the caller so `next`/`prev` cannot be used in a
    /// way that quietly ignores the setting.
    pub loop_back: bool,
    /// Which page of the current file, 0-based. Non-zero only for pages of a
    /// multi-page TIFF.
    ///
    /// **A cursor, not a list.** The alternative — splicing an entry per page into
    /// `files` — was rejected because `files` is the *unfiltered* shape that the
    /// filter, the resume point, the rescan and `all_files` are all keyed on, and
    /// growing it means growing the index arithmetic in every one of them. A
    /// cursor keeps `files` exactly as it was, so all of that is untouched and
    /// only the arithmetic in `next`/`prev` learns about pages.
    page: u32,
    /// How many pages each visited file has. Only ever holds files that have been
    /// opened, and pruned whenever `files` is rebuilt, so it cannot grow without
    /// bound as you browse.
    page_counts: std::collections::HashMap<PathBuf, u32>,
    /// Display dimensions of each page whose shape has been asked for, keyed by
    /// [`ImgKey`].
    ///
    /// **Per page, not per file**, and the reason is a document with a fold-out. A
    /// manga volume scanned as one multi-page TIFF can hold a landscape double-page
    /// spread among portrait singles: page 3 is a spread, page 4 is a page, and one
    /// entry for the file would judge both by page 0 and give every one of them the
    /// wrong turn. `ImgKey { path, page }` is the identity every pixel cache here
    /// already uses, and `PageCtx::dims` already has one in hand via `key(pos)`, so
    /// this is the keying that needed no new concept.
    ///
    /// What was traded for it: a 200-page document is measured as its pages are
    /// reached rather than in one read, which is the same bargain `plan_jobs` makes
    /// about prefetching only page 0 of a neighbour — you arrive at a later page by
    /// pressing right, and `learn_shapes` runs on that arrival.
    ///
    /// Cleared with `page_counts` whenever `files` is rebuilt, for the same reason a
    /// count must not outlive the list it describes: these are keyed on paths and
    /// would otherwise accumulate every page the session ever opened.
    shapes: std::collections::HashMap<ImgKey, (u32, u32)>,
    /// Persisted dims/orientation/capture-date sidecar, keyed by file path.
    /// Loaded once from `%LOCALAPPDATA%\image-viewer\shapes.json`, saved when
    /// dirty. Invalidated by mtime+length so an edited file is re-measured.
    shape_cache: std::collections::HashMap<String, FileShape>,
    shape_cache_loaded: bool,
    shape_cache_dirty: bool,
}

/// One file's measured shape, persisted across sessions.
///
/// `date_taken` is EXIF `DateTimeOriginal` as "YYYY:MM:DD HH:MM:SS", empty when
/// absent or unreadable. `orientation` is the EXIF tag (1-8), 0 when absent.
/// `mtime`/`len` are the invalidate key: a file whose either changed is re-measured.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct FileShape {
    pub w: u32,
    pub h: u32,
    pub orientation: u16,
    pub date_taken: String,
    pub mtime: u64,
    pub len: u64,
}

/// A filter needle interpreted as a rating query rather than a name substring.
///
/// Checked before the substring fallback, in this order: `stars:N`, `stars:N+`,
/// `flagged` / `unflagged`, `tag:NAME` (case-insensitive substring of the tag
/// list). Both `stars:` forms mean *at least* N — the pinned test has `stars:4`
/// matching a 5-star file, so `stars:N` is not an equality check and the `+`
/// is accepted but redundant. The needle handed here is already trimmed and
/// lowercased, so `tag:` matching lowercases only the stored tags.
pub(crate) enum RatingTerm {
    StarsAtLeast(u8),
    Flagged,
    Unflagged,
    Tag(String),
}

/// Parse an already-trimmed-and-lowercased needle as a rating query.
///
/// `flagged` / `unflagged` match only the whole needle: `flagged pics` stays a
/// name substring, since a word that merely leads a longer needle is not the
/// query. `stars:` and `tag:` match on prefix; an empty `tag:` (`tag:` alone)
/// is not a query and falls back to substring, while an unparseable `stars:`
/// value is not a query either and likewise falls back to substring.
pub(crate) fn parse_rating_term(needle: &str) -> Option<RatingTerm> {
    if let Some(rest) = needle.strip_prefix("stars:") {
        if let Some(min) = rest.strip_suffix('+') {
            return min.parse::<u8>().ok().map(RatingTerm::StarsAtLeast);
        }
        return rest.parse::<u8>().ok().map(RatingTerm::StarsAtLeast);
    }
    if needle == "flagged" {
        return Some(RatingTerm::Flagged);
    }
    if needle == "unflagged" {
        return Some(RatingTerm::Unflagged);
    }
    if let Some(name) = needle.strip_prefix("tag:")
        && !name.is_empty()
    {
        return Some(RatingTerm::Tag(name.to_string()));
    }
    None
}

pub(crate) fn rating_term_matches(term: &RatingTerm, ratings: &Ratings) -> bool {
    match term {
        RatingTerm::StarsAtLeast(n) => ratings.rating.stars >= *n,
        RatingTerm::Flagged => ratings.rating.flag,
        RatingTerm::Unflagged => !ratings.rating.flag,
        RatingTerm::Tag(name) => ratings.tags.iter().any(|t| t.to_lowercase().contains(name)),
    }
}

impl Gallery {
    pub fn new(folder: &Path) -> Self {
        Self {
            files: scan_folder(folder),
            index: 0,
            loop_back: true,
            page: 0,
            page_counts: std::collections::HashMap::new(),
            shapes: std::collections::HashMap::new(),
            shape_cache: std::collections::HashMap::new(),
            shape_cache_loaded: false,
            shape_cache_dirty: false,
        }
    }

    /// An empty gallery.
    ///
    /// For the path where the caller is about to install its own list - start-up
    /// with `load_all_on_startup` calls [`scan_folder_sorted`] immediately after.
    /// [`Self::new`] would walk the directory and have the result thrown away,
    /// which on a 1001-image folder is ~50 ms before the window is even up.
    pub fn empty() -> Self {
        Self {
            files: Vec::new(),
            index: 0,
            loop_back: true,
            page: 0,
            page_counts: std::collections::HashMap::new(),
            shapes: std::collections::HashMap::new(),
            shape_cache: std::collections::HashMap::new(),
            shape_cache_loaded: false,
            shape_cache_dirty: false,
        }
    }

    /// Create a gallery containing a single file.
    pub fn from_file(path: PathBuf) -> Self {
        Self {
            files: vec![path],
            index: 0,
            loop_back: true,
            page: 0,
            page_counts: std::collections::HashMap::new(),
            shapes: std::collections::HashMap::new(),
            shape_cache: std::collections::HashMap::new(),
            shape_cache_loaded: false,
            shape_cache_dirty: false,
        }
    }

    /// The current page's position in the flat page sequence, which is what the
    /// pairing walk is expressed in terms of.
    pub fn page_pos(&self) -> PagePos {
        PagePos { index: self.index, page: self.page }
    }

    pub fn current(&self) -> Option<&Path> {
        self.files.get(self.index).map(|p| p.as_path())
    }

    /// The page of the current file to decode. 0 for everything but pages of a
    /// multi-page TIFF past the first.
    pub fn current_page(&self) -> u32 {
        self.page
    }

    /// Set page counts positionally, for `files` as it currently stands.
    ///
    /// **For tests.** The app learns counts one file at a time with
    /// [`Self::learn_pages`] as you arrive at them, which is the right shape
    /// because it costs four bytes for a non-TIFF. A test about *navigation*
    /// should not also depend on the IFD walker being correct — when that breaks,
    /// every navigation test fails for the wrong reason and the actual fault is
    /// buried. Counting has its own test.
    pub fn set_page_counts<I: IntoIterator<Item = u32>>(&mut self, counts: I) {
        self.page_counts.clear();
        for (path, n) in self.files.iter().zip(counts) {
            self.page_counts.insert(path.clone(), n.max(1));
        }
        self.page = 0;
    }

    /// How many files have a remembered page count. So a test can assert the map
    /// was actually emptied rather than merely that nothing broke.
    pub fn known_page_counts(&self) -> usize {
        self.page_counts.len()
    }

    /// How many **pages** have remembered dimensions. So a test can assert a
    /// `forget_shapes` really dropped *every* page of a document rather than one —
    /// the count is the only place that difference is visible, since a per-page map
    /// and a per-file one answer the same single-page query identically.
    pub fn known_shapes(&self) -> usize {
        self.shapes.len()
    }

    /// The position `key` names, as the inverse of [`PageCtx::key`].
    ///
    /// **Needed because "what is on screen" is a key and "which turn is it on" is a
    /// position**, and the pairing is asked entirely in positions. A turn's second
    /// half is derived from an anchor, so deriving it for the picture the app is
    /// painting means turning that key back into a position on every frame.
    ///
    /// **A linear scan, and that is the trade.** `set_filtered` already searches
    /// `files` this way for the same reason — a key does not know its index — so this
    /// is not a new shape, only a new caller. It is a `PathBuf` comparison per file,
    /// pointer-equal for the common case where the app asks about the cursor's own
    /// file; [`Self::page_pos`] answers that without the scan at all, which is why the
    /// app checks that first.
    ///
    /// **Only the path is matched, and the page is trusted**, for [`Self::goto_page`]'s
    /// reason: every caller in this crate hands over a key that some `key(pos)` built,
    /// and inventing a second page-range rule here is a place for the two to disagree.
    /// A key naming a page past the end of its file therefore answers a position
    /// rather than `None`.
    ///
    /// `None` when the path is not in `files` at all — a file removed while it was
    /// still on screen, or one from a gallery that has since been rebuilt.
    pub fn pos_of(&self, key: &ImgKey) -> Option<PagePos> {
        let index = self.files.iter().position(|f| f == &key.path)?;
        Some(PagePos { index, page: key.page })
    }

    /// Is the cursor on the first position of this listing?
    ///
    /// **About the whole listing, page included** — an archive's listing is its
    /// members, so "the first page of the archive" is `page == 0 && index == 0`
    /// and not merely the first file's first page. A reader who has to press
    /// `←` twice to get out of a multi-page document has been left in it.
    pub fn at_front(&self) -> bool {
        self.page == 0 && self.index == 0
    }

    /// Is the cursor on the **last** position of this listing?
    ///
    /// The mirror of [`Self::at_front`], and the reason an archive can be left
    /// forwards as well as backwards: without it, `→` at an archive's last
    /// member is `pos_next`'s ordinary end-of-list wrap, which inside an archive
    /// wraps to the archive's *own* first member. The reader is then cycling
    /// through one volume's pages and can never reach the next archive in the
    /// folder — which reads as "only the first zip loads".
    ///
    /// The page half is what makes it correct for a multi-page TIFF at either
    /// end: `index + 1 == len` is the last *file*, not the last *position*.
    pub fn at_back(&self) -> bool {
        !self.files.is_empty()
            && self.index + 1 == self.files.len()
            && self.page + 1 >= self.pages_in_current()
    }

    /// Whether advancing past either end wraps around. Separate from the field so
    /// a test can turn it off without reaching into the struct.
    pub fn set_loop_back(&mut self, on: bool) {
        self.loop_back = on;
    }

    /// How many pages the current file has, as far as this gallery knows. 1 until
    /// [`Self::learn_pages`] has been asked about the file.
    pub fn pages_in_current(&self) -> u32 {
        self.current().and_then(|p| self.page_counts.get(p)).copied().unwrap_or(1).max(1)
    }

    /// Find out how many pages `path` has and remember it.
    ///
    /// Called when a file becomes current, not during a scan — see
    /// [`tiff_page_count`] for why the cost has to stay off the folder walk.
    ///
    /// **Answers from the cache when it can.** This runs on *every* navigation, so
    /// the first version — which always re-read, because "asking is cheap" was
    /// true of the four bytes and not of the `File::open` around them — put a file
    /// open on the critical path of every single keypress. Browsing back and forth
    /// over three photos is three opens, three closes and three reads to re-learn
    /// something already known.
    pub fn learn_pages(&mut self, path: &Path) -> u32 {
        if let Some(n) = self.page_counts.get(path) {
            return *n;
        }
        let n = tiff_page_count(path);
        self.page_counts.insert(path.to_path_buf(), n);
        if self.current() == Some(path) {
            self.page = self.page.min(n.saturating_sub(1));
        }
        n
    }

    /// Forget every page count. Called whenever `files` is rebuilt, so the map
    /// cannot outlive the list it describes — a stale count would put the cursor
    /// on a page that no longer exists.
    fn forget_pages(&mut self) {
        self.page_counts.clear();
        // The dimensions go with the counts, and for the same reason plus one
        // more: they are keyed on paths, so keeping them would grow with every
        // file the session ever opened rather than with `files`.
        self.shapes.clear();
        self.page = 0;
    }

    /// Find out how large `pos` displays at and remember it for the shape test — **and
    /// the next page's, because the pairing reads one page ahead.**
    ///
    /// A position rather than a `(path, page)` pair because the next-page step needs
    /// the file's *index*: finding it from a path would be a scan of `files` on the
    /// critical path of every keypress, and [`Self::page_pos`] already has it.
    ///
    /// **The page asked about, not page 0.** `pos.page` is 0 for every ordinary
    /// image, so this is unchanged for a photo folder; for a multi-page document it is
    /// the difference between knowing that page 3 is a landscape spread and judging it
    /// by whatever page 0 happened to be. The caller is `goto`, so this is the "learn
    /// on arrival" rule the page count has always followed, applied to the page that
    /// actually arrived.
    ///
    /// ## Why the next page too, and why it is not optional
    ///
    /// [`lone`] decides a turn by asking about `anchor + 1`, and an unknown `+1` reads
    /// as [`Shape::Single`] — the optimistic default. So a turn is only correct if the
    /// *next* page's dimensions are right, and learning only the page under the cursor
    /// gets it wrong in the worst possible way: **arriving at page 5 changes page 5's
    /// shape, which changes `lone`, which changes the turn the reader is already
    /// looking at.** A spread that re-flows mid-read is worse than one that was wrong
    /// from the start, because the reader watched it change.
    ///
    /// Three things bound the cost, so this is not a general "read ahead" policy:
    ///
    /// * **Only for a file known to have more than one page.** `pages_in` is 1 for
    ///   every ordinary image and for every file whose count has not been learned, and
    ///   an ordinary single-page file — the common case, and the whole of a photo
    ///   folder — therefore pays exactly what it paid before.
    /// * **One page, and inside the same document.** `step_no_wrap` rather than
    ///   `step`, so the last page does not learn the *first page of the last file*;
    ///   and a hop that crosses a file boundary is dropped, because the question is
    ///   the next page of *this* document. Nothing is lost by that: the next file's
    ///   first page is reported by its own preview landing (`note_shapes`) or measured
    ///   when the cursor gets there.
    /// * **Cache-first, per page**, so an already-known next page costs one map
    ///   lookup.
    ///
    /// **Which of those bounds is load-bearing, measured rather than assumed:** the
    /// same-document one. Dropping the page-count bound is a **no-op mutation** — a
    /// one-page file's next page is necessarily in a *different* file, so the
    /// same-document bound catches it too, and all 28 tests in the spreads suite stay green
    /// with the page-count check deleted (run twice, including after the bounds test
    /// was reordered). The page-count bound is kept because it states the read-ahead's
    /// scope where it is decided and skips one in-memory hop, **not** because anything
    /// depends on it — so do not read it as protection that is not there. The
    /// same-document bound, by contrast, is caught by
    /// `the_next_page_learn_stops_at_the_document`.
    ///
    /// **The EXIF orientation tag is applied here, and skipping it is the one
    /// mistake in this feature that is silent, total, and lands on real
    /// libraries.** A phone scan is stored landscape and tagged to display
    /// portrait: stored 1600x900 with orientation 6, the *stored* pair's ratio is
    /// 1.78, above [`SPREAD_ASPECT`], so a cache that stored what [`page_dims`]
    /// reported would call every page in the volume a spread. Every page would
    /// take a whole turn to itself, nothing would ever pair, and nothing on screen
    /// would say why. That is why [`page_dims`] reports the *stored* size and this
    /// is a separate step rather than one folded-together function:
    /// `decode_page` reads the same pair and applies the same tag to produce
    /// `PreviewAndFull::orig`, so the two routes agree by construction rather than
    /// by convention. **Do not fold the tag into `page_dims` to "fix" this** — the
    /// swap is its own inverse, so applying it a second time is not a wrong value to
    /// be spotted but the identity, which puts the *stored* pair in the cache and
    /// reproduces the failure above exactly.
    ///
    /// The tag is read under exactly the condition `decode_page` applies it under —
    /// JPEG only — because the dimensions have to be the ones the *pixels* end up
    /// at. A WebP or HEIC carrying an orientation tag is displayed unrotated here
    /// (the `heic` crate applies the container transforms itself), so judging it by
    /// a rotated size would be a second, quieter version of the same bug.
    ///
    /// **Answers from the cache when it can**, for [`Self::learn_pages`]' reason
    /// and with more force: this runs on *every* navigation, and unlike the four
    /// bytes of a page count this is a bounded read plus a format sniff plus, for
    /// a JPEG, an APP1 seek. Re-reading per visit would put a file open on the
    /// critical path of every keypress. The cache check is **per page**, so pressing
    /// right through a document measures each page once and no more.
    ///
    /// Does nothing for a page whose dimensions cannot be read. An unmeasured page is
    /// [`Shape::Single`] and pairs optimistically, which is the state the whole
    /// pairing is built to survive, so there is no error to report and nothing to
    /// retry: the arrived preview's `orig` will fill this in via
    /// [`Self::note_shapes`].
    pub fn learn_shapes(&mut self, pos: PagePos) {
        self.learn_one_page(pos);
        if self.pages_in(pos.index) <= 1 {
            return;
        }
        let Some(next) = self.step_no_wrap(pos, 1) else {
            return;
        };
        if next.index != pos.index {
            return;
        }
        self.learn_one_page(next);
    }

    /// The measurement half of [`Self::learn_shapes`], for one page.
    ///
    /// Separate so the page being learned and the page learned *for* go through
    /// byte-identical code: two copies of "read the header, apply the tag, store" is
    /// exactly where a fix to one and not the other would live, and the whole reason
    /// the EXIF swap is load-bearing is that both copies must do it.
    fn learn_one_page(&mut self, pos: PagePos) {
        let Some(path) = self.files.get(pos.index).cloned() else {
            return;
        };
        let key = ImgKey { path: path.clone(), page: pos.page };
        if self.shapes.contains_key(&key) {
            return;
        }
        // Check the persisted cache first: a cache hit avoids re-opening the
        // file entirely, which is the whole point of the sidecar. Cache is
        // per-file (page 0), so multi-page documents still measure each page
        // on first arrival — page 0 comes from the cache, later pages from
        // their own header reads.
        if !self.shape_cache_loaded {
            self.shape_cache = load_shape_cache();
            self.shape_cache_loaded = true;
        }
        if pos.page == 0
            && let Ok(metadata) = std::fs::metadata(&path)
        {
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if let Some(cached) = self.shape_cache.get(&path_str(&path))
                && cached.mtime == mtime
                && cached.len == metadata.len()
            {
                self.insert_shapes(key, (cached.w, cached.h));
                return;
            }
        }
        let Some(((w, h), jpeg)) = page_dims_and_jpeg(&path, pos.page) else {
            return;
        };
        // 0 for anything but a JPEG, matching `decode_page`: the tag is only ever
        // applied there, and `exif_orientation` costs a seek out of the file.
        let tag = if jpeg { exif_orientation(&path) } else { 0 };
        self.insert_shapes(key, if orientation_swaps_dims(tag) { (h, w) } else { (w, h) });
        // Update the persisted cache for page 0: dims, orientation, capture
        // date, mtime and length. Multi-page pages are not cached here — their
        // dims vary per page and the in-memory `shapes` map already covers them.
        if pos.page == 0
            && let Ok(metadata) = std::fs::metadata(&path)
        {
            let mtime = metadata
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let date_taken = if jpeg { exif_date_taken(&path) } else { String::new() };
            self.shape_cache.insert(
                path_str(&path),
                FileShape { w, h, orientation: tag, date_taken, mtime, len: metadata.len() },
            );
            self.shape_cache_dirty = true;
            self.save_shape_cache();
        }
    }

    /// Record the size a page **displays** at, from a decode that already knows.
    ///
    /// A neighbour's preview reports its own dimensions on its way past, which makes
    /// this the cheapest source of the pairing's input there is, and it is what fills
    /// a page in before the cursor arrives at it.
    ///
    /// **Fill-only: this never replaces an entry that is already there.** `learn_shapes`
    /// measures the source header and that is the better number, so a decode's
    /// dimensions only fill a gap.
    ///
    /// The alternative — last writer wins — would let a decode that reported a
    /// *downscaled* size replace a measurement, and `learn_shapes` short-circuits on
    /// the entry existing, so the better number could never come back. **Refusing is
    /// not free, and the trade is worth stating:** an approximation that arrives
    /// first is kept and is never re-measured. What makes that acceptable is the next
    /// paragraph — route 1 agrees with `learn_shapes` by construction, so the common
    /// case has nothing to displace, and routes 2-4 are ratio-equivalent, so
    /// `shape_of` reads them correctly either way. The case this actually gives up
    /// is a route-4 fallback — a file whose header the decode could not sniff at all,
    /// where the preview size is kept instead of a later header measurement.
    ///
    /// **What the caller passes, precisely.** `dims` must already be
    /// display-oriented, and there are four routes into it. Only the first is a
    /// source measurement:
    ///
    /// 1. [`PreviewAndFull::orig`] as `decode_page` builds it for an ordinary
    ///    decode: `image_dims_bytes` plus the JPEG-only tag swap. A measurement, and
    ///    the same one [`Self::learn_shapes`] would compute from the same header — so
    ///    the two routes agree by construction rather than by convention. The
    ///    DCT-preview and embedded-thumbnail paths report this too: they change the
    ///    *pixels*, not the reported source size.
    /// 2. An **animation**: `orig` comes from `anim.frames[0]`, and
    ///    `collect_animation` has already downscaled that frame to
    ///    [`PREVIEW_TEXTURE_DIM`](crate::PREVIEW_TEXTURE_DIM) (8192). An animation
    ///    wider than that records a downscaled size.
    /// 3. A **page past the first of a TIFF**: `orig` is that page's *decoded*
    ///    dimensions, and `decode_tiff_page` has already downscaled it to the
    ///    request's target.
    /// 4. The **caller's fallback** for a file whose header could not be sniffed at
    ///    all, which is the size of the *decoded preview*.
    ///
    /// **Routes 2-4 are approximations, and fill-only is what makes them safe.** All
    /// the downscaling in them goes through `downscale_image`, which picks *one* scale
    /// factor and applies it to both axes, so the aspect ratio survives up to integer
    /// rounding — and the aspect ratio is the only thing
    /// [`shape_of`](crate::shape_of) reads. So an approximation that gets here first
    /// is still read correctly by `shape_of`, and one that arrives second is refused.
    /// The number in the cache is not always the source number; the *verdict* it
    /// produces is, which is all `dims` is asked for.
    ///
    /// Consequently a measurement taken from a decode that was already in flight
    /// carries that decode's bytes, so it inherits the preview cache's exposure to a
    /// result landing after the file was rewritten. Nothing here does anything about
    /// that: decodes are never abandoned, and the same exposure already exists for
    /// the texture those pixels become.
    pub fn note_shapes(&mut self, key: ImgKey, dims: (u32, u32)) {
        if self.shapes.contains_key(&key) {
            return;
        }
        self.shapes.insert(key, dims);
    }

    /// [`Self::note_shapes`] for a caller that has already established there is no
    /// entry — [`Self::learn_shapes`], which is the only one.
    fn insert_shapes(&mut self, key: ImgKey, dims: (u32, u32)) {
        self.shapes.insert(key, dims);
    }

    /// Forget every page of one file's remembered dimensions, so the next read
    /// measures the file's current contents.
    ///
    /// **Every page, not one.** The entries are keyed by [`ImgKey`], so a
    /// path-keyed `remove` would be a no-op for any page past the first — and a
    /// re-exported 200-page document would keep every one of its old shapes, with
    /// only page 0 ever re-measured. The same "all pages, not one" rule
    /// `invalidate_path` already applies to `previews`.
    ///
    /// **Must be called from `invalidate_path`,** beside `forget_page_count`. The
    /// file's *contents* changed, and only the caller knows that: a re-export can
    /// change a page's aspect from portrait to landscape, and the pairing is built
    /// to trust what it was told. Without this the shapes are frozen for the
    /// session and **every turn after that file stays mis-paired**, with no error
    /// and nothing on screen to say so.
    pub fn forget_shapes(&mut self, path: &Path) {
        self.shapes.retain(|key, _| key.path != path);
    }

    /// Forget one file's page count, and clamp the cursor if it was on that file.
    ///
    /// For a file whose *contents* changed. A 12-page document re-exported as one
    /// page has to stop offering 11 pages that do not exist, and the cursor has to
    /// come off page 12 rather than leaving `next` to walk off the end of a phantom
    /// document.
    pub fn forget_page_count(&mut self, path: &Path) {
        self.page_counts.remove(path);
        if self.current() == Some(path) {
            self.page = 0;
        }
    }

    /// Move to a file by index, starting at its first page.
    ///
    /// A *file* index, deliberately: a jump to a file has no page to go to, and
    /// resuming should not restore a page either, because the page count is
    /// learned on arrival and a remembered one could name a page that a rewritten
    /// file no longer has.
    pub fn goto_index(&mut self, index: usize) {
        if index < self.files.len() {
            self.index = index;
            self.page = 0;
        }
    }

    /// Move to a `(file, page)` position, as the turn walk produces one.
    ///
    /// The counterpart of [`Self::next`]/[`Self::prev`] for a position that is not
    /// one page away: manga mode's navigation hands the app the *anchor* of the next
    /// turn, and in a multi-page document that is a page in the middle of the file,
    /// which `next` can only reach by arriving there one page at a time.
    ///
    /// **A position is a whole answer, so this does not clamp the page** to
    /// `pages_in_current`. Every caller gets its position out of [`PageCtx::step`]
    /// or [`PageCtx::step_no_wrap`], which already respect the count, so a clamp
    /// here would be a second rule that silently disagrees with the walk's - and a
    /// disagreement here is a cursor somewhere the reader did not ask for.
    ///
    /// **Only a file index past the end of the list is refused**, and it changes
    /// nothing. That case cannot come out of the walk, which answers `None` past
    /// either end; it is here for a caller holding a stale position, because
    /// `next`/`prev` from an out-of-range index wrap to the front rather than
    /// refusing when `loop_back` is on - and this is the one of the three that does
    /// nothing either way.
    pub fn goto_page(&mut self, pos: PagePos) {
        if pos.index < self.files.len() {
            self.index = pos.index;
            self.page = pos.page;
        }
    }

    /// Rebuild [`Self::files`] from `all`, keeping only names matching `needle`.
    ///
    /// The match is a case-insensitive substring of the file name; an empty or
    /// whitespace-only needle means "no filter". The folder is scanned flat, so
    /// there is no directory component to match beyond the name.
    ///
    /// Before the substring fallback, a needle that parses as a rating query is
    /// decided from the sidecar instead of the name: `stars:N` and `stars:N+`
    /// (both: at least N), `flagged`, `unflagged`, or `tag:NAME`
    /// (case-insensitive substring of the tag list). A file with no sidecar
    /// reads as `stars:0` and not-flagged, so `stars:1+` excludes it rather
    /// than erroring; a damaged sidecar is likewise treated as unrated, so a
    /// broken sidecar can never hide its image. An unparseable `stars:` value
    /// (`stars:x`, `stars:`) is not a query and falls back to a name
    /// substring, which matches nothing unless a file is so named.
    ///
    /// Sidecars are read once per candidate per call. This runs once per filter
    /// change (never per frame), and only rating needles touch the disk at all,
    /// so there is no cross-call cache to go stale when a rating is edited
    /// while a filter is active.
    ///
    /// The current image is kept if it survived, otherwise the index falls back
    /// to the nearest surviving position (clamped, never out of range). Returns
    /// true if the current image changed, which is the caller's cue to re-goto.
    pub fn set_filtered(&mut self, all: &[PathBuf], needle: &str) -> bool {
        let current = self.current().map(|p| p.to_path_buf());
        let needle = needle.trim().to_lowercase();
        self.files = if needle.is_empty() {
            all.to_vec()
        } else if let Some(term) = parse_rating_term(&needle) {
            all.iter()
                .filter(|p| rating_term_matches(&term, &Ratings::read(p).unwrap_or_default()))
                .cloned()
                .collect()
        } else {
            all.iter()
                .filter(|p| file_name_of(p).to_lowercase().contains(&needle))
                .cloned()
                .collect()
        };
        self.index = current
            .as_ref()
            .and_then(|p| self.files.iter().position(|f| f == p))
            .unwrap_or_else(|| self.index.min(self.files.len().saturating_sub(1)));
        // The current file survived, so its page survives too and the user keeps
        // their place in a document they are part-way through. If it did not, the
        // index moved to a different file and the page must go with it.
        if self.current() != current.as_deref() {
            self.page = 0;
        }
        self.current() != current.as_deref()
    }

    /// Advance one image, or one page of a multi-page file.
    ///
    /// Within a document this moves by page; at the last page it moves to the
    /// first page of the next file. That is the whole point of making this a
    /// cursor: pressing right 199 times walks a 200-page scan and then carries on
    /// into the next folder entry, which is what every document viewer does and
    /// what nobody has to be told.
    ///
    /// Returns whether the view actually moved - `false` at the end with
    /// `loop_back` off, so the caller can skip the work that a real change would
    /// justify.
    ///
    /// Named `next` because it advances the gallery cursor to the next page/image,
    /// mirroring `Iterator::next` semantics deliberately — this is a gallery
    /// cursor, not an iterator, so the name is appropriate. `Gallery` is not an
    /// iterator (it borrows mutably and has state beyond a position), so
    /// implementing `Iterator` would be wrong.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> bool {
        let Some(to) = self.pos_next(self.index, self.page) else {
            return false;
        };
        self.index = to.index;
        self.page = to.page;
        true
    }

    /// The position one page on from `(index, page)`, or `None` past the end when
    /// `loop_back` is off.
    ///
    /// [`Self::next`]'s arithmetic, written against a position rather than the
    /// cursor so `PageCtx::step` can ask about a page it is not on **and get the
    /// same answer**. There is one implementation of these rules on purpose: `prev`
    /// from page 0 landing on the *last* page of the previous file is a
    /// deliberate rule that exists because two presses per file feels broken, and a
    /// second copy of it would be a second place for it to be wrong - silently, and
    /// only in manga mode.
    ///
    /// It is a separate function rather than a clone-and-call because
    /// `PageCtx::step` has to answer about a position the cursor is **not** on, and
    /// the state `next`/`prev` read beyond `(index, page)` — `files` and
    /// `page_counts` — is already available on `&self`. A clone would be a way of
    /// manufacturing a `&mut self` cursor to answer a question that needs neither
    /// the cursor nor the mutation.
    fn pos_next(&self, index: usize, page: u32) -> Option<PagePos> {
        if self.files.is_empty() {
            return None;
        }
        if page + 1 < self.pages_in(index) {
            return Some(PagePos { index, page: page + 1 });
        }
        // Last page (or an ordinary file): on to the next one, at its first page.
        match index.checked_add(1) {
            Some(i) if i < self.files.len() => Some(PagePos { index: i, page: 0 }),
            _ if self.loop_back => Some(PagePos { index: 0, page: 0 }),
            _ => None,
        }
    }

    /// Go back one image, or one page of a multi-page file.
    ///
    /// Symmetric with [`Self::next`]: from page 0 it goes to the *last* page of
    /// the previous file, not its first. Anything else makes stepping backwards
    /// through a folder need two presses per file, which is a small thing that
    /// feels broken.
    pub fn prev(&mut self) -> bool {
        let Some(to) = self.pos_prev(self.index, self.page) else {
            return false;
        };
        self.index = to.index;
        self.page = to.page;
        true
    }

    /// The position one page back from `(index, page)`, or `None` before the front
    /// when `loop_back` is off. [`Self::pos_next`]'s counterpart, for
    /// [`Self::prev`]'s reasons.
    fn pos_prev(&self, index: usize, page: u32) -> Option<PagePos> {
        if self.files.is_empty() {
            return None;
        }
        if page > 0 {
            return Some(PagePos { index, page: page - 1 });
        }
        // The new file's page count is only known if it was visited, which it was,
        // since the user is stepping back through where they came. `pages_in`
        // falls back to 1 for anything unknown, so this is always in range.
        match index.checked_sub(1) {
            Some(i) => Some(PagePos { index: i, page: self.pages_in(i).saturating_sub(1) }),
            None if self.loop_back => {
                let last = self.files.len() - 1;
                Some(PagePos { index: last, page: self.pages_in(last).saturating_sub(1) })
            }
            None => None,
        }
    }

    /// How many pages the file at `index` has, as far as this gallery knows.
    /// 1 until [`Self::learn_pages`] has been asked about it.
    ///
    /// [`Self::pages_in_current`] is the same question about the *cursor's*
    /// file, which is not the same thing: the pairing asks about pages the
    /// cursor is not on, and answering with the current file's page count would
    /// make a three-page document's last page look like an ordinary file's.
    pub fn pages_in(&self, index: usize) -> u32 {
        self.files.get(index).and_then(|p| self.page_counts.get(p)).copied().unwrap_or(1).max(1)
    }

    /// The page `by` pages from `pos`, without honouring `loop_back`.
    ///
    /// This is [`Self::next`] and [`Self::prev`]'s page arithmetic with the
    /// wrap taken out, which is what [`PageCtx::step_no_wrap`] needs so a
    /// gallery's last turn cannot straddle the wrap. `None` past either end
    /// whatever `loop_back` says.
    ///
    /// Written against a position rather than `&mut self` so the pairing can
    /// ask about a page it is not currently on; the caller in Task 2's
    /// `PageCtx for Gallery` is a one-liner over this.
    ///
    /// **`by` may be more than one page**, because `prev_anchor`'s walk steps by
    /// a whole turn, which is 1 or 2. Handled as a loop rather than a `match` on
    /// 1 and -1: a version that only understood single steps returns `None` for
    /// every turn of two, which is the same shape of bug as using
    /// `pages_in_current()` for another file's page count - correct-looking, and
    /// invisible until a real gallery is walked.
    ///
    /// Every caller in this crate passes 1, 2 or -1. A `by` of `isize::MIN`
    /// would loop 2^63 times, so it is not defended against: no caller can
    /// produce one, and the alternative - a saturating bound - would be a branch
    /// that cannot change an answer for any reachable input.
    pub fn step_no_wrap(&self, pos: PagePos, by: isize) -> Option<PagePos> {
        if pos.index >= self.files.len() {
            return None;
        }
        let mut cur = pos;
        for _ in 0..by.unsigned_abs() {
            cur = match by.signum() {
                1 if cur.page + 1 < self.pages_in(cur.index) => {
                    PagePos { index: cur.index, page: cur.page + 1 }
                }
                // Last page (or an ordinary file): on to the next one, at its
                // first page. `None` at the last file rather than wrapping.
                1 => cur
                    .index
                    .checked_add(1)
                    .filter(|&i| i < self.files.len())
                    .map(|index| PagePos { index, page: 0 })?,
                -1 if cur.page > 0 => PagePos { index: cur.index, page: cur.page - 1 },
                // From page 0 of a file: the *last* page of the previous one, not
                // its first, matching `prev`. `None` at the first file rather
                // than wrapping.
                -1 => cur
                    .index
                    .checked_sub(1)
                    .map(|index| PagePos { index, page: self.pages_in(index) - 1 })?,
                _ => return None,
            };
        }
        Some(cur)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Load all images from the folder, preserving the current image's position.
    /// Sorts with the natural (Explorer-like) name order.
    pub fn load_all_from_folder(&mut self, folder: &Path) {
        self.load_all_from(&scan_folder(folder));
    }

    /// Load all images from the folder tree, preserving the current image's
    /// position. Same contract as [`Self::load_all_from_folder`], over the
    /// recursive walk instead of one level.
    pub fn load_all_from_folder_recursive(&mut self, folder: &Path) {
        self.load_all_from(&scan_folder_recursive(folder));
    }

    /// Install a scanned list, keeping the current image selected when it is
    /// still in it. Both loaders above are this with different scans, so a
    /// position that survives in one survives in the other.
    fn load_all_from(&mut self, new_files: &[PathBuf]) {
        // Remember current image path
        let current_path = match self.current() {
            Some(p) => p.to_path_buf(),
            None => PathBuf::new(),
        };

        // Find position of current image in sorted list
        let mut new_index = 0;
        if !current_path.as_os_str().is_empty() && !new_files.is_empty() {
            for (i, file) in new_files.iter().enumerate() {
                if file == &current_path {
                    new_index = i;
                    break;
                }
            }
        }

        self.files = new_files.to_vec();
        self.index = new_index;
        // The list these counts described is gone.
        self.forget_pages();
    }
}

/// Should this keypress step out of the open archive?
///
/// **Both edges, and `can_leave` before either.** The forward edge is the one
/// that was missing, and its absence is what trapped the reader: `→` at an
/// archive's last page had nowhere to go but back to its own first page, so a
/// folder of archives behaved like a folder of one.
///
/// `can_leave` is the parent's own answer to "is there anywhere to go", and it
/// is asked *before* leaving. An archive with `loop_back` off whose entry is the
/// folder's last must not be left by a press that has nowhere to land —
/// otherwise the reader is dropped into the parent folder and the press looks
/// broken, which is a worse answer than staying put.
///
/// Pure, and pure for the usual reason: the key handling that applies it is in
/// the binary, unreachable from a test, so the rule it exists to apply is the
/// part worth being able to test.
pub fn leave_archive_at(forward: bool, at_front: bool, at_back: bool, can_leave: bool) -> bool {
    can_leave && if forward { at_back } else { at_front }
}

/// The real [`PageCtx`]: a gallery, its page counts, and the shape cache.
///
/// Each method answers from something that already exists — the position
/// arithmetic `next`/`prev` and `step_no_wrap` share, or the two caches — which
/// is the point of the trait having no default bodies: a default would be a guess
/// about a gallery this section cannot see, and the guess would be silently wrong
/// for archives and for a `page_counts` that has not been learned yet.
///
/// Two traps this impl walks straight into, both answered *here* rather than left
/// for the next reader:
///
/// * **`pages_in`, never `pages_in_current`.** The latter is about `self.index`,
///   so it is right for the cursor and wrong for every other position - and the
///   pairing spends its whole life asking about positions it is not on.
/// * **`flat` reads `page_counts` directly, not through `pages_in`.** The other
///   way round: `pages_in` answers 1 for a file it knows nothing about, which is
///   the right default for "how many pages does this file have" and a lie for
///   "how many pages come before this one".
impl PageCtx for Gallery {
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos> {
        if from.index >= self.files.len() {
            // `pos_next`/`pos_prev` would answer `Some(0, 0)` here rather than
            // `None`: an index past the end finds no next file and falls into the
            // `loop_back` arm. A position that does not exist has nothing to step
            // from, and wrapping to the front would say a page exists that does not.
            return None;
        }
        // A loop rather than a `match` on 1 and -1, for the reason
        // `Gallery::step_no_wrap` gives: `prev_anchor`'s walk steps by a whole
        // turn, and a turn is 1 or 2. `by` of 0 runs no iterations, so it answers
        // `Some(from)`, which is also what `step_no_wrap` does.
        //
        // `by.unsigned_abs()` is undefended here for the reason it is undefended
        // there: no caller can produce `isize::MIN` without writing it out by hand,
        // and a saturating bound would be a branch that cannot change an output for
        // any reachable input. **Unlike `step_no_wrap`'s loop, this one could spin**
        // — `pos_next` and `pos_prev` both wrap when `loop_back` is on, so a `by` of
        // `isize::MIN` would not return. Nothing reaches that: the only caller of
        // `PageCtx::step` is `next_anchor`, which passes `turn_len`, and `turn_len`
        // returns 1 or 2 by construction.
        let mut cur = from;
        for _ in 0..by.unsigned_abs() {
            cur = if by < 0 {
                self.pos_prev(cur.index, cur.page)?
            } else {
                self.pos_next(cur.index, cur.page)?
            };
        }
        Some(cur)
    }

    fn step_no_wrap(&self, from: PagePos, by: isize) -> Option<PagePos> {
        Gallery::step_no_wrap(self, from, by)
    }

    fn key(&self, pos: PagePos) -> Option<ImgKey> {
        self.files.get(pos.index).map(|path| ImgKey { path: path.clone(), page: pos.page })
    }

    /// The cached display dimensions, and **not** a re-read.
    ///
    /// A lookup and nothing else, because this is on the pairing's hot path:
    /// `prev_anchor` calls `pairable` once per page of its O(n) walk, and each of
    /// those makes four `dims` calls. Reading a file here would turn a query into
    /// disk I/O in the middle of a keypress.
    ///
    /// **Keyed by the position's own [`ImgKey`], not by its file** — so a page whose
    /// dimensions have not been measured yet is `None` while its neighbour is
    /// `Some`, and that is the whole point: a document with a landscape spread among
    /// portrait pages has to be able to say so.
    ///
    /// `None` is a state the pairing is built to survive - an unmeasured page is
    /// `Single` and pairs optimistically - so it is not an error here, and there is
    /// nothing to fall back to.
    fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
        self.key(pos).and_then(|key| self.shapes.get(&key)).copied()
    }

    /// Always `None` today. The user's override is a separate feature and it is
    /// asked for separately rather than folded into `dims`, because it beats the
    /// detector outright - including when the detector is confidently wrong, which
    /// is the case the override exists for.
    fn override_shape(&self, _pos: PagePos) -> Option<Shape> {
        None
    }

    /// The page's ordinal in the whole gallery, and `None` when a preceding file's
    /// page count is unknown.
    ///
    /// `page_counts.get(..)?` rather than [`Gallery::pages_in`]: that answers 1 for
    /// an unknown file, and substituting its default would turn "I do not know how
    /// many pages this document has" into a confidently wrong ordinal. The walk only
    /// ever asks about the leading pages, where the cursor's having arrived
    /// guarantees the counts are known - so this is an optimisation in practice and
    /// a lie avoided in principle.
    fn flat(&self, pos: PagePos) -> Option<usize> {
        if pos.index >= self.files.len() {
            return None;
        }
        let mut n = pos.page as usize;
        for path in &self.files[..pos.index] {
            n += *self.page_counts.get(path)? as usize;
        }
        Some(n)
    }
}

/// Where the shape sidecar lives: `%LOCALAPPDATA%\image-viewer\shapes.json`.
/// Same rules as `spreads_root`: local, not roaming; beside the cache, not
/// the config. `None` means "do not persist".
#[allow(clippy::manual_map)]
pub(crate) fn shape_cache_root() -> Option<PathBuf> {
    // `if let / else if let` is more readable than `map`/`or_else` here: each arm has a different fallback path.
    if let Some(dir) = portable_dir() {
        return Some(dir.join("shapes.json"));
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        Some(PathBuf::from(local).join("image-viewer").join("shapes.json"))
    } else if let Some(home) = std::env::var_os("HOME") {
        Some(PathBuf::from(home).join(".cache").join("image-viewer").join("shapes.json"))
    } else {
        None
    }
}

/// The sidecar's `.tmp.<pid>` name — same per-process atomic-write guard as
/// `spreads_tmp`.
pub(crate) fn shape_cache_tmp(store: &Path, pid: u32) -> PathBuf {
    let mut name = store.as_os_str().to_os_string();
    name.push(format!(".tmp.{pid}"));
    PathBuf::from(name)
}

/// Load the persisted shape cache, or an empty map if the file is absent or
/// unreadable. An unreadable store is treated as empty and replaced on save,
/// the same rule `load_spread_overrides` uses.
pub(crate) fn load_shape_cache() -> std::collections::HashMap<String, FileShape> {
    let Some(root) = shape_cache_root() else {
        return Default::default();
    };
    let Ok(data) = std::fs::read(&root) else {
        return Default::default();
    };
    serde_json::from_slice(&data).unwrap_or_default()
}

/// Save the shape cache atomically (tmp + rename). A store that cannot be
/// parsed is treated as empty and replaced on the next save; a save that
/// cannot be written is silently dropped — the cache is a performance hint,
/// not critical data.
pub(crate) fn save_shape_cache(cache: &std::collections::HashMap<String, FileShape>) {
    let Some(root) = shape_cache_root() else {
        return;
    };
    let Ok(json) = serde_json::to_string_pretty(cache) else {
        return;
    };
    let tmp = shape_cache_tmp(&root, std::process::id());
    if std::fs::write(&tmp, &json).is_err() {
        return;
    }
    let _ = std::fs::rename(&tmp, &root);
}

/// A file's path as a lossy String, the cache's key type.
fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

impl Gallery {
    /// Persist the shape cache if anything changed since the last save.
    ///
    /// Called from `learn_one_page` after a cache insert. Saves every time
    /// because the cache is small (a few KB per 1000 files) and writes are
    /// atomic — simpler than a debounce timer that could lose data on exit.
    pub(crate) fn save_shape_cache(&mut self) {
        if !self.shape_cache_dirty {
            return;
        }
        save_shape_cache(&self.shape_cache);
        self.shape_cache_dirty = false;
    }
}
