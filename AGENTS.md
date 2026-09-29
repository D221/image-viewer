# AGENTS.md

Single Rust crate (edition 2021): a speed-first egui/eframe image viewer with a background worker-pool decode pipeline.

## Commands

- `cargo test` — run all tests; targets: `unit`, `property`, `integration`, `standard`, `embedded_thumbs`, `filter_and_paging`, plus the in-crate `--lib` perf tests. It **stops at the first failing target**, so use `cargo test --no-fail-fast` to see everything.
- `cargo test --test unit` — one test file; `cargo test <name>` — one test by name
- `cargo bench` — criterion benches in `tests/bench.rs` + `tests/bench_variants.rs` (separate targets, not part of `cargo test`)
- `cargo test --release --test pathological -- --ignored --nocapture` — generate and time 40/60 MP images; the only way to see where big-image time actually goes
- `cargo test --release --test bench_browse -- --ignored --nocapture` — **browsing benchmark**; flips through a generated folder at several press rates and prints time-to-first-pixels, wasted decode share, peak MB, cache-hit rate. Median of 3 (`IV_BENCH_RUNS`), 24 images by default (`IV_BENCH_N`). This is the number to move when changing the queue, the preloading window or the cache policy
- `cargo run -- --perf <folder>` — turn on latency sampling; the report prints on exit and is written to `%TEMP%\image-viewer-perf.txt` (a release build has no console, so the file is the real output)
- `PERF_PHASE=1` — per-phase timing inside the linear-light resampler
- `cargo run` — launch the GUI viewer; `cargo run -- --register` / `--unregister` manage file-type registration (HKCU, no admin) and exit
- Release builds hide the console (`windows_subsystem = "windows"`); debug keeps it for `eprintln!` diagnostics — don't "fix" this by removing the attribute

## Performance (the core goal — read before touching the pipeline)

- **Measure, don't guess.** `src/perf.rs` samples 15 latency metrics into fixed
  256-entry atomic rings (p50/p95/p99) plus 9 always-on counters. Enable with
  `--perf` or `PERF=1`. "First visible pixels" is committed in `frame_prelude`,
  i.e. *after* the painting frame was presented — never record it when the
  worker message arrives, that is not on screen yet.
- **`display-gap frames` must stay 0.** It counts frames that wanted to paint the
  current image and had no texture. Nonzero means something threw away the
  texture the user is looking at.
- **The resampler is the hot path for big non-JPEG sources.** `linear_resize`:
  sRGB box pre-reduction to ≤2x the target (integer running sums, rayon,
  precomputed reciprocals) → 12-bit linear conversion of *that* buffer → two
  kernel passes. Hard-won: the conversion must never run over the
  full-resolution source; a reciprocal **division inside the inner loop cost more
  than everything else combined**; the vertical pass needs its running column
  accumulator or it misses cache on every pixel.
- **A saved rotation is only correct if it is *composed*, and the composition is
  the whole problem.** "Save what I see" is two transforms, not one: the EXIF
  orientation the camera wrote, and the quarter turns the user pressed since.
  Getting the order or the sign wrong produces a mirrored or double-rotated
  file — and it is **invisible in this viewer**, because the viewer applies the
  EXIF tag itself and so cancels the error out. It is glaring in every other
  program. `Xform::then` is in `lib.rs` for that reason: the tests compare a save
  against `apply_exif_orientation` plus the viewer's own turns, over all 9 tags x
  4 turns, so the two cannot drift.
- **Never transcribe a transform table by hand.** `Xform::Transverse` was
  written as `(h-1-oy, w-1-ox)` and should be `(w-1-oy, h-1-ox)`; it compiles,
  it type-checks, and it underflows the moment the image is wider than it is
  tall. `Xform::then` composes by *probing* — substitute a point through both ops
  and find which of the eight matches — rather than by a multiplication table,
  and `composition_is_closed_over_all_sixty_four_pairs` checks the result against
  real pixels.
- **The orientation tag must be neutralised when pixels are baked in.** A saved
  file whose pixels are already in display orientation must report orientation
  1, or every other viewer double-rotates it. Only overwrite bytes that already
  exist: `neutralize_exif_orientation` sets the tag and zeroes IFD0's next-IFD
  pointer (which unlinks the now-stale thumbnail) and never reserialises the
  APP1 segment, because a malformed EXIF block is worse than an unreferenced
  thumbnail.
- **"Lossless" has to be demonstrated, not asserted.** `bench_rotate_save.rs`
  reports the output/input size ratio: 1.00-1.02x. A re-encode makes a fresh
  rate-distortion decision per block and moves that number; a DCT-coefficient
  transform does not. That ratio is the only evidence the claim is true, and it
  is a property of the output, so nothing in the code can assert it.
