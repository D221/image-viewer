# AGENTS.md

Windows-only Rust desktop app: egui/eframe 0.36 on wgpu. A speed-first image
viewer built on one rule — **the UI thread never decodes.** Every decode,
preview and full-res tile runs on a worker pool; the UI only paints textures
that are already in memory. One crate, no workspace, no CI.

## Commands

- `cargo test --no-fail-fast` — the whole suite. Plain `cargo test` stops at the
  first failing target, and `--lib` (the in-crate `perf` tests) is a target no
  per-target command below ever runs.
- `cargo test --test <file>` — one test file; `cargo test <test_name>` — one test.
- 21 test targets: `bench`, `bench_browse`, `bench_resize`, `bench_rotate_save`,
  `bench_startup`, `bench_variants`, `color_profiles`, `embedded_thumbs`,
  `filter_and_paging`, `flip_through`, `formats`, `icon`, `integration`,
  `keybindings`, `multiframe`, `pathological`, `property`, `renderer`,
  `rotate_save`, `standard`, `unit`.
- **`cargo bench` runs nothing.** `tests/bench.rs` and `tests/bench_variants.rs`
  use `criterion_main!` but are ordinary `{test}` targets with the default
  harness (there is no `[[bench]] harness = false` in `Cargo.toml`), so
  criterion's `main` is never called: `cargo bench` prints "running 0 tests" and
  no criterion output. The benchmarks that actually produce numbers are the
  `#[ignore]`d ones below.
- `cargo test --release --test <bench_*> -- --ignored --nocapture`:
  - `bench_browse` — the number to move when changing the queue, the preload
    window or the cache policy: time-to-first-pixels, wasted decode share, peak
    MB, cache-hit rate while flipping a generated folder. `IV_BENCH_N` (24),
    `IV_BENCH_RUNS` (3).
  - `bench_resize` — decodes per window drag.
  - `bench_rotate_save` — lossless save time **and output/input size ratio**;
    1.00–1.02x is the only evidence "lossless" is true, because it is a property
    of the output that no code can assert.
  - `bench_startup` — start-up per candidate. `IV_BENCH_FOLDER` is an env var
    because `args().nth(2)` picks up `--nocapture`.
  - `pathological` — generated 40/60 MP images; the only way to see where
    big-image time actually goes.
- `powershell -File tests\<script>.ps1` — console session only (see Measuring):
  - `bench_launch_ab.ps1 -Runs 9` — interleaved A/B of two builds via the app's
    own `--perf` report. The one to trust for "did my change help".
  - `bench_launch.ps1 -Runs 5` — launch vs JPEGView_L (checked out in
    `test-images`).
  - `bench_launch_apps.ps1 -Rounds 7` — vs IrfanView and Windows Photo Viewer.
    Photo Viewer was removed in Win11 24H2/25H2 and cannot be benchmarked here;
    IrfanView and Oculante can, and Oculante (also egui) is the fair one — it is
    not faster, only more variable.
  - `bench_framerate.ps1` — gl vs dx12 frame rate; refuses to run over RDP.
  - `probe_chords.ps1` — "did that keystroke actually do the thing?" Injects
    `Ctrl+C` / `Ctrl+S` and checks the two effects that cannot be faked: the
    clipboard and a new file on disk.
- `cargo run` — the GUI. `-- --register` / `--unregister` (HKCU, no admin, then
  exit). `-- --icc-scan <folder>` classifies every image in a folder and prints
  the tally (it is deliberately **not** in `path_arg`'s `FLAGS_WITH_VALUE`: there
  the value *is* the folder, and adding it would leave the command with none).
  `-- --perf <folder>` — latency report on exit, also written to
  `%TEMP%\image-viewer-perf.txt`; **a release build has no console, so the file
  is the real output.** `PERF=1` for sampling without a folder, `PERF_PHASE=1`
  for per-phase resampler timing. `--probe-renderer <name>` is the internal flag
  the backend probe spawns children with.
- Release hides the console via `#![cfg_attr(not(debug_assertions),
  windows_subsystem = "windows")]` (`src/main.rs:21`); debug keeps it for
  `eprintln!`. Don't "fix" that by removing the attribute.
- `cargo run --release --example make_icon` regenerates `assets/app.ico`;
  `cargo run --release --example bare_eframe` is the control that measures the
  renderer's own cost with no viewer code at all.
- Needs the MSVC toolchain and CMake (turbojpeg-sys builds libjpeg-turbo).

## Layout

- `src/lib.rs` (~4.6k lines) — everything testable: sniffing, decode, gallery
  and paging, the job planner, the loader queue, the resize policy, the
  transform algebra, ICC, archives, config. **If a rule can be a pure function it
  belongs here** — every bug that could not be tested lived in `main.rs`.
- `src/main.rs` (~4k lines) — the binary: `MyApp`, egui, worker wiring, dialogs,
  clipboard, wallpaper, Windows integration. Unreachable from a test.
- `src/perf.rs` (16 latency metrics, 11 always-on counters), `src/fileassoc.rs`
  (HKCU registration), `src/icon.rs` (generated icon artwork).
