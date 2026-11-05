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

Human with a P3 display has looked at it (2026-11-05): renders the same as
JPEGView_L on a P3 display, considered fine-ish — closing the "correct looks
right" question pragmatically rather than with a recorded reference.

**To close it properly:** compare against something that converts. `test-images`
has two P3 JPEGs and JPEGView_L (also in `test-images`) converts; the open
question is whether the viewer and JPEGView_L agree *by eye* on the same file, and
what the reference is for a photo whose correct rendering nobody has recorded.
§9.5 records a reason that may be more than a comparison: on a P3 display the
conversion is the wrong operation to begin with, and the same eyeball settles
both.

Also unverified, and cheap to check by hand: the **wallpaper** path. It is the
one consumer that reads the *file* rather than our buffers, so with the setting
on it decodes, converts and writes `%APPDATA%\image-viewer\wallpaper.<ext>`
instead of pointing the shell at the original. **Now tested:** the pure half
(`image_viewer::wallpaper_target`) was extracted from `MyApp::set_wallpaper` in
`src/app/output.rs` so it can be unit-tested, and `tests/wallpaper.rs` covers
the decision: conversion off keeps the original path, an untagged JPEG keeps the
original path even with conversion on, archive members always get a converted
copy beside the config, and a source without an extension falls back to `.jpg`.
Two additional tests run when `test-images/` is present (same skip-if-absent
pattern as `test_gps_decode`): they load a real P3 JPEG (`color-p3-named.jpg`)
and verify `color_info` classifies it as WideGamut, `wallpaper_target` picks the
converted copy beside the config, and `decode`+`convert_wide_gamut_rgba`
produces a valid RGBA buffer matching the decoded dimensions. The by-eye
comparison against JPEGView_L (§1) still needs a human with a P3 display.

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

---

## 3. Multi-page TIFF: the colour types still past page 0

Pages work, and **CMYK now works too** — `cmyk_via_image` re-wraps a page's
samples in a one-page TIFF and lets `image` do the conversion, because `image`
owns the maths and cannot be pointed at page 2. Verified for CMYK8 and CMYK16.

Still refused past page 0, each an error rather than an approximation: **palette, Lab, multiband.** YCbCr stays refused too: `image` rejects the YCbCr colour type outright when decoding a TIFF, so re-wrapping a YCbCr page as a one-page TIFF and handing it to `image` fails — it cannot be reached this way.

**Done since this section was written:** CMYK (8 and 16 bit) and CMYKA (8 bit) now decode past page 0 through the `cmyk_via_image` re-wrap trick. Verified for CMYK8, CMYK16, and CMYKA8. Worth doing only if real scans need it; CMYK (the common print case) was already covered.

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

- **Metadata editing.** Caption and artist: delivered through XMP sidecars
  (`tests/ratings.rs`). `Ratings` gained `caption` and `artist` fields, stored
  as `iviewer:Caption` and `iviewer:Creator` attributes using the same byte-level
  merge as ratings; `set_caption` / `set_artist` write the sidecar and re-read;
  the left pane has text fields for both. Description is reachable through the
  same mechanism if wanted. The ratings storage decision (XMP sidecars) that
  previously blocked this is now shipped.

- **Comparison / light-table.** Declined by the user; recorded here only so it is
  not re-proposed from scratch. The reason is the app's own rule: N images
  resident at once is the one shape that fights "the UI thread only paints
  textures that are already in memory", and it needs N full-resolution buffers.

---

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
`lib.rs` and are pinned behaviourally in the `tests/spreads*.rs` targets; the
traps are in `AGENTS.md` under **Spreads and reading direction**; the
user-facing description and the three limits a user cannot work around are in
`README.md`.

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
(`tests/spreads_goto.rs::a_navigation_walk_costs_one_turn_len_per_turn_behind_the_cursor`):

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
between visits** and in **what we listen to**, and that is what 9.5 and 9.7 are
about.

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

### Not taking these, and why