- **Decoding is never interrupted, and nothing is ever abandoned.**
  A job runs to completion even if the user has navigated away. An earlier
  design published a set of "wanted" paths from `schedule()` and cancelled any
  in-flight decode whose path left it; combined with `Loader::replace` dropping
  queued-but-busy jobs, that **stranded images that were never re-requested** —
  "random images don't load, and the stall watchdog says they are not cached, not
  poisoned, and nothing is decoding them". Do not reintroduce cancellation
  without proving every cancelled job is re-queued. Third-party decoders
  (turbojpeg/libheif/jxl-oxide) are not interruptible regardless.
- **A decode error is permanent for the session.** `failed` is a
  `HashSet<(PathBuf, Kind)>` and nothing clears it broadly — only that one path
  on triage (`remove_current_from_gallery`), the resize drop, and the filter
  change (`refilter`). A *transient* error (a file still hydrating from
  OneDrive, a partially-copied file) therefore poisons the image until restart,
  and the viewer keeps showing the old frame. If a load failure looks
  mysterious, suspect this before anything else.
- **Preloading is a fixed symmetric window**: `PRELOAD_RADIUS` (3) each side,
  "next" at `10 + 2d` and "previous" at `11 + 2d` so a step forward is slightly
  favoured. The current image is always `PRIO_CURRENT_PREVIEW` (0) and every
  prefetch job is ≥ 10, so prefetch can never starve it. Direction-aware radii
  were tried and removed.
- **`queued jobs dropped by rebuild` is the coalescing signal.** Large values mean
  the user outran the decoder — tune the preload window, don't add workers.
- **Grid thumbnails use the camera's embedded EXIF thumbnail** when there is
  one, which skips decoding the source entirely (measured 16-40x on the
  `test-images` JPEGs: 631 ms -> 16 ms, 748 ms -> 25 ms). Gated on
  `preview_max_dim <= EMBEDDED_THUMB_MAX_TARGET` (512), so it only ever applies
  to `Kind::Thumb` grid cells — the main view always decodes for real. The
  thumbnail is ~160 px, so a grid cell is slightly softer than a real 256 px
  decode; that is the trade, and it is the one every photo browser makes.
- **Never guess the embedded thumbnail's rotation.** The spec does not say
  whether a camera stores the thumbnail already rotated, and makers differ.
  `embedded_thumb_rotation` decides *per file from the aspect ratio* (a
  quarter-turn orientation makes the two hypotheses produce mirrored ratios) and
  returns `None` when it cannot — mirrored/180 orientations and near-square
  sources — so those files simply decode normally. A guessed rotation shows
  sideways photos in the grid, which is worse than being slow.
