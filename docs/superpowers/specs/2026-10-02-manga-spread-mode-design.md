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
/// VALID ONLY AT AN ANCHOR - which the cursor always is, because `goto` snaps it.
/// "front matter" is `is_lead(s)`, NOT `flat(s) < lead_lone` - see below.
lone(s)         =  is_lead(s)                         // front matter
                || shape(s) == Double                // a fold-out
                || !pairable(s, step_no_wrap(s, +1)) // nothing to pair with

turn_len(s)     = 1 if lone(s) else 2
next_anchor(a)  = step(a, turn_len(a))               // the ONLY wrapping step
anchor_of(p)    = walk forward from page 0 to the turn holding `p`

/// The anchor before the one anchored at `a`. **TWO meanings of `None`:**
/// there is no turn before the first, AND `a` is not an anchor at all.
prev_anchor(a):
    anchor = page 0
    loop:
        next = step_no_wrap(anchor, turn_len(anchor))
        if next is None  { return None }              // ran off the front
        if next == a     { return Some(anchor) }
        anchor = next
```

`lead_lone` defaults to **1** (the cover). `O` adjusts it; see §6.

**`is_lead(s)`, not `flat(s) < lead_lone`.** Two reasons, both measured. `Gallery::flat`
sums the page counts of every file before `s` with a `PathBuf`-keyed lookup each, so the
walk cost **117-385 ms** on a folder of 2000 loose images (release, cursor at the far end)
— and `flat` is `None` whenever a *preceding* file's count is unlearned, so the guarded
spelling that replaced it answered "not front matter" for a front page behind an unlearned
file. `step_no_wrap(s, -(n as isize)).is_none()` asks the position arithmetic: O(1),
always answerable, and the same statement.

**Forward is one addition; backward is a walk from page 0, and it has to be.**
`prev_anchor` cannot be a local test: on `[Single, Single, Single, Double]` with
`lead_lone = 0`, forward gives `{0,1} | {2} | {3}` — so page 2 *is* an anchor, it went alone
because nothing was ahead of it — yet `pairable(1, 2)` is true, so "could 2 pair with the
page behind it" cannot tell that from "2 is the second half of `{1,2}`". Whether `b` is an
anchor depends on `b-1`'s turn length, which depends on `b-2`'s, so it only resolves at the
front. **Three local formulations were tried and all three are wrong** (enumerated in
`prev_anchor`'s doc comment); the walk is in-memory arithmetic over already-cached
dimensions. The earlier claim in this document that backward is "two local reads" is
**withdrawn**.

### Worked trace

Pages `0`–`7`, all portrait except page 5, which is landscape. `lead_lone = 1`.

Forward from anchor 0:

```
anchor  lone?                          why                        turn
  0     true                           is_lead(0)                 {0}
  1     false                          pairable(1,2)              {1,2}
  3     false                          pairable(3,4)              {3,4}
  4     --                             not an anchor, never asked
  5     true                           shape(5) == Double         {5}
  6     false                          pairable(6,7)              {6,7}
