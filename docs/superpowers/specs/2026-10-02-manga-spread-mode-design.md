# Manga mode: spreads, right-to-left, and double-page detection

**Date:** 2026-10-02
**Status:** Design approved in conversation, pending written review.
**Scope:** One crate, no workspace. `src/lib.rs` gains the whole testable core; `src/main.rs`
gains a second painted image and a settings row.

---

## 1. What this is

A **manga mode**: images are shown as **turns** — one or two pages side by side, opening
right-to-left by default — where a single image that is itself a scan of two printed pages
takes a whole turn alone and **re-pairs everything after it**.

Three separable pieces, in dependency order:

1. **A spread layout** — paint two pages instead of one, sharing one zoom/pan.
2. **Double-page detection** — decide whether a page takes a whole turn, from a cheap
   dimension read plus a persisted manual override.
3. **Right-to-left reading** — the turn opens on the right and the navigation keys follow
   the screen rather than the reading order.

Every rule here is a pure function in `src/lib.rs`. `src/main.rs` is unreachable from tests
and is where every bug this project has not yet had would come from.

### Deliberately out of scope

- Content-based detection (gutter, spine, fold). **Nobody in the researched field does it**,
  and it would require a decode *before* pairing is known, which inverts the ordering the
  whole design rests on (see §4).
- A configurable gutter. Zero gutter, matching Mihon and Aidoku (§7).
- Full-resolution tiles for the partner page (§9, limitation 1).
- Persisting the *detector's* verdict. Nothing in the field does; only the user's own
  overrides are persisted (§6).

---

## 2. Decisions, and what drove them

| Decision | Choice | Driver |
|---|---|---|
| Pairing after a detected spread | **Reshift** | Consensus of 4 of 5 readers read at source. Komga's tests pin the identical trace. |
| Detection + override | **Auto + persisted manual override** | No reader has both a per-book offset nudge *and* a per-page override together; that gap is tracked as open issues in all three mature projects. |
| Zoom across a spread | **One shared scale/pan, top-aligned** | Book convention; keeps a single `view`. |
| Keypress unit | **One turn** | How reading works; needs no second cursor. |
| Sequence start | **First page alone** | Book convention. Modelled as `lead_lone`, tunable. |
| Arrow direction | **One toggle, `manga_rtl` default ON** | Mihon: RTL books are read RTL, so the keys should track the hand. |
| Full-res in a spread | **Cursor page only** | `MAX_TILES` is ~256 MB today and doubling it rewrites the tile paint path. |

---

## 3. Data model

`ImgKey` (`src/lib.rs:839-845`) already derives `Hash + Eq + Ord` and is the identity every
pixel cache uses. It is the right unit here too — a turn straddling two files is already
expressible.

```rust
/// One page of the flat sequence a gallery walks.
pub type PagePos = (usize, u32);   // (gallery index, page within a multi-page document)

/// What a page looks like, which decides whether it takes a whole turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape { Single, Double }

/// The aspect ratio at or above which an untagged page is a scan of two printed pages.
/// 1.2, not 1.0: a real page is ~0.707 and a real spread ~1.414, so 1.0 sits on the
/// midpoint and would flag a square-ish single page. Every reader in the field uses
/// 1.0; Mihon's newer WebGPU reader uses 1.2.
pub const SPREAD_ASPECT: f32 = 1.2;

/// How far two pages' aspect ratios may differ and still be one spread. Mihon's
/// `pairAspectTolerance`.
pub const PAIR_ASPECT_TOLERANCE: f32 = 0.1;
```

### The accessor the pairing runs against

Pairing must be testable with no filesystem, so it runs against a four-method trait.
`Gallery` plus the shape cache implements it for real; a `Vec<Shape>` implements it in
tests.

