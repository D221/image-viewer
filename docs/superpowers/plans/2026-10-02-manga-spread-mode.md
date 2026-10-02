# Manga Mode Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a manga mode to the image viewer: images shown as turns of one or two pages, opening right-to-left by default, with a single image that is itself a two-page scan taking a whole turn and re-pairing everything after it.

**Architecture:** The pairing is a pure function over a flat page cursor in `src/lib.rs`, built on four small functions (`shape_of`, `pairable`, `lone`, `turn_len`) and two walk steps (`next_anchor`, `prev_anchor`). The invariant that makes it cheap is that the cursor is *always a turn anchor*, so "is this page alone?" is only ever asked at an anchor, where it is one local test. A new `PageCtx` trait supplies the four facts the walk needs (step, key, dims, override), implemented for real by `Gallery` and by a test double. Painting changes minimally: `shown` stays the **cursor** page so every view-shaped code path is untouched, and a new `shown_partner` field is added that only the paint loop reads.

**Tech Stack:** Rust, egui/eframe 0.36 on wgpu, `image` 0.25, `turbojpeg`, `nom_exif`, serde/serde_json, proptest (dev).

**Spec:** `docs/superpowers/specs/2026-10-02-manga-spread-mode-design.md` — read it alongside this plan; the plan argues from it and the two are not interchangeable.

## Global Constraints

- `src/lib.rs` has **no egui dependency on purpose** (`src/lib.rs:1803-1804`). All new geometry in `lib.rs` is plain floats; `egui::Rect` is constructed in `main.rs`.
- `SPREAD_ASPECT: f32 = 1.2`, `PAIR_ASPECT_TOLERANCE: f32 = 0.1`, `LEAD_LONE_DEFAULT: u32 = 1`, `LEAD_LONE_MAX: u32 = 4`. These exact values, from the spec.
- `manga_rtl` defaults to **true**. `Config` fields need `#[serde(default)]` or `manga_mode`, and `#[serde(default = "default_true")]` for `manga_rtl`, or an old config fails to load. **Never add `deny_unknown_fields`.**
- Overrides live in `%LOCALAPPDATA%\image-viewer\spreads.json`, never `%APPDATA%` and never beside the config. Never persist a temp path: an archive's overrides key on the **original** archive path.
- Zero gutter. Top-aligned. One shared scale. A lone page keeps its own half with the seam on the viewport centre.
- Full-resolution tiles, animation, crop, EXIF, `Del`, `Ctrl+S`, `Ctrl+C`, turn/mirror and wallpaper are **cursor page only**.
- The loader must never cancel or abandon a decode, and `schedule()` is the only thing that calls `loader.replace`.
- Every new bare letter needs a line in `tests/keybindings.rs`'s `BARE_LETTERS`, and adding one is a claim that the action cannot lose data.
- Tests build their own fixtures in a `tempfile::tempdir()`. No test may read `test-images/`.
- A test that passes on the broken code is decoration: each guard below is written to be verified failing with the guard removed, and that verification is part of the step.

## Review Focus

Five input classes the spec implies but does not spell out, most likely to bite first. Each is pinned to a test in the task that owns the code.

1. **A folder of ordinary landscape photographs with manga mode on.** A 3:2 photo has an aspect ratio of 1.5, above `SPREAD_ASPECT`, so *every page takes a whole turn* and nothing ever pairs. That is arguably right (a landscape photo cannot share a spread with anything) but it reads as "manga mode is broken", so it must render sanely at full width. → Task 5.
2. **A single landscape page among portrait pages** — a splash page or an insert. Must take its own turn, never be squashed into a half. This is Paperback #251 and Aidoku #939. → Task 1.
3. **A multi-page TIFF in manga mode.** A turn straddles a page boundary, and `Gallery::prev` from page 0 lands on the *last page of the previous file*, so a turn can also straddle a file boundary. → Task 1.
4. **An archive, re-extracted.** The `%TEMP%` directory changes when the archive does; overrides must survive that and must never be keyed on it. → Task 3.
5. **An empty or single-image gallery in manga mode, and `loop_back` off at the ends.** No division by zero in `spread_fit`, no wrap past the last anchor, no phantom partner. → Tasks 1 and 5.

---

### Task 1: The pairing core

**Files:**
- Modify: `src/lib.rs` (new section, placed immediately before the `Gallery` impl at `src/lib.rs:5868`)
- Test: `tests/spreads.rs` (new)

**Interfaces:**
- Consumes: nothing. This is the bottom of the dependency order.
- Produces — everything later tasks are named against:
  ```rust
  pub type Rect4 = [f32; 4];                    // x, y, width, height
  pub const SPREAD_ASPECT: f32 = 1.2;
  pub const PAIR_ASPECT_TOLERANCE: f32 = 0.1;
  pub const LEAD_LONE_DEFAULT: u32 = 1;
  pub const LEAD_LONE_MAX: u32 = 4;

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Shape { Single, Double }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct PagePos { pub index: usize, pub page: u32 }

  pub trait PageCtx {
      fn step(&self, from: PagePos, by: isize) -> Option<PagePos>;
      fn key(&self, pos: PagePos) -> Option<ImgKey>;
      fn dims(&self, pos: PagePos) -> Option<(u32, u32)>;
      fn override_shape(&self, pos: PagePos) -> Option<Shape>;
      fn flat(&self, pos: PagePos) -> Option<usize>;
  }

  pub fn shape_of(pos: PagePos, ctx: &dyn PageCtx) -> Shape;
  pub fn pairable(a: PagePos, b: PagePos, ctx: &dyn PageCtx) -> bool;
  pub fn lone(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> bool;
  pub fn turn_len(anchor: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> usize;
  pub fn next_anchor(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos>;
  pub fn prev_anchor(a: PagePos, lead_lone: u32, ctx: &dyn PageCtx) -> Option<PagePos>;
  ```
  `Turn` is deliberately **not** a type here: a turn is `[anchor] + maybe [anchor+1]`, which every caller already knows how to express, and a struct would only add a conversion.

