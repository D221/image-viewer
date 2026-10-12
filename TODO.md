# TODO

**Open work only.** Anything finished is in the git history, and the durable
lessons from it are in `AGENTS.md` — neither belongs in this file. Each item
states what is *known* (verified), what is *undecided* (needs a human) and what
is *unverified* (an honest gap), so the investigation does not have to be
repeated. `PLAN.md`, the early-project feature list, was deleted once its work
landed.

---

## 1. Display P3 → sRGB — done, except that "correct" is still unproven

Shipped: `convert_wide_gamut` in Settings, **off by default**. The matrix is
derived from the two spaces' chromaticities rather than copied from a spec, and
the derivation is checked against the D50-adapted primary measured in 192 real
profiles — it agrees to 5 units in 15-bit fixed point. `AGENTS.md` has the traps.

What is *not* established is that the result **looks** right. Every test here can
only say the maths is self-consistent: white stays white, the shared blue primary
is untouched, mid-tones move in the direction a wider gamut implies, out-of-gamut
colours clip. A matrix transposed in the wrong place passes most of those and
still looks wrong to an eye.

**To close it properly:** compare against something that converts. `test-images`
has two P3 JPEGs and JPEGView_L (also in `test-images`) converts; the open
question is whether the viewer and JPEGView_L agree *by eye* on the same file, and
what the reference is for a photo whose correct rendering nobody has recorded.

Also unverified, and cheap to check by hand: the **wallpaper** path. It is the one
consumer that reads the *file* rather than our buffers, so with the setting on it
decodes, converts and writes `%APPDATA%\image-viewer\wallpaper.<ext>` instead of
pointing the shell at the original. Nothing tests it.

Cost when on, measured: 166 MP/s across the thread pool, so ~35 ms for a 2 MP
preview and ~145 ms for a 24 MP full decode.

### Known, verified

- **200 of 1001** images in `phone_backup\Download\Ttg` were Display P3 when this
  was first measured; a re-run after HEIC support landed says **229**, the
  library having grown. Another 111 are sRGB-profiled, 641 untagged sRGB, 0
  unreadable. Re-measure with `--icc-scan` rather than trusting these.
- The primaries never mix: **192 files at exactly `(33759,15807,-69)`** (P3) and
  111 at `(28578,14581,912)` (sRGB), both D50-adapted. These are *measured* —
  do not "correct" them from memory, a guessed P3 value was wrong once already.
- **170 of the 200 P3 files have no `desc` tag at all**, so classification must
  work from primaries. That part is done (`icc_tags_valid`, `ColorSpace`).
- Display P3 is the *only* wide-gamut space in a real library. AdobeRGB, Rec.2020
  and "RGB-to-GBR" appear only in the synthetic charts in `test-images`, so a
  fixed 3×3 is enough and **lcms2 is not needed**.
- (Per-file numbers came from a throwaway probe; see git history. The detector
  itself is `image_viewer::color_info`.)

### Do it like this, so it is not written wrong

Convert primaries **in linear light**, not in gamma space — doing it in gamma
space is the classic mistake that yields dark, muddy images: decode sRGB→linear
with the sRGB curve, apply the P3→sRGB matrix, re-encode linear→sRGB.

**No transfer-function change is needed on the output side.** The destination
framebuffer is already sRGB-encoded 8-bit (egui wants a gamma-space surface), so
the sRGB curve in and out cancels; what is actually needed is only the primaries
matrix.

### Where to apply — undecided

- **In `load_image_preview_full`**, on the worker: the texture, the tiles and
  the clipboard's cached-full fast path come from this decode, so most consumers
  are correct automatically — at the cost of one LUT+matrix per pixel on the hot
  path, which is where the earlier instability came from. **Two are not**: the
  `Ctrl+C` fallback decodes with `load_image_full` on its own thread, and
  wallpaper never goes through our decode at all (`set_wallpaper` hands the OS a
  file *path*, so it needs a converted file, not a converted buffer).
- **As a separate post-pass**: zero risk to the decode path, but it has to cover
  the same five places, and the wallpaper one is not a pixel buffer.

### Safety requirement

The transform must fire **only** on a structurally-validated P3 profile.
Converting an sRGB image through a P3 matrix visibly wrecks it, which is the same
class of error as the reverse. `ColorSpace::WideGamut` alone is not sufficient
signal — confirm `red_primaries == Some(P3_D50_RED)` before touching pixels.

---

## 2. WebP colour — the last uninspected format