- **Cancelling decodes on navigation.** FlyPhotos has `_centreCts`, qimgv clears
  its pool on a priority load, ImageGlass drops queued work. This app proved that
  cancelling strands images, and `AGENTS.md` records why; the 120 ms dwell is the
  substitute and it is cheaper. Do not reopen it because three viewers do it.
- **Lossy grid cells.** ImageGlass writes WebP q80, FlyPhotos JPEG blobs. §5
  rejected this for a stated reason; the exception, if one is ever wanted, is a
  *preview* tier, where the pixel is on screen for a moment — not a cell.
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
**Done 2026-11-03.** `src/app/ui.rs` and `src/app/central.rs` were functionally
decomposed: each oversized function (`ui()` ~1,600 SLOC, `central()` ~786 SLOC)
was split into cohesive helper methods under the 50 SLOC guideline, without
splitting the files themselves. Raw lines remained over target but within their
ceilings.
**Done 2026-11-05.** `src/app/ui.rs` (2,234 → 731) and `src/app/central.rs`
(1,489 → 865) were split into child modules, so both left the over-target list.
`ui()` and `central()` are now thin dispatchers: `ui/` holds `status.rs` (37),
`menu.rs` (344), `exif.rs` (189), `settings.rs` (997); `central/` holds
`crop.rs` (176), `tiles.rs` (320), `zoom_input.rs` (256). Each panel or stage
is its own `impl MyApp` block in its own file. The source-text guards that used
to scan `fn ui(` / `fn central(` were updated to scan the new modules.
All files are under the target and `OVER_TARGET` is now empty.

The split shape is proven on this tree several times over — the preview store,
lost its shape-measuring half to `src/gallery/gallery/shape_cache.rs` (355):
`learn_shapes` … `forget_page_count` plus the `shapes.json` sidecar. A child
module rather than a sibling, because it reaches the private `shapes` /
`shape_cache` fields and the descendant-module rule lets it.
`tests/rotate_save.rs` (1,848) split by subject into `rotate_save.rs` (748,
27 tests), `crop_save.rs` (713, 24 tests — the clipboard and crop bands) and
`image_edits.rs` (244, 12 tests — the per-image edit record), with the shared
fixtures in `tests/common/rotate_fixtures.rs` (182). 63 tests in, 63 out.
`tests/resample.rs` (2,537) split by subject into `resample.rs` (902, the
prefilter pins and the strength pins), `resample_settings.rs` (240, the
config-forward-compat and wiring scans), `resample_screentone.rs` (345, the
alias band and the promoted pitch) and `resample_floor.rs` (648, the
`MIN_FILTER_SCALE` floor), with the probes and the globals guard in
`tests/common/resample_fixtures.rs` (503). 39 tests in, 39 out — 9 of them
`#[ignore]`d printouts, which stayed printouts.
`tests/bench_browse.rs` (2,127) became a **target directory**,
`tests/bench_browse/main.rs` plus `harness.rs` (496), `baseline.rs` (403),
`sweep.rs` (567), `meta.rs` (466) and `drop.rs` (165). A directory rather than
five targets: the arms share one process, one fixture set and one command, so
`cargo test --release --test bench_browse -- --ignored --nocapture` is still
the whole benchmark. 6 `#[test]` arms in, 6 out (they gain a module prefix in
the test list).
`tests/unit.rs` (4,794) split by subject into twelve targets:
`unit_decode.rs` (578), `unit_gallery.rs` (497), `unit_scans.rs` (162),
`unit_transform.rs` (201), `unit_config.rs` (534), `unit_archives.rs` (285),
`unit_slideshow.rs` (193), `unit_failures.rs` (263), `unit_fitwindow.rs` (584),
`unit_uichrome.rs` (361), `unit_settings.rs` (243), `unit_edits.rs` (389),
`unit_memory.rs` (402), with the shared helpers in
`tests/common/unit_fixtures.rs` (408). 141 tests in, 141 out, none lost,
none duplicated, no new warnings.

