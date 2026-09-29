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

## 3. ~~egui 0.27 → 0.36.2~~ ← **done**, see the commit

**Landed.** `egui`/`eframe` are 0.36.2, as its own commit with nothing else in
it. What the notes below predicted is what actually cost the time, and one
prediction was wrong in an interesting way:

- `App::update` → `App::ui(&mut self, ui: &mut Ui, frame: &mut Frame)`. The
  `&Context` parameter is gone; our `update()` opens with `ui.ctx().clone()`.
  Because panels are now shown on a parent `Ui`, the whole panel sequence nests
  under the `Ui` instead of being rooted at the `Context`.
- **`SidePanel` and `TopBottomPanel` were merged into one `egui::Panel`**
  (`Panel::left/right/top/bottom`). The "cannot find `TopBottomPanel` in `egui`"
  error is real, not a cascade. Defaults are unchanged: `top`/`bottom` are still
  not resizable, `left`/`right` still default to 200 px.
- `Ui::child_ui(rect, layout)` is gone. The migration target is
  **`Ui::new_child(UiBuilder)`**, *not* `Ui::scope_builder(..)` — see the trap
  in AGENTS.md, it is silent and it changes the layout.
- `Frame::none()` → `Frame::new()` (documented as identical to `Frame::NONE`).
- `ComboBox::from_id_source` → **`from_id_salt`**, and note `ComboBox::new(id, "")`
  is *not* the same thing: it stores `label: Some("")` and renders an empty label
  widget, which shifts the settings panel.
- `Context::wants_keyboard_input` → `egui_wants_keyboard_input`. `Painter::rect`
  gained a `StrokeKind`. `ColorImage` gained `source_size`. `Label::truncate(true)`
  → `truncate()`. `DroppedFile::path` is a method. `on_exit` lost its glow param.
- `wgpu 0.19 → 30`, `winit 0.29 → 0.30`, `glow 0.13 → 0.17`.

**The rendering backend changed underneath, as predicted — and that turned out to
be the only thing that actually broke.** eframe 0.27's `default` features enabled
`glow`; 0.36's enable `wgpu`. So the upgrade silently swapped OpenGL for wgpu,
and on this machine the unpinned wgpu default **access-violates
(`0xC0000005`) before the window opens**. Root cause: wgpu enumerates *all*
backends during adapter discovery, and this machine has a Vulkan loader with
**zero registered ICDs** (`HKLM\SOFTWARE\Khronos\Vulkan\Drivers` is empty — an
Intel UHD 620 plus a Microsoft Remote Display Adapter, i.e. an RDP/VM session).
Enumerating that empty Vulkan stack crashes wgpu-native rather than returning
"no devices". Verified by isolating each backend:

| config | result |
|---|---|
| 0.27 (glow) | runs |
| 0.36, default (all backends) | `0xC0000005` |
| 0.36, `WGPU_BACKEND=dx12` | runs |
| 0.36, `WGPU_BACKEND=vulkan` | `0xC0000005` |
| 0.36, `WGPU_BACKEND=opengl` | runs |
| 0.36, DX12 pinned in code | runs |

Fix is `wgpu_options()` in `main.rs`: **DX12 always, Vulkan only when explicitly
asked** via `IV_WGPU_BACKEND=vulkan`. wgpu offers no "try this else that" —
`backends` filters enumeration *before* any adapter selector runs, so the set
has to be decided before instance creation.

An intermediate version gated Vulkan on whether `Khronos\Vulkan\Drivers` had an
ICD. That was wrong, and only measurable on a real (non-RDP) display: the
registry key is **absent** on this machine while `vulkaninfo` reports a healthy
Intel UHD 620 at API 1.2.187, and wgpu + Vulkan still crashes 3/3. Vulkan
working does not imply wgpu-on-Vulkan working, and the only way to learn that is
to crash, so the probe was deleted rather than kept as a "usually right"
heuristic that can silently pick a crashing backend.

### What the upgrade actually cost and gained

Verified on the **console** session (not RDP), release build, 60 s with a
visible window:

| | idle CPU%, static photo | animation fps (30 fps GIF) |
|---|---|---|
| 0.27 + glow *(before)* | 6.5 | 30 |
| 0.36 + glow | 5.4 | 30 |
| 0.36 + wgpu DX12 *(now)* | **4.6** | 30 |

Net win: lower idle CPU than the build it replaced, and animated GIFs play at
exactly the right rate on every configuration. The one behavioural difference is
that 0.36 draws at 60 fps (vsync-locked) where 0.27 drew at 30 — two draw calls
per animation frame. That is minor waste, not a defect.

**An earlier version of this section claimed egui 0.36 cost ~3.2x more CPU per
frame and that DX12 halved animation frame rate. Both were wrong, both measured
over an RDP session.** Retracted rather than left to be re-derived.

---

## 4. ~~Animated GIFs are choppy on DX12~~  ← was an RDP artifact, no defect

Retained only because the reasoning error is the useful part.