- `test-images/` is gitignored. `test-images/JPEGView_L` and
  `test-images/oculante` are reference checkouts, not fixtures.
- `README.md` is the user-facing document (features, keys, settings, caveats) and
  is kept in step with the code; `TODO.md` is the deferred-work list, with the
  investigation already done. Read `TODO.md` before starting anything colour- or
  GUI-related — it records verified numbers that are expensive to re-derive.
  `PLAN.md` no longer exists; don't cite it.

## Decode pipeline

- **Single-pass decode** (`load_image_preview_full`): one decode, the preview is
  derived from it and sized to the *display area* (`Job.target_dim`, central
  panel in physical px). Two exceptions: JPEGs over `FAST_FULL_MAX_PIXELS`
  (12 MP) get a turbojpeg DCT preview then a full decode after `FULL_DWELL`
  (120 ms), and grid thumbs (`Kind::Thumb`, 256) may come from the embedded EXIF
  thumbnail — then `full` is `None` and `below_target` is true, which is
  correct: nothing adopts a grid thumbnail.
- **The full image rides on the preview cache** (`Preview.full`, plus `anim` for
  animations); `sync_shown` adopts it. `needs_full()` checks data availability,
  *not* texture size vs source size — a size heuristic permanently skipped
  full-res for preloaded neighbours.