```rust
pub trait PageCtx {
    /// Step the flat page cursor by `by` pages, honouring `loop_back`.
    /// `None` at the ends when looping is off. The ONLY primitive that moves.
    fn step(&self, from: PagePos, by: isize) -> Option<PagePos>;
    /// The key for a position, for the shape cache and the loader.
    fn key(&self, pos: PagePos) -> Option<ImgKey>;
    /// Cached display dimensions. `None` = not learned yet.
    fn dims(&self, pos: PagePos) -> Option<(u32, u32)>;
    /// The user's override for this page, if any.
    fn override_shape(&self, pos: PagePos) -> Option<Shape>;
    /// The page's ordinal in the whole gallery, so it can be compared with
    /// `lead_lone`. `None` when a preceding file's page count is not yet learned.
    /// Only ever asked about the first few pages of a gallery, where the
    /// cursor's having arrived there guarantees the counts are known.
    fn flat(&self, pos: PagePos) -> Option<usize>;
}
```

`step` wrapping through `loop_back` means no `len()` is needed, so the trait never forces
a full enumeration of the gallery.

---

## 4. The pairing core

The cursor is **always a turn anchor** — a page a turn opens on. That invariant is what
makes the whole thing cheap, and it is worth stating first because the obvious formulation
of this is wrong.

### The trap

The naive predicate is "could this page pair with the next one?" used as *is this page
alone?* — and it silently breaks:

> Page 4 is portrait and page 5 is landscape, so 4 "cannot pair with 5", so 4 is alone.
> **False.** Page 4 paired with page 3.

Asking whether `j` pairs with `j+1` cannot distinguish *"j is the first half of a pair"* from
*"j is alone"* — it does not know what happened behind `j`. Every formulation that tries to
answer "is `j` alone?" from a local look at `j` and `j+1` is wrong for exactly this case,
and a wrong pairing is invisible until you are three pages into the wrong book.

So: **"is this page alone?" is only ever asked at an anchor**, where by construction
nothing paired from behind, and there it is a single local test.

### The formulation

```
pairable(j, k)  =  shape(j) != Double
                && shape(k) != Double
                && (dims(j) is None || dims(k) is None
                    || |ratio(j) - ratio(k)| <= PAIR_ASPECT_TOLERANCE)

/// Does the turn anchored at `s` hold only that page?
/// VALID ONLY AT AN ANCHOR - which the cursor always is.
/// `s < lead_lone` is a flat-page comparison, via `PageCtx::flat`.
lone(s)         =  flat(s) < lead_lone                // front matter
                || shape(s) == Double                // a fold-out
                || !pairable(s, step(s, +1))         // nothing to pair with

turn_len(s)     = 1 if lone(s) else 2
next_anchor(a)  = a + turn_len(a)

/// The anchor before the one anchored at `a`. `None` at the start.
prev_anchor(a):
    b = step(a, -1)
    if b is None              { return None }          // start of the gallery
    if flat(b) < lead_lone
       || shape(b) == Double
       || !pairable(step(b,-1), b)   { return Some(b) } // b took a whole turn alone
    return Some(step(b, -1))                          // b was the second of {b-1, b}
```

`lead_lone` defaults to **1** (the cover). `O` adjusts it; see §6.

Forward is one addition. Backward is **two local `pairable` calls** — `b` either took a
whole turn alone, or it was the second half of `{b-1, b}`, and those are distinguished by
asking whether `b` paired with `b-1`. Both tests are reads, not scans.

### Worked trace

Pages `0`–`7`, all portrait except page 5, which is landscape. `lead_lone = 1`.

Forward from anchor 0:

```
anchor  lone?                          why                        turn
  0     true                           flat(0) < lead_lone       {0}
  1     false                          pairable(1,2)              {1,2}
  3     false                          pairable(3,4)              {3,4}
  4     --                             not an anchor, never asked
  5     true                           shape(5) == Double         {5}
  6     false                          pairable(6,7)              {6,7}
```

Backward from anchor 6: `b = 5`, `shape(5) == Double` ⇒ `prev_anchor(6) = 5` ✓
Backward from anchor 5: `b = 4`, `shape(4)` is not Double and
`pairable(3,4)` holds (both ~0.70) ⇒ 4 was the second of `{3,4}` ⇒ `prev_anchor(5) = 3` ✓