- **`find_embedded_thumbnail` parses untrusted input.** Offsets and lengths come
  straight out of the file. Every read is bounds-checked against the APP1
  segment, the IFD entry count is capped, the slice must start `FFD8` and end
  `FFD9`, and any failure returns `None` (decode normally) rather than panicking
  on a worker. Keep it that way if you touch it. IFD1 is reached via IFD0's
  *next-IFD pointer* (which sits after IFD0's entry table), not the header's
  IFD0 offset — reading the wrong one looks like it works and silently finds
  nothing.
- **Texture churn is already low**: previews upload once per decode, tiles are
  created once and reused, animated GIF frames update in place via `set`. Count
  uploads (`GPU texture uploads`) before "optimising" this.
- **Known waste:** `exif_orientation(path)` calls `nom_exif::read_exif(path)`,
  which re-reads the whole file even though `load_image_preview_full` already
  has it in memory. The new `Tiff` walker reads orientation's neighbours for
  free, so this is a cheap follow-up if decode time ever matters again.
- **Memory is the failure mode that looks like a hang.** `MAX_FULL_PIXELS` is
  300 MP = **1.2 GB** for one full-res buffer, and every cached preview carries
  one for free (single-pass decode), so the whole preview cache can hold several
  GB. It is bounded by **count** only (`PREVIEW_CACHE_CAP` = 16, LRU, never
  evicting the current image or its ±`PRELOAD_RADIUS` neighbours). If you raise
  the radius, re-check that. A worker panic here is usually OOM, and its payload
  is kept and logged (`C_DECODE_PANICS`) — don't reduce it back to a bare
  "decoder panicked"; that hid the real cause.
- **`stalls (>5s to first pixels)` must stay 0.** The watchdog reports the whole
  state (cached / poisoned / shown==cur / cache sizes / panics) once per request.
  It fires only when no pixels were painted at all — check `recorded_first`, not
  just age, or it cries wolf on cache hits.

## Gotchas

- **egui/eframe is 0.36; the renderer is `wgpu` on DX12, with Vulkan opt-in.**
  The renderer changed *as a side effect* of the 0.27→0.36 upgrade: 0.27's
  default features enabled `glow`, 0.36's enable `wgpu`. wgpu enumerates every
  enabled backend during adapter discovery, **before** picking one, and on this
  machine that enumeration is a hard `0xC0000005` before the window opens, with
  nothing on stderr. wgpu offers no "try this, else that" —
  `native_adapter_selector` still filters enumeration by `backends` first — so
  the set must be decided up front. `wgpu_backend_set()` therefore returns
  **exactly one bit, always**, and `wgpu_options()` pins it. Never make it return
  a set: two backends is not a fallback, it is a way to crash.
- **"Automatic" probing is per-backend child processes, and it must reach
  `request_device`.** The failure is an access violation, not an error return, so
  nothing in-process can survive it — the only place a crash can be contained is
  a child whose exit code the parent reads. The first version of the probe
  stopped at `enumerate_adapters` and reported **Vulkan as working** on the very
  machine where the app access-violates on it, which would have made Automatic
  crash the app on every launch. Enumeration walks the loader's list; the failure
  is further in, where the driver initialises. Probe as far as an adapter **and a
  device**; a surface is not probed because that needs a window, so a
  surface-only failure still passes. Order is `PROBE_ORDER` (DX12, GL, Vulkan)
  and must match `WgpuBackend::CHOICES`; the policy itself is pure and lives in
  `pick_probed` so it is testable at all.
- **Changing the renderer restarts the renderer, and the app cannot ask for
  that itself.** `run_native` only returns when the window closes, so the app
  writes a request into a shared `RestartSlot` and sends
  `ViewportCommand::Close`; the launch loop re-enters the event loop. The app is
  rebuilt from the config each time, so a restart keeps folder, sort and
  settings but loses the preview cache. If you add another thing that needs the
  event loop restarted, use the slot.
- **`WgpuBackend` serialises as lowercase, and that is load-bearing.** Without
  `#[serde(rename_all = "lowercase")]` the config file gets `"Dx12"` while
  `name()` and `parse()` say `"dx12"`, so a hand-edited config using the
  documented spelling fails to load. Found by reading a file the app had just
  written; `the_config_file_uses_the_same_spellings_as_parse_and_name` pins it.
- **There is no DX11 backend and there cannot be one.** wgpu removed its
  Direct3D 11 backend upstream; it is absent from `Backends` in wgpu 22+. Do not
  add a `Dx11` variant to satisfy a request for it — the answer is a documented
  "no", not a stub.
- **"Is Vulkan installed" is the wrong question, and the registry is a liar.**
  Do not reintroduce a `Khronos\Vulkan\Drivers` probe. Measured here: `vulkaninfo`
  reports a healthy Intel UHD 620 at API 1.2.187 with a clean environment, while
  `Khronos\Vulkan\Drivers` is **absent** (only `ImplicitLayers` exists, no
  `VK_*` vars) — the loader finds the driver's `igvk64.dll` some other way. And
  wgpu + Vulkan still crashes 3/3. So Vulkan working does not imply
  wgpu-on-Vulkan working, and the only way to discover that is to crash. Do not
  set `Backends::all()`, and do not enable Vulkan unconditionally.
- **Frame rates measured over RDP are meaningless — they were the reason for a
  whole wrong theory.** On the RDP session the animated GIF ran at 12.7 fps on
  wgpu vs 21 fps on glow, which looked like a wgpu smoothness regression. On the
  real 60 Hz display: **58.7 fps on wgpu, 59.5 on glow, 30.0 on 0.27** — the
  "regression" was entirely the remote display adapter. Always check whether the
  session is remote (`Win32_VideoController` showing a *Microsoft Remote Display
  Adapter*) before believing any frame-rate number.
- **Verify the session is NOT remote before trusting any frame rate.** This bit
  twice in one session: a whole "wgpu animation runs at 12 fps vs 21 fps on
  OpenGL" investigation, and a "LOW_LATENCY made it worse, 12 → 9.3 fps"
  experiment, were both artifacts of measuring over RDP. On the console session
  every renderer plays the animation at exactly 30 fps. Check with
  `quser` (look for `console`, not `rdp-tcp#N`) and `Get-CimInstance
  Win32_VideoController` (a *Microsoft Remote Display Adapter* means remote).
- **Frame-rate measurement traps, all of which produced wrong numbers here.**
  (1) RDP, above — the big one. (2) `-WindowStyle Hidden`: a hidden window gets
  no ordinary paint/vsync delivery; every fps measured that way was garbage
  (swung 12/13/19/59 across runs). (3) Deriving fps from
  `frames_built / process_lifetime` folds in startup and idle. (4) Reading only
  percentiles from the 256-entry ring, whose contents depend on sample age. Use
  a **visible** window, a settle period longer than any decode, and instrument
  the thing you are actually measuring.
- **Verified on the console session, 60 s visible, static photo / 30 fps GIF:**

  | | idle CPU% (static) | animation fps |
  |---|---|---|
  | 0.27 + glow *(before)* | 6.5 | 30 |
  | 0.36 + glow | 5.4 | 30 |
  | 0.36 + wgpu DX12 *(now)* | **4.6** | 30 |

  So the upgrade is a net win on CPU and animation is correct everywhere. The
  only difference worth noting: 0.36 draws at 60 fps (vsync-locked) where 0.27
  drew at 30, i.e. two draw calls per animation frame. That is a small waste,
  not a defect, and being vsync-locked is arguably the better behaviour.
- **The loader and the job plan live in `lib.rs`, and `tests/flip_through.rs`
  drives them.** They used to be in `main.rs`, which is why the stranding bug
  went uncaught. `Loader` is generic over its message type (it takes a `run`
  closure and a `panic_msg` closure), `plan_jobs` is a pure function of a `Plan`,
  and job priorities live beside the planner so they cannot drift.
- **The queue keeps a `wanted` set, and that is what makes abandoning work.**
  `replace()` records what the viewer wants; if a job then finishes *without
  producing a result* and is still wanted, it is re-queued. Do not "simplify"
  this to re-queue on completion — a job that already delivered would be decoded
  forever. Do not reimplement it by remembering only the wanted-but-*running*
  jobs either: that was the first attempt and it is wrong, because a job that
  was merely *queued* at the time and abandoned later is then lost. The only
  correct question on finish is "is this still wanted?".
- **Why that matters here:** `schedule()` only re-runs on a step, when the
  *current* image's preview arrives, on a dwell expiry, or on a resize. A
  stranded *neighbour* triggers none of those, so it stays stranded for the
  session — the "random images never load" report. `tests/flip_through.rs`
  pins this: it ends with a single final `replace` while a wanted job is in
  flight and then never prompts again, and asserts the plan for the current
  position is empty. Reinstating the old `replace` makes it fail; note that
  looping until convergence instead of stopping makes it *pass*, so do not
  "fix" a failure by adding re-asks.
- **A result is sent before its worker frees the busy slot.** So a queue that
  looks idle can still have an unread message, and a test that checks state
  after waiting for idle must drain once more or it will report a strand for a
  picture that was in fact delivered. This cost two false failures.
- **`--perf <folder>` used to open the wrong folder, silently.** `MyApp::new`
  took the path as `args_os().nth(1)` while `main` scanned the whole list for
  flags, so `--perf` was taken as the path, matched nothing, and the app fell
  through to the persisted/default folder. The perf report still got written —
  for the wrong folder. Fixed by `image_viewer::path_arg` (in `lib.rs`, not
  `main.rs`, so a test can see it; `main.rs` being a binary is why this
  survived). **If you add a CLI flag, add it to `path_arg` and its test.** The
  tell that it bit: a run of a *one-file* folder reporting 7 decode calls.
- **Check the session is not remote before trusting any frame rate — and treat
  that as covering input and resize events too, not just fps.** This has now
  cost three wrong conclusions in one session: a GIF frame-rate "regression" that
  was the remote display adapter, and a resize debounce whose before/after
  comparison (11 vs 9 re-decodes) was meaningless because RDP coalesces and
  delays window-resize events. Separately, synthetic key injection
  (`keybd_event`/`SetForegroundWindow` from another process) silently does
  nothing in an RDP session — `frames with input events` stays 0, which is how to
  notice. `quser` reporting `console` rather than `rdp-tcp#N` is the check.
- **A second read of a file you just read is free.** `exif_orientation` calls
  `nom_exif::read_exif(path)` although the bytes are already in memory, which
  reads like an obvious win. Measured: 0.1-0.2 ms against a 10-17 ms `fs::read`,
  because the page cache has it. Don't do this "optimisation" — it is ~0.02% of a
  preview decode.
- **Never debounce a resize with a wall-clock timer here.** Coalesce by
  comparing with the *previous frame* instead. A large preview takes ~800 ms to
  decode, so while one is in flight frames are ~800 ms apart — and a timer
  shorter than that expires between every pair of them and never fires. A
  reverted attempt used `RESIZE_SETTLE = 140 ms` and made almost no difference
  for exactly this reason. The test is "the size did not change since the last
  frame"; one of those frames is requested explicitly, or the *final* size of a
  drag is never examined because nothing else asks for a repaint.
- **A frame count does not mean the app is spinning.** Frame rate can be set by
  the presentation pipeline rather than by demand for frames; a 20x smaller
  window produced the same cadence here, which is what gave it away. Before
  concluding the app burns CPU idly, check `frames with input events` (0 while
  idle) and `frame interval` in the `--perf` report, and ask egui who asked:
  the report's "who asked for a repaint" section lists the `file:line` of every
  `request_repaint`. On a static photo the only source is the 2 s folder
  watcher (`maybe_rescan`) and the app idles at ~2 fps. That is correct.
- **`ms/frame` is meaningless when frames are rare.** It is CPU amortised over
  whatever frames happened; at 2 fps on a static photo it is just startup
  spread thin. Use total CPU% unless frames are frequent (an animation).
- **Use `Ui::new_child(UiBuilder)`, never `Ui::scope_builder(..)`, to replace
  0.27's `Ui::child_ui(rect, layout)`.** They look interchangeable and are not.
  `scope_builder` runs the closure and *then* calls
  `advance_cursor_after_rect` on the parent, so calling it on a `Ui` that has
  already allocated its own rect double-advances the cursor. In `grid_cell` that
  silently ate 4 pt of `item_spacing` per cell — cells tightened from 8 pt to
  4 pt, no error, no warning. `new_child` returns an owned `Ui` and leaves the
  parent's cursor alone, which is exactly what `child_ui` did.
- **Headless egui tests must clear `TexturesDelta` (0.36).** epaint now
  debug-asserts when one is dropped unapplied. A `Context::run_ui` pass emits
  the font-atlas delta every frame and nothing applies it (eframe's renderer
  does in the real app), so tests go through the `run_pass` helper in
  `tests/unit.rs`, which calls `out.textures_delta.clear()`. Note
  `Context::run(input, |ctx| ..)` is now `Context::run_ui(input, |ui| ..)` and
  the closure gets a `Ui`; `CentralPanel`/`Panel` are shown on that `Ui`.
- **`RectShape` moved its texture fields into a `Brush`.** 0.36 has
  `brush: Option<Arc<Brush>>` holding `fill_texture_id` + `uv`; a plain
  `RectShape::filled` has `brush: None`. `test_grid_thumbnail_is_centred_and_textured`
  inspects it to prove a cell isn't a solid colour block, and it has to look
  through the `Option` now.
- **Don't hardcode egui's default `item_spacing` in a test.** The grid test used
  to assert an 8 pt gap between cells. `grid_row` now reports the spacing from
  the layout that actually placed them. Egui's defaults move between releases and
  a hardcoded number fails on upgrade for no real reason.
- **`ComboBox::from_id_source` → `from_id_salt`, and `ComboBox::new(id, "")` is
  NOT the replacement.** 0.36 stores `label: Option<WidgetText>` and calls
  `ui.label(label)` when it is `Some`, so `new(id, "")` renders an empty label
  widget and shifts the settings panel. Use `from_id_salt`.
- **Every suite is fixture-free now; do not reintroduce hardcoded
  `test-images/` paths into tests.** `test-images/` is gitignored, so a test
  that reads from it passes on your machine and fails in a fresh clone —
  `tests/standard.rs` did exactly that for the whole history of this repo and
  died with a bare `NotFound`. Build the image you need with the `image` crate
  in a `tempfile::tempdir()`. The two **bench** files are the deliberate
  exception (they measure a real large photo, so generating a synthetic one
  would change the numbers): they pick the largest JPEG in `test-images/` and
  print a "drop a photo in test-images/ to benchmark" message instead of
  panicking if there is none.
- **`load_image`'s `max_dim` is a minimum long-side target, not a hard cap.**
  `decode_jpeg_scaled` picks a power-of-two DCT factor and deliberately chooses
  the smallest reduction that still leaves `>= max_dim`, so a source sitting
  between two factors comes back *larger* than asked for rather than smaller and
  blurry: `load_image(2400x1600, 1600)` returns 2400x1600, because 2400/2 = 1200
  is already below the target. Assert the real contract in tests; I wrote the
  intuitive one first and it failed.
- **`cargo test` aborts after the first failing target** — use
  `cargo test --no-fail-fast` when you want the whole picture, and note that
  `--lib` (the in-crate `perf` tests) is a target that the per-target commands in
  this file never run.
- **The icon is generated code, and its look is asserted, not reviewed.** It
  lives in `src/icon.rs` (not `examples/`) precisely so `tests/icon.rs` can check
  it. Two bugs got through a working build, a working `ExtractIconExW` and a
  passing registration test, because generated artwork is not a diff anyone reads:
  the badge had a **half-extent of 0.22 in a 1×1 canvas**, so the icon filled
  **44%** of its own bitmap and read as a stamp in an empty box; and the artwork's
  local coordinates divided by the badge *width* (`2*HALF`) instead of its
  half-extent, so `lx` only reached ±0.53, which squeezed everything inboard and
  pushed the sun under the rounded corner. `coverage()` and
  `local_coordinates_really_span_minus_one_to_one` exist for exactly this — and
  both were verified to *fail* with the bug reintroduced, because a test that
  passes on the broken code is decoration. **Look at
  `assets/app-icon-preview.png` after changing it.** Also: to verify a bug fix,
  `Copy-Item` restores the *backup's* old timestamp, so cargo skips the rebuild
  and you test a stale binary — `touch` the file or the green run is a lie.
- **The window icon is `ViewportBuilder::with_icon`. Do not reach for `WM_SETICON`.**
  Four attempts went into this before the obvious API turned up, and every earlier
  one was the icon arriving *too late*: `WM_SETICON` only works if the window
  procedure forwards it to `DefWindowProc` (winit's does not — it serves icons from
  its own state, so `GWL_HICON` reads 0 while `WM_GETICON` does not, and reading
  the long as authoritative is how you conclude the icon is missing when it is
  not); DWM caches the title bar's icon when the non-client area is built, before
  any of our code runs, and a later `WM_SETICON` plus `RedrawWindow` does not
  displace it; and setting the window *class* icon works and still is not enough.
  `with_icon` is the only moment the window does not exist yet. **Check for a
  framework API before writing a P/Invoke module** — there is no `winicon.rs` here
  any more, and that is the point.
- **Explorer and the taskbar get the icon by different routes.** Explorer reads
  the registered `DefaultIcon`; the title bar and the running taskbar button read
  the *window*. Fixing one tells you nothing about the other, which is why
  "Explorer looks right, the taskbar does not" is not a contradiction.
- **An AppUserModelID's `IconUri` must be a bare path.** Quoting it (the quotes
  in Microsoft's examples are JSON syntax, not part of the value) makes the shell
  fail to resolve the icon, with a very confusing symptom: a *pinned* taskbar
  button still shows the right icon, because pinning builds its own reference to
  the exe, while the running window's button falls back to the default. Restarting
  Explorer does not help — it re-reads the same unparseable value.
- **`build.rs` is load-bearing**: it passes `/NODEFAULTLIB:LIBCMT(D)` because turbojpeg is built with the static CRT while Rust links the dynamic one. Delete it and every build emits LNK4098 warnings. It also embeds `assets/app.ico`; a running viewer locks the exe and the build fails to replace it.
- **Format handling is content-based, never extension-based.** Magic-byte sniffing picks the decoder (JPEG/PNG/WebP/GIF/TIFF/BMP/ICO/QOI via the `image` crate; JXL via `jxl-oxide`; HEIC/HEIF/AVIF via the `heic` crate; misnamed files like a PNG saved as `.jpg` must keep loading — regression test exists). `image_dims_bytes` probes header dims per format. This is also why several `image` crate decoders are deliberately left disabled, most sharply **Targa, which has no signature at all** and is therefore unidentifiable here — `a_targa_cannot_be_identified_from_its_content` demonstrates that with a hand-built valid Targa instead of asserting it. Do not "fix" it by passing the extension into the decode path. Full list with reasons: `IMAGE_CRATE_DECODERS_WE_SKIP` in `lib.rs`.
- **A registered file type needs a `DefaultIcon`, and must NOT have a `ShellEx\ThumbnailHandler`.** Without the first, Explorer shows the generic *application* icon — which is what happens once the viewer is the default, because the ProgID has no icon and the exe has no embedded one either. `build.rs` embeds `assets/app.ico` via `winresource`; an `.ico` beside the binary is not consulted, because the shell reads the exe's *resources*. Without the second, WIC keeps supplying real photo thumbnails — claiming that key would displace WIC and replace actual thumbnails with an icon, which is strictly worse. Icon and thumbnail are separate concerns; fix only the icon. Explorer caches icons per user, so a registration change needs `ie4uinit.exe -show` to become visible.
- **`normalized()` (lib.rs) guards every decode exit** — and it must guard the
  *downscale* exit too, not just decode. Two ways a buffer can disagree with its
  declared dims, both of which abort a worker on
  `ColorImage::from_rgba_unmultiplied`'s `assert_eq!(size[0]*size[1]*4, rgba.len())`:
  a decoder that mis-reports its size (truncated files, OneDrive hydration
  races), and **`DynamicImage::resize` silently returning a smaller image than
  requested** (it goes through `resize_dimensions(fill = false)`, which
  preserves the aspect ratio — 980x552 asked for 300x168 comes back 298x168).
  Report the size the library actually produced, and keep the `normalized()`
  wrap. This one presented as "decoder panicked" on `6.png`, permanently
  poisoned the image in `failed`, and looked machine- and window-dependent only
  because previews are sized to the display area, so a small target rounds hard
  enough for the aspect-ratio clamp to bite.
- **EXIF orientation is applied only to JPEGs.** For HEIF/AVIF the `heic` crate already applies container transforms (`irot`/`imir`); applying the EXIF tag on top would double-rotate. Some HEICs in test dumps have no EXIF at all (exiftool-verified) — that's correct, not a bug.
- **Colour is *detected*, never transformed (yet).** `color_info` parses the ICC
  profile and reports the space; no pixel is touched. The renderer uploads
  `ColorImage` as `Rgba8UnormSrgb` and egui wants a gamma-space surface, so the
  pipeline is 8-bit sRGB end to end in **0.27 and 0.36 alike** — a newer egui
  does not unlock HDR or wide-gamut output (`TextureOptions` has no `srgb` field;
  `App::clear_color` is documented as gamma-space sRGB). Converting P3 → sRGB is
  therefore not a downgrade on an HDR monitor; it is the only way to display P3
  correctly at all. Real numbers: **200 of 1001** images in a phone backup are
  Display P3.
- **Classify colour by primaries, not by the description string.** 170 of those
  200 files carry a profile with **no `desc` tag at all**. The two primaries that
  actually occur are sRGB `(28578, 14581, 912)` and Display P3
  `(33759, 15807, -69)`, both D50-adapted, and they never mix. Those are
  *measured* values — do not "correct" them from memory; I guessed P3 as
  `(43682, 22250)` once and it was wrong.
- **PNG `iCCP` is zlib-compressed.** Handing the raw chunk to the ICC parser
  finds no `acsp` at offset 36 and silently reports "unknown". Inflating it is
  not optional. This shipped as a bug until real P3 PNGs appeared to test against.
- **`icc_tags_valid` refuses corrupt profiles outright.** Every tag must lie
  inside the profile and the declared count must fit. Both wrong answers are
  harmful: claiming sRGB skips a conversion the image needs, and claiming wide
  gamut pushes a correct image through a P3 matrix. "Unknown" is the safe answer.
- **HEIC/AVIF/JXL report `NotInspected`, deliberately not assumed sRGB.** Their
  colour lives in an ISO-BMFF `colr` box or the codestream. Assuming is how a
  wide-gamut HEIC looks flat and nobody notices, because everything "works".
  29 of the 1001 files in that backup are HEIF and are currently a blind spot.

## Architecture (things that are easy to break)

- **Decode is single-pass** (`load_image_preview_full` in `src/lib.rs`): the file is decoded once; the preview texture is derived from the full decode and sized to the *display area* (`Job.target_dim`, the central panel in physical px). Large JPEGs (> 12 MP, `FAST_FULL_MAX_PIXELS`) are the exception — turbojpeg DCT preview, then a full decode after a 120 ms dwell. Grid thumbnails (`Kind::Thumb`, target 256) are the other exception: they may come from the embedded EXIF thumbnail, in which case `full` is `None` and `below_target` is true, which is correct — nothing adopts a grid thumbnail, and the main view never asks for one.
- **The full image rides on the preview cache.** `Preview` carries `full: Option<Arc<ImageData>>` (and `anim` for GIFs); `sync_shown(ctx)` adopts it for the current image. `needs_full()` checks data availability, *not* texture size vs. source size — the size heuristic was a bug that permanently skipped full-res for preloaded neighbors. Don't reintroduce it.
- **Resize events**: growing the display drops `below_target` previews and re-decodes at the new target. Panel toggles and F11 set `suppress_resize_redecode` for exactly one transition so layout changes don't count as "window grew".
- **Hold-to-browse** (`block_while_loading` setting): `step()` is throttled while `waiting_load`; on preview arrival a `pending_advance` is queued and consumed at the *start of the next frame* — the arrived image is guaranteed to have been painted. Consuming it in the same frame skips the render (that was the original bug).
- **Animated GIFs** decode to all composited frames (`Animation`, caps: 240 frames / 256 MB), ride the preview cache, never get tiles, and play back through a single re-uploaded texture with delays floored to 20 ms.
- **Never decode on the UI thread.** All decode runs on the worker pool (`Loader` in `main.rs`); the UI only paints textures. Grid thumbnails use `Kind::Thumb` jobs (`Loader::push`, prio 45) and an LRU-capped `thumbs` cache — `grid_cell` requests them lazily for visible cells only.
- **Folder watcher**: `maybe_rescan` polls every 2 s. `check_changed` re-stats
  every gallery file (`Stamp` = mtime + len) and invalidates anything whose
  contents moved; `rescan` adds/removes files. Change detection is deliberately
  **not** gated on `load_all_on_startup` — that setting is about listing files at
  startup, and noticing that the file you are looking at was just re-exported
  should not depend on it. Both are skipped while a cbz/zip archive is open.
- **Reloading a changed file is self-healing, and that is the point.**
  `invalidate_path` clears the `failed` entry as well as the caches. A file
  re-read *while it is still being written* fails to decode, and `failed` is
  permanent for the session — but the writer finishing moves the stamp again, so
  the next poll re-detects and retries. Do not "optimise" that `failed.retain`
  away: without it, editing the image you are looking at reliably poisons it,
  which is the "random images never load" bug from this session. The reload
  deliberately does **not** keep the stale texture on screen (a re-export can
  change the dimensions); it blanks for one decode, like navigating to an
  uncached image.
- **The filter is the whole viewer, not just the grid.** `MyApp.all_files` is
  the unfiltered list; `gallery.files` is it run through `filter` by
  `Gallery::set_filtered` (in lib.rs, so it is testable). Every site that
  rebuilds the whole file list goes through `set_files`/`adopt_gallery` so the
  filter cannot be forgotten. The filter is *not* persisted — a filter that
  silently survived a restart would hide files with no obvious way back.
- **Global single-key shortcuts must stand down while typing.** The filter box
  is the first text input this app has, so `App::ui` reads
  `ctx.egui_wants_keyboard_input()` and gates every single-letter and navigation
  key on it. Without that, typing "g" into the filter toggles the grid and "r"
  reloads the folder. Esc while the box is focused clears the filter (or
  defocuses) and must never fall through to "close the grid / quit".
- **The grid's scrollbar needed `auto_shrink([false, true])`.** With
  `[false, false]` the `ScrollArea` demands the full panel width, leaving no room
  for the bar, so a long folder had no visible scroll position.
- **End-of-folder navigation is a `Gallery` policy, not a caller concern.**
  `Gallery::loop_back` (mirrors `Config::loop_back`, default **on** so old
  configs keep looping) lives on the gallery so `next`/`prev` cannot be used in a
  way that quietly ignores the setting. Both return `bool` — whether the view
  actually moved — and `step()` skips `goto()` when it did not, which avoids
  resetting the full-res dwell timer and re-planning the queue for an image that
  did not change. The grid's own selection already clamped and never wrapped.
- **Key bindings to know**: F11 = fullscreen + hides all chrome; I = panels; G = gallery grid; `/` = focus the filter box; in the grid the arrows/Home/End/PageUp/PageDown move a *selection* (they do not change the shown image; Page moves one viewport, tracked as `grid_page_rows`) and Enter opens it; Del/M/C = triage; E = open folder in default file browser; W = wallpaper; Ctrl+C = copy image; R = reload folder; Esc = clear filter (if typing) → close grid → exit fullscreen → close window. Side mouse buttons = prev/next. Rotate ⟲/⟳ are bottom-panel buttons only (temporary UI; `self.rot` is a view transform, re-fits after each turn).

## Gotchas by feature

- **CBZ/ZIP**: opening an archive extracts supported entries to `%TEMP%\image-viewer\<hash(path, mtime, size)>` and opens it as a folder gallery. The temp dir is reused for unchanged archives, deleted on folder switch and exit. Triage (Del/M/C) is disabled for archives; wallpaper copies the member to `%APPDATA%\image-viewer\wallpaper.<ext>` first (temp dir dies). Don't persist the temp path into `config.folder`.
- **Resume (`last_by_folder`)** stores **file names, not full paths** (archives and folders share the format); the folder path is the key. An explicit CLI file argument beats resume — don't reintroduce the unconditional resume lookup.
- **File registration** (`src/fileassoc.rs`): per-user HKCU ProgID + `OpenWithProgids` for all 9 extensions; never claims default silently (Windows 8+ policy). ProgID embeds the exe folder name — moving the exe requires re-registering.
- **Linear-light resize** (`downscale_image` → `linear_resize`): 12-bit sRGB↔linear LUTs, runtime-selectable filter (`DownscaleFilter`: Hermite/Catrom/Lanczos3 via `set_downscale_filter`), custom separable resampler because the `image` crate's `Filter` is `pub(crate)`. Overshoot must clamp LUT indices to 4095 (index 4097 was a worker panic). Known wart: toggling `linear_resample` doesn't re-decode existing previews (the filter dropdown does) — see `refilter` in the settings handler if you touch this.

## Misc

- **Config location is platform-dependent:** `%APPDATA%\image-viewer\config.json` on Windows — see `Config::default_path()` in `src/lib.rs`. New `Config` fields need `#[serde(default)]` or old configs fail to load.
- **`test-images/JPEGView_L` is a checked-out reference viewer** (JPEGView fork, C++) used for feature ideas, not part of the app.
- **`PLAN.md` is a stale early-project feature list, not documentation of current behavior** — most of it is already implemented. `TODO.md` holds the actual deferred work (P3 → sRGB conversion, the HEIC `colr` blind spot) with the investigation already done. Read `TODO.md` before starting anything colour- or GUI-related; it records verified numbers that are expensive to re-derive.
