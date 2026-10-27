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
§9.5 records a reason that may be more than a comparison: on a P3 display the
conversion is the wrong operation to begin with, and the same eyeball settles
both.

Also unverified, and cheap to check by hand: the **wallpaper** path. It is the one
consumer that reads the *file* rather than our buffers, so with the setting on it
decodes, converts and writes `%APPDATA%\image-viewer\wallpaper.<ext>` instead of
pointing the shell at the original. Nothing tests it.

Cost when on, measured: 166 MP/s across the thread pool, so ~35 ms for a 2 MP
preview and ~145 ms for a 24 MP full decode.

### Known, verified

- **229 wide gamut, 111 sRGB-profiled, 661 sRGB-untagged, 0 unknown, 0
  unreadable, 0 not inspected** — re-measured 2026-10-19 over the same 1001
  images in `phone_backup\Download\Ttg`. Against the previous tally the only
  movement is the **20 files that were `not inspected` (13 WebP + 7 GIF) are
  now `sRGB (untagged)`**, which is exactly what §2's landed convention
  predicted (641 + 20 = 661); the wide-gamut and profiled counts are
  unchanged. The convention is at equilibrium in this library; re-measure
  with `--icc-scan` rather than trusting these.
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

- **Histogram / exposure readout.** Shipped: per-channel levels plus
  black/white clipped counts in the left pane, counted on the decode
  worker from the preview buffer. As a P3 instrument it remains what the
  section above says: comparison, not ground truth.

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

- **The 2026-10-20 pane rework has never been seen in the running app.** The
  right settings pane was restructured into collapsible groups with the small
  print moved into hover tooltips, and the bottom button row became four menus
  (Transform / Save / File / Tools) with `Left pane` / `Right pane` glowing
  toggles beside `Panels`. Everything that *can* be tested is: handlers,
  enable/disable rules and live-chord suffixes are pinned by source scans
  (`tests/unit.rs::the_bottom_bar_menus_carry_the_buttons_actions` and friends),
  and the `⏷` menu marker is glyph-checked by the font test. What a scan
  cannot say — this session's own recorded lesson — is whether any of it
  *works*: that a menu opens and closes where you expect, that a disabled
  item reads as disabled, and that nothing past the first menu item lands
  invisible on a short window. (The groups' open/closed state is egui memory
  keyed by the group's title, so it is remembered across a `Panels` toggle
  within the session and forgotten on restart — that much is how the widget
  works, not a claim to verify.) **One release-build pass over both panes
  closes it** — the same shape of
  pass §6 records for the spreads, and unpaid since the screen capture on this
  box cannot see the swapchain anyway.
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
  per format. §9.7 is the one candidate route on Windows (WIC), and it is a
  measurement before it is a dependency.

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

## 6. Manga mode: verified by arithmetic, and nobody has looked at a spread — SHELVED (2026-10-19)

**Parked at the user's request; do not resume without being asked.** The
feature itself is shipped, test-guarded and **default-off**
(`Config::manga_mode: false`), so shelving required no code change — nothing
was deleted, nothing is broken, and `M` still works if anyone reaches for it.
What is parked is the *unfinished* part: the console-session look at a real
spread (below), the three checklist items the visual checks would have missed,
and the deferred anchor list. This section's investigation stays intact so it
does not have to be repeated if the feature is picked up again.

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


## 8. Pixel-art mode: shipped

Shipped: the **Pixel art** toggle in Settings, **off by default**. When on,
every content texture — the preview, the zoom tiles, the animation frames — is
uploaded with a **nearest magnifier and a linear minifier**, so a sprite drawn
larger than its own texels samples its own pixels, while a zoomed-out fit
still gets the averaging filter (a minifying nearest sample is what *drops*
pixel rows, so the minification axis keeps linear). The decision is
`pixel_art_sampling` in `lib.rs`, pinned behaviourally; the wiring is a source
scan in `tests/unit.rs` mirroring the Smooth-toggle scan, including the
`refilter` walk that catches textures already uploaded when the toggle flips.

The shape decision, taken with the user: explicit toggle, **not**
auto-detect — an "under N px and zoomed past 100%" rule would also catch small
photos and thumbnails, where nearest looks broken, and no shape excludes them
without another measured threshold.

And one trap fell out of reading `TextureOptions` itself: it names
`magnification` and `minification` **separately**. A decision glued to the
zoom level current at upload time would have had to re-roll a live texture
the moment zoom crossed 100% — the "is the re-upload hitch free?" question
this item was left with. Naming the sampler axes instead means one upload
serves both draw directions and **nothing ever flips live**, so that
unverified hitch no longer has a door to arrive through.

Grid thumbnails deliberately stay linear whatever the toggle says: a cell is
150 pt of a 256-texel image and the grid also browses photos. The
`the_pixel_art_toggle_is_wired...` scan names that site as the deliberate
counter-example; if a pixel-art library ever reports otherwise, that is the
call to revisit, with the scan to extend.