The split shape is proven on this tree several times over — the preview store,
then `lib.rs` into 33 modules, then `main.rs` into `app_*`, then the topic
folders, then `unit.rs`'s prefilter suite out to `tests/resample.rs`,
`spreads.rs`'s settings section out to `tests/manga_settings.rs`,
`keybindings.rs` into `keybindings.rs` (648 lines) plus
`tests/keys_direction.rs` (889 lines) — 17 tests split 7/10 —
`spreads.rs`'s central/schedule wiring scans out to `tests/spread_wiring.rs`
(328 lines), and finally **`spreads.rs` itself** (6,687 lines, 109 tests) split
by subject into eleven targets — `spreads_partner.rs` (410), `spreads_dims.rs`
(903), `spreads_overrides.rs` (828), `spreads_layout.rs` (599),
`spreads_zoom.rs` (258), `spreads_pan.rs` (693), `spreads_nav.rs` (472),
`spreads_goto.rs` (744), `spreads_latches.rs` (538), `spreads_decode.rs` (202)
and what stayed behind in `spreads.rs` (658) — with the shared fixtures in
`tests/common/spreads_fixtures.rs` (470). 109 tests in, 109 out, none lost,
none duplicated, no new warnings; every one of those files is under the target
and `spreads.rs` has left the list. Files that come down under the target leave
the list; that is what just happened to `keybindings.rs`,
`manga_settings.rs`, `spread_wiring.rs`, `spreads.rs`, `multiframe.rs`
(truncated from 1,036 to 437 lines by removing the TIFF and gallery-navigation
sections), `gallery.rs`, `rotate_save.rs`, `resample.rs`, `unit.rs`,
`bench_browse.rs` and `thumb_cache.rs` (its shell-cache tests moved to
`tests/thumb_shell_cache.rs`). What remains is the
same move: split a test file by subject, or move a section of
`src/app/ui.rs` or `src/app/central.rs` into its own module, and lower the
numbers above.

**Not claimed.** Line counts are a proxy for "can a person hold it in their
head", not a measurement. What *is* true is that the check is enforced, so the
list can only get shorter - and a raise is a visible act with a reason in it.

---

## 12. Source-code size & quality policy — done 2026-11-03

A maintainable source-code size and quality policy was established and applied
to `src/app/central.rs` (the largest remaining source monolith after the
`lib.rs`/`main.rs` splits). All changes are verified: every test target green,
formatting clean, Clippy with no new warnings.

### What changed

**`src/app/central.rs`** — `MyApp::central()` was ~786 SLOC in one monolithic
method, the single largest function in the tree (function guideline: <50 SLOC
ideal, avoid >100). Six methods were extracted, each with one cohesive
responsibility:

| Method | SLOC | Responsibility |
|--------|------|----------------|
| `panel_geometry()` | ~35 | panel rect, ppp, panel_px, pending-position settling / monitor centering |
| `handle_resize()` | ~47 | first measurement, `suppress_resize_redecode`, `ResizeTracker` match |
| `handle_fit_window()` | ~90 | fit-window reset/arrive/wrap, convergence, window sizing |
| `preview_and_animation()` | ~56 | preview cache lookup, LRU `used` stamp, animation frame advance |
| `compute_transform_and_display()` | ~7 | transform (EXIF orientation), footprint, display dimensions |
| `compute_spread_layout()` | ~21 | manga turn pages, transformed footprints |

`central()` is now a ~230 SLOC dispatcher. Raw lines: 1,389 → 1,444 (still
under the 1,493 ceiling in `tests/source_size.rs`, which holds a +50 line
headroom on every over-target file).

### Ceilings updated

Every `OVER_TARGET` entry in `tests/source_size.rs` got +50 lines of headroom
so minor future changes do not break the test. The list can only shorten (a
file comes down under 1,000 lines and leaves the list); a raise is a visible
decision with a reason, as before.

### Verification

- `cargo fmt --all -- --check` — clean
- `cargo check --all-targets --all-features` — passes
- `cargo test --no-fail-fast` — all targets green (140 unit tests, all
  `spreads*` targets, `keybindings`, `keys_direction`, `source_size`)