This item was written as "HEIF that states its colour with `nclx` instead of a
profile". **Running `--icc-scan` over the real library says otherwise.** Of 1001
images in `phone_backup\Download\Ttg`:

| | |
|---|---|
| wide gamut | **229** |
| sRGB profiled | 111 |
| sRGB untagged | 641 |
| unknown profile | 0 |
| not inspected | **20** — and all 20 are **13 WebP and 7 GIF** |

**Zero HEIF.** Every HEIF/AVIF in that library carries an ICC profile, so the
`nclx` case this item was about does not occur in it at all. The `nclx` handling
is still correct to keep — `None` must not be read as sRGB — it is simply not
where the remaining gap is.

(Done: GIF reports `UntaggedSrgb` — a GIF has no colour profile chunk, so by
the same convention untagged PNG and JPEG get. Landed with its test.
WebP reports `UntaggedSrgb` — a WebP has no ICC profile chunk, so by the same
convention untagged PNG and JPEG get. Landed with its test.)

It is not on the conversion path (§1): it only makes the report honest.

---

## 3. Multi-page TIFF: the colour types still past page 0

Pages work, and **CMYK now works too** — `cmyk_via_image` re-wraps a page's
samples in a one-page TIFF and lets `image` do the conversion, because `image`
owns the maths and cannot be pointed at page 2. Verified for CMYK8 and CMYK16.

Still refused past page 0, each an error rather than an approximation: **YCbCr,
Lab, palette, CMYK+alpha, multiband.** The blocker is not the decoder — `image`
can convert YCbCr and Lab — it is that the re-wrap trick needs a `tiff`-crate
*encoder* type for the source colour type, and there isn't a usable one:

- `tiff::encoder::colortype::YCbCr8` has `Inner = u8` with `BITS_PER_SAMPLE =
  [8, 8, 8]`, which cannot carry three samples per pixel. Writing a YCbCr page
  through it is not possible as written.
- palette needs the `colormap` tag applied, and Lab is not a photometric
  interpretation the encoder offers at all.

So this item is now "find or write a way to express those types in a one-page
TIFF", not "make `image` do the conversion". Worth doing only if real scans need
it; a 24-bit CMYK scan is the common print case and it is already covered.

---

## 4. Feature gaps, surveyed