Still unverified, of the kind a test cannot reach: that the result **looks**
right on a real sprite zoomed past 100% (a mouse-eye check, not a
measurement — how the zoomed pixel grid should look is a decision the stage
reader has already made by asking for the toggle). Sampling at draw time
follows the same options the texture was uploaded with on every backend the
app runs; nothing here touches the GL/DX12 gap recorded elsewhere.

---

## 9. Twelve other viewers, read not run — surveyed 2026-10-20

Twelve checkouts in `test-images\projects`, each at its own HEAD (2026-01-19 to
2026-10-20), **read, not built and not run**. Every figure below is either a
constant quoted from their source or a number this repo already recorded; none
of their runtime behaviour was reproduced, and nothing here says "faster" — that
word belongs to a measurement.

**The finding is a division of labour.** Their speed comes from *not decoding*;
ours comes from decoding well. The decode core here — one pass serving preview
and full, the DCT preview then a dwell, the linear-light prefilter, tiles cut on
demand — is better than anything in that folder. The gap is in **what we keep
between visits** and in **what we listen to**, and that is what 9.1-9.4 are
about.

### 9.1 Grid cells from the Windows shell cache - done

**Known.** PicView tries the shell first for common formats (`GetThumbAsync` →
`GetShellThumb`, `PicView.Avalonia\ImageHandling\GetThumbnails.cs`); ImageGlass
exposes it with an explicit cache-only flag (`Win32ShellThumbnailApi.GetThumbnail`
→ `IShellItemImageFactory::GetImage` + `SIIGBF_INCACHEONLY`). The flag is the
whole reason this is cheap: a miss is a miss, the call never *causes* an
extraction, so it cannot make the cold case slower or start a decode behind our
back.

**What it buys, and it is not the cold case.** A folder Explorer has already
visited paints its cells from the shell's cache instead of decoding. Against
§5's floor (700-900 ms per large scan, 24 of them over 4 workers, ~4 s) that is
the difference between a wait and a frame. A folder Explorer has never visited
gets nothing, and a folder *we* have visited is already served by §5's cache -
so the only folders this helps are ones browsed elsewhere first, which on
Windows is most of them. This is a second chance at a warm grid, not a faster
cold one.

**Decided, and by measurement: cache-only, with no swap.** The shell's cell is
painted and our decode is never asked for that cell. The doubt was whether what
is then on screen is close enough to our decode for §5's "a hit is byte for byte
the decode it replaced" to keep describing the grid; on real files it is 1.8-5.4
codes out of 255 per channel, where a *different* file scores 36-195 on the same
comparison - the same picture, not a placeholder. A provisional-then-sharpen arm
would buy nothing and cost a cell that moves under the reader. Either way the
shell's result is **never written into the thumb cache**: the entry has to stay
the decode it claims to be.

**Delivered.** `load_grid_thumbnail` asks, in order, our own disk cache, then
Explorer's (`shell_thumbnail`), then `load_thumbnail`. The interface
(`IShellItemImageFactory`) is declared by hand in `lib.rs`, because
`windows-sys` - already a dependency - carries `SHCreateItemFromParsingName` and
the `SIIGBF_*` flags but not the interface, and the `windows` crate would be a
second binding set for one method. Six decisions, each with a test in
`tests/thumb_cache.rs` (`mod shell_cache`):

- **`SIIGBF_THUMBNAILONLY | SIIGBF_INCACHEONLY | SIIGBF_BIGGERSIZEOK`**, always.
  Without `INCACHEONLY` the call extracts, which is the one thing it may never
  do (red-verified: the miss "succeeds" and the test's first round extracts);
  without `THUMBNAILONLY` a miss can come back as the file type's *icon*, i.e. a
  picture of the wrong thing.
- **The ask is bounded per folder.** A miss is not free - 8.5-16 ms measured
  here, against a 595-709 ms decode of a 17-26 MB scan (nothing) but ~7% of a
  124 ms small-photo decode, and a folder Explorer never visited misses on
  *every* cell. `ShellAsk` stops asking after `SHELL_MISS_GIVEUP` (4) consecutive
  misses on one folder, a hit clears the record so a partly cached folder is not
  abandoned, and the record is capped at `ShellAsk::MAX_FOLDERS` (8).
- **A cell too small to be a cell is refused.** Measured: the size argument picks
  which entry to look for, it does not scale one - fill the cache at 32 px and a
  256 px ask comes back 32x24. `SHELL_MIN_CELL_UPSCALE` (2) refuses anything
  under half the target and the decode runs instead, because a smear is worse
  than the wait it saved.
- **Grid-sized targets only**, the same gate as the embedded EXIF thumbnail, so a
  256 px picture can never reach the main view. Red-verified, and not
  theoretically: with the gate removed a target of `0` came back as a
  *default-sized* thumbnail.
- **The DIB is read top-down** (`biHeight` negative) or every cell is upside
  down, and **its alpha channel is not trusted**: the shell often draws into a
  32-bit bitmap whose fourth byte is left at zero, and honouring that paints the
  cell blank. An all-zero alpha becomes opaque; a real one is kept.
- **Two counters**, `grid cells from the Windows shell cache` and `shell cache
  misses (grid-sized asks)`, because a probe otherwise cannot tell a
  shell-served grid from a decoded one.

**Measured** - `cargo test --release --test bench_browse thumb_cache_ab --
--ignored --nocapture`, which now prints the real-file rows beside the fixture's:

| file | size | cold miss | hit | shell | ours | mean abs diff |
|---|---|---|---|---|---|---|
| FFA02639.jpg | 17.2 MB | 16.13 ms | 7.09 ms | 171x256 | 107x160 | 2.18 codes |
| 1_booklet_front.jpg | 24.4 MB | 8.49 ms | 6.10 ms | 256x84 | 256x84 | 4.45 codes |
| 1_acover.jpg | 26.0 MB | 8.72 ms | 6.97 ms | 256x256 | 256x256 | 1.81 codes |
| image001.jpg | 26.4 MB | (cached) | 7.49 ms | 256x125 | 159x78 | 5.35 codes |

The same files' own decodes are 595-709 ms (`real_scan_cells`, in the same
benchmark), so a hit is 80-100x cheaper. The first row also shows the two
sources disagreeing about size and not about the picture: our DCT pass stopped at
107x160 where the shell had upscaled that file's entry to 171x256, 2.18 codes
apart. On the synthetic 3000x2000 fixtures the same benchmark gives a miss p50 of
8.7 ms, a hit p50 of 11.3 ms, and 3.8 codes against our decode (a *different*
file scores 36).