Result: `{0} {1,2} {3,4} {5} {6,7}` — 4 spreads over 6 pages, which is Komga's own
assertion for this shape (`book-spreads.spec.ts`: 6 pages → 4 spreads, reindexed to
0-based). With `lead_lone = 0` it becomes `{0,1} {2,3} {4,5} {6,7}`, which is the reported
failure case of a scan that dropped its blank front-matter page.

Note the guard at anchor 4 in the trace above: **`lone` is never evaluated there.** That is
the whole point of the invariant, and the exhaustive round-trip test in §10 is what proves
it stays true.

### Why this needs no page map

Mihon needed a `TreeSet` of lone indices because it looks up an arbitrary prefix of an
unknown-length chapter. Aidoku maintains an `isolatedPages` set *and* a
`shiftIsolation(after:by:)` to repair its indices when it physically inserts split pages.
Kavita precomputes the entire pairing table server-side and must invalidate the whole thing
when a file changes. We have none of that, because **every query is relative to a cursor
that is always an anchor**, and both directions are a bounded number of local reads.

This is also why no whole-book precomputation appears anywhere in the design: a precomputed
table would have to be rebuilt whenever a dimension is learned, a file is re-exported, or
the filter changes — three invalidation paths for zero benefit, since the table is never
read more than one turn at a time.

### The irreducible ambiguity, and why it is survivable

**A single landscape page and a double-page scan are geometrically identical** — both about
1.414. No aspect-ratio detector can separate them. Every reader in the researched field has
this bug (Paperback #251: *"the double page always start on the left panel"*; Aidoku #939:
wide images *"forced into double-page layout, resulting in black screen"*).

Two things contain it:

1. **`pairable`'s tolerance check.** A landscape page will not pair with a portrait one; it
   takes its own turn.
2. **The shared scale.** A wrongly-paired page is *never distorted* — both halves render at
   one scale, so a mismatched partner shows with a gap under it.

So the detector's only job is **whether to look for a partner**, never **which side**. A
wrong guess costs a pairing, not a broken picture. This is why the feature is safe to ship
with a heuristic that we know is imperfect.

---

## 5. Where dimensions come from

Three sources, cheapest first. **No folder scan reads anything** — same rule as
`page_counts`.

1. **Free: `Preview.orig`** from a decode we already pay for. Neighbour preloads run at
   priority 12–15 and report their own dimensions. Zero cost.
2. **`page_dims(path, page) -> Option<(u32,u32)>`** — the one new I/O capability. Reads a
   short prefix (~64 KB, enough to clear a JPEG's APP1/EXIF segment and reach the SOF
   marker), falling back to a full read if the format will not resolve from a prefix.
   Returns **stored** dimensions; the shape test applies `exif_orientation` so a page stored
   landscape with an EXIF rotate is judged as it displays.

   This is `tiff_page_count`'s shape exactly — read a few bytes, give up cheaply, learn on
   arrival. Its doc comment already says the cost is *"four bytes for a non-TIFF"*.
3. **Unknown ⇒ `Shape::Single`, and an unknown ratio never blocks a pair.** Being
   optimistic is deliberate: refusing to pair on missing data would strand a turn, which is
   worse than a pair that turns out to be wrong.

Cached in a `HashMap<PathBuf, Vec<(u32, u32)>>` beside `page_counts`, learned by
`Gallery::learn_shapes` on arrival and dropped by `invalidate_path` exactly as
`forget_page_count` drops a page count.

**Stability.** A learned shape never changes, so the pairing cannot oscillate within a
session. The first visit to a cold book may shift once as dims arrive; it self-corrects on
the second visit. This is Mihon #3812's *"when cache deleted the dual page works"* avoided
by caching the dimensions rather than the verdict.

---

## 6. Persistence

`%LOCALAPPDATA%\image-viewer\spreads.json` — `%LOCALAPPDATA%` and not `%APPDATA%` for the
same reason `thumb_cache_root` is: it is a cache of *this machine's* reads of files that may
be on a network share.