`test-images/MouseUtils.gif` is **147 frames, delays cycling 30/30/40 ms, total
4900 ms — a 30 fps animation**. (An earlier claim that it was 4 frames came from
a hand-rolled GIF parser that desynchronised on the sub-block skip; the `image`
crate's decoder is the authority, and the frame count matters when computing
advancement — ignoring the loop wrap makes the animation look like it is running
at half speed when it is not.)

The reported defect was that DX12 drew ~12 fps against OpenGL's ~21, and that
`SurfaceConfig::LOW_LATENCY` made it worse still (9.3 fps). On the console
session both vanish: every renderer draws 60 fps and advances the animation at
30 fps, including `LOW_LATENCY`. Nothing was wrong with the renderer, the
scheduling, or the frame-delay logic, and no code change was warranted.

The lesson is in AGENTS.md: check `quser` for `console` and the video adapter
list before believing a frame rate. This cost two full investigations in one
session, and one of them was committed as fact before it was caught.

**Still unverified by the suite** (see §5): nothing about rendering is covered by
tests, and a 20 s `--perf` run is the only end-to-end evidence. It reported
`display-gap frames 0`, `stalls 0`, `worker decode panics 0`, 7 decodes / 7 GPU
texture uploads, graceful exit 0 — but that is one folder on one machine.

---

## 5. Resize re-decodes once per frame  ← plausible, but UNMEASURED

The one speed item from the reverted set that looks like it has no downside, so
it is the one worth attempting. The current code drops every `below_target`
preview and re-decodes whenever the display area grows by even one physical
pixel, so dragging a window edge re-decodes repeatedly and blanks the picture
each time. Deferring the refine until the size stops moving keeps the existing
texture on screen and decodes once.

Two costs, so it is not free: the picture is briefly soft after a resize instead
of being sharp immediately, and a drag that ends is only detected if another
frame arrives.

**Measured: nothing.** An attempt measured "8-11 re-decodes for one drag" and a
deferral that only got it to 9 — but every one of those numbers was taken over
RDP, which coalesces and delays window-resize events. A debounce is sensitive to
exactly that cadence, so the comparison is meaningless and the diagnosis built on
it ("frames are ~850 ms apart, so a wall-clock timer expires between every pair
of them") is probably an artifact. Only the per-decode cost (~800 ms for a
28 MP preview) survived; that is CPU-bound and is real.

**What to do when a local session is available.** Drive a real drag with
`SetWindowPos` (that part works over RDP) and read the app's own
`resize-driven re-decodes` counter. Note that a *wall-clock* settle is likely the
wrong design here for a reason worth keeping: a large preview takes ~800 ms to
decode, so frames are ~800 ms apart and a timer shorter than that expires
between every pair of them. "The size did not change since the previous frame"
is frame-based and immune to that. But check it locally — under RDP, coalesced
events could make a same-size pair appear mid-drag and cause a spurious refine,
which is exactly what would make the frame-based version look worse than it is.

The predicate to extract and test when this is picked up is
`needs_refine(old, new, floor) -> bool` — pure, and it belongs in `lib.rs` next
to `plan_jobs` so it can be tested at all. (It was written and then reverted along
with the rest of the attempt; there is no `needs_refine` in the tree today.)

---

## 6. Smaller known items

- ~~**`exif_orientation` re-reads the whole file.**~~ **Measured: not worth
  doing.** It does re-open the file (`nom_exif::read_exif(path)`) even though
  `load_image_preview_full` already holds the bytes, and the `Tiff` walker could
  read Orientation for free instead. But measured on the five largest JPEGs in
  `test-images`, `read_exif` costs **0.1-0.2 ms** against a 10-17 ms `fs::read`:
  1-2%. The second read is served from the OS page cache, so there is no I/O
  storm to save, and against a ~800 ms preview decode it is ~0.02%. The
  "re-reads the whole file" framing is what made this look like a win.
- **Large scans are the grid's real bottleneck now.** The three ~25 MB booklet
  JPEGs have no embedded thumbnail and take ~800 ms each even via the turbojpeg
  DCT path, which still decompresses every coefficient. Embedded EXIF thumbnails
  (landed in `914bc42`) took the other files 15-40x faster; these are the residue
  and need a different approach.
- **Consider a `--icc-scan <folder>` CLI mode.** Stage 1 detection exists but is
  only reachable from the EXIF panel, so auditing a library means clicking
  through it. The classifier is already a cheap per-file call.

---

## 7. What the test suite cannot reach — be suspicious here

`main.rs` is a binary, so none of these are testable today. Each has already cost
time, and moving logic into `lib.rs` is the fix.

- **The file-change watcher** (`check_changed`, `invalidate_path`). Needs a real
  app and a real editor. Check: change a file on disk, confirm it reloads within
  a few seconds, *including while it is mid-write* — that case is the one that
  would poison the file if `invalidate_path` stopped clearing `failed`.
- **Everything in `eframe::App::ui`** (keys, panels, Esc precedence).
- **Rendering behaviour** after the egui 0.36 upgrade. The backend swap is the
  one part of that commit no test touches; §3 has the one run that was done.
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