```

Backward is the same tiling read from the other end: `prev_anchor(6) = 5` ✓,
`prev_anchor(5) = 3` ✓, `prev_anchor(0) = None` (first turn) ✓. All three come out of the
walk, not out of a local test on the page behind the cursor.

Result: `{0} {1,2} {3,4} {5} {6,7}` — 4 spreads over 6 pages, which is Komga's own
assertion for this shape (`book-spreads.spec.ts`: 6 pages → 4 spreads, reindexed to
0-based). With `lead_lone = 0` it becomes `{0,1} {2,3} {4,5} {6,7}`, which is the reported
failure case of a scan that dropped its blank front-matter page.

Note the guard at anchor 4 in the trace above: **`lone` is never evaluated there.** That is
the whole point of the invariant, and the round-trip property in §10 is what proves it
stays true — over **256 random** galleries, not over every shape pattern.

### Why this needs no page map

Mihon needed a `TreeSet` of lone indices because it looks up an arbitrary prefix of an
unknown-length chapter. Aidoku maintains an `isolatedPages` set *and* a
`shiftIsolation(after:by:)` to repair its indices when it physically inserts split pages.
Kavita precomputes the entire pairing table server-side and must invalidate the whole thing
when a file changes. We have none of that, because **every query is relative to a cursor
that is always an anchor**, and no state is carried between queries. **Both directions
cost O(distance from the front)**, because the walk that finds an anchor starts at page
0 — this document originally claimed "forward is O(1); backward is O(distance from the
front)", and **that "O(1)" was wrong**: `next_anchor` is one addition *when asked of an
anchor*, but the navigation the app actually performs is `next_turn`, which snaps to the
anchor first, so a forward press pays the whole walk. The walk is in-memory arithmetic
over cached dimensions and is measured at ~5-10 `dims` reads per turn visited
(`tests/spreads.rs::a_navigation_walk_costs_one_turn_len_per_turn_behind_the_cursor`),
so the pathological case is a press at the back of a 2000-file gallery — microseconds
against a decode measured in hundreds of milliseconds, and not a hang, but not O(1)
either. The deferred fix is a per-gallery anchor list; see `TODO.md`.

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

Cached in a `HashMap<ImgKey, (u32, u32)>` beside `page_counts`, learned by
`Gallery::learn_shapes` on arrival and dropped by `invalidate_path` exactly as
`forget_page_count` drops a page count. **Keyed by `ImgKey { path, page }`, not by path** —
a document with a landscape fold-out among portrait pages has one shape per page, and one
entry for the file would judge page 3 by whatever page 0 happened to be. `learn_shapes` also
reads **the next page, in the same document only**, because `lone` decides a turn by asking
about `anchor + 1`: learning only the page under the cursor means arriving at page 5 changes
page 5's shape, which changes `lone`, which changes the turn the reader is already looking
at.

**Stability.** A learned shape never changes for an unchanged file, so the pairing cannot
oscillinate within a session — and `note_shapes` (the decode-reporting route) is **fill-only**,
so an approximation that arrives second cannot displace a header measurement. The first visit
to a cold book may shift once as dims arrive; it self-corrects on the second visit. This is
Mihon #3812's *"when cache deleted the dual page works"* avoided by caching the dimensions
rather than the verdict.

---

## 6. Persistence

`%LOCALAPPDATA%\image-viewer\spreads.json` — `%LOCALAPPDATA%` and not `%APPDATA%` for the
same reason `thumb_cache_root` is: it is a cache of *this machine's* reads of files that may
be on a network share.

```json
{
  "<gallery root>": {
    "lead_lone": 1,
    "pages": { "<bare file name>": { "<page number>": "single" } }
  }
}
```

- **Keyed by the gallery root**, which for an archive is the **original** path, never the
  `%TEMP%` extraction path. Persisting a temp path is the mistake `AGENTS.md` records for
  `config.folder`, and it would orphan every override whenever the archive changes.
- **Per-page overrides are keyed by bare file name and page number**, not by path — the same
  archive reason — and the page number is a nested level because `serde_json` cannot write a
  `(String, u32)` map key and nested objects are also what a person can edit. An unparseable
  page key is dropped rather than fatal, so one typo does not cost every override in the file.
- **The store is a read-modify-write, and that is the whole of its risk profile.** One file
  holds every gallery, so writing one gallery's entry over the top would delete every other
  gallery's decisions — and an unparseable store is replaced, so one bad byte becomes that on
  the *next* press. Three consequences followed from it and are load-bearing: a UTF-8 BOM is
  stripped on both the read and the write (in `Config::load` too, where the same BOM silently
  reverted every setting); the temporary is `spreads.json.tmp.<pid>` rather than a fixed name,
  because two instances writing the same file milliseconds apart would interleave; and an
  over-cap store is honoured as **empty** by a load but left **untouched** by a save, because
  the save's read is the first half of the write.
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
  spread they derive from the cursor page's rect only. None of them needed changing, which is
  the evidence that they were already about the cursor rather than about the picture.
- **Layout comes from dimensions, paint comes from textures.** `turn_pages` reads each page's
  *cached* dimensions and falls back to the cursor page's own for a partner nothing has
  measured. If the layout waited on the partner's texture instead, the spread would reflow
  when that texture landed — under the reader, mid-read — which is worse than a wrong slot.
- **Two rectangles per page, and conflating them paints the transform twice.** The *slot* is
  where the page goes (its transformed footprint, which is what `spread_rects` placed); the
  *paint rect* is what goes into `paint_tex`, and it stays in unrotated image space at the
  page's stored dimensions, centred on the slot. Handing `paint_tex` the slot paints a
  `Rot90` page's transform a second time, and the picture lands on the swapped rectangle.
  Each page gets its **own** `Xform`, looked up by its own path.

### The seam is not the content's centre

**The seam sits on the panel's centre and the content's centre does not**, which is the one
assumption this feature broke twice, both times silently and both times reported by a user
running the app:

1. **The zoom anchor.** `anchor - panel.center()` is correct only when the content is centred
   on the panel. A turn's content is displaced on **both** axes — horizontally by half the
   width difference (zero for two equal pages, which is why it hid), vertically because the
   turn is top-aligned, so its centre is the panel top plus half the *tallest* page. The
   anchor is measured with `spread_centre` **asked at two scales**, because the content's
   centre is a function of the scale and bracketing the same content point between the two
   is what keeps it under the cursor.
2. **The pan clamp.** The symmetric `±(content·scale − panel)/2` bounds hold a content
   *centred on the panel's centre*. Panning moves content the other way, so the range is the
   symmetric one **translated by minus the displacement** — not narrowed, moved. Measured on
   the running app over 41 zoom steps of a 2000-wide and a 2800-wide page, the old expression
   truncated **36** of them, by up to **67 points**. The new bound is read off the same
   rectangles `spread_rects` returns, so the clamp and the picture are one geometry rather
   than two that agree today.

Gutter is zero and not a setting. Mihon and Aidoku have none; OpenComic derives its gutter
from the page margin. YAGNI until someone asks.

---

## 8. Navigation and keys

The cursor stays a flat `(index, page)` position, and **`Gallery::next`/`prev` remain the
only primitives that move it.** The pairing is arithmetic on top. Consequence:
`tests/multiframe.rs:697-918` keeps testing what it tests, unchanged, and `Gallery`'s
documented *"a cursor, not a list"* shape is untouched.

- **Forward:** `next_turn(a)` = `next_anchor(anchor_of(a))` — one snap walk plus one addition.
- **Backward:** `prev_turn(a)` — `prev_anchor`, and `anchor_of` **only when it answers
  `None`**, because that `None` is ambiguous (§4). `last_anchor` for the `loop_back` wrap.
- The turn painted is always `[anchor]` or `[anchor, anchor+1]`, decided by `turn_len`.
- `Gallery` gains exactly two things: `goto_page(PagePos)` and the shape cache.
  (`goto_page` takes a whole `(index, page)` position rather than two arguments, because
  every caller gets one out of a walk and a two-argument form invites a caller to
  assemble a position from a file index and a page number separately.)

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
| `End` | the **last** turn, found by `last_anchor` — a forward walk from page 0, *not* by walking back from the final page. That rule is unimplementable: `prev_anchor` answers `None` at a final page that is a second half, and the turn *before* the last one at a final page that opens the last turn, and neither is the last turn |
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
- **`protected_preview_keys` needs no change — but the premise was wrong, so do not
  trust it.** The partner of a turn across two files is index ±1, well inside
  `PRELOAD_RADIUS = 3`. **For a turn inside a multi-page document the partner is
  `(index, page + 1)`, not `ImgKey::first` of a neighbour file**, and the window
  inserts `ImgKey::first` of each neighbour — so an in-document partner is *not* in the
  protected set. It is not the LRU victim in practice because `central` stamps the
  on-screen page's `used` every frame, but that is the mitigation, not the policy. If
  the eviction policy ever changes to prefer anything other than `used`, this set is
  where the in-document partner has to be added. Recorded on
  `protected_preview_keys` itself rather than left here.
- **`PREVIEW_CACHE_CAP = 16`** holds two pages per turn without strain.
- **`display_dim` halves** in a spread, so `preview_target` is computed from the half panel.
- **Config:** `manga_mode: bool` (`#[serde(default)]`) and `manga_rtl: bool`
  (`#[serde(default = "default_true")]`). Both get the Settings-panel checkbox pattern at
  `src/main.rs:4272-4518`. No `deny_unknown_fields`, ever.