- [ ] **Step 1: Write the test double and the trace test**

  `tests/spreads.rs`, at the top:

  ```rust
  use image_viewer::{lone, next_anchor, prev_anchor, turn_len, PageCtx, PagePos, Shape};

  /// A gallery of documents, each a list of page shapes. One entry per file, so a
  /// multi-page TIFF is a document with more than one page - which is what the
  /// straddle tests need.
  struct TestGallery { docs: Vec<Vec<Shape>>, loop_back: bool }

  impl PageCtx for TestGallery {
      fn step(&self, from: PagePos, by: isize) -> Option<PagePos> { /* walk the flat
          page list; at either end return None when !loop_back, else wrap */ }
      fn key(&self, pos: PagePos) -> Option<image_viewer::ImgKey> { None }
      fn dims(&self, pos: PagePos) -> Option<(u32, u32)> {
          match self.shape_at(pos) { Some(Shape::Single) => Some((1000, 1400)),
                                      Some(Shape::Double) => Some((2000, 1400)), None => None }
      }
      fn override_shape(&self, _pos: PagePos) -> Option<Shape> { None }
      fn flat(&self, pos: PagePos) -> Option<usize> { /* ordinal of pos in the flat list */ }
  }
  ```
  With `TestGallery::shape_at`, `TestGallery::page_at(n)` (the `n`th page of the flat list, so a test can name a page position), and `TestGallery::anchors(lead_lone)` — the full anchor list by repeated `next_anchor`, ending when it stops moving.

  Then `a_turn_runs_cover_pairs_a_wide_page_then_pairs_again`: one document, page shapes `[Single, Single, Single, Single, Double, Single, Single, Single]`, `lead_lone = 1`. Assert `anchors == [0, 1, 3, 5, 6]` and the turns are `{0} {1,2} {3,4} {5} {6,7}`.

- [ ] **Step 2: Run it to verify it fails**

  Run: `cargo test --test spreads`
  Expected: compile error — `PageCtx`, `Shape`, `PagePos` and the four functions do not exist.

- [ ] **Step 3: Implement `Shape`, `PagePos`, `PageCtx` and the four constants in `src/lib.rs`**

  `Shape` derives `Debug, Clone, Copy, PartialEq, Eq`. `PagePos` derives `Debug, Clone, Copy, PartialEq, Eq`. `PageCtx` is a trait with the five methods above and no default bodies.

  `Rect4` is `pub type Rect4 = [f32; 4];` with a doc comment reading *"A screen rectangle as `[x, y, width, height]`. Plain floats rather than `egui::Rect` so it can be tested: this module has no egui dependency on purpose."* — matching `dim_bands` at `src/lib.rs:1805`.

- [ ] **Step 4: Run it to verify it still fails**

  Run: `cargo test --test spreads`
  Expected: compile error naming `lone`/`next_anchor`/`prev_anchor`/`turn_len` as unresolved.

- [ ] **Step 5: Implement the four facts and the walk, with a doc comment on `lone` carrying the invariant**

  `shape_of` returns `ctx.override_shape(pos)` if present, else `Double` when
  `dims(pos)` is `Some((w, h))` with `h > 0 && (w as f32 / h as f32) > SPREAD_ASPECT`,
  else `Single`. Dims are compared as `f32` and a zero height is `Single`, never a
  division by zero.

  `pairable(a, b)` is `shape_of(a) != Double && shape_of(b) != Double &&
  (dims(a).is_none() || dims(b).is_none() || (ratio(a) - ratio(b)).abs() <= PAIR_ASPECT_TOLERANCE)`
  where `ratio` is `w as f32 / h as f32` guarded against `h == 0` by returning `f32::NAN`
  — a `NAN` comparison is false, so an unmeasurable page never *blocks* a pair on its own
  account, and the `is_none()` arm is what keeps a wholly unknown page optimistic.

  `lone(anchor, lead_lone, ctx)` is
  `ctx.flat(anchor).is_some_and(|f| f < lead_lone as usize) || shape_of(anchor) == Double || match ctx.step(anchor, 1) { Some(next) => !pairable(anchor, next, ctx), None => true }`.

  Its doc comment must state, in substance: **only ever call this at an anchor.** At a
  non-anchor it is wrong, and the concrete example is that page 4 of a run where page 5 is
  wide cannot pair with 5 and therefore "cannot pair" — yet it paired with 3. That is the
  one thing a future editor must not "simplify" away.

  `turn_len` is `if lone(..) { 1 } else { 2 }`.

  `next_anchor(a, ..)` is `ctx.step(a, turn_len(a, ..) as isize)`.

  `prev_anchor(a, ..)` is:
  ```rust
  let Some(b) = ctx.step(a, -1) else { return None };
  let took_alone = ctx.flat(b).is_some_and(|f| f < lead_lone as usize)
      || shape_of(b, ctx) == Double
      || !matches!(ctx.step(b, -1), Some(prev) if pairable(prev, b, ctx));
  Some(if took_alone { b } else { ctx.step(b, -1).expect("paired with prev, which exists") })
  ```

- [ ] **Step 6: Run it to verify it passes**

  Run: `cargo test --test spreads`
  Expected: the trace test passes.

- [ ] **Step 7: Verify the trace test can fail — the guard check**

  Change `lone` to ignore the `shape_of(anchor) == Double` arm and re-run. Expected: the trace test fails at `{5}` — the wide page pairs with 4 instead of taking its own turn. Restore `lone`.

  This matters because the spec's §4 trap is a formulation that passes a forward-only test. Confirm the guard is load-bearing before moving on.

- [ ] **Step 8: Write the round-trip, no-overlap and straddle tests**

  All in `tests/spreads.rs`:
  - `walking_forwards_visits_every_page_exactly_once` — proptest over length 1..=12 and
    a `Vec<Shape>`; assert no two turns overlap, every page is in exactly one turn, and
    the anchors are strictly increasing.
  - `walking_backwards_returns_to_the_anchor_it_started_from` — proptest over the same
    space; from the last anchor, `prev_anchor` exactly *n* times returns to the start.
  - `backwards_agrees_with_forwards` — build the anchor list forwards, then walk back from
    the end and assert the identical list reversed.
  - `a_wide_page_never_shares_a_turn` — Review Focus 2. Shapes `[Single, Double, Single]`,
    `lead_lone = 0`; assert anchors `[0, 1, 2]` and `turn_len(1) == 1`.
  - `a_turn_straddles_a_page_boundary` — Review Focus 3. `docs = [[Single, Single], [Single]]`
    with page 1 of the first document `Double`; assert the anchor sequence and that no
    turn mixes a page and a file in the wrong order.
  - `a_turn_straddles_two_files` — `docs = [[Single], [Single, Single]]`, `lead_lone = 0`;
    assert anchors `[0, 1]`.
  - `leading_pages_can_be_lone` — `lead_lone` 0, 1, 2 produce `{0,1}{2,3}…`, `{0}{1,2}…`,
    `{0}{1}{2,3}…`.
  - `nothing_moves_past_either_end_when_looping_is_off` — Review Focus 5.
  - `an_unknown_page_pairs_and_is_a_turn_of_one_until_it_is_measured` — a ctx returning
    `None` from `dims`; assert `shape_of == Single` and `turn_len == 2`.
  - `a_shape_at_the_threshold_is_not_a_spread` — ratios `1.2` and `1.199` are `Single`,
    `1.21` is `Double`.