- **Never cancel or abandon a decode.** A job runs to completion even after the
  user moves on. An earlier design cancelled in-flight decodes whose path left
  the wanted set; combined with `Loader::replace` dropping queued-but-busy jobs
  that stranded images that were never re-requested ("random images never load,
  and nothing is decoding them"). Third-party decoders aren't interruptible
  anyway.
- **The queue's `wanted` map is what makes abandonment safe**: on finish, a job
  that produced *nothing* and is still wanted is re-queued. The only correct
  question on finish is "is this still wanted?" — not "was it wanted and running a
  moment ago", and not "did it already deliver" (that decodes forever). A
  **result is sent before the worker frees its busy slot**, so a queue that looks
  idle can still have an unread message: a test that waits for idle must drain
  once more or it invents a strand.
- `schedule()` only re-runs on a step, on the current preview arriving, on a
  dwell expiry or on a resize. A stranded *neighbour* triggers none of them, so
  it stays stranded for the session. `tests/flip_through.rs` pins this and is
  verified to fail with the old `replace`; note that looping until convergence
  instead of stopping also makes it pass, so don't "fix" a failure with re-asks.
- `plan_jobs` is pure (`Plan` in, `Vec<Job>` out) and priorities live beside it:
  current preview 0, current EXIF 5, full 15, every prefetch ≥ 10 ("next" at
  `10 + 2d`, "previous" at `11 + 2d`), thumbs from 45. `PRELOAD_RADIUS` is a
  fixed symmetric 3 — direction-aware radii were tried and removed. `replace()`
  makes the queue match exactly (navigation); `push()` is additive (grid thumbs
  re-arm every frame, so they survive a rebuild). `queued jobs dropped by
  rebuild` is the coalescing signal: large values mean the user outran the
  decoder — widen the window, don't add workers.
- **`failed` is `HashSet<(ImgKey, Kind)>` and a failure is permanent for the
  session.** Only triage removal, the resize drop, the filter change
  (`refilter`) and `invalidate_path` clear entries. A transient error (a file
  still hydrating from OneDrive, a half-copied file) poisons the image until
  restart while the viewer keeps painting the old frame. If a load failure looks
  mysterious, suspect this first.
- **Reloading a changed file is self-healing and must stay that way**:
  `invalidate_path` clears the `failed` entry, so a file re-read mid-write fails
  once and succeeds on the next poll. It deliberately does *not* keep the stale
  texture (a re-export can change the dimensions) — it blanks for one decode.
  Watcher: `maybe_rescan` every 2 s, `Stamp` = mtime + len, deliberately **not**
  gated on `load_all_on_startup`, both halves skipped while an archive is open.
- **Resize is coalesced by comparing with the previous frame**, never with a
  wall-clock timer: `needs_refine` + `ResizeTracker` in `lib.rs` return
  `Wait` / `Refine` / `Idle`. A big preview takes ~800 ms to decode, so frames
  are ~800 ms apart and a shorter timer expires between every pair and never
  fires. `Wait` must call `request_repaint_after(RESIZE_SETTLE_REPAINT)` or the
  final size of a drag is never examined (nothing else asks for a repaint).
  Panel toggles and F11 call `suppress()` so layout changes don't read as growth.
- **Hold-to-browse**: `step()` is throttled while `waiting_load`; the queued
  `pending_advance` is consumed at the *start of the next frame* — consuming it
  in the same frame skips the render.
- **Memory is the failure mode that looks like a hang.** `MAX_FULL_PIXELS` is
  300 MP = 1.2 GB for one buffer, and every cached preview carries one for free.
  Bounded by count only: `PREVIEW_CACHE_CAP` 16 (LRU), `MAX_TILES` 64 × 1024
  (≈256 MB), `THUMB_CACHE_CAP` 512 with `THUMB_TRIES` 3, animations 240 frames
  / 256 MB with delays floored to 20 ms. `protected_preview_keys` is a pure
  function in `lib.rs` for a reason: the obvious reading — protect every page of
  every protected *file* — makes the cache **unbounded** on a 200-page document,
  which cannot happen on a photo folder and is invisible until you read memory.
  If you raise `PRELOAD_RADIUS`, re-check all of this. A worker panic here is
  usually OOM; its payload is logged (`C_DECODE_PANICS`) — don't reduce it back
  to a bare "decoder panicked", that hid the cause.
- **The filter is the whole viewer.** `MyApp.all_files` is unfiltered;
  `gallery.files` is it through `Gallery::set_filtered`. Every site that rebuilds
  the list goes through `set_files` / `adopt_gallery` so the filter cannot be
  forgotten. The filter is not persisted (a silent filter would hide files with
  no way back). Every single-letter and navigation key is gated on
  `!typing` (`ctx.egui_wants_keyboard_input()`), and `Esc` while typing must
  never fall through to "close the grid / quit".
- `Gallery::loop_back` is a field (mirrors `Config::loop_back`, default on so old
  configs keep looping) and `next`/`prev` return whether the view moved, so
  `step()` can skip `goto()` and leave the dwell timer and the queue plan alone.
- `Gallery::empty()` is the start-up path: `Gallery::new` scans the folder, and
  `load_all_from_folder` used to scan it a second time only for that result to be
  overwritten on the next line (worth 71 ms median, 7.8%).
- `startup_index(files, named, resume)` decides the start index and is the only
  place the "an explicit CLI file beats resume" rule lives. Resume
  (`last_by_folder`) stores file *names*, not paths, keyed by folder — archives
  and folders share the format.

## Paging (multi-page TIFF)

- **`ImgKey { path, page }` keys every cache that holds pixels** — preview
  cache, `failed`, loader `wanted`/`busy`, `Job`, `Msg::Preview`/`Full`, `cur`,
  `shown`. A path-keyed cache hands back page 1 when you asked for page 3,
  silently, and only for multi-page files. Still keyed on the *path*: grid
  thumbs, watcher stamps, `ImageEdits` (a turn or a crop is an edit to the
  document, and it is what `Ctrl+S` writes). `Job::path()` is the filesystem half.
- **The page is a cursor, not a list**: one entry per file in `Gallery.files`,
  the page is a separate field. Consequence: `prev` from page 0 goes to the
  **last** page of the previous file, not its first.
- **Page counts are learned on arrival, never during a scan.**
  `tiff_page_count` reads four bytes and gives up for anything that is not a
  TIFF, because it runs on every `goto`; the answer is cached per file
  (`learn_pages`) and must be dropped on re-export (`forget_page_count`). A test
  proves it by *deleting* the file and requiring the answer to survive.
- **CMYK pages are converted by `image`, and everything else is still refused
  with an error.** `image` owns the `(255 - c) * (1 - k/255)` maths and it cannot
  be pointed at page 2 — its TIFF decoder only reads page 0, which is the entire
  reason `tiff` is a direct dependency — so `cmyk_via_image` re-wraps the samples
  we already hold in a fresh one-page TIFF and decodes *that*. One copy of the
  page, on a path only reached for a type the plain reinterpretation cannot
  express. Palette, CMYKA, YCbCr, Lab and multiband have no such route and stay
  errors, because an approximation would produce pages that are subtly and
  silently the wrong colours — an error is visible, wrong pixels are not.
  `failed` is keyed per image so one bad page cannot poison a document's other
  199. `a_cmyk_past_the_first_page_is_converted_not_refused` asserts the actual
  converted values, not just that a page appeared.
- `Ctrl+S` on a page is **refused, and the refusal is the feature**: the lossless
  path rewrites the whole file and the re-encode fallback starts at page 1, so
  saving would write page 1's pixels under the document's name.
- The grid shows page 1 only — one cell per file, deliberately.
- `image` 0.25 cannot read page 2: `TiffDecoder` implements only `ImageDecoder`
  (whose `read_image` consumes it) and is not an `AnimationDecoder`, which is why
  the `tiff` crate is a direct dependency. A test fixture must therefore use the
  `tiff` crate's encoder, and it must chain IFDs via `write_image` —
  `new_directory` makes a directory explicitly *not* chained, so a fixture built
  that way is single-page however many times the loop runs. `image` 0.25 also
  writes only single-page TIFFs.

## Transforms, crop and saving

- `Xform` is one of the eight square-lattice symmetries — the complete set that
  an EXIF tag *and* a lossless JPEG transform can express. A value maps **output
  pixels back to input pixels**, so `then` composes by *probing* (substitute a
  point through both, find which of the eight matches) rather than by a
  multiplication table. `Xform::Transverse` was once hand-transcribed as
  `(h-1-oy, w-1-ox)`, should be `(w-1-oy, h-1-ox)`, compiled, type-checked, and
  underflowed the moment the image was wider than tall. **Never transcribe one of
  these tables** — same failure mode as the wgpu backend bits below.
- **A saved rotation is correct only if it is *composed***: EXIF orientation ×
  the user's turns and mirrors. Get the order or sign wrong and the result is
  invisible *in this viewer* (it re-applies the tag itself and cancels the
  error) and glaring everywhere else.
- `paint_matrix` is input→output (drawing); `map` is the other way (sampling).
  The matrix is derived by probing `map` and transposing, never transcribed.
  Getting the direction backwards still *looks* like a rotation, just the wrong
  one. `Mesh::transform` cannot help — `TSTransform` has no room for a
  reflection — so `paint_tex` moves the vertices.
- `map(coords, input_dims)`: display→stored is `display.map(.., stored_dims)`
  directly (using `.inverse()` transposes the rectangle — a valid crop of the
  wrong corner); stored→display does need `inverse().map(.., display_dims)`.
  `swaps_axes` drives `xformed_footprint`: four of the eight swap, and both
  diagonal mirrors *do* (a diagonal reflection is a quarter turn plus a flip).
- **`ImageEdits` (in `lib.rs`) is the one per-image store** for transform *and*
  crop, keyed by path. Turn and mirror are the same kind of edit and do not
  commute (turn-then-mirror is a diagonal mirror), so the store holds a whole
  `Xform`, not a turn count. Look the edit up *by the path being saved*.
- **`shown` and `cur` are different images.** `cur` is what is wanted (set by
  `goto`), `shown` is what is painted (set by `sync_shown`). Key the draw path
  off `shown_xform` and everything else off `shown`; `current_xform` is right
  only for saving. Reading the transform from `cur` painted one picture under
  another picture's transform — visible on every navigation into a turned image,
  silent when neither is turned.
- **Crop is a view transform; the picture is not cut on screen.** The frame
  floats over the whole image so turns and mirrors still apply to it, and it
  follows the image if you turn it afterwards, because the stored rectangle names
  the same pixels either way.
- **The frame you see is the frame that gets saved.** Chain: screen points →
  displayed pixels → stored pixels (`Crop::from_display_rect`) →
  `snapped_to_mcu` → back to the screen. `crop_from_screen` and
  `crop_screen_rect` in `main.rs` are two halves of one conversion and must stay
  in step. The origin lands on the MCU grid (8 px for 4:4:4, 16 for the usual
  4:2:0) and the rectangle is **grown** to fit, so the user gets up to one block
  of *extra* picture rather than one block cut off the edge they framed.
- **A crop and a turn are two TurboJPEG passes, forced not chosen.** Crop
  coordinates are read in the transform's *destination* space, and the four
  axis-swapping operations refuse a crop outright ("Invalid crop request").
  `save_transformed` crops with the identity op, then turns the cropped result.
  Both rearrange DCT coefficients, so it is still lossless. Do not "optimise"
  this into one call: it passes the identity case and fails every other.
- `dim_bands` (in `lib.rs`, over plain floats, no egui dependency) must tile the
  difference *exactly*: top and bottom span the full picture width, left and
  right only between them. Getting that wrong reads as a rendering bug and is
  arithmetic — a test asserts the four areas sum to `picture - frame`.
- **`neutralize_exif_orientation`** sets the tag to 1 and zeroes IFD0's next-IFD
  pointer (unlinking the now-stale thumbnail). It overwrites bytes that already
  exist and never reserialises the APP1 segment: a malformed EXIF block is worse
  than an unreferenced thumbnail.
- Refused with a message rather than re-encoded: WebP, GIF, HEIC, JXL. A button
  that says "save" must not quietly create a second generation of compression.
- **A bare letter may only do something that cannot lose data.** `M` (move) and
  `C` (copy) used to be one keystroke from writing to the filesystem; `C` is now
  Crop, a view transform until `Ctrl+S`. `Del` is the accepted exception and it
  goes to the Recycle Bin. `tests/keybindings.rs` enforces this from an explicit
  `BARE_LETTERS` list **where adding a line is a claim that the action cannot
  lose data** — write down why.

## Renderer (wgpu)

- **`PROBE_ORDER` is GL, DX12, Vulkan.** GL leads on measurement, with
  `examples/bare_eframe.rs` as the control so the number is the renderer's and
  not ours:

  | backend | adapter + device | bare eframe | this viewer |
  |---|---|---|---|
  | gl | **186 ms** | **84 MB** | **189 MB** |
  | dx12 | 455 ms | 321 MB | 343 MB |
  | vulkan | 474 ms | crashes | crashes |

  237 MB of the footprint was eframe's, and all of it was the backend. DX12 is
  second because it goes through a different driver stack. Frame rate is equal
  (76.8 fps both, interval p50 16.6 ms both, measured on a console session with
  `tests/bench_framerate.ps1`) — same speed, 154 MB less, 269 ms cheaper.
- **`wgpu_backend_set()` returns exactly one bit, always.** wgpu enumerates every
  *enabled* backend during adapter discovery, before picking one, and on this
  machine that enumeration is a hard `0xC0000005` before the window opens with
  nothing on stderr. wgpu has no "try this, else that":
  `native_adapter_selector` still filters enumeration by `backends` first, so the
  set must be decided up front. Two backends is not a fallback, it is a way to
  crash. Never set `Backends::all()` and never enable Vulkan unconditionally.
- **The backend bits are wgpu's numbering, not ours.** wgpu 30 renumbered
  `Backend` (`Noop = 0` was inserted, so `Gl` moved from bit 1 to **bit 4**), and
  the old test asserted the same hand-written literals the code used — a test
  that could not fail, which is why only the debug build (the one nobody runs)
  noticed. `tests/renderer.rs` now compares against `Backends::GL`/`DX12`/`VULKAN`
  from wgpu itself. Same "never transcribe a table" rule as `Xform`.
- **"Automatic" probing is one child process per backend and must reach
  `request_device`.** The failure is an access violation, not an error return,
  so nothing in-process can survive it; a process that has crashed cannot catch
  its own way out. A probe that stopped at `enumerate_adapters` reported Vulkan
  as working on the very machine where the app access-violates on it — enumeration
  only walks the loader's list, the failure is further in. A surface is not
  probed (that needs a window), so a surface-only failure still passes.
- `probe_backends` **breaks at the first success** (first-launch median
  1370 ms → 212 ms; it used to pay for all three and throw two away). The answer
  is cached in `config.json` (`RendererPref.probed`), so only a cold launch is
  affected. A shortened list contains at most one success, so it cannot change
  what `pick_probed` decides — but the function is in a binary, so the early exit
  is pinned against the source text in `tests/renderer.rs`.
- **"Is Vulkan installed" is the wrong question and the registry is a liar.**
  `vulkaninfo` reports a healthy Intel UHD 620 while the
  `Khronos\Vulkan\Drivers` registry key is absent (the loader finds
  `igvk64.dll` another way). Do not reintroduce that probe.
- **Changing the renderer restarts the renderer, and the app cannot ask for that
  itself.** `run_native` only returns when the window closes, so the app writes a
  `RestartSlot` and sends `ViewportCommand::Close`; the launch loop re-enters the
  event loop. The app is rebuilt from config, so folder/sort/settings survive and
  the preview cache does not.
- `#[serde(rename_all = "lowercase")]` on `WgpuBackend` is load-bearing: without
  it the config says `"Dx12"` while `name()`/`parse()` say `"dx12"`, and a
  hand-edited config using the documented spelling fails to load.
- **There is no DX11 option and there cannot be one** — wgpu removed it upstream
  and it is absent from `Backends`. The answer to a request for it is a
  documented "no", not a stub.

## Measuring (this project has drawn several wrong conclusions here)

- **Verify the session is not remote before trusting any frame rate, input event
  or resize event.** `quser` must say `console`, not `rdp-tcp#N`, and
  `Get-CimInstance Win32_VideoController` must not show a *Microsoft Remote
  Display Adapter*. Over RDP a 30 fps animation measured 12.7 fps, a "LOW_LATENCY
  made it worse" experiment was noise, a resize debounce comparison (11 vs 9
  re-decodes) was meaningless, and synthetic key injection silently does nothing
  (`frames with input events` stays 0). This cost four wrong conclusions; treat a
  "the harness cannot do X" claim with suspicion — the tell that it is wrong is
  when it would excuse a bug.