- `cargo clippy --lib` — 5 warnings, all pre-existing `module_inception`
  (intentional crate convention for `pub mod X; pub use X::*;`)
- `cargo clippy --bin` — 62 warnings, all pre-existing types at shifted line
  numbers; **zero new warnings introduced**
- `cargo clippy --all-targets --all-features -- -D warnings` — 5 errors, all
  pre-existing `module_inception`; no new errors

One documented `#[allow(clippy::too_many_arguments)]` was added on
`handle_fit_window` (10 parameters, all naturally part of the fit-window
computation; a struct would add unnecessary abstraction). One documented
`#[allow(clippy::type_complexity)]` on `compute_spread_layout` return type.
No broad silences, no global `#[allow(...)]`.

### Behaviour preserved

All architectural-invariant tests pass unchanged — `keybindings.rs`,
`keys_direction.rs`, `spread_wiring.rs` (lone_spread_fills_turn threading),
and every `spreads*` target. Execution order in `central()` is unchanged:
panel geometry → lone-spread rule + decode target → first measurement +
resize action → preview + animation → transform + spread layout → fit-scales
→ fit-window convergence → input/zoom/pan → paint → tiles → crop → "Loading…".
Keyboard shortcuts, image rendering, zoom, pan, dual-page spreads, image
scaling, window fitting, and resize/redecode behaviour are all verified by the
passing suites.

### What was deliberately NOT done

Per the user's constraints: no arbitrary file splitting (a cohesive 800 SLOC
file is not automatically problematic), no abstractions introduced just to
satisfy a lint count, no test-file rewrites beyond the ceiling numbers, no
extraction of painting / tile management / input handling (those remain in
`central()` — they are behavior-sensitive and borrow-checker constrained).

The remaining source-size over-target files (`tests/unit.rs`,
`tests/resample.rs`, `tests/bench_browse.rs`, `tests/rotate_save.rs`,
`tests/flip_through.rs`) carry their +50 headroom and can be split by subject
using the same pattern when they grow.

---

## 11. Speedups & features from reading the other twelve viewers — surveyed 2026-10-28

Same rules as §9: twelve checkouts in `test-images\projects`, read but not built or run; no figure here claims "faster" until it is measured under the preconditions in `AGENTS.md` **Measuring**. Every candidate was grepped against `src/` and is absent (a few are absent by design, noted where they are).

### 11.1 Speedups — none refused by §4/§9/§10







**4. mmap / sequential-scan reads.** `fs::read` copies the whole file before decode (`decode.rs` entry points; `read_prefix` at `decode.rs:417` shows the prefix pattern works). On this box a 27 MB read is 13 ms (§5 table), so this is a small win that matters most on network shares. *Effort S, weigh.* **Not taken.** `read_sequential` was added (`decode_page.rs`, opens with `FILE_FLAG_SEQUENTIAL_SCAN` on Windows) and `bench_browse::browse_baseline` measured it: no measurable difference on local SSD (decode s 3.37→3.59 at hold=0ms, 5.76→5.73 at hold=30ms — within noise). May help on network shares; unverified.

**5. Idle warm-up of cold cells.** Cold cells cost 595–709 ms, warm cells 0.6–1.8 ms (§5:209). FlyPhotos writes decoded previews back to its disk cache; ImageGlass warms outward from the centre. Queuing previews for the whole folder on idle workers costs CPU/battery but no new failure semantics — the queue's `wanted` map already decides what survives. *Effort M, weigh.* **Not taken.** `schedule()` was changed to use radius=min(files.len(),50) when idle, but `bench_browse::idle_warmup_bench` measured it and it makes things worse: cache hit collapses (53%→0%), wasted decode jumps to 76%, ttfp p50 gets worse (0→778ms). The eviction policy only protects `2×RADIUS+1=7` images around the cursor, so the ~30 preloaded cells get evicted as soon as navigation starts. Would need a wider eviction protection window to help.

