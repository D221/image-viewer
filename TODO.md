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

### What the upgrade actually cost and gained (real display, not RDP)

Release build, median of 3, animated GIF and static photo:

| | ms CPU/frame | GIF fps | GIF CPU% | static CPU% |
|---|---|---|---|---|
| 0.27 + glow *(before)* | 4.22 | 30.0 | 12.7 | 7.1 |
| 0.36 + glow | 13.69 | 59.5 | 81.0 | 8.4 |
| 0.36 + wgpu DX12 *(now)* | **3.52** | 58.7 | 20.7 | 7.8 |

- **egui 0.36 costs ~3.2x more CPU per frame than 0.27** (4.22 → 13.69 ms,
  same glow backend, so it is the version and not the backend). The wgpu swap
  more than reverses it: 3.52 ms, the best of the three.
- **The smoothness worry was an artifact of RDP.** Over the remote session the
  GIF ran at 12.7 fps on wgpu vs 21 on glow and it looked like a wgpu
  regression. On the real 60 Hz display it is 58.7 vs 59.5 — no difference. If
  you read any frame-rate number from this repo, check the session is not remote
  first.
- **One real regression remains, and it is pacing, not cost:** 0.27 played the
  GIF at its own ~30 fps, 0.36 paints at vsync (~59 fps). Per frame is cheaper,
  but drawing twice as many frames makes total animation CPU *worse* than the
  old build (20.7% vs 12.7%). Honouring the frame delay would land it near
  10.5%, i.e. below 0.27. Unfixed — see below.
- Static photos are idle-bound and show no meaningful difference across all three.

### 4. Animated GIF playback ignores the frame delay  ← new, real, unfixed

The fix belongs in the repaint path around `request_repaint_after(next_in)` in
`central()`. Something there (or vsync interaction in 0.36) is overriding the
GIF's own 30 ms delay with a per-vsync repaint, so the app renders ~2x more
frames than the animation needs. Worth doing: it is the only measured case where
the current build spends more CPU than the one it replaced.

**Still unverified by the suite** (see §5): nothing about rendering is covered by
tests, and a 20 s `--perf` run is the only end-to-end evidence. It reported
`display-gap frames 0`, `stalls 0`, `worker decode panics 0`, 7 decodes / 7 GPU
texture uploads, graceful exit 0 — but that is one folder on one machine.

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