- **A visible window.** `-WindowStyle Hidden` gets no ordinary paint/vsync
  delivery; every fps measured that way was garbage (12/13/19/59 across runs).
  Settle longer than any decode.
- **"First visible pixels" is committed in `frame_prelude`**, i.e. after the
  painting frame was presented. Never record it when the worker message arrives —
  that is not on screen yet. A harness that timed "the message arrived" once
  produced a plausible wrong number for a hang that was three steps away.
- **Measure the step you suspect, not the step you can see.** Timing the frame,
  then the decode, then a per-step timer inside the viewer is how the truncated-GIF
  hang was localised; the harness and the raw decoder both said "fine".
- `display-gap frames` (wanted to paint, no texture) and `stalls (>5s to first
  pixels)` must stay 0. The stall watchdog reports the whole state once per
  request and fires only when nothing was painted at all — check `recorded_first`,
  not just age, or it cries wolf on cache hits. Keep a worker panic's payload.
- **A frame count does not mean the app is spinning.** Presentation can set the
  cadence; a 20× smaller window produced the same one. Before blaming idle CPU,
  read `frames with input events` (0 while idle), `frame interval`, and the
  report's "who asked for a repaint" tally (egui tags every `request_repaint` with
  its call site). On a static photo the only source is the 2 s watcher and the app
  idles at ~2 fps — that is correct. `ms/frame` is CPU amortised over whatever
  frames happened, so it is meaningless at 2 fps; use total CPU% unless frames
  are frequent.