- [ ] **Step 9: Run them**

  Run: `cargo test --test spreads`
  Expected: all pass, including the proptests.

- [ ] **Step 10: Commit**

  ```bash
  git add src/lib.rs tests/spreads.rs
  git commit -m "Add the spread pairing core: shapes, the anchor invariant, and the walk"
  ```

---

### Task 2: `page_dims` and the shape cache

**Files:**
- Modify: `src/lib.rs` — `page_dims` beside `tiff_page_count` (`src/lib.rs:3948`); `Gallery` gains `shapes`, `learn_shapes`, `forget_shapes`, `page_pos`, `page_count`, `flat_page`, and an `impl PageCtx for Gallery`
- Modify: `src/main.rs` — `learn_shapes` call where `learn_pages` is called; `pump`'s `Msg::Preview` arm feeds `orig` into the cache
- Test: `tests/spreads.rs`

**Interfaces:**
- Consumes: Task 1's `PageCtx`, `Shape`, `PagePos`, `shape_of`.
- Produces:
  ```rust
  pub fn page_dims(path: &Path, page: u32) -> Option<(u32, u32)>;   // stored, not EXIF-oriented

  impl Gallery {
      /// The current page's position, for the pairing walk.
      pub fn page_pos(&self) -> PagePos;
      /// Learn and remember the display dimensions of the current page.
      pub fn learn_shapes(&mut self, path: &Path);
      /// Drop one file's remembered dimensions. **Must be called from
      /// `invalidate_path`**, or a re-exported file keeps its old page's shape and
      /// every spread after it stays mis-paired for the session.
      pub fn forget_shapes(&mut self, path: &Path);
  }
  impl PageCtx for Gallery { /* all five methods */ }
  ```

- [ ] **Step 1: Write the failing tests**

  In `tests/spreads.rs`, using a real `tempfile::tempdir()`:
  - `page_dims_reads_stored_dimensions_without_decoding` — write a PNG with the `image`
    crate at 800x1200; assert `page_dims(p, 0) == Some((800, 1200))`.
  - `page_dims_returns_none_for_a_file_that_is_not_an_image` — a file of random bytes.
  - `the_shape_of_a_rotated_jpeg_ignores_the_tag` — write a JPEG 1600x900 carrying EXIF
    orientation 6; assert `page_dims` reports `(1600, 900)` (stored) while the gallery's
    cached shape treats it as `Single`, i.e. the orientation is applied by the cache, not
    by `page_dims`.
  - `a_learned_dimension_never_changes` — learn, then overwrite the file with a
    differently-sized image, and assert the cache still answers the old value until
    `forget_shapes`.
  - `forget_shapes_makes_the_dimension_change_again`.
  - `a_gallery_walks_pages_the_same_way_the_planner_sees_them` — a `Gallery` over three
    files, asserting `PageCtx::step` agrees with `Gallery::next`/`prev` page for page.
  - `an_unknown_preceding_page_count_makes_flat_unavailable` — a `Gallery` with only
    file 1 visited; `flat(PagePos { index: 3, page: 0 })` is `None`, and
    `flat(PagePos { index: 0, page: 0 })` is `Some(0)`.
  - `invalidate_path_drops_the_cached_dimensions` — learn a file's dimensions, call
    `invalidate_path` for it, and assert `dims` reports `None` afterwards so the shape
    is re-learned from the new contents. Without this a re-exported file's shape is
    frozen for the session and every spread downstream stays wrong, invisibly.

- [ ] **Step 2: Run to verify it fails**

  Run: `cargo test --test spreads`
  Expected: compile error — `page_dims` and the `Gallery` additions do not exist.

- [ ] **Step 3: Implement `page_dims`**

  Read a prefix of `PROBE_PREFIX_BYTES = 65_536` bytes with one `File::open` and
  `read_exact`-into-a-preallocated-buffer that tolerates a short file. Then:
  ```rust
  pub fn page_dims(path: &Path, page: u32) -> Option<(u32, u32)> {
      // page > 0 is only meaningful for a TIFF, whose pages differ; the decode
      // reports a later page's own dimensions anyway.
      if page > 0 { return tiff_page_dim(path, page); }
      image_dims_bytes(&buf).or_else(|| image_dims_path(path))
  }
  ```
  where `image_dims_path` is the full `fs::read` fallback and `tiff_page_dim` walks the
  IFD chain for the `page`th directory and returns its dimensions, or `None`.

  `image_dims_bytes` already exists at `src/lib.rs:3744` and handles JXL and HEIF too. Its
  doc comment must record **why the prefix is safe**: every format's dimension marker sits
  within the first 64 KB (a JPEG's APP1/EXIF segment is capped at 64 KB by the spec, and
  the SOF marker follows it), and `image_dims_bytes` returning `None` — not a wrong answer —
  is the signal to fall back. `decode_page` already does the equivalent header read for the
  MCU grid at `src/lib.rs:5035-5043`, so the pattern is established.

- [ ] **Step 4: Add `Gallery`'s shape cache and its `PageCtx` impl**

  Add `shapes: HashMap<PathBuf, (u32, u32)>` to `Gallery` (one entry per file: the shape
  test judges a *file*, and for a multi-page document the later pages arrive by pressing
  right and get judged then — the same argument `plan_jobs` makes at `src/lib.rs:1148-1152`).

  - `learn_shapes(path)` answers from the cache when it can, reads `page_dims`, applies
    `exif_orientation` to orient the result, stores, and returns. Its doc comment repeats
    `learn_pages`' warning: this runs on every navigation, so re-reading is not free.
  - `forget_shapes(path)` removes the entry.
  - Initialise `shapes` in `new`, `empty` and `from_file`, and clear it in the existing
    `forget_pages` (which runs whenever `files` is rebuilt).
  - `page_count(index)` is `page_counts.get(&files[index])`, defaulting to 1.
  - `flat(pos)` sums `page_count` for indices before `pos.index` plus `pos.page`,
    returning `None` if any preceding count is unknown — i.e. if any of them is absent
    from `page_counts`.
  - `step(from, by)` is `Gallery`'s own `next`/`prev` arithmetic on a copy, so it honours
    `loop_back` and the page-before-file order without duplicating the rules.
  - `dims(pos)` is the cached value. `key(pos)` is `ImgKey { path, page }`.
  - `override_shape` is `None` here; Task 3 supplies the real one.

- [ ] **Step 5: Wire `main.rs`**

  Call `gallery.learn_shapes(path)` beside the existing `learn_pages` call. In `pump`'s
  `Msg::Preview` arm, insert the arrived `orig` into the gallery's cache for that key —
  a neighbour's decode reports its own dimensions for free, which is the cheapest source
  in the spec's §5.

  **In `invalidate_path`, call `gallery.forget_shapes(path)`** beside the existing
  `forget_page_count`. This is the self-healing rule `AGENTS.md` already relies on for
  page counts: a file re-read mid-write must be re-judged, or a re-export that changes a
  page's aspect leaves every spread after it mis-paired with nothing on screen to say so.

