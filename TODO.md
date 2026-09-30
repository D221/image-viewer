# TODO

Deferred work, with the investigation already done so it does not have to be
repeated. Each item states what is *known* (verified), what is *undecided*
(needs a human), and what is *untested by the suite* (honest gaps).

Completed work is not kept here — it is in the git history, and the durable
lessons from it are in `AGENTS.md`, which is where a trap belongs. Several
sections that used to live here were removed once the work landed, and the traps
they recorded (the remote-session measurement errors, the wall-clock resize
debounce, the `glow`→`wgpu` backend swap, the second-read-is-free measurement)
are all in `AGENTS.md` instead.

`PLAN.md` is a separate, stale early-project feature list — most of it is already
implemented. Do not add to either from the other; AGENTS.md already warns that
`PLAN.md` does not describe current behaviour.

---

## 1. Display P3 → sRGB conversion  ← the big one, waiting on 2 decisions

### Undecided — needs you

1. **Default on or off?** Argument for on: 20% of a real library is P3 and
   displays wrong today; converting is strictly better on an sRGB display.
   Argument for off: it is a visible change to how your photos look, and you
   raised (correctly) that it is a downgrade on a wide-gamut display.
2. **Ground truth for "the colours are now right"?** I can test that the matrix
   runs, and that output stays in gamut. I cannot test *correct appearance*
   without a P3 file whose right answer is known. `test-images/color-p3-named.jpg`
   and `color-p3-anon.jpg` are the candidates — compare against JPEGView_L (in
   `test-images`), which can convert.

### Known, verified

- **200 of 1001** images in `phone_backup\Download\Ttg` are Display P3.
  Another 111 are sRGB-profiled, 641 untagged sRGB, 49 uninspected.
- The primaries never mix: **192 files at exactly `(33759,15807,-69)`** (P3) and
  111 at `(28578,14581,912)` (sRGB), both D50-adapted. These are *measured*.
  Do not "correct" them from memory — a guessed P3 value was wrong once already.
- **170 of the 200 P3 files have no `desc` tag at all**, so classification must
  work from primaries. That part is done (`icc_tags_valid`, `ColorSpace`).
- Display P3 is the *only* wide-gamut space in a real library. AdobeRGB, Rec.2020
  and "RGB-to-GBR" appear only in the synthetic charts in `test-images`.
  So a fixed 3×3 is enough; **lcms2 is not needed.**

### The technical correction, so it is not written wrong

Convert primaries **in linear light**, not in gamma space — doing it in gamma
space is the classic mistake that yields dark, muddy images. Concretely:
decode sRGB→linear with the sRGB curve, apply the P3→sRGB matrix, re-encode
linear→sRGB.

**No transfer-function change is needed on the output side.** The destination
framebuffer is already sRGB-encoded 8-bit (egui wants a gamma-space surface), so
the sRGB curve in and out cancels; what is actually needed is only the primaries
matrix.

### Where to apply — undecided

- **In `load_image_preview_full`**, on the worker: every consumer (texture,
  tiles, wallpaper, clipboard) is correct automatically, at the cost of one
  LUT+matrix per pixel on the hot path — which is where the earlier instability
  came from.
- **As a separate post-pass**: zero risk to the decode path, but must cover
  preview, tiles *and* animation frames, with more places to miss one.

### Safety requirement

The transform must fire **only** on a structurally-validated P3 profile.
Converting an sRGB image through a P3 matrix visibly wrecks it, which is the same
class of error as the reverse. `ColorSpace::WideGamut` alone is not sufficient
signal — confirm `red_primaries == Some(P3_D50_RED)` before touching pixels.

---

## 2. HEIC / AVIF `colr` box  ← the remaining blind spot

29 of the 1001 files in the phone backup are HEIF, reported `NotInspected` because
their colour lives in an ISO-BMFF `colr` box rather than a JPEG/PNG chunk. iPhone
HEIC is frequently P3, so some of those are probably displaying wrong *and*
invisible to the detector. Check whether the `heic` crate already exposes the
profile before hand-parsing the box.

The reason it is `NotInspected` rather than assumed sRGB is deliberate: assuming
is how a wide-gamut HEIC looks flat and nobody notices, because everything
"works".

---

## 3. Animated WebP decode — implemented, unverified

The animated-WebP path is written and reached (`animated_format` →
`animation_frame_count` → `load_animation` with `WebPDecoder`), and the *detection*
is tested: a still WebP is recognised as WebP and reported as **not** an animation,
which is the part that would otherwise decode the file twice.