- **A/B must interleave** (all of A then all of B lets machine drift invent a
  120 ms regression), and **prefer min over median** for launch timings — the
  median on this box is ±100 ms when loaded. In `bench_launch_ab.ps1` the
  parameters are `$PathA`/`$PathB`: PowerShell variable names are
  case-insensitive, and a loop variable `$a` silently overwrote `$A` and ran
  `Start-Process -FilePath 809.4`, which is indistinguishable from a missing
  binary.
- **Read the config back after writing it and assert the value.** A run that set
  `renderer.choice = gl` reported identical memory for both backends, which would
  have said the backend is irrelevant.
- **A test that passes on the broken code is decoration.** For every trap here
  that has a test, the test was verified to *fail* with the bug reintroduced.
  Where a premise is an assumption about a third party (that a chord never
  arrives as a `Key` event, that a probe is contained), the test checks the
  premise against a real `egui::Context` / a real queue instead of asserting it
  in a comment.

## egui 0.36 / wgpu API notes

- 0.36's default features enabled `wgpu` where 0.27 enabled `glow` — the
  renderer changed as a *side effect* of the upgrade.
- **Headless tests must go through the `run_pass` helper in `tests/unit.rs`**,
  which clears `textures_delta`: epaint debug-asserts when a delta is dropped
  unapplied, and a `run_ui` pass emits the font-atlas delta every frame.
  `Context::run` is now `Context::run_ui` and the closure gets a `Ui`.