- [ ] **Step 6: Run the tests**

  Run: `cargo test --test spreads`
  Expected: all pass.

- [ ] **Step 7: Run the full suite**

  Run: `cargo test --no-fail-fast`
  Expected: green. Nothing existing reads `shapes`, so nothing else can move.

- [ ] **Step 8: Commit**

  ```bash
  git add src/lib.rs src/main.rs tests/spreads.rs
  git commit -m "Read page dimensions from a header prefix, and cache them beside the page counts"
  ```

---

### Task 3: The persisted overrides

**Files:**
- Modify: `src/lib.rs` (new section beside the thumbnail cache, around `src/lib.rs:4468`)
- Test: `tests/spreads.rs`

**Interfaces:**
- Consumes: Task 1's `Shape`, `PagePos`.
- Produces:
  ```rust
  #[derive(Debug, Default, Clone, PartialEq)]
  pub struct GallerySpreads {
      pub lead_lone: u32,
      pub pages: HashMap<(String, u32), Shape>,
  }

  pub fn spreads_root() -> Option<PathBuf>;                       // %LOCALAPPDATA%\image-viewer\spreads.json
  pub fn load_spread_overrides(file: &Path) -> GallerySpreads;     // never fails
  pub fn save_spread_overrides(file: &Path, g: &GallerySpreads) -> std::io::Result<()>;

  impl GallerySpreads {
      pub fn shape_for(&self, path: &Path, page: u32) -> Option<Shape>;
      pub fn set_shape(&mut self, path: &Path, page: u32, shape: Shape);
      pub fn toggle(&mut self, path: &Path, page: u32, detected: Shape) -> Shape;
      pub fn nudge_lead_lone(&mut self, by: i32);
  }
  ```

- [ ] **Step 1: Write the failing tests**

  In `tests/spreads.rs`, each in its own `tempdir()`:
  - `an_override_beats_the_detector`
  - `overrides_survive_a_reload_from_disk` — set, save, load into a fresh value, assert equal.
  - `a_malformed_overrides_file_is_an_empty_override_set` — write `{ not json`, assert
    `load_spread_overrides` returns the default and does not panic. **A file this app
    writes must never be able to stop it starting.**
  - `a_missing_overrides_file_is_an_empty_override_set`
  - `nudging_lead_lone_is_clamped_to_the_documented_range` — decrement from 0, increment
    from `LEAD_LONE_MAX`, assert both clamp.
  - `toggling_a_page_reports_the_shape_it_landed_on` — `toggle` given `Shape::Double`
    returns and stores `Shape::Single`, and vice versa.
  - `an_archive_is_keyed_by_its_original_path` — Review Focus 4. Overrides saved under
    `C:\books\vol3.cbz` are found again after the `%TEMP%` extraction directory changes;
    assert nothing was written under a path containing the temp directory.
  - `overrides_are_scoped_to_one_gallery` — two `GallerySpreads` values for two roots do
    not see each other's pages.

- [ ] **Step 2: Run to verify it fails**

  Run: `cargo test --test spreads`
  Expected: compile error — `GallerySpreads` does not exist.

- [ ] **Step 3: Implement**

  Serialise with `serde_json`, `#[serde(default)]` on both fields so a file written by an
  older build still loads. `Shape` needs `#[derive(serde::Serialize, serde::Deserialize)]`
  with `#[serde(rename_all = "lowercase")]` — the load-bearing pattern `AGENTS.md` records
  for `WgpuBackend`, because without it a hand-edited file using the documented spelling
  fails to load.

  `load_spread_overrides` reads the whole file, `serde_json::from_str`, and returns
  `unwrap_or_default()` on any error — including a read error. Every failure is an empty
  override set.

  `save_spread_overrides` creates the parent directory and writes to `spreads.json.tmp`
  then `fs::rename`, the half-written-entry guard the thumbnail cache uses
  (`AGENTS.md`: rename replaces on Windows).

  `spreads_root` mirrors `thumb_cache_root`'s `%LOCALAPPDATA%` resolution. Its doc comment
  must say why it is not `%APPDATA%`: these are *this machine's* reads of files that may
  be on a network share, and they are worthless on another machine.

  Keys are `(file_name, page)` strings inside a per-root object, so a relative path inside
  an archive does not carry a drive letter. Doc comment: **the archive's original path is
  the key; a `%TEMP%` extraction path must never be written.**

- [ ] **Step 4: Run to verify it passes**

  Run: `cargo test --test spreads`
  Expected: all pass.

- [ ] **Step 5: Commit**

  ```bash
  git add src/lib.rs tests/spreads.rs
  git commit -m "Persist per-page shape overrides and the leading-lone offset"
  ```

---

### Task 4: The planner learns the partner

**Files:**
- Modify: `src/lib.rs:1099-1119` (`Plan`) and `src/lib.rs:1140` (`plan_jobs`)
- Modify: `src/main.rs:1571-1611` (`schedule`)
- Test: `tests/flip_through.rs`

**Interfaces:**
- Consumes: Task 1's `turn_len`, `PagePos`; Task 3's `GallerySpreads`.
- Produces:
  ```rust
  /// `Plan` gains:
  /// The other page of the current turn, in a spread. Planned at the current
  /// image's own priority so both halves arrive as a pair; a neighbour-priority
  /// decode would leave the spread half-painted.
  pub partner: Option<ImgKey>,
  ```
  and in `main.rs`, the turn query both later tasks build on:
  ```rust
  /// The app's `PageCtx`: `Gallery` plus the persisted overrides. Declared here
  /// because `schedule` is the first caller and both Tasks 6 and 7 need it too.
  pub struct AppPageCtx<'a> { pub gallery: &'a Gallery, pub spreads: &'a GallerySpreads }
  impl PageCtx for AppPageCtx { /* delegates to gallery; override_shape reads spreads */ }

  impl MyApp {
      /// The turn's second page, or `None` when manga mode is off, the turn has one
      /// page, or the gallery is empty. The cursor page is never returned - it is
      /// already `cur`.
      pub fn turn_partner(&self) -> Option<ImgKey>;
  }
  ```
  `turn_partner` is `None` whenever `manga_mode` is off, unconditionally, so
  single-image browsing plans byte-identically to before this feature existed.