**Two bugs the tests were needed for, because neither is visible in a return
code.** `canonicalize` returns a verbatim `\\?\C:\...` path and the shell answers
`E_INVALIDARG` for it, so the first version returned `None` on *every* file -
indistinguishable from "Explorer has never thumbnailed anything".
`shell_api_path` now uses `std::path::absolute` and strips a verbatim prefix
(`\\?\C:\a` → `C:\a`, `\\?\UNC\srv\share\a` → `\\srv\share\a`). And `GetImage`
needs the file **closed**: extracting a file the caller still holds open fails
with `ERROR_SHARING_VIOLATION`, which is why the tests write with
`std::fs::write` and hold no handle.

**Red-verified**, each mutation reintroduced and the named test failing:
`canonicalize` at the call site fails 4 shell tests; dropping `INCACHEONLY` fails
`a_cache_only_miss_stays_a_miss`; removing the size rule fails
`a_cached_entry_too_small_is_refused_rather_than_painted`; removing the target
gate fails `a_missing_file_or_a_target_that_is_not_a_cell_is_a_clean_none`; and
writing the shell's cell into our own cache fails the wiring scan, which is a
byte-offset test (one `write_cached_thumbnail`, after the only `load_thumbnail`,
with every shell arm returning first) rather than a search for a call.

**Not claimed.** The case the item is *for* - a grid painting from the shell's
cache in the running app, on a folder browsed in Explorer first - is **not run**:
no GUI can be launched from this session (`AGENTS.md`), and the tests have to
manufacture the cache entry with one extraction. What is proven is that the
cache-only read returns that entry afterwards, in 6-7 ms; the end-to-end half
belongs to `tests/probe_thumb_cache.ps1`, which now has the two counters to read
and is still owed. Also unmeasured: whether `INCACHEONLY` misses for archive
members (`%TEMP%`) and network paths - both plausible, both the safe direction
(a decode). EXIF-rotated JPEGs go to the shell as they are and Explorer applies
the tag itself, which is its own contract and is not tested here.

### 9.2 A persisted preview tier - done

**Known.** FlyPhotos keeps an **800 px preview** as a JPEG blob in SQLite,
`MaxItemCount = 20_000`, LRU with batch eviction
(`Services\DiskCacherWithSqliteFaster.cs`), and that is what its hold-to-browse
rides on; the HQ tier is separate and parks during a burst. Ours was
`PREVIEW_CACHE_CAP` 16 in memory and nothing on disk. **The browse mode itself
is not missing** - `block_while_loading` off is ImageGlass's "Turbo" under
another name (advance at key rate and let `replace()` drop the skipped decodes),
on is "Sequential". What was missing is the cheap source that makes Turbo *show*
something instead of holding the previous frame for a decode.

**What it buys.** A revisit, a slideshow loop, or flipping back through a
chapter stops decoding the file again. Measured here, the decode of a large scan
is **547-726 ms** and the same picture from the store is on screen in **22-50 ms**
- fifteen to thirty times sooner - and the whole-file read it skips is 5-60 ms
more on a slow disk.

**Delivered.** `preview_store` in `lib.rs`: `%LOCALAPPDATA%\image-viewer\previews`,
one entry per picture, a PNG payload behind a small binary header. It is asked
by the decode path **before the file is read** and written by a **thread of its
own** after the picture is on screen. A setting (`Cache previews of large
pictures on disk`, on by default) turns it off, and turning it off deletes what
it wrote: a setting that stops the writing but leaves 512 MB behind is a
promise kept in letter only.

**The four decisions, as taken.** The first one is the design.