- **`Ui::new_child(UiBuilder)`, never `Ui::scope_builder(..)`**, to replace
  0.27's `child_ui(rect, layout)`. `scope_builder` advances the *parent's* cursor
  after the closure runs, which in `grid_cell` silently ate 4 pt of
  `item_spacing` per cell.
- `ComboBox::from_id_source` → `from_id_salt`. `ComboBox::new(id, "")` is **not**
  the replacement: 0.36 stores `label: Option<WidgetText>` and renders an empty
  label widget, shifting the settings panel.
- `RectShape` moved its texture fields into `brush: Option<Arc<Brush>>`; a plain
  `filled` has `brush: None`.
- Don't hardcode egui's defaults in a test (`item_spacing` moves between
  releases) — report the spacing from the layout that actually placed the cells.
- **egui's default font is a subset; a missing glyph is a blank box, not an
  error.** `no_ui_text_uses_a_glyph_the_font_lacks` scans `src/main.rs` *and*
  `src/lib.rs` and asks the real font at two sizes. Only `… — · ° ⟲ ⟳` work
  beyond ASCII — `Mirror ⇋ / ⇅` and `Prev (←)` / `Next (→)` shipped as empty
  squares. The test reads the sources, so it cannot fall behind.
- **Clipboard chords never arrive as `Key` events.** `egui-winit` intercepts
  `is_cut_command`/`is_copy_command`/`is_paste_command`, pushes
  `Event::Copy`/`Cut`/`Paste`, and **returns before pushing the key it matched**
  (`command` is `ctrl` on Windows). `Ctrl+C` was bound to `key_pressed(Key::C) &&
  modifiers.ctrl`, which reads correctly and can never be true; `Ctrl+S` is not
  on that list, which is exactly why one chord worked and the other never did
  from the same closure. Read `egui::Event::Copy` — inside the `!typing` block,
  so `Ctrl+C` in the filter box still copies text. **Check any new chord against
  those three functions before binding it.**

## Formats, colour, resampling

- **Format handling is content-based, never extension-based.** Magic bytes pick
  the decoder: JPEG (turbojpeg) / PNG, WebP, GIF / TIFF, BMP, ICO, QOI (`image`);
  JXL (`jxl-oxide`); HEIC/HEIF/AVIF (`heic`). A PNG saved as `.jpg` must load.
  `IMAGE_CRATE_DECODERS_WE_SKIP` in `lib.rs` lists the deliberately disabled
  `image` decoders with reasons — sharpest is **Targa, which has no signature at
  all** and is therefore unidentifiable here. Do not "fix" this by passing the
  extension into the decode path.
- **`normalized()` guards every decode exit *and* the downscale exit.** A buffer
  that disagrees with its declared dims aborts a worker in
  `ColorImage::from_rgba_unmultiplied`'s `assert_eq!`. Two ways it happens: a
  decoder that mis-reports its size (truncated files, hydration races), and
  `DynamicImage::resize` returning *smaller* than asked, because it preserves the
  aspect ratio (980×552 asked for 300×168 comes back 298×168). Report the size
  the library produced; keep the wrap.
- `load_image`'s `max_dim` is a **minimum long-side target, not a hard cap**:
  `decode_jpeg_scaled` picks a power-of-two DCT factor and the smallest reduction
  that still leaves `>= max_dim`, so `load_image(2400x1600, 1600)` returns
  2400×1600. Assert the real contract in tests.
- **EXIF orientation is applied to JPEGs only.** The `heic` crate already applies
  HEIF container transforms (`irot`/`imir`); the tag on top would double-rotate.
- **Colour is detected, never transformed (yet).** `color_info` parses the ICC
  profile and reports the space; no pixel is touched, and the pipeline is 8-bit
  sRGB end to end (`TextureOptions` has no `srgb` field; egui wants a gamma-space
  surface). 200 of 1001 images in a real phone backup are Display P3.