- [ ] **Step 1: Write the failing test**

  In `tests/flip_through.rs`:
  ```rust
  #[test]
  fn the_partner_of_a_turn_is_planned_at_the_current_priority() {
      let f = files(&["a.jpg", "b.jpg", "c.jpg"]);
      let cached = HashSet::new();
      let failed = HashSet::new();
      let jobs = plan_jobs(&Plan { files: &f, index: 0, current_page: 0, cached: &cached,
          failed: &failed, wants_exif: false, wants_full: false, current_stale: false,
          target_dim: 1024, radius: 3,
          partner: Some(ImgKey::first(&f[1])) });
      let partner = jobs.iter().find(|j| j.path() == f[1] && j.kind == Kind::Preview)
          .expect("the partner must be planned at all");
      assert_eq!(partner.prio, image_viewer::PRIO_CURRENT_PREVIEW,
          "a partner decoded at neighbour priority leaves a half-painted spread");
      assert_eq!(jobs.iter().filter(|j| j.prio == image_viewer::PRIO_CURRENT_PREVIEW).count(), 2);
  }
  ```
  Plus `a_partner_already_cached_is_not_planned_again`, `a_failed_partner_is_not_retried`
  and `no_partner_leaves_the_plan_exactly_as_it_was` — the last asserting the job list is
  byte-identical to the same plan with `partner: None`, which is what keeps
  `the_plan_wraps_around_a_short_folder` and every other existing assertion green.

- [ ] **Step 2: Run to verify it fails**

  Run: `cargo test --test flip_through`
  Expected: compile error — `Plan` has no field `partner`.

- [ ] **Step 3: Implement**

  Add the field with a doc comment in the shape above. In `plan_jobs`, after the current
  preview push and before the exif push, add a partner push at `PRIO_CURRENT_PREVIEW`
  guarded by `need_preview(&partner)` — but **not** by `current_stale`: a stale current is
  about the *current* image's texture being softer than the display, and refining the
  partner as well would double the decodes after a resize for no gain. The partner is
  also not added to the preload window's `seen` set: it is already a neighbour of the
  current index, so `seen` would skip it, and adding it there would change the priority
  the window gives it. Its doc comment must record that the window deliberately prefetches
  page 0 while the partner carries its real page number.

- [ ] **Step 4: Run the planner tests**

  Run: `cargo test --test flip_through`
  Expected: all pass, including the existing strand check at `flip_through.rs:495-506`
  and the three-image wrap at `:572-598`.

- [ ] **Step 5: Wire `schedule`**

  Build `AppPageCtx` over `&self.gallery` and `&self.spreads`, then pass
  `self.turn_partner()` as `Plan::partner`. When manga mode is off `turn_partner` returns
  `None`, so single-image browsing plans byte-identically to before this feature existed.

- [ ] **Step 6: Run the full suite**

  Run: `cargo test --no-fail-fast`
  Expected: green.

- [ ] **Step 7: Commit**

  ```bash
  git add src/lib.rs src/main.rs tests/flip_through.rs
  git commit -m "Plan a turn's partner at the current priority so a spread paints whole"
  ```

---

### Task 5: Spread layout maths

**Files:**
- Modify: `src/lib.rs` (new section beside `fit_scale_no_upscale` at `src/lib.rs:684`)
- Test: `tests/spreads.rs`

**Interfaces:**
- Consumes: Task 1's `Rect4`.
- Produces:
  ```rust
  /// The scale a whole turn is drawn at: the more constrained fit of every page
  /// present into *its half* of the panel, so every page fits at one shared factor.
  pub fn spread_fit(panel: (f32, f32), pages: &[(f32, f32)]) -> f32;

  /// Where each page of a turn goes, **in input order**, not screen order.
  ///
  /// One shared scale, zero gutter, top-aligned. The seam is always at the panel's
  /// horizontal centre: page 0 extends from the seam outward in `rtl`'s direction and
  /// page 1 the other way, so a lone page needs no special case — it simply has nothing
  /// on the far side of the seam, and renders at half the panel's width, which is what a
  /// book's cover does.
  pub fn spread_rects(panel: Rect4, pages: &[(f32, f32)], scale: f32, rtl: bool) -> Vec<Rect4>;
  ```

- [ ] **Step 1: Write the failing tests**

  In `tests/spreads.rs`:
  - `a_two_page_turn_meets_at_the_seam` — panel `[0,0,800,600]`, pages
    `[(600,800),(600,800)]`, scale 1.0, `rtl` false. Assert the two rects touch at
    `x = 400` and neither overlaps the other.
  - `a_right_to_left_turn_opens_on_the_right` — same inputs, `rtl` true: assert page 0's
    rect has `min.x == 400` and page 1's has `max.x == 400`, i.e. page 0 is the right one.
  - `a_lone_page_rests_with_the_seam_on_the_centre` — one page, `rtl` true: assert the
    rect's `min.x == 400` and `width <= 400`. This is Mihon's *"centring the span would
    pull it to the middle, indistinguishable from a page with no partner"*, and it is the
    behaviour that reads as a bug if it is ever changed to centring.
  - `a_lone_page_rests_on_the_left_when_reading_left_to_right` — one page, `rtl` false:
    assert `max.x == 400`.
  - `the_whole_turn_is_top_aligned` — all returned rects share the panel's `y` and have
    their own heights, so a short page leaves a gap beneath rather than being centred.
  - `rects_come_back_in_input_order` — a deliberately asymmetric pair
    `[(400,800),(700,800)]`; assert `rects[0]` is the 400-wide one under both `rtl` values.
    This is the footgun guard: an implementation that returns screen order silently swaps
    which page is which.
  - `spread_fit_is_the_more_constrained_of_the_pages` — pages `[(400,800),(400,1600)]` in
    an `800x600` panel; assert the result is the scale the *taller* page needs, not the
    shorter one's.
  - `a_landscape_photo_folder_leaves_every_page_a_whole_turn` — Review Focus 1. A panel
    `800x600` and a single page `[(1200,800)]` (ratio 1.5, above `SPREAD_ASPECT`): assert
    `spread_fit` is that page's own fit into a half, that `spread_rects` gives it the
    half the seam is on, that nothing panics and no empty vector comes back. The point of
    the test is that a folder of 3:2 photographs in manga mode is *one page per turn* and
    must render sanely rather than looking broken.
  - `spread_fit_survives_an_empty_panel_and_a_zero_sized_page` — panel `(0, 0)`,
    page `(0, 0)`: assert a finite, non-zero scale and no panic.

- [ ] **Step 2: Run to verify it fails**

  Run: `cargo test --test spreads`
  Expected: compile error — `spread_fit` and `spread_rects` do not exist.