Surveyed 2026-10-09 against the shipped feature set. **Zoom-to-cursor was on this
list and is not**: it was assumed absent and is in fact implemented and careful
(`main.rs` keeps the point under the cursor fixed, asking `spread_centre` at two
different scales so a turn's anchor holds as the layout moves). Do not "add" it.

Shipped from this survey: **undo** (`Ctrl+Z`/`Ctrl+Shift+Z`, view
transforms only), **ratings/tags** (XMP sidecars, 0-5 stars + flag + keywords,
with `stars:`/`flagged`/`tag:` in the filter box), **grid cell size** (setting,
150pt default) and **slideshow shuffle** (reshuffled every show, folder order
restored on stop). Both specced in
`docs/superpowers/specs/2026-10-09-undo-and-ratings-design.md`.

- **Histogram / exposure readout.** Nothing from the pixels themselves — the EXIF
  panel shows what the camera recorded, never what the file actually contains.
  Worth it on its own terms (clipping and black-point at a glance), and it is
  also **the cheapest instrument available for §1**: wide-gamut correctness has no
  ground truth, and a per-channel histogram plus black/white clipping counts is
  something that can be *compared* rather than squinted at. It would also inform
  the `Smooth` low-pass decision, which is currently made by looking at pages.

- **Metadata editing.** Caption, artist, description: all absent, and there is no
  write path at all — `nom-exif` is read-only in this tree. Blocked on the same
  question as the ratings' storage decision (§ ratings above), and if the XMP
  sidecar lands first then most of this is the same parser with more attributes.
  **Do it after, not before.**

- **Comparison / light-table.** Declined by the user; recorded here only so it is
  not re-proposed from scratch. The reason is the app's own rule: N images
  resident at once is the one shape that fights "the UI thread only paints
  textures that are already in memory", and it needs N full-resolution buffers.

---

## 4a. Smaller queued items

- **The file-change watcher is unverified** (`check_changed`, `invalidate_path` —
  both in `main.rs`, so no test can reach them). Worth one manual pass: change a
  file on disk, confirm it reloads within a few seconds, *including while it is
  mid-write* — that is the case which would poison the file if `invalidate_path`
  ever stopped clearing `failed`.
- **Colour correctness has no ground truth** — blocked on decision 2 in §1.

---

## 5. Grid thumbnails: done, and the cold case is a floor rather than a bug

Shipped: a grid cell is a **different request** from a preview (`Purpose::Thumb`
in `lib.rs`), and there is a **persistent cache** in front of it
(`%LOCALAPPDATA%\image-viewer\thumbs`). `AGENTS.md` has the traps, the tests are
`tests/thumb_cache.rs`, the microbenchmark is `bench_browse thumb_cache_ab`, and
the end-to-end check is `tests/probe_thumb_cache.ps1`.

**Verified through the real app**, on a console session with no Remote Display
Adapter (`probe_thumb_cache.ps1`, presses `G`, then reads the exit report):

| | cold | warm |
|---|---|---|
| `grid thumbnails from cache` / `decoded (miss)` | 0 / 4 | **4 / 0** |
| `gallery open -> first thumbs` | 46.7 ms | **8.7 ms** |
| `worker: thumbnail decode` p50 | — | **1.0 ms** (p95 2.0 ms) |
| entries on disk | 4 (0.2 MB) | 4 |
| display-gap frames / stalls / worker panics | 0 / 0 / 0 | 0 / 0 / 0 |

Screenshots of both runs are in `%TEMP%\iv-thumb-{cold,warm}.png` and the grids
are pixel-identical, which is the visual half of "a hit is the same picture".

**Read the cold `first thumbs` figure correctly: it is 46.7 ms and not ~700 ms
because it measures the *first* cell, and with four cells queued at the same
priority the cheap GIF wins the race.** It is not the grid filling. The fill is
what `thumb_cache_ab` measures, and it is unchanged — see below.

**The finding that shaped it is negative, and it is the whole item.** A 95 MP
baseline JPEG has no cheaper full-image read in any conformant decoder: every
pixel needs every DCT coefficient, so the entropy decode *is* the floor. Measured
on this box, interleaved, min of 4, release:

| | 95.7 MP baseline 4:2:0, 27 MB | 16.8 MP 4:4:4, 26 MB |
|---|---|---|
| `fs::read` | 13 ms | 11 ms |
| DCT 1/8 (what the grid used) | **833 ms** | **580 ms** |
| DCT 1/1 | 1242 ms | 677 ms |
| cell texture that got uploaded | 5851 KB | 512 KB |

DCT scaling buys 25-30% and no more, so "a different approach" could not mean
"decode it more cheaply" — it had to mean *not decoding it again*, which is a
cache. Measured over 24 synthetic cells: **cold 535 ms, warm 9 ms**, i.e. 1.14 ms
per cell against 66.9 ms. On the real scans in `test-images` a warm cell is
0.6-1.8 ms against 700-900 ms.

What did **not** work, recorded so nobody re-derives it:

- **The progressive DC-scan trick is real, and useless here.** Truncating a
  progressive JPEG after its first DC-only scan and decoding that is **3.3x**
  (24.2 MP: 148 ms against 474 ms, reading 429 KB of 3.1 MB) — but **every large
  file in this library is baseline**: 0 of the 5 big scans in `test-images` and 0 of
  the 5 big photographs in `phone_backup\Download\Ttg`. It also has a correctness
  trap: a first scan with `Ns=1` is not interleaved, so truncating there yields one
  component's DC and a plausible wrong picture. Not built.
- **The transient full-size allocation is still there.** `dynamic_to_image_data`
  decodes at source size and resizes for every format with no reduced-size read
  (PNG, HEIC, JXL, TIFF). What the thumb path removed is the *retention* and the
  *second* downscale, not the allocation. Fixing that needs a row-streaming decode
  per format.

**Known gap, stated rather than hidden:** the first visit to a folder of large
scans is exactly as slow as it was — 24 cells at ~700 ms over 4 workers is still
~4 s, and nothing here touches that. The only two ways to move it are a faster
entropy decoder (there isn't one) or a thumbnail the file already carries
(embedded EXIF, already used, 15-40x — and the residue has none).

### Known, verified

- **A cell's texture was 44x what the cell asked for.** 1/8 of a 17000 px scan is
  2100 px, and `decode_jpeg_scaled` can only pick 1/8, 1/4, 1/2 or 1, so nothing
  ever landed on 256. With `THUMB_CACHE_CAP` at 512 that is a **2.9 GB** ceiling on
  the grid's texture cache. The downscale now happens in `load_thumbnail`, once,
  after every return point of `decode_page` — the same argument the wide-gamut
  wrapper makes.
- **A cell of an animation used to decode the whole loop.** `Kind::Thumb` went
  through `load_page_preview_full`, which decodes up to 240 frames and 256 MB and
  then keeps frame 0. The 900x318 GIF in `test-images`: **344 ms → 7.9 ms**, and
  1117 KB of texture → 90 KB.
- **A lossy thumbnail format was rejected on purpose.** A q90 JPEG cell would be
  ~8x smaller on disk and would give up the one property worth asserting: that a
  cache hit is *byte for byte* the decode it replaced.
- Deflate effort for the stored entry, measured (bytes / encode time): `Best` buys
  under 1% of size for 15-36x the time, and that time lands *before* the cell's
  message is sent. `Fast` it is. An entry is 13-80 KB, about 1.5 bytes/pixel —
  deflate is bad at photographic content — so 256 MB is 3 000-19 000 cells.
- **A truncated entry was a *hit*, not a miss.** `image`'s PNG decoder accepts a
  stream missing its last 64 bytes and returns a full-size image with the tail
  zeroed, so a half-copied file produced a cell with a black band across it and no
  error anywhere. PNG's own `IEND` terminator is the exact and free check. Found by
  the damaged-entry test, which is why that test enumerates damage instead of
  asserting one case.

### Still open, in order of value

- **The animation speed claim is still microbenchmark-only.** Now that a console
  session is available it *could* be measured end to end (a scratch folder of
  only animations, `G`, and the exit report) the same way the cache was. It was
  not done because `load_thumbnail` on a GIF is the exact code the app calls and
  `thumb_cache_ab` measures it on this box in release — so the number is
  legitimate, just not end-to-end. Worth doing only if the GIF case is ever
  reported to a user.

- **A timing-ratio unit test for the animation fix was tried and abandoned.** Two
  consecutive artefacts: the arm that ran first absorbed every one-time cost in the
  process (550 ms against 53 ms — a "10x regression" that was pure warm-up), and
  after interleaving and min-of-two it was rayon's global thread pool being built by
  the first `linear_resize` (~450 ms in debug), landing on whichever arm went first.
  The speed claim lives in `bench_browse thumb_cache_ab`; the unit test pins the
  pixels. Same class as the four RDP conclusions in `AGENTS.md`.
- **`THUMB_TRIM_EVERY` (64 writes) is a guess** — it trades a directory walk against
  how often the cache is over its cap. Nothing here has ever reached 256 MB, so the
  trim is tested directly (`prune_thumb_cache`) but the *cadence* is unmeasured.
- **Archive members cache under their temp path.** A `.cbz` extracts to
  `%TEMP%\image-viewer\<hash(path, mtime, size)>` and that hash changes when the
  archive does, so editing an archive orphans every member's cell. They age out
  like anything else; no special case was written.

---

## 6. Manga mode: verified by arithmetic, and nobody has looked at a spread

Shipped. The pairing rules and the layout geometry are pure functions in
`lib.rs` and are pinned behaviourally in `tests/spreads.rs`; the traps are in
`AGENTS.md` under **Spreads and reading direction**; the user-facing description
and the three limits a user cannot work around are in `README.md`.

**What is unverified is the part a test cannot reach.** `main.rs` is unreachable
from every integration test, so the paint and navigation wiring is covered by
source scans and by one-off geometry dumps from a temporary instrumentation.
The displaced-centre numbers, the zoom anchor's two-scale form (matched to the
running app's own per-frame offsets to ~0.002 pt over six zoom steps) and the
pan-clamp counts all come from those dumps. **That evidence is not a test and
does not survive in the tree.**

**Six of the guards added for this feature were no-ops** — the call existed, in the
right function, and moving or deleting it changed nothing observable, because the
thing it guarded was itself unreachable. So the scans below are not decorative, but
the *number* of scans is not the measure of coverage either; the fix wave that
produced the three critical findings below added **five more scans, of which the
decisive one checks a call's position inside the file rather than its presence**,
because the bug was a call that was present and in the wrong block.

**Screen capture cannot see this app's swapchain on this box** — three routes,
all blank, including for the untouched single-image path and for a solid-red
fixture. So this is not "the screenshot was hard to get": on this machine it is
currently impossible, and the substitution (dump the numbers, match the app's own
values) is weaker than a look, because every number it checked was chosen by the
same person who wrote the code.

**To close it:** one pass on a console session. Open a book, turn through it, and
check the four things the geometry cannot tell you — that the seam is where the
reader expects it under both `D` settings; that a lone page sits where a cover
should and not centred; that the two halves are sharp together at a deliberate
zoom; and that a turned page inside a turn stays inside its own half. A
photograph folder with `M` on is the other half of it (one photo per turn, as
`README.md` says). Until then the feature is **arithmetically correct and
unseen**, and that is the honest state of it.

### The checklist as it stood would have missed both critical bugs

The four checks above are all *visual*: seam, cover, sharpness, rotation. **A picture
can be laid out perfectly and still be the wrong picture**, which is what the final
review's two critical findings were, and neither of them changes where anything is on
screen. So three more, and each is named after the failure it would have caught:

- **Press `M` on a stationary cursor and confirm a spread appears.** With the cursor
  not moving, `shown_partner` was latched at the last adopted preview, so `M` flipped
  the setting, halved the panel and laid out **one** page: a half-width picture beside
  an empty half, with no error and no counter, until the next `→` produced a correct
  spread. The visual checks pass on that screen — a lone cover *is* what a lone cover
  looks like, and a lone page sitting at half width beside nothing is exactly what the
  seam rule prescribes. **The tell is that nothing appears where a spread should be**,
  so the check has to be "a spread appeared", not "the page looks right". `O` and `X`
  have the same latch and the same check.
- **Open a multi-page document, page forward, zoom, and confirm the tiles are *that
  page's*.** `self.full` was keyed by path, and `decode_page` returns no full buffer
  for a page past the first — so page 0's pixels were cut with page N's `uv` and drawn
  over the page on screen. Sharp, correctly sized, correctly placed: every visual
  property a screenshot can judge, and the wrong picture. Look for content that changes
  when it should not, or a page that is visibly the previous one.
- **`Ctrl+C` on page 2 of a document, and confirm the pasted pixels are page 2.** Same
  cause, and the only check that reaches it is outside the window entirely.
  `tests/probe_chords.ps1` uses a real `.jpg`, which has exactly one page, so the
  script cannot see this and the checklist has to say so by hand.

**The generalisable form, and the reason this section exists:** the useful question is
not "does it look right" but **"which value is on screen, and is it the one the user
asked for"**. Every bug in this feature's review history was a value that was latched
at one event and read at another — and a latch produces a *stable, plausible* screen,
which is exactly what a visual check passes.

### Deferred from the fix wave: a per-gallery anchor list

`anchor_of` and `prev_anchor` both walk forward from `(0, 0)`, because backward cannot
be answered locally. **The corrected cost, measured as a count rather than a time**
(`tests/spreads.rs::a_navigation_walk_costs_one_turn_len_per_turn_behind_the_cursor`):

| | turns | `dims` reads | per turn |
|---|---|---|---|
| `anchor_of` to the last turn, 400 portrait files | 200 | 1990 | 10 |
| `prev_anchor` from the last turn, same gallery | 200 | 995 | 5 |

Linear in the turns *behind* the cursor, exactly, and **the same order of magnitude in
both directions** — `MyApp::turn_forward` calls `next_turn`, which snaps first, so
forward is marginally the more expensive of the two rather than the free one. At the
back of a 2000-file gallery that is ~1000-2000 warm-cache lookups: microseconds,
against a preview decode measured in hundreds of milliseconds, so this is not a hang
and not a frame-time risk.

**The fix is an anchor list per gallery**, recomputed when a dimension is learned, a
file is re-exported or the filter changes — which is exactly the three-invalidation-
paths cost the design declined to pay (see the spec, "Why this needs no page map").
That reasoning holds today, at a cost nobody has measured in a real folder; it stops
holding if a 2000-file gallery ever shows up in a frame time. So: **measure it before
building it**, and `bench_browse` is where that measurement belongs. Do not build it
because a test says O(1) — the test says linear, which is the corrected claim.

**And do not "optimise" either walk into a local rule.** Three local formulations were
tried and all three are wrong on `[Single, Single, Single, Double]`; the enumeration is
in `prev_anchor`'s doc comment, and the walk is the price of not having a wrong answer.

---


## 7. UserCmd `mspaint.exe` freeze — parked, one open question

Ctrl+1 with program `mspaint.exe` (empty args, later `{file}`) froze the whole
box hard enough to need a reset. Twice. Investigation parked at the reporter's
request — do not ask them to re-test; the one experiment below can wait.

### Known, verified

- The original freeze mechanism was real and is fixed twice over: the chord
  went through `key_pressed`, which fires on OS key-repeat, so a held Ctrl+1
  spawned one Paint per frame. Now the chord takes only `repeat: false`
  events (traced winit → egui-winit → egui core marking) and every spawn site
  shares a 1/s `SPAWN_COOLDOWN`. Both would have to fail at once to storm.
- The spawn path itself is exonerated by the reporter's own test: `cmd.exe`
  `/c echo hi` via the chord spawns exactly once. Chord, expansion and spawn
  all behaved.
- Paint × that file is exonerated too: double-clicking the same file in
  Explorer opens it in Paint with no freeze.

### Unverified, and it is the whole ball game

- **Whether any freeze was ever observed on a fixed build.** Every freeze
  report came from `39d7cf3`-dirty binaries, and that dirty tree evolved
  through the session: the UserCmd UI (with the `key_pressed` chord, no
  cooldown) landed well before the repeat filter and the cooldown did. A
  binary built in between has the UI and neither fix — and freezes exactly
  as reported. The reporter never confirmed running `790f8c5` (clean tree,
  Build line without `-dirty`, "Empty args just passes `{file}`." hint
  present). So the remaining freeze, if it reproduces there, is a *different*
  bug from the repeat storm, and there is currently no evidence either way.
- If it does reproduce on `790f8c5` with a tap (not a hold), the difference
  between the working `cmd` case and the freezing `mspaint` case is only the
  target program plus the file argument — suspects then are the file itself
  (size/format: check before anything else), where it lives (a hanging
  network mount stalls the opener's thread), or AV hooking process creation,
  in that order. Do not add guards blind; reproduce first, and note that a
  single Paint open is what Explorer does on double-click, so a freeze there
  is a system/file problem no viewer code can fix.

---


## 8. Pixel-art mode: no filtering when zoomed

Zooming into sprites smears them: every texture here uploads
`TextureOptions::LINEAR`, so past 100% the GPU blends neighbouring pixels.
A pixel-art mode would sample `NEAREST` instead — crisp squares, no blur.

What is *known*: `TextureHandle::set` takes options per upload, so the
mechanism is one flag at the three upload sites (preview, tiles, animation
frames) rather than a pipeline change. What is *undecided* (needs a human):
toggle or automatic — auto-detecting ("source under N px *and* zoomed past
100%") would also catch small photos and thumbnails, where nearest looks
broken rather than crisp, so the heuristic needs a shape that excludes them
or it has to be a per-image/per-session switch. What is *unverified*: that
flipping options on a live texture is free of a re-upload hitch on the
esoteric backends (GL is the shipped path; DX12/Vulkan unmeasured).

---

## Deliberately not doing — don't re-open without a reason

- **HEIF / AVIF image sequences.** An ISO-BMFF `meta` box can hold several
  images (a burst saved as one HEIF), and `heic` 0.1.6 has no sequence support at
  all: primary image, thumbnail, depth, gain map, auxiliary images, and that is
  everything. Reading a sequence means hand-walking `meta`/`iinf`/`iref` for the
  item offsets, which is how you get a viewer showing item 1 of a 3-image burst
  with no sign that two more exist. Sequences are rare and the per-item work is
  guessable from the spec; if it is ever wanted, the walk belongs in `lib.rs`
  with tests, and "is this a sequence?" has to be asked while sniffing rather
  than after decoding the primary.
- **`Ctrl+S` on page 2+ of a document.** Refusing is the feature. The lossless
  path rewrites the whole file's coefficients and the re-encode fallback starts
  from page 1, so saving would write page 1's pixels under the document's name and
  call it a lossless save of what you were looking at. Supporting it means a new
  save mode and re-examining the "lossless" claim rather than assuming it.
- **`Ctrl+C` on page 2+ of a document**, refused for the same family of reason
  though not the same one. The save's reason is that the *write* is about a file;
  the clipboard's is that the **crop and the turn live in `ImageEdits`, which is
  keyed by path** because they are an edit to the document — so a frame drawn on
  page 1 applied to page 3's picture is a valid picture of the wrong part of the
  scan, with no error anywhere. (The `self.full` page bug that made `Ctrl+C` paste
  page 1 outright is fixed; what is left is the crop, and the crop cannot be right
  for a page until the edit store is keyed per page or the path-keyed crop is
  explicitly scoped to page 1.)
- **One grid cell per file**, so a 200-page document is not a 200-cell row.