- *The key.* Neither of the two shapes the item offered. A preview is decoded
  **for the panel**, so a fixed bucket would have to be resampled to the panel
  on arrival - and this crate's own resample is 20-30 ms, which is most of what
  the store saves. A key that includes the target would be thrown away by every
  resize. So the key is the **path and the page**, and the entry carries the
  target it was decoded for; the existing [`needs_refine`] rule then judges
  whether it may still be used, exactly as it judges the in-memory cache's
  preview. One entry per picture, and a resize behaves the way it already does.
- *Lossless or not.* **Lossless**, and not because the item's rule was inherited:
  measured, PNG is *also the faster of the two* here. The `image` crate's JPEG
  encoder is the slow pure-Rust one - a preview encode is **14-33 ms** as PNG
  against **39-77 ms** as JPEG q90 - and the decode is faster too (12-24 ms
  against 12-26 ms). JPEG's only win is size (0.4-0.7 MB against 1.8-3.2 MB),
  and its loss is small (mean 0.08-0.14 codes per channel), so the trade the
  item anticipated - fidelity against disk - does not arise: PNG wins on time
  *and* fidelity and loses only on disk. The disk is capped at 512 MB, which is
  about 170 of them.
- *What `full` means.* A stored preview carries **no full-resolution buffer**,
  which is the shape a large source's decode already has (it is decoded
  DCT-scaled), so the app asks for the full separately, as it already does for
  a big JPEG, and nothing downstream changes. **This is why only sources over
  12 MP are stored**: a smaller source is decoded in one pass that yields the
  preview *and* the full together, so storing its preview alone would mean
  decoding the file a second time for the other half - the store would make
  that file slower, not faster. The memory ceiling `PREVIEW_CACHE_CAP x
  MAX_FULL_PIXELS` therefore describes the in-memory cache only, and a hit
  never carries a full buffer at all.
- *Where the encode is paid.* **After the picture is on screen, on a thread of
  its own.** The decode hands its pixels to a store thread and returns; the
  encode (14-33 ms) happens there. It cannot land in front of the first
  pixels, which is the one regression this feature is not allowed to cause
  (the cold floor), and it cannot delay the next job on the worker that
  decoded the picture.

**Measured** - release, this box, real photographs in `test-images/`, the
panel target 957 px:

| file | size | preview | decode | PNG encode | PNG on disk | PNG hit |
|---|---|---|---|---|---|---|
| 0_booklet_inside.jpg | 28.4 MB | 2131x703 | 691 ms | 19.7 ms | 3.2 MB | 31.5 ms |
| 1_acover.jpg | 27.3 MB | 1024x1024 | 706 ms | 14.3 ms | 1.76 MB | 21.9 ms |
| FFA02639.jpg | 18.1 MB | 1265x1896 | 547 ms | 33.1 ms | 3.09 MB | 49.5 ms |
| image001.jpg | 27.7 MB | 1910x936 | 726 ms | 23.3 ms | 3.07 MB | 36.4 ms |

"PNG hit" is the whole cost of showing it again: read the entry (0.5-2.1 ms of
raw read) and decode the PNG (12-24 ms). **The item's "2-3 ms read" is the raw
read, not the hit**: an entry is a compressed picture, and getting the pixels
back out of it is a decode. The honest number is 22-50 ms, and it is still
fifteen to thirty times sooner than the decode.

**The four cache guards in `tests/thumb_cache.rs` transfer unchanged**, including
the stamp being mtime **and** length, and two more were needed: the entry is
keyed by path *and* page (a multi-page document's pages are different pictures),
and the entry carries the source's size, the EXIF tag, the crop grid and the
colour, because a hit that could not answer what the app asks of a preview
would be a different picture wearing the same pixels.

**Red-verified**, each mutation reintroduced and the named test failing: the
stamp check (`a_changed_file_is_a_miss`), the path-and-page check
(`another_path_and_another_page_are_misses`), the truncation check
(`a_truncated_entry_is_a_miss`), the header's magic and version
(`a_damaged_header_is_a_miss`), the usability rule
(`a_preview_for_the_wrong_target_is_refused`), the "no full buffer" rule
(`only_a_preview_without_a_full_buffer_is_worth_storing`), and three source
scans - the store asked *before* the file is read, the hand-off non-blocking,
and the write renamed into place.

**Not claimed.** The end-to-end run in the app - a revisit painting from the
store - is **not** measured here: no GUI can be launched from this session
(`AGENTS.md`), and the store's own root is the real `%LOCALAPPDATA%`, so a test
cannot drive the decode path against a temporary one without a hook this
feature does not have. What is proven is the store's round trip (a hit is the
decode's own pixels, byte for byte), the policy (what is stored and what is
not), and the wiring (asked before the read, written after the picture). The
counters `previews served from the on-disk store` and `store asks that did not
have the picture` are what a probe would read to tell whether the store is
doing anything at all; `tests/probe_thumb_cache.ps1` does not read them yet and
that is owed. Also unmeasured: the store's effect on the **cold** path, which
pays one extra `stat` and one 1 KB read per preview decode (tens of
microseconds against a decode measured in hundreds of milliseconds).