- [ ] **Step 3: Implement `spread_fit`**

  ```rust
  pub fn spread_fit(panel: (f32, f32), pages: &[(f32, f32)]) -> f32 {
      let half_w = panel.0 / 2.0;
      if half_w <= 0.0 || panel.1 <= 0.0 || pages.is_empty() {
          return 1.0;
      }
      pages.iter()
          .map(|&(w, h)| (half_w / w.max(1.0)).min(panel.1 / h.max(1.0)))
          .fold(f32::INFINITY, f32::min)
          .clamp(1e-4, 8.0)
  }
  ```
  The `fold` over `min`, not a per-page fit, is deliberate and its doc comment must say
  why: this is `webgpuviewer`'s `minOf`, whose own comment says "independently" while the
  code takes the minimum. Two halves at two different scales cannot share one zoom level,
  so the more constrained one wins and the other has a margin.

- [ ] **Step 4: Implement `spread_rects`**

  ```rust
  pub fn spread_rects(panel: Rect4, pages: &[(f32, f32)], scale: f32, rtl: bool) -> Vec<Rect4> {
      let seam = panel[0] + panel[2] * 0.5;
      pages.iter().enumerate().map(|(i, &(pw, ph))| {
          let w = pw * scale;
          let on_right = if i == 0 { rtl } else { !rtl };
          [if on_right { seam } else { seam - w }, panel[1], w, ph * scale]
      }).collect()
  }
  ```
  No special case for a lone page: with one page there is nothing on the far side of the
  seam, which is the whole behaviour.

- [ ] **Step 5: Run to verify it passes**

  Run: `cargo test --test spreads`
  Expected: all pass.

- [ ] **Step 6: Commit**

  ```bash
  git add src/lib.rs tests/spreads.rs
  git commit -m "Lay a turn out at one shared scale with the seam at the centre"
  ```

---

### Task 6: Paint two pages

**Files:**
- Modify: `src/main.rs` — `MyApp` gains `shown_partner`; `central` (`src/main.rs:2636`)
- Test: `tests/unit.rs` (headless layout assertions)

**Interfaces:**
- Consumes: Task 5's `spread_fit`, `spread_rects`, `Rect4`; Task 4's `turn_partner`.
- Produces: `MyApp::shown_partner: Option<ImgKey>`, set in `sync_shown`
  (`src/main.rs:1835`), cleared in `reset_display` and `invalidate_path`.

  **`shown` stays the cursor page and keeps its current meaning.** Every view-shaped code
  path — `footprint`, the crop frame, `dim_bands`, full-res tiles, `anim`, EXIF, the
  latency commit at `src/main.rs:2877` — reads `shown` and is therefore untouched. Only
  the paint loop reads `shown_partner`. This is what keeps the task small and is why no
  crop, tile or save code changes in this plan at all.

- [ ] **Step 1: Write the failing test**

  Headless, through the `run_pass` helper in `tests/unit.rs` (`tests/unit.rs:798`), which
  clears `textures_delta`: `a_lone_page_and_a_pair_have_the_same_total_width_at_the_seam`
  is already covered by Task 5. For *this* task the assertion is that a spread does not
  move the cursor page's own footprint relative to the single-image case:
  `the_cursor_pages_footprint_is_unchanged_by_manga_mode` — drive a real pass with
  `shown` set and `shown_partner` set, and assert the cursor page's painted rect is the
  same rectangle the single-image path produced at the same scale. Read it out of
  `out.shapes` by filtering `Shape::Rect` with a `Brush` whose
  `fill_texture_id != TextureId::default()`, the same technique as
  `test_grid_thumbnail_is_centred_and_textured` at `tests/unit.rs:1067`.

- [ ] **Step 2: Run to verify it fails**

  Run: `cargo test --test unit the_cursor_pages_footprint_is_unchanged_by_manga_mode`
  Expected: fails — nothing paints a second page.

- [ ] **Step 3: Add the field and `turn_pages`**

  `shown_partner: Option<ImgKey>` next to `shown` (`src/main.rs:1197`).

  In `sync_shown`, set it from `self.turn_partner()` whenever manga mode is on and the
  anchor preview has been adopted; clear it in `reset_display` (`src/main.rs:1538`) and
  `invalidate_path` (`src/main.rs:2152`) beside the existing `shown = None`.

  `sync_shown`'s doc comment gets one line: the partner is adopted in the same act as the
  cursor, so the spread never shows one page from this turn beside a stale page from the
  last.

- [ ] **Step 4: Split the panel and paint the turn**

  In `central`, after the grid early-return and before the single-image lookup:

  - When manga mode is off, take the existing path unchanged. Guard the whole new code
    behind `self.config.manga_mode` so single-image browsing is byte-identical.
  - Build `pages: Vec<(f32, f32)>` from the turn's members' **dimensions** — the gallery's
    shape cache first, the preview's `orig` second — *not* from the textures. Layout from
    dimensions, paint from textures, so the layout does not reflow when a partner's
    texture lands. The dims are already known because arriving at an anchor ran
    `pairable`, which reads both pages' dimensions; that is the pairing query doubling as
    the dimension prefetch, and it is worth saying so in a comment.
  - `scale = spread_fit((panel.width(), panel.height()), &dims)`, and use that as the
    `fit_scale` for the shared view so zoom clamping behaves exactly as it does today.
  - `rects = spread_rects(panel_as_Rect4_translated_by_view_offset, &dims, scale, self.config.manga_rtl)`.
  - For each `(key, rect)` pair, look up `self.previews.get(&key)` and `paint_tex` it at
    that rect, snapping `x` and `y` to the physical pixel grid with the existing `snap`
    closure. `paint_tex` itself is unchanged — it already takes an arbitrary `Rect`.
  - The transparency checkerboard backdrop is painted per page, from that page's `rect`.
  - Then compute the **cursor page's** `rect` and `footprint` from `rects[0]`'s
    position and the cursor page's own size, and leave everything from there
    (`src/main.rs:2881` onward: tiles, crop, bands, the loading overlay) reading exactly
    what it reads today.

  `MAX_SCALE`, `self.view`, the zoom anchor maths and the pan clamp all stay as they are.
  The pan clamp's `panel.size()` becomes the turn's total footprint so panning stops at
  the spread's edge rather than the panel's.

- [ ] **Step 5: Run to verify it passes**

  Run: `cargo test --test unit`
  Expected: all pass.

- [ ] **Step 6: Run the full suite**

  Run: `cargo test --no-fail-fast`
  Expected: green.

- [ ] **Step 7: Verify by hand, on a console session**

  Build a folder of eight portrait images plus one wide one, open it, press `M`. Confirm
  the pair meets at the seam with no gap, the wide image takes a whole turn alone at full
  width, and the cover renders at half width with the seam on the centre. Note that `quser`
  must report `console` before believing anything about how it looks or feels.

- [ ] **Step 8: Commit**

  ```bash
  git add src/main.rs tests/unit.rs
  git commit -m "Paint a turn: two pages at one shared scale, seam at the centre"
  ```

---

### Task 7: Navigate by turn

