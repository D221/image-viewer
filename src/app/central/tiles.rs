//! Full-resolution tile cutting and eviction for the central render pass.
//!
//! Extracted out of `MyApp::central` when `src/app/central.rs` passed the
//! 1000-line target. Runs over every page of a turn, not just the cursor.

use crate::*;

impl MyApp {
    /// Cut visible tiles for every page of the turn and bound the tile cache.
    ///
    /// Returns `(pending, sharpened)`: `pending` when the per-frame tile budget
    /// ran out (a repaint is requested), `sharpened` when at least one tile was
    /// cut this frame (viewport tiles are marked ready).
    // The render pass threads this many values through; a params struct would
    // be a larger refactor than the function.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn central_tiles(
        &mut self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        panel: egui::Rect,
        scale: f32,
        ppp: f32,
        xform: Xform,
        rect: egui::Rect,
        pages: &[(ImgKey, (u32, u32), Xform)],
        rects: &[egui::Rect],
        page_rects: &[egui::Rect],
        footprint: egui::Rect,
        uv: egui::Rect,
        tex_w: f32,
        img_px: egui::Vec2,
        shown: &ImgKey,
    ) -> (bool, bool) {
        // --- full-resolution tiles ---
        //
        // **Over every page of the turn, not the cursor's alone.** This loop used to
        // be filtered to `self.full` matching `shown.path` - the cursor - and that
        // threw away a buffer which was *already in memory*: `sync_shown` adopts the
        // cursor's full image by `Arc::clone` rather than moving it, and the
        // single-pass decode hands every cached preview one, so a turn's second half
        // has its own full-resolution buffer sitting on `previews[partner].full`. The
        // expensive decode had already happened; nothing was looking at the result.
        //
        // For a source over `FAST_FULL_MAX_PIXELS` there is no such buffer - the
        // preview is a DCT-scaled decode - so the plan asks for one
        // (`Plan::partner_needs_full`) and `Msg::Full` keeps it on that page's own
        // preview. Those are ordinary manga scans, so this is the common case and not
        // a corner one. Until that decode lands the page is simply not sharpened, and
        // its preview is on screen underneath.
        //
        // **`MAX_TILES` is still the single bound**, and it has to be counted across
        // pages: cuts are on demand (`TILES_PER_FRAME`, and only over what is
        // visible), so the steady state is "a zoomed spread" - 64 tiles between two
        // pages, not 64 each - and the eviction pass below sums every page's map
        // before dropping anything. What a page is *not* is a page that keeps its
        // tiles after it leaves the screen, which is why the count is global rather
        // than per page: one map per cached page, each with its own 64, is 16 x 256 MB
        // and the ceiling would have been raised by accident.
        let screen_density = scale * ppp; // physical px per source px
        self.frame += 1;
        let frame = self.frame;
        // What each page of the turn can be sharpened from, gathered **before** any
        // mutable borrow and owned: an `Arc` clone (a refcount) plus that page's own
        // density and its two rectangles. `None` is a page with no full buffer.
        //
        // **Page 0 is the cursor, and `pages` is cursor-first**, so the entry the old
        // code computed for it is the one at index 0 - `pv.px.0 / ow` is the same
        // `tex_w / img_px.x` (`px` *is* the texture's size, and `ow` is `orig`), and
        // the single-image path below is spelled out separately so it does not depend
        // on that being true.
        // The cursor page's source, whether or not a turn is on screen: the adopted
        // `FullImage`, at the density the single-image path has always used.
        //
        // **Page 0 of a turn reads this too**, and not `previews[shown].full` which
        // holds the same buffer whenever the single-pass decode produced one. Two
        // reasons, and the second is the load-bearing one: a source over
        // `FAST_FULL_MAX_PIXELS` never gets a `pv.full` *at all* - its full decode
        // lands in `self.full` - so the preview slot is empty for exactly the pages
        // that need tiles most; and the cursor page's tiles are kept in
        // `FullImage.tiles`, which `sync_shown` clears whenever the cursor moves.
        // Two maps for one page would be two budgets' worth of GPU memory and a
        // re-cut on every navigation.
        let cursor_page = || {
            self.full.as_ref().and_then(|f| {
                // **The key, not the path.** This closure hands `f.data` — one page's
                // pixels — to `make_tile`, and the rect and density beside it are
                // `shown`'s. With the page missing, page 0's buffer satisfied the check
                // on every page of a document and the loop cut **page 0's tiles** with
                // page N's `uv`: page 0 drawn over page N, no error and no counter.
                (f.is(shown)).then(|| TilePage {
                    data: Arc::clone(&f.data),
                    // texture px per source px
                    preview_density: tex_w / img_px.x,
                    paint: if pages.is_empty() { rect } else { page_rects[0] },
                    slot: if pages.is_empty() { footprint } else { rects[0] },
                    xform,
                    // `img_px` is the source's own size (`orig`), whatever the
                    // buffer decoded to: for a DCT-scaled buffer this is where
                    // the pitch-up ratio comes from.
                    src: img_px,
                })
            })
        };
        let tile_pages: Vec<Option<TilePage>> = if pages.is_empty() {
            // Manga mode off. `self.full` and `rect`/`footprint` are the whole of
            // what this has ever been given, and they are spelled that way on purpose.
            Vec::from_iter([cursor_page()])
        } else {
            pages
                .iter()
                .enumerate()
                .map(|(i, (key, (ow, oh), xf))| {
                    if i == 0 {
                        return cursor_page();
                    }
                    let pv = self.previews.get(key)?;
                    let data = Arc::clone(pv.full.as_ref()?);
                    Some(TilePage {
                        data,
                        // **This page's** texture against **this page's** source. The
                        // cursor's numbers on the partner's would decide whether to cut
                        // tiles from a buffer that is a different size from the one the
                        // decision was made about.
                        preview_density: pv.px.0 as f32 / (*ow).max(1) as f32,
                        // The picture in unrotated image space - the tile grid is in
                        // this page's own source pixels - and the same rectangle after
                        // `xform`, which is what can be on screen.
                        paint: page_rects[i],
                        slot: rects[i],
                        xform: *xf,
                        // `ow`/`oh` are this page's source size, as `img_px` is the
                        // cursor's: the ratio against the buffer is per page.
                        src: egui::Vec2::new(*ow as f32, *oh as f32),
                    })
                })
                .collect()
        };
        // The map each page keeps its tiles in, **moved out** for the length of the
        // loop and put back below: the cursor page's on the adopted `FullImage`
        // (rebuilt, and so cleared, whenever the cursor moves - the rule that has
        // always bounded it) and every other page's on its own preview.
        //
        // Moved rather than borrowed because two pages' worth have to be held **at
        // once**, and the borrow checker will not lend `self.full` and
        // `self.previews` mutably inside one loop even though they are different
        // fields. `mem::take` leaves an empty map with no allocation, so this is a
        // pointer move rather than a copy of anything.
        enum TileMapOwner {
            Full,
            Page(ImgKey),
        }
        type TileMap = (HashMap<(u32, u32), Tile>, TileMapOwner);
        let mut maps: Vec<TileMap> = Vec::with_capacity(tile_pages.len());
        for (i, tp) in tile_pages.iter().enumerate() {
            if tp.is_none() {
                continue;
            }
            if i == 0 {
                if let Some(f) = self.full.as_mut().filter(|f| f.is(shown)) {
                    maps.push((std::mem::take(&mut f.tiles), TileMapOwner::Full));
                }
            } else if let Some(pv) = self.previews.get_mut(&pages[i].0) {
                let key = pages[i].0.clone();
                maps.push((std::mem::take(&mut pv.tiles), TileMapOwner::Page(key)));
            }
        }
        let mut budget = TILES_PER_FRAME;
        let mut pending = false;
        let mut sharpened = false;
        // Decided once per frame, not per tile: every tile this frame cuts
        // should carry the same sampling, and reading the config once keeps
        // that true even if a settings arm ran between two `make_tile` calls.
        let tile_options = content_texture_options(self.config.pixel_art);
        for (tp, (tiles, _owner)) in tile_pages.iter().zip(maps.iter_mut()) {
            let Some(tp) = tp else { continue };
            let preview_density = tp.preview_density;
            let (paint, slot, page_xform) = (tp.paint, tp.slot, tp.xform);
            if screen_density > preview_density {
                let (fw, fh) = (tp.data.width, tp.data.height);
                let vis = panel.intersect(slot);
                if vis.width() > 0.0 && vis.height() > 0.0 {
                    sharpened = true;
                    // Source pixels per buffer pixel, per axis: 1.0 when the
                    // buffer is the full decode, 2.0 when it is a half-res DCT
                    // decode of a source past `MAX_TEXTURE_DIM`. The range below
                    // is named in *buffer* pixels and the rect is pitched back
                    // up — see `tile_axis_range` / `tile_draw_rect`, whose tests
                    // pin both halves against the ghost this fixes.
                    let kx = tp.src.x / fw.max(1) as f32;
                    let ky = tp.src.y / fh.max(1) as f32;
                    let (x0, x1) = image_viewer::tile_axis_range(
                        vis.min.x,
                        vis.max.x,
                        paint.min.x,
                        scale,
                        kx,
                        fw,
                    );
                    let (y0, y1) = image_viewer::tile_axis_range(
                        vis.min.y,
                        vis.max.y,
                        paint.min.y,
                        scale,
                        ky,
                        fh,
                    );
                    if x1 > x0 && y1 > y0 {
                        for ty in (y0 / TILE)..=((y1 - 1) / TILE) {
                            for tx in (x0 / TILE)..=((x1 - 1) / TILE) {
                                if let std::collections::hash_map::Entry::Vacant(e) =
                                    tiles.entry((tx, ty))
                                {
                                    if budget == 0 {
                                        pending = true;
                                        continue;
                                    }
                                    budget -= 1;
                                    let tex = make_tile(ctx, &tp.data, tx, ty, tile_options);
                                    perf::count(perf::C_TILES_CREATED, 1);
                                    e.insert(Tile { tex, last_drawn: frame });
                                }
                                let tile = tiles.get_mut(&(tx, ty)).unwrap();
                                tile.last_drawn = frame;
                                let (px, py) = (tx * TILE, ty * TILE);
                                let (tw, th) = (TILE.min(fw - px), TILE.min(fh - py));
                                let q = image_viewer::tile_draw_rect(
                                    (paint.min.x, paint.min.y),
                                    px,
                                    py,
                                    tw,
                                    th,
                                    scale,
                                    kx,
                                    ky,
                                );
                                let r = Rect::from_min_size(
                                    Pos2::new(q[0], q[1]),
                                    Vec2::new(q[2], q[3]),
                                );
                                paint_tex(painter, tile.tex.id(), r, uv, page_xform);
                            }
                        }
                    }
                }
            } else if screen_density < preview_density * 0.5 {
                tiles.clear(); // zoomed well out: release tile textures
            }
        }
        // Put every page's tiles back where they came from, **before** the bound is
        // counted - the count is over `self`, not over the locals above.
        //
        // A map whose owner has gone is dropped rather than put back anywhere else.
        // `self.full` cannot be dropped between here and there (nothing in the loop
        // touches it) and `previews` cannot lose an entry to `evict_previews` in the
        // same breath, so this is belt-and-braces: a tile texture with no owner is
        // released, which is the safe direction to be wrong in.
        for (tiles, owner) in maps {
            match owner {
                TileMapOwner::Full => {
                    if let Some(f) = self.full.as_mut() {
                        f.tiles = tiles;
                    }
                }
                TileMapOwner::Page(key) => {
                    if let Some(pv) = self.previews.get_mut(&key) {
                        pv.tiles = tiles;
                    }
                }
            }
        }
        // Bound GPU memory: drop the least recently drawn tiles, **counting every
        // page's map**. One page's map used to be the whole of it, and it is the
        // whole of it in the single-image case still - so with manga mode off this is
        // the rule that was always here, with an empty sum added.
        //
        // Counted over the whole cache rather than the pages on screen, because a
        // page that leaves the screen keeps its tiles until it is evicted: counting
        // only the visible pages would let each new turn start a fresh budget and the
        // total would climb with every turn the reader flipped through.
        let held: usize = self.full.as_ref().map_or(0, |f| f.tiles.len())
            + self.previews.values().map(|pv| pv.tiles.len()).sum::<usize>();
        if held > MAX_TILES {
            enum Owner {
                Full,
                Page(ImgKey),
            }
            let mut old: Vec<(Owner, (u32, u32), u64)> = Vec::new();
            if let Some(f) = self.full.as_ref() {
                old.extend(
                    f.tiles
                        .iter()
                        .filter(|(_, t)| t.last_drawn < frame)
                        .map(|(k, t)| (Owner::Full, *k, t.last_drawn)),
                );
            }
            for (key, pv) in self.previews.iter() {
                old.extend(
                    pv.tiles
                        .iter()
                        .filter(|(_, t)| t.last_drawn < frame)
                        .map(|(k, t)| (Owner::Page(key.clone()), *k, t.last_drawn)),
                );
            }
            old.sort_by_key(|(_, _, last)| *last);
            for (owner, k, _) in old.into_iter().take(held - MAX_TILES) {
                match owner {
                    Owner::Full => {
                        if let Some(f) = self.full.as_mut() {
                            f.tiles.remove(&k);
                        }
                    }
                    Owner::Page(key) => {
                        if let Some(pv) = self.previews.get_mut(&key) {
                            pv.tiles.remove(&k);
                        }
                    }
                }
            }
        }
        // The two outcomes, once every borrow of `self` is done: `self.lat` cannot be
        // touched while `self.previews` is lent out, which is why they are flags.
        (pending, sharpened)
    }
}