### 9.3 Preload bounded by bytes - done (256 MB)

**Known.** ImageGlass bounds preload four ways at once — `CacheMaxFiles` 10,
`CacheMaxMemoryInMb` 1000, `CacheMaxFileSizeInMb` 100, `CacheMaxDimension` 8000
— walking outward from the cursor (`PhotoManager_Caching.cs`). qView refuses to
preload any file larger than half its own cost budget (`qvimagecore.cpp`), so one
huge file cannot evict the neighbours. Ours is `PRELOAD_RADIUS` 3 symmetric and
`PREVIEW_CACHE_CAP` 16 **by count**; `AGENTS.md` already says to re-check every
memory ceiling if the radius is raised. On a manga page (~7 MP) the count is
fine; on 24 MP photographs the same radius holds 7 x 96 MB of RGBA plus whatever
`Preview.full` retains.

**The work is small and it is subtraction.** Priorities are already nearest-first
(`10 + 2d` / `11 + 2d`), so a budget only *removes* jobs the plan already emits —
but it needs the sizes: `Plan` carries no dimensions today, so a dims slice (or
the byte estimate itself) joins the struct and is handed in, keeping `plan_jobs`
pure for the same reason the stranding fix made it pure.

**Delivered.** `Plan` gained `file_bytes` (parallel to `files`) and
`preload_budget`; `plan_jobs` spends the budget outward, **nearest-first and
only on admissions**. Decisions, each with a test in `tests/preload_budget.rs`:
- **The unit is file length, not decoded bytes**, because that is what is
  observable before a decode exists — a neighbour that has never been decoded is
  exactly the thing a dims slice cannot know. The lengths come from the
  watcher's stamp registry (`Stamp.len`) which already covers every scanned
  file, so the plan path stats nothing; `schedule` collects the slice and the
  scan test (`the_preload_budget_is_wired_from_the_watcher_s_stamp_registry`)
  pins that, the order (collected before the `Plan` literal) and that
  `PRELOAD_BUDGET_BYTES` — not `u64::MAX` — is what is handed in.
- **The number is 256 MB.** Radius 3 plans six neighbours; 6 x 24 MB
  photographs is the largest ordinary folder a photo library brings and sits
  under it, so ordinary folders plan exactly what they always did. Measured, not
  assumed: `bench_browse browse_baseline` (24 fixtures, real lengths and the
  shipped budget in the `Plan`) reports the folder at **44.5 MB total** and the
  cache-hit column at **0 / 4 / 67 / 92 / 96%** across the 0/30/60/150/400 ms
  holds — identical to the unbounded arm, which is the "keep the hit rate
  constant" number this item asked for. The **real** library dump agrees: the
  whole of `test-images` is 645 images / 287.2 MB, and at the first, middle and
  last cursor the window admitted **53.3 / 8.2 / 77.6 MB** — byte-identical to
  the unbounded plan at all three, because the budget bounds one *window*, not
  the folder. What it removes is the tail: a 200-500 MB scan is past the whole
  budget and is never prefetched, which is the reachable half of the
  `MAX_FULL_PIXELS` ceiling. **That arm is not exercised by this library** — its
  biggest file is 47.6 MB — so the skip rule's evidence is the policy test with
  a 500 MB neighbour, not a real file.
- **A file larger than the whole budget is not preloaded at all** (the item's
  own preference: 1.2 GB decoded is past any sensible budget), and that falls
  out of the same test rather than a branch.
- **A skip is not a hole and holds nothing.** A priced-out neighbour does not
  break the walk and does not spend the budget, so a small neighbour behind a
  huge one is still planned (pinned by
  `the_budget_spends_nearest_first_and_a_skip_is_not_a_hole`).
- **Only the window spends.** The current preview, the turn's partner, the full
  decodes and the EXIF read are what the reader asked for; a zero budget leaves
  every one of them planned (`a_zero_budget_never_touches_current_partner_full_or_exif`).
- **An un-stamped file is admitted** (`0`, the fail-open reading): the budget is
  calibrated to the scan the session has seen, and a registry that has not
  caught up plans exactly the unbounded walk — asserted, not commented.

Both central guards verified red by mutation: with the budget check disabled 4
of the 5 policy tests fail; with the skip turned into a `break` 3 fail.
`the_budget_changes_nothing_for_small_files` is the golden arm — a 7 MB folder
must plan identically under 256 MB and `u64::MAX`.

Still open, and deliberately: the **shape** knob ImageGlass has (a per-file
dimension cap) is not modelled, because dimensions are not observable before
the decode; and a *decoded*-byte budget would need dims the planner does not
have. If a folder of 30 MB files turns out to need fewer neighbours than six,
the lever is the constant, not the policy.

### 9.4 OS file-change notifications instead of the 2 s poll - done