**Files:**
- Modify: `src/main.rs` — `goto` (`src/main.rs:1423`), `step` (`src/main.rs:1483`)
- Test: `tests/spreads.rs`

**Interfaces:**
- Consumes: Task 1's `next_anchor`, `prev_anchor`, `turn_len`, `PagePos`; Task 2's
  `Gallery::page_pos`.
- Produces: `MyApp::step` walks turns instead of pages when manga mode is on. `Gallery`
  gains `pub fn goto_page(&mut self, pos: PagePos)`.

- [ ] **Step 1: Write the failing tests**

  In `tests/spreads.rs`, driving `Gallery` with `set_page_counts` so no filesystem is
  involved:
  - `a_step_forward_advances_a_whole_turn` — eight single-page files; assert the position
    after one step is the second anchor, not the next page.
  - `a_step_backward_returns_to_the_previous_turn_and_not_the_previous_page` — the
    assertion the whole `prev_anchor` exists for.
  - `a_wide_page_is_stepped_past_as_one_turn` — with page 4 wide, the turn sequence
    contains 4 exactly once and the cursor never rests on 3 as an anchor.
  - `navigating_a_full_round_returns_to_the_start` — with `loop_back` on, N forward turns
    then N backward turns returns to the original anchor.
  - `a_multi_page_document_is_navigated_by_turn_too` — `set_page_counts([3, 1])`,
    `lead_lone = 0`: assert the turn sequence steps across the document's page boundary
    exactly as `Gallery::next` does, so a turn can hold page 2 of file 0 and page 0 of
    file 1.
  - `removing_the_cursor_page_leaves_its_partner_a_turn_of_one` — the pairing
    consequence of `Del`. Drop page 0 from a `Single, Single` pair and assert page 1
    becomes an anchor with `turn_len == 1`: it is alone on screen, not stranded. The
    dangerous alternative — deleting both — is what `AGENTS.md` forbids.
  - `nothing_moves_with_looping_off_at_the_end` — Review Focus 5.

- [ ] **Step 2: Run to verify it fails**

  Run: `cargo test --test spreads`
  Expected: fail — navigation is still per page.

- [ ] **Step 3: Add `Gallery::goto_page`**

  ```rust
  pub fn goto_page(&mut self, pos: PagePos) {
      if pos.index < self.files.len() {
          self.index = pos.index;
          self.page = pos.page;
      }
  }
  ```
  Deliberately not clamped to `pages_in_current`: the walk only produces positions it got
  from `PageCtx::step`, which already respects the page count, and clamping here would be
  a second, silently different rule.

- [ ] **Step 4: Make `step` walk turns**

  ```rust
  fn step(&mut self, ctx: &Context, forward: bool) {
      if self.config.block_while_loading && self.waiting_load { return; }
      let moved = if self.config.manga_mode {
          if forward { self.turn_forward() } else { self.turn_backward() }
      } else if forward { self.gallery.next() } else { self.gallery.prev() };
      if moved { self.goto(ctx); }
  }
  ```
  `turn_forward` is `gallery.goto_page(next_anchor(gallery.page_pos(), lead, ctx))`,
  `turn_backward` is the same with `prev_anchor`, each returning whether it moved. Both
  take the lead value and the override set through a `&dyn PageCtx` the app builds from
  `self.gallery` plus `self.spreads`.

  **The context must answer `override_shape` from the persisted overrides**, so Task 2's
  `Gallery` impl is not the whole context in the app — the app wraps it. Introduce
  `struct AppPageCtx<'a> { gallery: &'a Gallery, spreads: &'a GallerySpreads }` in
  `main.rs` and `impl PageCtx for AppPageCtx` by delegating all five methods, overriding
  only `override_shape` with `spreads.shape_for(path, page)`.

- [ ] **Step 5: Run to verify it passes**

  Run: `cargo test --test spreads`
  Expected: all pass.

- [ ] **Step 6: Run the full suite, and check the page-walk tests specifically**

  Run: `cargo test --test multiframe --test filter_and_paging --test integration --test unit`
  Expected: green **unmodified**. `tests/multiframe.rs:697-918` is the evidence that the
  page cursor was not disturbed — if those needed editing, this task is wrong.

- [ ] **Step 7: Commit**

  ```bash
  git add src/main.rs src/lib.rs tests/spreads.rs
  git commit -m "Step a whole turn at a time, keeping the page cursor underneath"
  ```

---

### Task 8: Keys, direction, and the override controls

**Files:**
- Modify: `src/main.rs:3842-3856` (input read), `src/main.rs:3966-4027` (dispatch),
  `src/main.rs:3462-3476` (`pending_advance`), `src/main.rs:4272-4518` (Settings)
- Modify: `src/lib.rs:167-239` (`Config`)
- Test: `tests/keybindings.rs`

**Interfaces:**
- Consumes: Task 3's `GallerySpreads`; Task 7's `turn_forward`/`turn_backward`.
- Produces:
  ```rust
  // Config
  #[serde(default)]              pub manga_mode: bool,
  #[serde(default = "default_true")] pub manga_rtl: bool,
  ```
  and one pure function, which is the only testable part of the whole direction change:
  ```rust
  /// Whether a navigation input means *forwards* in reading order.
  ///
  /// Under `manga_rtl` the keys track the screen, so a rightward press is backwards
  /// in a book that opens to the left.
  pub fn nav_is_forward(input: NavInput, rtl: bool) -> bool;

  pub enum NavInput { Right, Left, Next, Previous, WheelDown }
  ```

- [ ] **Step 1: Write the failing tests**

  In `tests/keybindings.rs` (which asserts against `src/main.rs` as text — see its module
  docs for why):
  - `every_navigation_key_agrees_about_which_way_is_forwards` — for `rtl` true and false,
    `nav_is_forward(Right, rtl) == nav_is_forward(Next, rtl)` and
    `nav_is_forward(Left, rtl) == nav_is_forward(Previous, rtl)` and the two differ from
    each other. **This is the test the spec exists for:** OpenComic ships a live bug from
    omitting `Space` from its `mangaInvert` set, so Space silently becomes "previous
    page" in manga mode.
  - `the_wheel_does_not_follow_the_arrow_direction` — `nav_is_forward(WheelDown, rtl)` is
    `true` for both values of `rtl`.
  - Add `M`, `D`, `O` and `X` to `BARE_LETTERS` with their reasons: `M` and `D` are
    view-only; `O` and `X` write only to `spreads.json` under `%LOCALAPPDATA%`, a file
    this app owns, so they cannot lose a user's images.
  - Extend the brace-matched `!typing` block assertion (`tests/keybindings.rs:28-51`) to
    cover the four new letters.
  - `an_old_config_without_the_manga_settings_still_loads` — a `config.json` carrying only
    `load_all_on_startup`, `folder` and the pre-existing keys; assert `manga_mode` is
    `false` and `manga_rtl` is `true`.