- **`Gallery` additions:** `goto_page`, `learn_shapes`, `forget_shapes`.

### Known limitations

1. **WITHDRAWN — no longer true.** This document originally listed "the partner page renders
   at preview resolution when you zoom past 1:1", and justified it as blocked on the ~256 MB
   `MAX_TILES` ceiling. That ceiling was never the constraint: the tile loop was filtered to
   the *cursor's* full buffer, while `sync_shown` adopts by `Arc::clone` rather than moving,
   so the partner's full-resolution buffer was **already in memory** at
   `previews[partner].full` and the expensive decode had already happened. Both halves are
   now cut from their own buffers, and a `Kind::Full` job is planned for a partner that has
   none (`Plan::partner_needs_full`) — which is the common case, not the corner one, because
   a source over `FAST_FULL_MAX_PIXELS` is decoded DCT-scaled and carries no full buffer, and
   manga scans are frequently large JPEGs. `MAX_TILES` stays one global bound counted across
   both pages, because cuts are on demand.
2. The first visit to a cold book can shift the pairing once as dims arrive, and self-corrects
   on the second visit.
3. A single landscape page and a double-page scan are geometrically identical — a limit of
   the input, contained by the tolerance check and ended by the `X` override.
4. No content-based detection, deliberately (§1).
5. An animated page in a spread animates while its partner stays still. Weird, harmless.
6. A folder of 3:2 photographs in manga mode is **one photograph per turn** — 1.5 is above
   `SPREAD_ASPECT`, so every photograph is judged a two-page scan. Inherent to a threshold
   detector, and the reason `SPREAD_ASPECT` is 1.2 rather than 1.0: a real page is ~0.707 and
   a real spread ~1.414, so 1.0 sits on the midpoint and calls a square-ish single page a
   spread. 3:2 is genuinely past that line.

---

## 10. Testing

New target `tests/spreads.rs`. One test per guard, **each verified to fail with the guard
removed** — a test that passes on the broken code is decoration.

**The pairing**
- The §4 trace, asserted turn by turn, forward and backward. Page 4's absence from the
  `lone` column is itself an assertion: **`lone` must never be evaluated at a non-anchor.**
- **Random, not exhaustive**: over 256 generated galleries (1..=12 pages, leading-lone
  0..=2, both `loop_back` settings, every page independently Single or Double), walking
  forward N turns visits every anchor exactly once, no two turns overlap, and every page is
  in exactly one turn; forward-then-back-N returns to the start. `tests/property.rs` already
  has proptest. The round-trip and the no-overlap check are what catch a wrong `prev_anchor` —
  the naive predicate in §4's trap passes a forward-only test and fails these. **256 draws do
  not cover 2^12 shape patterns**, so the cases that matter are additionally spelled out in
  named tests rather than left to the draw.
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
| `Gallery`: `goto_page`, `learn_shapes`, `forget_shapes` | `src/lib.rs` |
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