**The decode itself is not verified, because nothing in this dependency set can
produce an animated WebP to test against.** Checked and ruled out:
`image` 0.25 writes lossless VP8L *stills* only; `image-webp` 0.2.4 has an encoder
but it is single-frame; there is no libwebp binding. Hand-building an animated
WebP means emitting a `VP8X` + `ANIM` + `ANMF` container around a real VP8 or
VP8L bitstream, and the VP8L one is Huffman-coded — not worth writing by hand to
test three lines that call an upstream decoder.

To close it: find one real animated WebP (a 2 KB one is enough), and either commit
it as a fixture or add a decoder to a test-only helper. The APNG fixture in
`tests/multiframe.rs` is the model — it is assembled chunk by chunk because nothing
else could produce one.

## 3b. HEIF / AVIF image sequences — not started, and the crate cannot do it

An ISO-BMFF `meta` box can hold several images (a burst saved as one HEIF). The
`heic` 0.1.6 API has **no** sequence support at all: primary image, thumbnail,
depth, gain map, auxiliary images — and that is everything. Reading a sequence
would mean hand-walking `meta`/`iinf`/`iref` to find the item offsets and then
calling `decode` on a constructed range per item.

Left alone deliberately: sequences are rare, the per-item work is guessable from
the spec, and guessing at a container layout is how you get a viewer that shows
item 1 of a 3-image burst with no indication that two more exist — the exact
failure mode item 2 above is written to avoid. If it is ever wanted, the ISO-BMFF
walk belongs in `lib.rs` with tests, and the "is this a sequence?" question has to
be asked during sniffing, not after decoding the primary.

## 3c. Multi-page TIFF: what is deliberately not supported

Pages work (see `AGENTS.md`), with three limits that are choices rather than gaps:

- **Colour types that need a colormap or colour transform** — CMYK, YCbCr, Lab,
  palette — are refused with an error past page 0. Page 1 of such a file still
  works, because page 1 goes through `image`. If these turn out to be common in
  real scans, the fix is to have `image` do the conversion for *every* page rather
  than only the first, and the branch to add is small.
- **`Ctrl+S` on a page is refused.** "Save what I see" is a statement about a file.
  Supporting it means `save_transformed` gains a page, and the lossless JPEG path
  gains a whole new mode (a multi-page TIFF save is a re-encode, not a DCT
  transform), so the "lossless" claim would have to be re-examined rather than
  assumed.
- **The grid shows page 1 only**, one cell per file. A document is one thing to
  browse, and a grid of 200 cells for one file is worse than one cell.

---

## 4. Smaller known items

- **Thumbnails of a multi-page document are page 1, always**, and the decode for a
  grid cell is the full pipeline at 256 px. For a 25 MB scan that is the dominant
  cost of opening the grid, and it is the same cost as for any other large file —
  see the next item.
- **Large scans are the grid's real bottleneck now.** The three ~25 MB booklet
  JPEGs have no embedded thumbnail and take ~800 ms each even via the turbojpeg
  DCT path, which still decompresses every coefficient. Embedded EXIF thumbnails
  took the other files 15-40x faster; these are the residue and need a different
  approach.
- **Consider a `--icc-scan <folder>` CLI mode.** Stage 1 detection exists but is
  only reachable from the EXIF panel, so auditing a library means clicking
  through it. The classifier is already a cheap per-file call.

---

## 5. What the test suite cannot reach — be suspicious here

`main.rs` is a binary, so none of these are testable today. Each has already cost
time, and moving logic into `lib.rs` is the fix — which is what was done for the
job planner, the resize policy and the rotation algebra, all now in `lib.rs` with
tests.

- **The file-change watcher** (`check_changed`, `invalidate_path`). Needs a real
  app and a real editor. Check: change a file on disk, confirm it reloads within
  a few seconds, *including while it is mid-write* — that case is the one that
  would poison the file if `invalidate_path` stopped clearing `failed`.
- **Everything in `eframe::App::ui`** (keys, panels, Esc precedence).
- **Rendering behaviour**, including the `glow`→`wgpu` backend swap. No test
  touches it; the one end-to-end run that was done is recorded in `AGENTS.md`,
  and it has to be a visible window on a *console* session or the numbers are
  wrong.
- **Colour correctness** — see decision 2 above. There is no ground truth.

---

## Reference: how the numbers above were produced

```sh
# per-file colour classification, needs the DIRS/DIR env var probe
# (see git history for the throwaway probe; the detector itself is image_viewer::color_info)

# 40/60 MP timing, the only way to see where big-image time goes
cargo test --release --test pathological -- --ignored --nocapture

# latency report
cargo run -- --perf <folder>        # also written to %TEMP%\image-viewer-perf.txt
```

Full suite: `cargo test --no-fail-fast` (plain `cargo test` stops at the first
failing target). Benchmarks need a real photo in `test-images/` and say so if
there is none.