- **Classify by primaries, not the `desc` string** — 170 of those 200 have no
  `desc` at all. The two primaries that occur are sRGB `(28578, 14581, 912)` and
  Display P3 `(33759, 15807, -69)`, both D50-adapted, and they never mix. Those
  are *measured*; a remembered P3 value was wrong once. PNG `iCCP` is
  zlib-compressed — inflating it is not optional, or the parser finds no `acsp`
  and silently reports "unknown". `icc_tags_valid` refuses corrupt profiles
  outright: both wrong answers are harmful (claiming sRGB skips a needed
  conversion; claiming wide gamut pushes a correct image through a P3 matrix).
  HEIC/AVIF report `NotInspected` when their `colr` box carries **`nclx`
  parameters instead of a profile**, which is how a phone HEIC often says
  Display P3; JXL reports it because its colour is inside the codestream.
  Assuming sRGB in either case is how a wide-gamut photo looks flat and nobody
  notices, because everything "works". `heic::DecoderConfig::extract_icc` returns
  the raw `colr` bytes, so reading a profile is one call — it was a whole blind
  spot only because that was not known.
- **Never `into_frames().count()`.** The frame count's only job is "is this more
  than one?": read a header for PNG and WebP, take **two** frames for GIF
  (`is_animated`). `count()` decoded the whole animation, and a truncated GIF
  yields `Err` and then *keeps* yielding `Err`, so the worker never answered —
  the app sat with six neighbours decoded and none of the image you asked for,
  no error and no panic. Two `next` calls and no loop cannot hang.
- `image` 0.25 implements `AnimationDecoder` for `GifDecoder`, `ApngDecoder`
  **and** `WebPDecoder`; using one and not the others froze APNG and animated WebP
  on frame 0 with no error at all. Ask the frame count *before* decoding
  (`ApngDecoder` over a still PNG yields zero frames; `WebPDecoder` over a still
  WebP yields one frame the caller throws away, then decodes again), and
  `"RIFF"` is not enough to sniff WebP — WAV and AVI start the same way. Nothing
  in this tree can *encode* an APNG or an animated WebP (`image` writes lossless
  VP8L stills and has no `write_frames`; `image-webp`'s encoder is single-frame),
  so both fixtures are the exception `tests/fixtures/` exists for: the APNG is
  assembled chunk by chunk in `tests/multiframe.rs`, and the animated WebP is a
  vendored 4.9 KB file (MIT/Apache, `webp-animation` 0.9.0) attributed in
  `tests/fixtures/README.md`. **That gap closed by finding a file, not by writing
  code**: the animated WebP path had never been *run*, which from the outside is
  indistinguishable from broken. Both now decode every frame, with the numbers
  pinned — and for the WebP, the frames asserted to differ from each other, since
  ten copies of one picture would satisfy every other assertion.
- **Embedded EXIF thumbnails** skip decoding the source entirely (measured
  16–40× on real JPEGs) and are gated on `target <= EMBEDDED_THUMB_MAX_TARGET`
  (512), so they only ever serve `Kind::Thumb` grid cells — the main view always
  decodes for real. **Never guess the thumbnail's rotation**: the spec does not
  say whether cameras store it already rotated, so `embedded_thumb_rotation`
  decides per file from the aspect ratio and returns `None` when ambiguous
  (mirrored/180 orientations, near-square sources), which falls back to a normal
  decode. A wrong guess shows sideways photos, which is worse than being slow.
- `find_embedded_thumbnail` parses untrusted input: offsets and lengths come
  straight out of the file, so every read is bounds-checked against the APP1
  segment, the IFD entry count is capped, the slice must start `FFD8` and end
  `FFD9`, and any failure returns `None` rather than panicking on a worker. IFD1
  is reached via IFD0's *next-IFD pointer* (after IFD0's entry table), not the
  header's IFD0 offset — reading the wrong one looks like it works and silently
  finds nothing.
- **`linear_resize` is the hot path for big non-JPEG sources**: sRGB box
  pre-reduction to ≤2× the target (integer running sums, rayon, precomputed
  reciprocals) → 12-bit linear conversion of *that* buffer → two kernel passes.
  The conversion must never run over the full-resolution source; a reciprocal
  **division inside the inner loop cost more than everything else combined**; the
  vertical pass needs its running column accumulator or it misses cache on every
  pixel; overshoot must clamp LUT indices to 4095 (index 4097 was a worker
  panic). Toggling `linear_resample` does not re-decode cached previews (the
  filter dropdown does — see `refilter`).
- Known waste, not worth fixing: `exif_orientation(path)` re-reads the file with
  `nom_exif::read_exif` although the bytes are already in memory. That is
  0.1–0.2 ms against a 10–17 ms `fs::read` — ~0.02% of a preview decode.

## Windows integration

- **`build.rs` is load-bearing.** It passes `/NODEFAULTLIB:LIBCMT(D)` (turbojpeg
  is built with the static CRT, Rust links the dynamic one — delete it and every
  build emits LNK4098) and embeds `assets/app.ico` via `winresource`. A running
  viewer locks the exe, so the build then fails to replace it.