**Known.** Five of the twelve were verified to take OS notifications: qimgv
(`components\directorymanager\watchers\windows\windowsworker.cpp`,
`ReadDirectoryChangesW`), ImageGlass (`FileWatcherEx`), nomacs and qView
(`QFileSystemWatcher`), PicView (`FileSystemWatcher`). None of the twelve was
found polling the folder on a timer, and this app did (`RESCAN_INTERVAL` 2 s,
`maybe_rescan`); that timer was also the only wake source on a static photo -
which is why idle was ~2 fps.

**Delivered.** `DirWatcher` in `lib.rs` (`ReadDirectoryChangesW` on its own
thread, one 32 KiB buffer, `bWatchSubtree` only for the recursive listing) and
`sync_watcher` in `main.rs`, which **derives** the wanted watch from
`(folder, recursive, archive)` every frame instead of being set at each open.
With a watch running the timer does not tick at all and the thread's
`ctx.request_repaint()` is the only wake source; with none, the poll runs
exactly as it always did. Decisions, each with a test in `tests/watcher.rs`
unless noted:

- **The poll is the fallback, and every way a watch can fail lands on it.**
  `start` returns `Err` for a path that is not a directory - an explicit
  `is_dir` check, because `FILE_FLAG_BACKUP_SEMANTICS` happily opens a **file**
  and the first read on that handle then fails, i.e. a watch dead before it
  reports anything - and for a share that refuses the handle. A read that fails
  later (a share without change notification, the folder removed) clears
  `alive()`, and the app drops the watch and polls. `watcher_key` stops a folder
  that refuses a watch from being asked again on every frame.
- **One save arrives as five records and the dangerous one comes first.**
  Measured here, a rename-over is `ADDED page.jpg.tmp`, `MODIFIED page.jpg.tmp`,
  **`REMOVED page.jpg`**, `RENAMED_OLD_NAME page.jpg.tmp`,
  `RENAMED_NEW_NAME page.jpg`. Acted on record by record, that `REMOVED` for a
  known path is a membership change and every save scans the whole folder.
  `coalesce_changes` folds a name that is removed **and** added/renamed-to in
  one batch to the record that says its bytes are new; a real delete has no
  matching add and survives.
- **A content change settles for 200 ms (`CONTENT_SETTLE`); a listing change is
  throttled at 1 s (`RESCAN_MIN_GAP`) and never dropped.** A chunked save emits
  an event per write, so decoding the first decodes a half-written file; the
  clock restarts per event and the stamp is read when it expires. One save is
  one decode, of the finished bytes.
- **A path with no baseline stamp counts as changed**: an event is the OS saying
  the bytes moved, and the alternative is to miss the first re-export of a
  session - exactly what the old first poll caught.
- **`Drop` cancels the blocking read** (`CancelSynchronousIo`, retried because
  the thread may not have entered the call yet) rather than abandoning it, and
  a detach is counted by `watchers_detached()` so the test can assert zero:
  "`Drop` returned quickly" is also true of a version that leaks one thread and
  one directory handle per folder visited.
- **A buffer overflow is `CHANGE_OVERFLOW` and is answered with the full pass**,
  because names were dropped. `folder-watch events received` versus
  `files reloaded (watcher saw a change)` is what tells a silent OS from an
  ignored event in a perf report.

**Both premises the item listed as unverified are now measured**, not assumed: a
rename-over produces events (the sequence above, read from a real watch on a
real `tempfile::tempdir()`), and one recursive handle hears a change in a
subfolder (`a_recursive_watch_hears_a_change_in_a_subfolder`). The suite also
pins that a quiet folder sends nothing (the negative control - otherwise
"events arrived" could mean "events always arrive"), that a deleted folder makes
the watch report itself dead, that a file path is refused, and that stopping on
a quiet folder neither hangs nor leaks.

**Red-verified** (each mutation reintroduced, the named test failing, then
restored): removing the `is_dir` check fails the start test; not folding the
replace-target `REMOVED` fails both the pure fold test and the real rename-over
test; `RENAMED_NEW` on a known path classified as membership fails the table and
the rename-over test; dropping the `CancelSynchronousIo` call fails the
"stopping cancels" test (5 s, 3 detached); not clearing `alive` on thread exit
fails the deleted-folder test; and moving the fallback poll above the watch
branch fails the wiring scan.

**Not claimed.** A share that accepts the watch and then silently drops events
while staying alive is uncovered: there is deliberately no periodic sweep while
a watch runs, because that sweep *is* the idle tick this replaced. The archive
exclusion is a one-line guard plus a source-scan assertion, not a behavioural
test (an archive's temp dir cannot be watched without the app reporting its own
rewrites). The end-to-end "the running app re-decodes the changed picture" is
`tests/probe_watcher.ps1`'s job and was **not** re-run this round (GUI launches
from this session fail at adapter selection, see `AGENTS.md`), so that half
rests on the same counters the probe reads rather than on a run of it now.

### 9.5 Convert to the display's profile, not to sRGB

**Known.** qView converts to `getTargetColorSpace()`, where mode 1 is
`detectDisplayColorSpace()` (`qvimagecore.cpp`), **and it puts the MD5 of that
profile into the pixmap cache key**, so another monitor invalidates
automatically. gthumb reads the profile through colord. Ours converts P3 → sRGB,
full stop: right on an sRGB panel, *lossy on the panel the feature exists for*.