- [ ] **Step 2: Run to verify it fails**

  Run: `cargo test --test keybindings`
  Expected: compile error — `nav_is_forward` does not exist.

- [ ] **Step 3: Add the config fields and `nav_is_forward` in `src/lib.rs`**

  `nav_is_forward`:
  ```rust
  pub fn nav_is_forward(input: NavInput, rtl: bool) -> bool {
      match input {
          NavInput::WheelDown => true,          // direction-agnostic in both modes
          NavInput::Right | NavInput::Next => !rtl,
          NavInput::Left | NavInput::Previous => rtl,
      }
  }
  ```
  Its doc comment records the trade-off honestly: with `manga_rtl` on, `→` means *next* in
  single mode and *backwards* in a spread. That is the price of matching the screen, and
  it is what Mihon, Komga and Kavita all ship. The wheel is excluded because it has no
  spatial meaning to preserve — Mihon's deliberate choice.

- [ ] **Step 4: Wire the keys**

  Read the four new letters in the same `!typing` block, alongside the existing
  `next`/`prev` reads, and dispatch:

  - `M` — flip `config.manga_mode`, save, and re-`goto` so the turn is recomputed.
  - `D` — flip `config.manga_rtl`, save, repaint. No re-`goto`: the turn's *membership* is
    unchanged, only its screen order.
  - `O` / `Shift+O` — `spreads.nudge_lead_lone(-1 / +1)`, save, re-`goto`.
  - `X` — toggle the cursor page's shape from its detected value, save, re-`goto`.

  Replace the arrow/Space/PageUp/PageDown and mouse-button branches so each calls
  `self.step(ctx, nav_is_forward(...))` with the mapped direction, from one expression per
  input. **No branch may decide a direction on its own.** That is the guard against
  OpenComic's bug.

  `pending_advance` (`src/main.rs:3462-3476`) reads raw `ArrowRight`/`PageDown`/`Space`
  for hold-to-browse; route it through the same `nav_is_forward` calls. Its own comment
  must say it would otherwise disagree with the setting silently.

  `Home` and `End` in manga mode: `Home` goes to the turn anchored at page 0, `End` walks
  `prev_anchor` from the final page.

  **The grid (`G`) is unchanged** — one cell per file, page 1 only, as it is today — but
  `pick_grid_selection` must land on the **turn anchored at the picked file's first page**,
  not on whatever page it happened to be on. `goto_index` already resets the page, so this
  is mostly a matter of not regressing it when manga mode is on.

- [ ] **Step 5: Add the Settings rows**

  Two checkboxes beside the existing ones, following the `linear_resample` pattern at
  `src/main.rs:4328-4337`: change → `config.save(&self.config_path)`. `Manga mode` and
  `Right-to-left`. Under them, one line stating `spreads.json`'s path and that deleting it
  clears every override — the same disclosure `AGENTS.md` requires beside the thumbnail
  cache, because a file that silently re-pairs a library must not be enabled quietly.

- [ ] **Step 6: Run to verify it passes**

  Run: `cargo test --test keybindings`
  Expected: all pass.

- [ ] **Step 7: Run the full suite**

  Run: `cargo test --no-fail-fast`
  Expected: green.

- [ ] **Step 8: Commit**

  ```bash
  git add src/lib.rs src/main.rs tests/keybindings.rs
  git commit -m "Map every navigation key through one direction rule, and add the manga controls"
  ```

---

### Task 9: Docs

**Files:**
- Modify: `README.md` (feature list, key table, settings)
- Modify: `AGENTS.md` (a "Spreads and reading direction" section)
- Modify: `TODO.md` only if a limitation is not already stated in the spec

**Interfaces:**
- Consumes: everything. Produces: nothing.

- [ ] **Step 1: Write the README section**

  Add `M`, `D`, `O`, `X` to the key table with their real bindings. Add the two settings.
  Add a short "Manga mode" subsection covering: turns of one or two pages, right-to-left by
  default, a wide page taking a whole turn and re-pairing what follows, zero gutter, and
  the `X` override. State the limitation plainly — a single landscape page and a
  two-page scan are geometrically identical, so detection is a good guess with an escape
  hatch, not a certainty.

  Also state the two limits a user will meet and cannot fix: the partner page renders at
  preview resolution when you zoom past 1:1 (at fit-to-window on a 4K screen the preview
  *is* 1:1, so this only bites on a deliberate zoom), and the first visit to a cold book
  can shift the pairing once as dimensions arrive, self-correcting on the second visit.

- [ ] **Step 2: Write the AGENTS.md section**

  Record the traps, in the voice the rest of that file uses, and each one as a
  *consequence* rather than a rule to memorise:
  - **`lone` is only valid at an anchor.** The concrete failure — page 4 "cannot pair with"
    page 5 and therefore "cannot pair", yet it paired with page 3 — because it looks
    exactly like the naive predicate's output and a forward-only test cannot see it.
  - **`shown` is the cursor, not the turn.** Everything view-shaped reads it, which is why
    crop, tiles, EXIF, save and clipboard needed no change at all. Repointing them at the
    turn is the change that would break them.
  - **Layout comes from dimensions, paint comes from textures.** A partner whose texture
    has not landed must not move the layout, or the spread reflows under the reader.
  - **One shared scale means two halves cannot zoom independently**, and the pan clamp is
    the turn's footprint rather than the panel's.
  - **The wheel deliberately does not follow the arrows**, and OpenComic's `Space` bug is
    the reason every navigation key goes through `nav_is_forward`.
  - **Overrides are `%LOCALAPPDATA%` and keyed by the original archive path** — the temp
    path changes on every re-extract, and writing it would orphan every override.
  - **`SPREAD_ASPECT` is 1.2 and every reader in the field uses 1.0.** The reason is that a
    real page is ~0.707 and a real spread ~1.414, so 1.0 sits on the midpoint and flags
    square-ish single pages.

- [ ] **Step 3: Verify the glyph scan still passes**

  Run: `cargo test --test unit no_ui_text_uses_a_glyph_the_font_lacks`
  Expected: PASS. `⇄` and any arrow glyphs are **not** in egui's default font subset and
  render as empty boxes — the scan exists because `Mirror ⇋ / ⇅` and `Prev (←)` shipped
  that way. Write the new UI text in ASCII.

- [ ] **Step 4: Run the full suite**

  Run: `cargo test --no-fail-fast`
  Expected: green.

- [ ] **Step 5: Commit**

  ```bash
  git add README.md AGENTS.md TODO.md
  git commit -m "Document manga mode: the feature, the keys, and the traps"
  ```