- **The icon is generated code, and its look is asserted, not reviewed.**
  `src/icon.rs` exists in `src/` precisely so `tests/icon.rs` can check it
  (coverage, and that local coordinates really span −1..1); two bugs got through
  a working build, a working `ExtractIconExW` and a passing registration test
  because generated artwork is not a diff anyone reads. **Look at
  `assets/app-icon-preview.png` after changing it.** To verify a fix,
  `Copy-Item` restores the *backup's* old timestamp, so cargo skips the rebuild
  and you test a stale binary — `touch` the file or the green run is a lie.
- **The window icon is `ViewportBuilder::with_icon`. Do not reach for
  `WM_SETICON`.** Four attempts failed because the icon arrived too late:
  `WM_SETICON` needs the window procedure to forward to `DefWindowProc` (winit's
  does not — `GWL_HICON` reads 0 while `WM_GETICON` does not, and trusting the
  long is how you conclude the icon is missing when it is not), and DWM caches
  the title-bar icon when the non-client area is built, before your code runs.
  `with_icon` is the only moment the window does not exist yet. Check for a
  framework API before writing a P/Invoke module.
- **Explorer and the taskbar get the icon by different routes**: Explorer reads
  the registered `DefaultIcon`, the title bar and the running taskbar button read
  the *window*. Fixing one tells you nothing about the other.
- **A registered type needs a `DefaultIcon` and must NOT have a
  `ShellEx\ThumbnailHandler`.** The first is what stops Explorer showing the
  generic *application* icon (the ProgID has no icon and the shell reads the
  exe's *resources*, not an `.ico` beside it). The second would displace WIC's
  real photo thumbnails with an icon, which is strictly worse. Icon and thumbnail
  are separate concerns; fix only the icon. An AppUserModelID's `IconUri` must be
  a bare path (the quotes in Microsoft's examples are JSON syntax) — a pinned
  taskbar button then shows the right icon while the running window's does not.
  Explorer caches icons per user, so a registration change needs
  `ie4uinit.exe -show`.
- `src/fileassoc.rs`: per-user HKCU ProgID + `OpenWithProgids` for the 9
  extensions in `exts()`; never claims default silently (Windows 8+ policy). The
  ProgID embeds the exe folder name, so moving the exe requires re-registering.
- **Archives** (`.cbz`/`.zip`) extract to
  `%TEMP%\image-viewer\<hash(path, mtime, size)>` and open as a folder gallery:
  the temp dir is reused while the archive is unchanged and deleted on folder
  switch and exit. Triage is disabled for members and wallpaper copies the member
  to `%APPDATA%\image-viewer\wallpaper.<ext>` first. Never persist the temp path
  into `config.folder`.
- Config lives at `%APPDATA%\image-viewer\config.json` (`Config::default_path`).
  **New `Config` fields need `#[serde(default)]`** or old configs fail to load,
  and **never add `deny_unknown_fields`** — unknown keys are ignored, which is
  the only reason a config written by an older build still starts a newer one.
  `show_image_before_auto_fit` was removed on that reasoning (dead for its whole
  life: declared, defaulted, serialised, read by nothing), and
  `an_old_config_without_a_renderer_setting_still_loads` still carries the key
  so that tolerance stays pinned.
- **Any new CLI flag goes in `path_arg` (`lib.rs`) and its test.** Taking
  `args().nth(1)` is wrong the moment a flag can come first, and it failed
  *silently*: `--perf` was taken as the path, so the report described a folder
  the user never asked for. A flag that takes a value must be added to
  `FLAGS_WITH_VALUE` or its value is handed back as the folder.

## Tests

- **Every suite builds its own fixtures in a `tempfile::tempdir()`**, with one
  narrow exception: `tests/fixtures/` holds a file only when **nothing in the
  dependency set can write that format**. That is the same situation as the
  hand-assembled APNG in `multiframe.rs`, just with bytes instead of code — and
  the reason for the rule is that `test-images/` is gitignored, so a test reading
  it passes on your machine and dies with a bare `NotFound` in a fresh clone.
  Vendored files carry attribution in `tests/fixtures/README.md`. If a crate can
  produce the format, it must, and the fixture does not go there. The only
  remaining readers of `test-images/` are:
  - the **criterion targets**, which are the deliberate exception (they measure
    a real large photo, so a synthetic one would change the numbers): they pick
    the largest JPEG and print a "drop a photo in to benchmark" message.
  - `unit.rs::test_gps_decode`, which returns early without
    `test-images/geo.jpg` — and so skips its `dms_to_deg` checks on a clean
    clone too.
- Three suites assert against **source text**, deliberately, and say why in their
  module docs: `keybindings.rs` (a shortcut that does not exist is otherwise
  unobservable), `renderer.rs` (the probe's early exit is in a binary),
  `unit.rs` (the glyph scan). They check the *premise* against a real context or
  queue, not a comment — and none of them verifies that a bound key actually
  works, because that needs a synthetic keystroke.
- `main.rs` is unreachable from any test. Every rule that has bitten us has been
  moved into `lib.rs` for that reason: the job planner, the resize policy, the
  transform algebra, `dim_bands`, the page cursor, the preview-cache eviction
  policy, `startup_index`, `path_arg`. If you are about to write a decision in
  the binary, check whether it can be a pure function first.