```json
{
  "<gallery root>": {
    "lead_lone": 1,
    "pages": { "<relative path>": { "<page>": "single" } }
  }
}
```

- **Keyed by the gallery root**, which for an archive is the **original** path, never the
  `%TEMP%` extraction path. Persisting a temp path is the mistake `AGENTS.md` records for
  `config.folder`, and it would orphan every override whenever the archive changes.
- `lead_lone` is the `O` key's persisted value. `0` means "no lone cover", which is the fix
  for a scan that dropped its blank front-matter page — the single most-reported failure in
  every mature reader (Komga #123, #2132 and PR #1236; OpenComic #456; Paperback #251).
  Komga's maintainer explicitly refused to implement it (*"I have no intention on adding
  dynamic double-pages"*); it is cheap and it is the top user request.
- Overrides are **beaten by nothing** and beat the detector outright.
- The Settings panel states the path and how to clear the file, beside the thumbnail-cache
  line. A file that silently re-pairs a library must not be enabled quietly.

---

## 7. Paint and layout

`central` (`src/main.rs:2636`) currently derives one `rect` from the panel and one
`src_size`. In a spread:

- The panel splits into **two halves with a zero gutter**, so facing pages touch and a
  drawing spanning the seam is unbroken. This is OpenComic #479's shipped fix (*"some
  mangas have nice double page drawings, that get sadly interrupted by the black vertical
  gap"*). Mihon and Aidoku also use zero.
- **One shared scale** = the more-constrained fit over whichever pages are present, so both
  fit. Then **top-aligned**.
- A **lone page rests with the seam on the viewport centre** — it keeps its own half rather
  than centring. Mihon's reason verbatim: *"centring the span would pull it to the middle,
  indistinguishable from a page with no partner."* So in RTL the cover sits on the right
  with the left half empty.
- **A lone page therefore renders at half the panel's width**, by the same token as
  `width = left + right` with a missing side contributing zero. That is correct and
  intended — the cover of a manga volume is not twice the size of its inner pages — but it
  reads as a bug on first sight, so it is stated here and asserted in a test.
- The **cursor page owns everything view-shaped**: `footprint`, the crop frame, `dim_bands`,
  `crop_from_screen`/`crop_screen_rect`. These currently derive from the single `rect`; in a
  spread they derive from the cursor page's rect only.

Gutter is zero and not a setting. Mihon and Aidoku have none; OpenComic derives its gutter
from the page margin. YAGNI until someone asks.

---

## 8. Navigation and keys

The cursor stays a flat `(index, page)` position, and **`Gallery::next`/`prev` remain the
only primitives that move it.** The pairing is arithmetic on top. Consequence:
`tests/multiframe.rs:697-918` keeps testing what it tests, unchanged, and `Gallery`'s
documented *"a cursor, not a list"* shape is untouched.

- **Forward:** `anchor = next_anchor(anchor)` — one addition.
- **Backward:** `anchor = prev_anchor(anchor)` — two local reads.
- The turn painted is always `[anchor]` or `[anchor, anchor+1]`, decided by `turn_len`.
- `Gallery` gains exactly two things: `goto_index_page(usize, u32)` and the shape cache.

### `manga_rtl` (default **on**)

| | `manga_rtl = true` (default) | `manga_rtl = false` |
|---|---|---|
| Turn opens on | **right** | left |
| `→` | moves right on screen = **backwards** | forwards |
| `←` | forwards | backwards |
| `Space`, `PageUp`/`PageDown`, Extra1/Extra2 | follow the arrows | follow the arrows |
| Mouse wheel | **unchanged** | unchanged |

One setting, not a key hack: reading direction is a property of the book, and it drives
both the anchor side and the key mapping, exactly as Mihon's `isReversed` does.

**The arrows, `Space`, `PageUp`/`PageDown` and both mouse buttons move as one unit.** This is
the trap this feature has to survive: OpenComic ships a live bug from omitting exactly one of
them — `Space` is not in its `mangaInvert` set, so **Space silently becomes "previous page"
in manga mode**. A test asserts all five agree.

**The wheel does not flip**, because wheel-down means "next" in both directions. This is
Mihon's deliberate choice and the one control with no spatial meaning to preserve.

**Two consequences stated rather than discovered later:**

1. `→` means "next" in single mode and "backwards" in RTL manga mode. This is the price of
   matching the screen, and it is what Mihon, Komga and Kavita all ship.
2. `pending_advance` (`src/main.rs:3462-3476`) reads raw `ArrowRight`/`PageDown`/`Space` to
   pick a direction for hold-to-browse. It must read the **mapped** direction or it will
   silently disagree with the setting.

### Cursor-relative everything else

`Del`, `Ctrl+S`, `Ctrl+C`, turn/mirror, wallpaper and EXIF act on the **cursor page only**.
Deleting a turn would be a new destructive action. `Ctrl+S` on a page stays refused for the
reason `AGENTS.md` records. Cropping a spread is cropping one page.

Deleting the cursor page of a pair leaves the partner, which becomes a lone page on the
next turn. That is the right behaviour — the alternative, silently deleting two files, is
exactly the class of action `tests/keybindings.rs` exists to prevent.

### Interactions that must be defined rather than left to fall out

| | In manga mode |
|---|---|
| `Home` | the turn anchored at the gallery's first page |
| `End` | the last turn, found by walking back from the final page |
| `Del` | the cursor page only; the partner survives and becomes lone |
| Grid (`G`) | **unchanged** — one cell per file, page 1 only, as it is today. Picking a cell enters manga mode on the turn anchored at that file's first page. |
| EXIF panel | the cursor page's metadata |
| `W` wallpaper | the cursor page, copied out as it is for an archive member |
| Wheel | unchanged (see the table above) |

### New bindings

| Key | Action | Why it is safe for `BARE_LETTERS` |
|---|---|---|
| `M` | toggle manga mode | view-only |
| `D` | toggle `manga_rtl` | view-only |
| `O` | decrement `lead_lone`; `Shift+O` increments. Clamped `0..=4`. Persisted. | writes only to `spreads.json` under `%LOCALAPPDATA%` |
| `X` | toggle the cursor page between single and double | writes only to `spreads.json` |

`O` and `X` write, but only to a file this app owns, in the same class as `W` (wallpaper,
which writes to the OS). Neither can touch a user's image.

---

## 9. Loader, planner, config

- **`Plan` gains `partner: Option<ImgKey>`**, planned at `PRIO_CURRENT_PREVIEW` so both
  halves arrive as a pair rather than the partner trailing at neighbour priority 12.
  Defaults to `None`, so `tests/flip_through.rs` is unaffected.
- **`Msg::Preview` needs no change.** It is keyed by the key it carries and already inserts
  into the cache whatever arrives.
- **`protected_preview_keys` needs no change.** The partner is index ±1, well inside
  `PRELOAD_RADIUS = 3`.
- **`PREVIEW_CACHE_CAP = 16`** holds two pages per turn without strain.
- **`display_dim` halves** in a spread, so `preview_target` is computed from the half panel.
- **Config:** `manga_mode: bool` (`#[serde(default)]`) and `manga_rtl: bool`
  (`#[serde(default = "default_true")]`). Both get the Settings-panel checkbox pattern at
  `src/main.rs:4272-4518`. No `deny_unknown_fields`, ever.
- **`Gallery` additions:** `goto_index_page`, `learn_shapes`, `forget_shapes`.

### Known limitations

1. The partner page renders at **preview resolution** when you zoom past 1:1. At
   fit-to-window a 4096 px preview is already 1:1 on a 4K screen, so this only appears on a
   deliberate zoom. Full-res per turn is blocked on the ~256 MB `MAX_TILES` ceiling.
2. The first visit to a cold book can shift the pairing once as dims arrive.
3. A single landscape page and a double-page scan are geometrically identical — a limit of
   the input, contained by the tolerance check and ended by the override.
4. No content-based detection, deliberately (§1).
5. An animated page in a spread animates while its partner stays still. Weird, harmless.

---

## 10. Testing

New target `tests/spreads.rs`. One test per guard, **each verified to fail with the guard
removed** — a test that passes on the broken code is decoration.

**The pairing**
- The §4 trace, asserted turn by turn, forward and backward. Page 4's absence from the
  `lone` column is itself an assertion: **`lone` must never be evaluated at a non-anchor.**
- **Exhaustive**: for every length and every shape pattern, walking forward N turns visits
  every anchor exactly once, no two turns overlap, and every page is in exactly one turn;
  forward-then-back-N returns to the start. `tests/property.rs` already has proptest. The
  round-trip and the no-overlap check are what catch a wrong `prev_anchor` — the naive
  predicate in §4's trap passes a forward-only test and fails these.
- `lead_lone` 0, 1 and 2 → `{0,1}{2,3}…`, `{0}{1,2}…`, `{0}{1}{2,3}…`.
- `loop_back` off: `next_anchor`/`prev_anchor` at the ends do not move and do not wrap.
- A spread straddling two files, and one straddling two pages of a multi-page TIFF.
- The last turn of an even count, and a single-file gallery.

**Detection**
- Ratio 1.2 is not a spread; 1.21 is.
- A landscape page beside a portrait page does **not** pair (the tolerance guard — Paperback
  #251, Aidoku #939).
- Unknown dims ⇒ `Single`, and an unknown ratio does not block a pair.
- `page_dims` returns stored dims; the shape test applies EXIF orientation.
- A learned shape does not change, so the pairing is stable across repeated walks.

**Persistence**
- An override beats the detector and survives a reload from disk.
- `invalidate_path` drops both the cached dims and the override.
- A gallery rooted at an archive is keyed by the original path, never the temp path.
- A malformed `spreads.json` is an empty override set, never a failure to start.

**Keys — the trap that started this**
- One test asserting arrows, `Space`, `PageUp`/`PageDown` and both mouse buttons all move the
  same way as each other. "The arrows swapped" is otherwise unobservable from a test.
- `pending_advance` resolves hold-to-browse to the same mapped direction.

**Must keep passing, unmodified**
- `tests/multiframe.rs:697-918` — the page-walk and cursor-range assertions. This is the
  evidence the page cursor was not disturbed.
- `tests/flip_through.rs` — including the strand check at :495-506 and the 3-image wrap at
  :572-598.
- `tests/keybindings.rs` — extend `BARE_LETTERS` and the brace-matched `!typing` block
  assertion at :28-51; add a legacy-config-load test for the two new fields.

---

## 11. Files touched

| Concern | Location |
|---|---|
| `Shape`, `PagePos`, `PageCtx`, `pairable`, `lone`, `turn_len`, `next_anchor`, `prev_anchor`, `page_dims` | `src/lib.rs` (new) |
| `Gallery`: `goto_index_page`, `learn_shapes`, `forget_shapes` | `src/lib.rs:5868+` |
| `Plan.partner` | `src/lib.rs:1099-1119` |
| `Config.manga_mode`, `Config.manga_rtl` | `src/lib.rs:167-239` |
| Overrides: load, save, `root_path` | `src/lib.rs` (new, beside the thumb cache) |
| `shown` becomes a turn; second `rect`; gutter split | `src/main.rs:2636-3071` |
| Cursor-relative crop / `footprint` | `src/main.rs:2965-3060` |
| `schedule()` passes `partner` | `src/main.rs:1571-1611` |
| `step()` walks turns | `src/main.rs:1483-1499` |
| `pending_advance` mapped direction | `src/main.rs:3462-3476` |
| Keys `M`/`D`/`O`/`X`, direction mapping | `src/main.rs:3842-3856`, `3966-4027` |
| Settings panel rows | `src/main.rs:4272-4518` |
| New suite | `tests/spreads.rs` |
| Docs | `README.md` (feature, keys), `AGENTS.md` (traps) |