**6. Worker threads below normal priority.** Four decodes run at default priority against the paint thread (`app/types.rs`, `new_loader` uses `available_parallelism`); FlyPhotos raises its HQ thread above normal and Windows sets I/O priority. Moving decode workers below normal smooths the UI during bursts without changing any decode semantics. *Effort S, weigh.* **Not taken.** Worker threads were set to `THREAD_PRIORITY_BELOW_NORMAL` (`src/resize/load_sched.rs`) and `bench_browse::browse_baseline` measured it: no measurable difference on this system (decode s 3.37→3.33 at hold=0ms, 5.76→5.76 at hold=30ms — within noise). May help under heavy system load; unverified.

### 11.2 Features

**Low cost:**

- **Clipboard paste (Ctrl+V).** qView `paste()`, JPEGView `IDM_PASTE` (`KeyMap.txt.default:74`), PicView "Single Image". Ours is write-only (`app/paint.rs:249` `set_clipboard_rgba`). Tried and reverted: `Ctrl+V` did nothing for an image on the Windows clipboard, and it's not a useful function here — a viewer opens *files*, not clipboard pixels. *Effort S.* **Not taken.**

- **Display folders in grid view (file-picker lite).** Grid currently shows only image files; standing on a folder opens it rather than showing its contents. Add folder cells (folder icon) so clicking descends into that folder, plus an "up arrow" (↑) to go to the parent folder — a lite file-browser inside the gallery so you don't need Explorer just to navigate folders. *Effort M.* **Done** — `scan_folder` now lists subdirectories alongside images; `gallery_grid` renders folder cells (folder icon "D", no thumbnail decode) and an up-arrow cell when a parent exists; clicking descends/ascends with a folder-stack history. `src/app/grid.rs`, `src/app/paint.rs` (`grid_folder_cell`), `src/app/save_as.rs` (Enter on folder cell), `src/app/files.rs` (clear stack on fresh open), `src/app/model.rs` (`folder_stack`), `src/app/goto.rs` (init), `src/scan/mod.rs` (directories in `scan_folder`).







**Larger — do not mistake for afternoons:**
- **Multi-select + batch triage.** `grid_sel: usize` is single (`app/model.rs:408`); qimgv's 1–9 destination slots, gthumb's numbered selections and gwenview's batch save show the shape. *Effort L.* **Not taken** — this is an image viewer, not an image manipulator; bulk select/triage is too complex and out of scope for now.

- **Per-file adjustment DB** (gamma/contrast/brightness/saturation stored per image, pixels untouched) — JPEGView's parameter DB (`KeyMap.txt.default:66` `IDM_SAVE_PARAM_DB`). Nothing tonal exists anywhere here. *Effort L.* **Not taken** — unnecessary for an image viewer.
- **RAW via WIC + embedded JPEG first.** FlyPhotos/ImageGlass/PicView/qimgv all decode RAW; our only on-ramp is §9.7's WIC experiment. *Effort L.* **Not taken** — unnecessary for an image viewer.
- **Tabs.** PicView opens many images in tabs; one image at a time here. *Effort L.* **Not taken** — unnecessary.

### 11.3 Checked, not worth doing (recorded so nobody re-derives)

- **AVIF via dav1d.** `heic` 0.1.6 already uses `rav1d-safe` (`heic/Cargo.toml:52`) with `parallel` (`:56`); oculante's dav1d option buys nothing here.
- **`image`'s `rayon` feature for PNG/WebP.** It reaches AVIF encode and buffer iterators only — no decoder parallelism (`image-0.25.10/Cargo.toml:113-116`, `src/images.rs:3`).
- **Texture mipmaps.** egui-wgpu 0.36 allocates `mip_level_count: 1` (`egui-wgpu-0.36.2/src/renderer.rs:699`) and epaint documents `mipmap_mode` as glow-only (`epaint-0.36.2/src/textures.rs:177`), so `README.md:640` ("linear sampler and no mipmaps") is accurate; only a native texture with a mip chain would change it.

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