**The cheap version.** Read the window's monitor profile once (`GetICMProfile` /
`WcsGetDefaultColorProfile`), classify it with the code that already classifies
files (`icc_tags_valid` + primaries), and skip the conversion when the display's
gamut contains the source's. No lcms2, no 3D LUT — the same primaries question,
asked of a second profile.

**Settle this before building it:** whether the surface is colour-managed at all.
If the compositor already applies the monitor profile to the window, then on a P3
display "do nothing" is correct and the current conversion is a **double**
correction — which makes this a bug in §1 rather than a new feature. If it is not
managed, the conversion is needed and the display check is an improvement.
Nothing in the tree answers that today, and it is the same eyeball §1 owes.

**Consequence if built:** the target space joins what a cached preview is keyed
on, the way qView's is, or moving the window between two monitors keeps showing
the old conversion. That is `refilter`'s problem shape, and `refilter` is the
existing answer.

### 9.6 SVG

**Known.** Seven list SVG in their README (FlyPhotos via `ResvgWrap`, gthumb,
gwenview, ImageGlass, nomacs, oculante, pix); PicView and PhoXo have it in source
(`Avalonia.Svg.Skia`, nanosvg); qView's depends on a Qt plugin this checkout does
not prove. It is one of the two formats the field has and this app does not
(RAW is the other, §9.7), and `resvg` is pure Rust with no C build.

**Shape.** Sniff it by content like the rest — `<?xml` with an `<svg` root, or a
bare `<svg` — and render at the **panel target** the preview path already
computes — not a fixed 1024, because a vector has no natural size. There is no
`full` and no `below_target`: a re-render on resize is milliseconds, so the
honest model is "the preview *is* the decode" and a target change is a
`needs_refine`.

**The trap, and it is this file's kind.** `dims()` for an SVG is the viewport or
`None`, and `pairable` treats unknown dimensions optimistically — so a wide SVG
in manga mode is planned as a turn's partner and collapses the turn when the
shape is learned. Same failure as a wide page arriving late
(`arrival_reschedules`), which means an SVG wants an explicit shape rather than a
guess.

**Unverified:** that `resvg`'s XML parser bounds entity/expansion work on an
untrusted file — check, do not assume; and whether anyone using this app has SVGs
at all (a photo and manga library has none). It is a generality item and should
be argued on that, not on speed.

### 9.7 WIC, as an experiment behind a flag

**Known.** FlyPhotos decodes through WIC with `ScaledWidth`/`ScaledHeight`, which
gives EXIF orientation, colour and a reduced-size read for every installed
codec, plus RAW through the OS extension (`WicReader.cs`); ImageGlass and PicView
lean the same way. §5 records the gap this addresses: PNG, HEIC, JXL and TIFF
still decode at source size and shrink afterwards.

**Why it is not a port.** Colour classification and the wide-gamut conversion are
ours — a WIC decode that colour-manages internally would double-apply — the tests
assert decoded pixels, and the one format that dominates a photo library already
has a decoder here with DCT-domain scaling, i.e. the case where the current path
is *not* the problem. The only defensible scope is a **fallback for formats with
no reduced-size read**, behind a flag, and only if a measurement says the
transient buffer is the cost worth removing.

**The measurement:** a 60 MP PNG (the `pathological` generator makes 40/60 MP
fixtures), time-to-first-pixels with and without, interleaved and warmed, under
the `measuring-runtime` preconditions. If the difference is inside one decode's
noise, do not build it. `--icc-scan` and the decode must also still agree about
colour, which is a test to write before the path, not after.

### 9.8 Cache-directory hygiene — done

Landed: the cache root **and every two-hex fanout child** carry
`FILE_ATTRIBUTE_NOT_CONTENT_INDEXED`, set in `write_cached_thumbnail` where the
directories are born (and on the first new write into an older directory, which
brings existing caches up to date). The test
(`the_cache_dirs_announce_themselves_to_windows_search`) is the probe this item
called for: first write, the attribute read back via std's `MetadataExt`, and
one cache read proving the contents are unchanged. Verified red with the
`SetFileAttributesW` call disabled. **The unverified** half-claim below stays
unverified by the test — it cannot — but the real root on this box had 0 of 10
fanout dirs marked before the change, so the gap was real, and Windows Search's
actual crawl behaviour is the OS's to decide.

**Known.** nomacs sets `FILE_ATTRIBUTE_NOT_CONTENT_INDEXED` on its cache
directory on Windows (`disableContentIndexing`, `DkCachedThumb.cpp`) — the only
one of the twelve that does anything about it. Our thumbs dir holds 3 000-19 000
small PNGs (§5) in `%LOCALAPPDATA%`.

### 9.9 Per-image view state — DONE (this round, behind a setting)

**Known.** FlyPhotos offers three modes - reset, remember per photo, carry over
(`PerPhotoViewStore`). Ours carries the scale and **resets the pan** on a cursor
change (`pan_key`), deliberately and documented: a carried offset shifts the next
picture by the old one's drag.

