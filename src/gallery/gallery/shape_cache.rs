//! The shape sidecar and the per-page dimension cache.
//!
//! Split out of `gallery.rs` when it passed the 1000-line target. These are the
//! methods that *measure* a page's display size - the in-memory `shapes` map and
//! the persisted `shapes.json` sidecar behind it - and nothing else. They live in
//! a child module of `gallery` so they can still reach the private fields on
//! `Gallery` that hold that state. See `AGENTS.md` and `tests/source_size.rs`.

use crate::*;

impl Gallery {
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
