# TODO

Deferred work, with the investigation already done so it does not have to be
repeated. Each item states what is *known* (verified), what is *undecided*
(needs a human), and what is *untested by the suite* (honest gaps).

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

## 3. egui 0.27 → 0.36.2  ← overdue, not a colour prerequisite

The GUI layer is the only stale dependency: **egui/eframe 0.27.2 (July 2024)**
while `image` is 0.25.10, `nom-exif` 3.8, `rustc` 1.98. Latest stable is
**0.36.2**. Nine minor releases and ~2 years behind. It reads like a 2024
`eframe-template` that was never refreshed.

**Verified from 0.36.2 source — it does *not* unlock HDR or wide-gamut:**
`App::clear_color` is still documented as gamma-space sRGB, and `TextureOptions`
still has no `srgb` field. So the colour work does not need to wait for this.

Migration cost, measured:
- `App::update` is **replaced** by `App::ui(&mut self, ui: &mut Ui, frame: &mut Frame)`.
  Our `update()` loses its `&Context` parameter and must use `ui.ctx().clone()` —
  ~200 lines, structural rather than mechanical. There is also a new `logic()` hook.
- `wgpu 0.19 → 30`, `winit 0.29 → 0.30`, `glow 0.13 → 0.17`.
- At-risk API sites in our code are few: `TextureOptions::LINEAR` ×5,
  `show_rows` ×4, `Sense` ×3, `Frame::none` ×1, plus the `on_exit` signature.
- The real cost is the one the suite cannot check: the **rendering backend
  changes underneath** (drivers, texture upload paths, filtering).

Do it as its own commit with nothing else in it. Bundling a backend swap with a
decode-pipeline change is how you get an unexplained regression.

---

## 4. Smaller known items

- **`exif_orientation` re-reads the whole file.** It calls
  `nom_exif::read_exif(path)` even though `load_image_preview_full` already has
  the bytes in memory. The new `Tiff` walker reads Orientation for free, so this
  is a cheap win if decode time ever matters again.
- **Large scans are the grid's real bottleneck now.** The three ~25 MB booklet
  JPEGs have no embedded thumbnail and take ~800 ms each even via the turbojpeg
  DCT path, which still decompresses every coefficient. Embedded EXIF thumbnails
  (landed in `914bc42`) took the other files 15-40x faster; these are the residue
  and need a different approach.
- **Consider a `--icc-scan <folder>` CLI mode.** Stage 1 detection exists but is
  only reachable from the EXIF panel, so auditing a library means clicking
  through it. The classifier is already a cheap per-file call.

---

## 5. What the test suite cannot reach — be suspicious here

`main.rs` is a binary, so none of these are testable today. Each has already cost
time, and moving logic into `lib.rs` is the fix.

- **The file-change watcher** (`check_changed`, `invalidate_path`). Needs a real
  app and a real editor. Check: change a file on disk, confirm it reloads within
  a few seconds, *including while it is mid-write* — that case is the one that
  would poison the file if `invalidate_path` stopped clearing `failed`.
- **Everything in `eframe::App::update`** (keys, panels, Esc precedence).
- **Rendering behaviour** after the egui upgrade.
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