**The missing mode** is "remember": a second per-image map beside `ImageEdits`,
keyed by `ImgKey` (so a page of a document gets its own - `ImageEdits` is
path-keyed because a crop is an edit to the document, which is the distinction
`AGENTS.md` already draws). ~~**Undecided:** persisted (a config key, and it must
survive a restart to be worth having) or session-only, which is the smaller
change and the one matching `show_panels`' precedent for view state.~~

**Delivered.** New setting **Remember zoom and pan per image** (`remember_view`,
off by default), session-only per the `show_panels` precedent; a remembered view
is zoom **and** pan restored exactly; an un-remembered image answers the carry,
so the shipped behaviour is byte for byte reachable. The rules live in
`lib.rs` (`ViewMemory`, `SavedView`, the pure `arriving_view` decision, keyed by
`ImgKey`) and the wiring is in `goto`'s cursor-change block — the save happens
while the outgoing key is still in hand, before `pan_key` is reassigned, pinned
by position with `view_memory_is_wired_to_the_cursor_change_and_the_setting`
(verified red by moving the save after the reassignment). `invalidate_path`
drops a re-exported file's entries alongside its page counts and shapes. The two
fit modes still own the arriving view when they are on.

### Not taking these, and why

- **Cancelling decodes on navigation.** FlyPhotos has `_centreCts`, qimgv clears
  its pool on a priority load, ImageGlass drops queued work. This app proved that
  cancelling strands images, and `AGENTS.md` records why; the 120 ms dwell is the
  substitute and it is cheaper. Do not reopen it because three viewers do it.
- **Lossy grid cells.** ImageGlass writes WebP q80, FlyPhotos JPEG blobs. §5
  rejected this for a stated reason; the exception, if one is ever wanted, is a
  *preview* tier (§9.2), where the pixel is on screen for a moment — not a cell.
- **Tile mipmaps and gutters.** FlyPhotos' 1024 px tiles carry a 2 px gutter and
  a level pyramid because a level is drawn at fractional scale. Ours are 1:1
  crops drawn at >= 1:1, so a gutter is duplicated pixels and a pyramid is work
  for a case that cannot occur.
- **Cache staleness, simplified.** qView and oculante key on file *size*;
  ImageGlass compares the cache's mtime to the source's. Ours — mtime + length,
  path stored in the entry, tmp+rename, `IEND` — is stricter than all twelve. Do
  not move toward theirs.
- **The rest of their surface area:** comparison/split view (declined, §4),
  multi-instance sync (nomacs), plugin SDKs, video, DICOM, network listeners
  (oculante). Most of the twelve are editors or organisers as much as viewers,
  and qView — 22 source files — is the useful counterweight: that folder is a
  menu, not a checklist.

---

## 10. The monoliths: the source half is done, the test files are not

**Done 2026-10.** `src/lib.rs` (13,047 lines) and `src/main.rs` (10,713) were
split into topic modules and then reorganised into topic folders, all under the
1000-line target (`tests/source_size.rs` holds the teeth, and it is green).
What is still over the target, and each file's ceiling in `OVER_TARGET`:

| file | lines | what is in it |
|---|---|---|
| `tests/unit.rs` | 5,088 | the unit suite, one file for everything that does not deserve its own target |
| `tests/spreads.rs` | 6,687 | the manga-mode suite, same shape |
| `src/app/ui.rs` | 2,048 | pane layout against the egui UI |
| `tests/resample.rs` | 2,723 | the linear-light prefilter suite, split out of `unit.rs` (2026-10-26); new on the list with its own size |
| `tests/rotate_save.rs` | 1,921 | |
| `tests/flip_through.rs` | 1,647 | |
| `src/app/central.rs` | 1,402 | |
| `tests/bench_browse.rs` | 1,309 | |
| `tests/thumb_cache.rs` | 1,121 | |

The split shape is proven on this tree several times over — the preview store,
then `lib.rs` into 33 modules, then `main.rs` into `app_*`, then the topic
folders, then `unit.rs`'s prefilter suite out to `tests/resample.rs`,
`spreads.rs`'s settings section out to `tests/manga_settings.rs`,
`keybindings.rs` into `keybindings.rs` (648 lines) plus
`tests/keys_direction.rs` (889 lines) — 17 tests split 7/10 — and
`spreads.rs`'s central/schedule wiring scans out to `tests/spread_wiring.rs`
(328 lines) with the navigation-walk test staying in place — 113 → 109 tests
in spreads.rs, none lost, none duplicated, no new warnings. Files that come
down under the target leave the list; that is what just happened to
`keybindings.rs`, `manga_settings.rs` and `spread_wiring.rs`. What remains is
the same move: split a test file by subject, or move a section of
`src/app/ui.rs` or `src/app/central.rs` into its own module, and lower the
numbers above.

**Not claimed.** Line counts are a proxy for "can a person hold it in their
head", not a measurement. What *is* true is that the check is enforced, so the
list can only get shorter - and a raise is a visible act with a reason in it.

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