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
- 27 targets, 24 of them the test files `bench`, `bench_browse`, `bench_resize`,
  `bench_rotate_save`, `bench_startup`, `bench_variants`, `color_profiles`,
  `drops`, `embedded_thumbs`, `filter_and_paging`, `flip_through`, `formats`,
  `icon`, `integration`, `keybindings`, `multiframe`, `pathological`, `property`,
  `renderer`, `rotate_save`, `spreads`, `standard`, `thumb_cache`, `unit` — the
  other three are `--lib`, the binary's own target and doc-tests.
- **`cargo bench` runs nothing.** `tests/bench.rs` and `tests/bench_variants.rs`
  use `criterion_main!` but are ordinary `{test}` targets with the default
  harness (there is no `[[bench]] harness = false` in `Cargo.toml`), so
  criterion's `main` is never called: `cargo bench` prints "running 0 tests" and
  no criterion output. The benchmarks that actually produce numbers are the
  `#[ignore]`d ones below.
- `cargo test --release --test <bench_*> -- --ignored --nocapture`
  (**load the `measuring-runtime` skill first** — these print numbers, and the
  numbers are only as good as the session they ran in):
  - `bench_browse` — the number to move when changing the queue, the preload
    window or the cache policy: time-to-first-pixels, wasted decode share, peak
    MB, cache-hit rate while flipping a generated folder. `IV_BENCH_N` (24),
    `IV_BENCH_RUNS` (3). Its second test, `exif_ab`, is the A/B for the
    metadata read: `Plan::wants_exif` on or off, arms interleaved, and it runs
    the **real** `color_info` + `read_exif` so the job is not modelled as free.
    It prints the per-read cost on the fixture *and* on the largest real JPEGs
    in `test-images/` (skipped if absent, like the criterion targets), because
    the read is `fs::read` and its cost is the file's size — the same exemption
    for the same reason: no synthetic file has a real photo's bytes.
    `IV_BENCH_RUNS` (9) there, and only the holds 0 and 30 ms: at 150 ms and
    above the prefetch keeps up and both arms score 0, so the metric runs out of
    resolution before the effect does.
  - `bench_browse thumb_cache_ab` — the A/B for grid cells: `load_grid_thumbnail`
    cold against warm over a generated folder, arms interleaved, then the real
    per-cell cost of the large scans in `test-images/`. **It prints the cold
    number on purpose.** A baseline JPEG has no cheaper full-image read, so the
    cold figure is a floor that no amount of caching moves, and a benchmark that
    printed only the warm one would be how that got forgotten.
    `IV_BENCH_N` (24), `IV_BENCH_RUNS` (5).
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
  - `probe_thumb_cache.ps1` — the same shape for the grid: presses `G` and checks
    three things that cannot be faked (cells painted, entries on disk, cache
    **hits** in the perf report on a second open), then screenshots the window so
    "a window appeared" is never the evidence. It closes the window rather than
    killing it, because the report is written by `on_exit` and a killed process
    reports nothing — which reads exactly like a broken feature. Verdict on the
    real app, this box: cold 4 misses / 4 entries, warm **4 hits / 0 misses**,
    `worker: thumbnail decode` **1.0 ms p50**, `gallery open -> first thumbs`
    **8.7 ms**, display-gap frames / stalls / worker panics all 0.
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

- `src/lib.rs` (~5.9k lines) — everything testable: sniffing, decode, gallery
  and paging, the job planner, the loader queue, the resize policy, the
  transform algebra, ICC, archives, the grid-thumbnail cache, config. **If a rule
  can be a pure function it belongs here** — every bug that could not be tested
  lived in `main.rs`.
- `src/main.rs` (~4.3k lines) — the binary: `MyApp`, egui, worker wiring, dialogs,
  clipboard, wallpaper, Windows integration. Unreachable from a test.
- `src/perf.rs` (16 latency metrics, 13 always-on counters), `src/fileassoc.rs`
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
- **The EXIF job is only wanted while the panel that shows it is drawn**
  (`wants_exif` in `lib.rs`, on `show_panels`). It is the one job per navigation
  that exists to fill a panel, and it is not cheap: `color_info` does `fs::read`
  on the **whole file** to find the ICC profile, then `read_exif` seeks the APP1
  out — 4.4 ms on the 7 MB bench fixture and **7–12 ms on real 17–27 MB
  photographs** (`bench_browse exif_ab`, three runs) — at priority 5, i.e. *ahead
  of every prefetch*. It has two halves and both are load-bearing: `schedule`
  leaves it out of the plan while the panel is hidden, and the panel `push`es it
  on the frame it reappears.
  The second half is not belt-and-braces: re-planning from the F11 handler would
  be a frame late at best, and `frame_prelude` sees that toggle on the *next*
  frame, so on a static photo it can be the 2 s watcher. Asking where the data is
  displayed cannot be late. A gate with only the first half leaves the panel
  reading "Loading EXIF…" until the next repaint — a visible bug, which is the
  one outcome worse than the slow path it replaced.
- **Measured, and it is not a speedup you can feel**: one read is 7–12 ms against
  a 380–415 ms preview decode on this box, so it is ~2.5% of one decode, and
  `exif_ab` cannot separate the two arms at all — the per-run
  time-to-first-pixels distributions overlap in both directions and the sign of
  the median difference flipped between two runs of the same experiment. Queue
  coalescing already throws most of it away when flipping fast anyway (~1 of 24
  reads survives at a 0 ms hold, ~5 of 24 at 30 ms; the reads are paid for when
  you *dwell*). Keep the change because it is free and it stops doing work whose
  result is displayed nowhere — and because that ratio is set by the file size
  and the storage, not by this code: the same 27 MB read is 10 ms on a warm local
  SSD and tens to hundreds of ms on a network share, a spinning disk or a
  OneDrive file still hydrating. That case is **reasoning, not a measurement** —
  it needs storage this box does not have.
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
- **A drag-and-drop is a hand-off, not a reference, so a dropped file is copied
  before it is opened** (`image_viewer::own_dropped`, `tests/drops.rs`). Explorer
  does not pass a zip member's path: it extracts the member to a transient temp
  file, hands *that* path to the drop target, and deletes it a moment later. The
  app used to open that path in place, and on its own 2 s poll found a file whose
  *stamp had changed* — because it was gone, which is a different stamp — and did
  all three of its blanking moves at once: `invalidate_path` dropped the texture,
  `rescan` removed the file from the gallery, and the re-requested decode failed
  into a `failed` entry that is permanent for the session. Shown for about a
  second, then gone with no way back. **The watcher was right every time**; the
  input was a path with an expiry date on it. Two lessons:
  - **The two clearings are unrelated.** `check_changed` alone does not blank
    (with `load_all_on_startup` off, a vanished file's stamp is `{None, 0}`
    against the same `{None, 0}`, so `moved` is false) and `rescan` alone does not
    either, since it early-returns with that setting off. It is the **default**
    (`load_all_on_startup: true`) that makes `rescan` fire and empty the gallery.
    Both were measured; either alone was survivable, together they were fatal.
    Do not "fix" this by clearing `failed` more eagerly — that trades a visible
    bug for a decode storm, and `failed` being permanent is load-bearing.
  - **A hard link passes the property test and is still wrong**, which is the
    sharpest version of "a test that passes on the broken code". A link survives
    the *deletion* this is about, and shares the *file*, so any in-place write to
    the source mutates the copy — verified both ways a writer can do it (truncate-
    and-write, and rename-over). The app would show, and `Ctrl+S` would write,
    bytes under a path it believes it owns. It is `std::fs::copy`, at the cost of
    one read and one write of the dropped file on the UI thread, once per drop,
    which is less work than the archive path's whole-volume extract. **The copy
    directory name is a hash of path+mtime+len, so a repeated drop of an
    unchanged file reuses one copy** instead of growing `%TEMP%` per drop, and a
    re-exported source gets a *new* directory rather than overwriting a copy the
    app may still hold. One slot: `close_zip` became `close_transients` and
    releases both the archive dir and `drop_copy`, so the lifetime is one function
    — and the copy is taken **after** that call, a position
    `a_drop_copies_its_source_after_releasing_the_previous_copy` asserts by byte
    offset, because the reversed order deletes the copy with the call that opens
    it. `open_file`'s `transient` parameter has no default on purpose.
  - **Not claimed:** a mid-write first decode. It was a hypothesis and it is not
    what happens — the trace shows the file decoded and painted correctly at
    frame 2 and blanked at frame 14 because it vanished. A copy taken while
    Explorer is still writing captures a partial file; nothing here prevents that.
- **Resize is coalesced by comparing with the previous frame**, never with a
  wall-clock timer: `needs_refine` + `ResizeTracker` in `lib.rs` return
  `Wait` / `Refine` / `Idle`. A big preview takes ~800 ms to decode, so frames
  are ~800 ms apart and a shorter timer expires between every pair and never
  fires. `Wait` must call `request_repaint_after(RESIZE_SETTLE_REPAINT)` or the
  final size of a drag is never examined (nothing else asks for a repaint).
  Panel toggles and F11 call `suppress()` so layout changes don't read as growth.
- **A decode target the app has not measured is `None`, never a default — and
  `PREVIEW_TEXTURE_DIM` is the animation ceiling, never a decode request.** This
  was a reported regression and it is the sharpest version of "a value read at the
  wrong time". `schedule` runs from `goto` in `frame_prelude`, i.e. **before
  `central` has ever measured a panel**, and it used to answer
  `display_dim.map(preview_target).unwrap_or(PREVIEW_TEXTURE_DIM)` for the gap.
  So the **first plan of every session** — the current page, the turn's partner and
  all six preload neighbours — asked for a 4096-long texture, and for any source
  under 4096 px `downscale_image` is a **no-op**: `load_page_preview_full(path, 4096,
  0).preview` is *byte-identical* to the source decode. No prefilter, no kernel
  pass, no resampling anywhere in the pipeline, and a full-resolution page handed
  to a bilinear sampler with no mipmaps. Measured at a 957x914 physical panel
  painting one of those into the rect it is drawn at: **3.397x in normal mode,
  4.645x in manga mode**, against 1.045 and 1.365 for a texture decoded for that
  panel. **Whether a given page shows the lattice is a race with start-up** —
  whether it was in the first plan or a later one — which is why it presented as
  "fine until I press right", and why the mode toggle and the navigation key were
  both implicated and neither was the cause. `decode_target(Option<(u32,u32)>)`
  returns `Option<u32>`, `schedule` emits an **empty** plan when there is none, and
  `central` re-plans **inside the `if self.display_dim != Some(panel_px)` block**,
  on the first measurement only (re-planning on every change is a decode per frame
  of a drag, which is the storm `ResizeTracker` exists to stop). The re-plan is
  pinned by **position**, not presence: `the_plan_is_empty_until_the_display_area_is_measured`
  brace-matches that block, because a correct-looking `schedule()` one line above
  the assignment re-plans against the very `display_dim` it is filling — the
  `shown_partner` latch's defect shape.
  * **A one-sided rule about sharpness is a hole, not a feature.**
    `needs_refine` compared `decoded_for` in one direction only, so **no resize, for
    the rest of the session, could ever correct a texture decoded for a much larger
    target** — `914 <= 4096 + 8` short-circuits, and `source_long > preview_long`
    is false besides (3109 > 3109). It now also fires past
    `PREFILTER_FACTOR` × the target, which is `prefilter_dim`'s own factor: a
    texture more than that above the target it is painted at is one whose
    pre-reduction **could not fire**. Judged on the **request**, as the growth arm
    is, because a pixel rule re-decodes forever on a source smaller than the
    display (`an_oversized_refine_converges_after_one_decode`).
  * **What this costs, measured.** Zero on the navigation path: 8 × `→` in manga
    mode, before and after, 6 `decode calls started` and **0** resize-driven
    re-decodes in every run, `next/prev -> first pixels` p50 16.3–18.2 ms before
    and 16.6–17.6 ms after. It *removes* work: seven 27 MB full-resolution
    textures per cold open become seven 2 MB ones. It *adds* work only on an
    over-sized texture, and per decode the resize now actually runs — 178–225 ms
    p50 after against 161–207 ms before, which is the price of doing the resample
    the pipeline exists to do rather than skipping it.
  * **And one thing it unmasked, which is pre-existing.** Pressing `M` moves the
    panel's long side by 16 px (914 ↔ 957 on this box), which clears
    `RESIZE_PX_FLOOR`, so the **growth** arm now charges a burst of
    `resize-driven re-decodes` where it used to charge none — before this fix
    nothing was ever decoded at a real target, so the arm was unreachable rather
    than quiet. Bounded and self-healing (`M` + 8 × `→`: 0 or 6 re-decodes in 6
    runs; the same 8 × `→` with no toggle: **0 in 6 of 6**). **Not fixed here**:
    the growth floor is `RESIZE_PX_FLOOR`'s business and changing it is a policy
    call, not a bug fix.
  * **The band this leaves standing.** A page fitted into a *wider-than-tall*
    panel is height-bound, so the GPU minifies by up to the panel's own aspect on
    top of the `PREFILTER_FACTOR`: measured 2.313 for a portrait page on a
    1050x800 window, unchanged by this fix and unchanged by the `2×` threshold,
    because that is the pre-existing design (`the_decode_target_covers_the_picture_the_layout_draws`
    only ever pinned the *lower* bound). Tightening it would re-decode on every
    panel-shape change; it is a deliberate next task, not an oversight.
- **Hold-to-browse**: `step()` is throttled while `waiting_load`; the queued
  `pending_advance` is consumed at the *start of the next frame* — consuming it
  in the same frame skips the render.
- **A grid cell is not a small preview, and that is why `Purpose` exists.** A cell
  is 150 *points* of one fixed picture, and it used to be `load_page_preview_full`
  with a smaller number — which returned `full` and a 5851 KB texture for it. Two
  things that only a cell needs, both measured:
  * **It must be downscaled to its target, and nothing upstream can do it.**
    `decode_jpeg_scaled` picks the *smallest* reduction still covering the target
    out of 1/8, 1/4, 1/2 and 1, so a 17000 px scan yields 2100 px for a 256 px
    cell — 44x, and `THUMB_CACHE_CAP` 512 × that is a **2.9 GB** texture ceiling.
    The step lives in `load_thumbnail`, once, *after* `decode_page` returns,
    because that function has five return points and the same argument the
    wide-gamut wrapper makes.
  * **It must decode one animation frame, not the loop.** The animation branch
    used to run `load_animation` (240 frames, 256 MB) to keep `frames[0]`. The
    900x318 GIF in `test-images`: 344 ms → 7.9 ms. `first_frame` is one `next()`
    and no loop, like `is_animated`, and it deliberately does *not* ask
    `is_animated` first — that costs two frame decodes to answer a question a cell
    does not have.
  `ThumbData` in `main.rs` has neither `full` nor `anim`. Not having the fields is
  the point: there is no way to fill them by accident. Note what is *not* claimed —
  `dynamic_to_image_data` still decodes at source size and resizes for PNG, HEIC,
  JXL and TIFF, so the transient full-size buffer is still allocated; what went
  away is the retention and the second downscale.
- **A baseline JPEG thumbnail is a cache problem, not a decode problem.** Every
  pixel needs every DCT coefficient, so the entropy decode is the floor for *any*
  conformant decoder and no amount of clever scaling moves it: DCT 1/8 is 833 ms
  against 1/1's 1242 ms on a 95.7 MP scan, so DCT scaling buys 25-30% and no more
  (full table in the `measuring-runtime` skill's reference). So the answer to a
  slow cell is *not decoding it again* — `load_grid_thumbnail` and
  `%LOCALAPPDATA%\image-viewer\thumbs` — and the first visit to a folder of large
  scans is **unchanged and unchangeable** (~24 × 700 ms over 4 workers). Warm is
  0.6-1.8 ms. Do not let a benchmark print only the warm number;
  `bench_browse thumb_cache_ab` prints both for that reason. The one decode-side
  shortcut that *does* exist is the progressive DC-scan truncation (3.3x), and
  **every large file in this library is baseline** — 0 of 5 big scans, 0 of 5 big
  phone photos — so it was measured and not built. Its trap if anyone does: a
  first SOS with `Ns=1` is *not* interleaved, and truncating there yields one
  component's DC and a plausible wrong picture.
- **A cache's design is almost entirely about how it can be wrong**, and every
  "wrong" here is a grid cell showing the wrong picture, which is worse than a
  blank one and undiagnosable. Four guards, each with a test verified to fail
  without it (`tests/thumb_cache.rs`):
  * **Staleness** — the stamp (mtime **and** length) lives in the entry and is
    checked on every read, so correctness does *not* depend on the watcher
    remembering to invalidate. `forget_thumbnail` in `invalidate_path` is disk
    hygiene only. Length is in the pair because a re-save inside one filesystem
    tick changes the bytes without changing the time.
  * **Collisions** — the name is 64 bits of path hash, so the path is stored in
    the entry and compared. Without it two files that collide show each other's
    cells.
  * **Half-written entries** — write to `.tmp`, then `fs::rename` (which replaces
    on Windows), so a reader — *including a second instance of the app* — sees the
    old entry or the new one.
  * **Corruption and truncation** — **`image`'s PNG decoder accepts a stream
    missing its last 64 bytes** and hands back a full-size image with the tail
    zeroed. That was a live bug here: a half-copied cache file was a *hit* and a
    cell with a black band across it, no error anywhere. PNG's own `IEND`
    terminator is the exact and free completeness check. Assume other decoders in
    this tree are similarly lenient and check the container's terminator, not just
    the decoder's return value.
  * Every failure anywhere in it — unwritable directory, full disk, bad header —
    is a **miss**, and none may fail the decode the caller already has.
  * Stored **losslessly**, so "a hit is byte for byte the decode it replaced" is a
    testable statement. A q90 JPEG thumb would be ~8x smaller and would give that
    up. `CompressionType::Fast`, not `Best`: `Best` buys under 1% of size for
    15-36x the encode time, and that time lands *before* the cell's message is
    sent.
  * The name is a hash of the path **only**. Putting the stamp in the name would
    leave one orphan per save; in the header, a re-saved file overwrites its own
    entry. Two-hex fanout because 30 000 files in one NTFS directory is a directory
    big enough to make `read_dir` slow.
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
  - **`MAX_TILES` is one global bound and it is enforced** — the eviction pass sums
    every page's map (the cursor's on `self.full`, each other page's on its own
    preview) and drops the least-recently-drawn tiles across all of them. Verified,
    not assumed; `tests/spreads.rs` pins the geometry, not this count.
  - **But `Preview.full` now has a *second producer*.** It used to be written only
    by the single-pass decode, so `PREVIEW_CACHE_CAP × MAX_FULL_PIXELS` was reachable
    only by decoding 16 large sources in one folder. A `Kind::Full` result for a
    turn's **second half** is now also kept on that page's preview (`Msg::Full`'s
    else-arm), and the plan asks for one whenever `Plan::partner_needs_full`. So the
    same ceiling is now reachable **by flipping through turns in a manga volume**
    rather than only by browsing a folder of huge files — 16 cached previews, each
    holding a full-resolution buffer for a source over `FAST_FULL_MAX_PIXELS`, which
    is the shape of library this feature targets. Nothing is unbounded and no new cap
    was added, because the counts that bound it (`PREVIEW_CACHE_CAP`, `MAX_TILES`)
    did not change; what changed is *which workload fills them*.
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

## Spreads and reading direction

Manga mode shows a folder as **turns** of one page or two, opening
right-to-left by default. The shape is the same everywhere: the **pairing rules**
(`lone`, `pairable`, `shape_of`, `next_anchor`, `prev_anchor`, `turn_partner`,
`anchor_of`, `next_turn`, `prev_turn`) and the **layout** (`spread_fit`,
`spread_rects`, `spread_centre`, `spread_pan_bounds`) are pure functions in
`lib.rs`, and only the mode gate, the page tuple and the two rects live in
`main.rs`. Everything below is a consequence someone learned, and most of them
are wrong in a way that *looks* right.

- **A turn is anchored, and `lone` is only true of an anchor.** `lone(anchor)`
  asks "can the page *after* this one pair with it", which cannot distinguish
  "first half of a pair" from "alone", because the answer depends on what
  happened *behind* the anchor. At an anchor nothing paired from behind, so the
  local test is complete; anywhere else it is wrong, and **the failure is
  invisible going forward**: page 4 of a run where page 5 is a landscape insert
  "cannot pair with" 5, therefore "is alone" — yet it paired with page 3. Ask
  whether the page ahead pairs *only after* establishing that the page is an
  anchor, and never fold the anchor question into the pairing question. That is
  what `anchor_of` is for, and the cursor is on an anchor by construction because
  `MyApp::goto` calls `snap_to_turn_anchor` — the single funnel every way into
  the app passes through, rather than three call sites that could disagree.
- **`FullImage` is keyed by `(file, page)`, not by path — and it was not, and every
  site that read it was a silent wrong picture.** `decode_page` returns
  `full: None` for every page past the first of a TIFF, so a path-keyed buffer is
  frozen at whichever page happened to be current. Three consequences, all with no
  error and no counter:
  * `needs_full`'s first line answered "already done" from the cursor page's buffer
    for a partner that is `(index, page+1)` of the *same file* — which is **every
    turn inside a manga volume** at the default `lead_lone` — so `plan_jobs` was
    handed `partner_needs_full == false`, no `Kind::Full` job was ever planned, and
    the second half of a zoomed spread stayed soft for the session. Its own doc
    states the invariant its first line violated.
  * the tile loop handed page 0's pixels to `make_tile` with page N's `uv`.
  * `Ctrl+C` filtered the buffer by path and pasted page 1.
  `FullImage::is(&ImgKey)` is the one spelling, and it is a `page` comparison rather
  than a built key so it allocates nothing on a per-frame path.
- **`prev_anchor`'s `None` means two things**: there is no turn before the first
  one, **and** the position asked about is not an anchor at all. It walks the
  tiling forward from page 0 comparing successors, so a second half is never the
  successor it is looking for and the walk runs off the end — the same `None`.
  Reading it as only "at the front" is how a Left press jumped to the last
  spread of the volume from a resumed position, a grid pick or a `Del`, with
  `loop_back` both on and off. `prev_turn` asks `anchor_of` to tell the two
  apart; any new caller must too.
- **Both directions are O(distance from the front), and that is not an oversight.**
  `next_anchor = a + turn_len(a)` is one addition *when asked of an anchor* — but
  **the navigation the app performs is `next_turn`, which snaps first**, and the snap
  is `anchor_of`, which walks forward from `(0,0)`. So a forward press pays the same
  walk a backward one does, plus one addition. Three places asserted otherwise and
  all three are corrected: `anchor_of`'s doc, the design spec, and RULING 4 (which
  had the direction backwards as well as the bound). Measured as a *count*, since a
  timing assertion here would be an artefact
  (`a_navigation_walk_costs_one_turn_len_per_turn_behind_the_cursor`): 10 `dims` reads
  per turn for `anchor_of`, 5 for `prev_anchor`, exactly linear in the turns behind
  the cursor — ~2000 reads at the back of a 2000-file gallery. Microseconds against a
  decode measured in hundreds of milliseconds, so **not a hang**; but not O(1) either,
  and it is the claim that says no pairing table is needed. The deferred fix (a
  per-gallery anchor list) and its cost are in `TODO.md`. Backward cannot be made
  local at all: whether `b` is an anchor depends on `b-1`'s turn length, which
  depends on `b-2`'s, and it only resolves by walking back to page 0. Three local rules
  were tried and all three are wrong on `[Single, Single, Single, Double]` (they are
  enumerated in `prev_anchor`'s docs). **Do not "optimise" either walk into a
  two-candidate test**, and do not write in a doc that a direction is a bounded
  number of reads.
- **A document is stepped *over* unless its page count is learned before the step,
  and `goto` learns it for the turn's *other half*.** `pages_in` answers 1 for a file
  nobody has asked about, `pairable` treats an unmeasured page as pairing
  optimistically, and `step` applies `turn_len` one page at a time — so in
  `[a, b, vol.tiff(200pp), c]` the step from `b` claims the document's first page as
  its own second half and lands on `c`, and the tiling then says "one page" for the
  rest of the session. **199 pages unreachable by pressing `→`**, which is the content
  this feature exists for; the non-manga path is immune because `Gallery::next` steps
  one page and lands on it. `goto`'s `learn_partner_pages` reads the other half's file
  — one cache-first four-byte read, once per file for the session.
- **`prev_anchor` asks `step_no_wrap`, and `next_anchor` is the only caller of
  the wrapping `step`.** A turn must not straddle the wrap: with looping on, the
  last turn truncates to one page so the tiling is identical to the same gallery
  with looping off, and the partition property (every page in exactly one turn)
  holds for both. A wrapping step here tiles `{0} {1,2} {3,0}` and turning forward
  off the last spread shows page 4 beside page 1.
- **`shown` is the cursor's page, not the turn.** Every view-shaped path reads
  it — `footprint`, the crop frame, `dim_bands`, the tile source-pixel mapping,
  EXIF, `Ctrl+S`, `Ctrl+C` — and none of them needed to change for this feature,
  which is the evidence that they were already correct. `shown_partner` is a
  *second* key, and `turn_pages` is **cursor first** because `spread_rects` returns
  rects in input order. Repointing any of those paths at the turn is the change
  that would break them: the cursor page is the one the reader asked for, and it
  is the only one `Del`, save and copy are statements about.
- **`shown_partner` is DERIVED every frame, never latched, and that is the whole
  lesson of this feature's final review.** It was adopted in the same act as
  `shown`, inside `sync_shown`'s `if previews.contains_key(&cur) && shown != cur`
  gate — and that gate is **false on every frame the cursor stands still**, while
  `M`, `O` and `X` all re-`goto` **without moving the cursor**. So `→ → → M` in a
  folder of single pages halves the panel and lays out *one* page: a half-width
  picture beside an empty half, no error and no counter, until the next `→`
  produces a correct spread. Now `sync_shown_partner()` runs every frame, from
  `shown`'s own position via `Gallery::pos_of`. Two consequences:
  * **The call's position is the property, not its presence** — it sits *after*
    the adopt gate. `tests/spreads.rs::the_turns_second_half_is_derived_not_latched`
    brace-matches the gate and fails if the call is inside it. Verified by moving
    it back: the test fails. A `contains` scan would have passed on the bug.
  * **The workaround it needed is deleted.** `invalidate_path` used to clear and
    re-ask `shown_partner`, because after a partner's re-export the gate was false
    from there to the end of the session. With derivation, the next frame asks the
    pairing again — which also covers the re-export that made the partner *stop*
    pairing, a case the re-arm could not.
  * **I2 was the same defect from the other side**: a partner whose decode reveals
    it is a two-page scan collapses the *plan* (`arrival_reschedules`), and a latch
    left three printed pages squashed into one viewport until the reader navigated.
    Re-deriving every frame closes that as a class rather than at one site.
  * **The re-derivation does not snap to an anchor**, deliberately: `anchor_of` is
    an O(distance from the front) walk and this is per-frame. It does not need to,
    because `lone`'s three arms can only go from false to true as a dimension
    resolves, so **an anchor can only gain turns around it, never lose its own** —
    `tests/spreads.rs::an_anchor_stays_an_anchor_as_the_pairing_learns`.
  * **The review question to ask of anything here: "what is the lifetime of this
    value, and who re-derives it?"** C1, C2, I2 were all a value latched at one
    event and read at another, and each had a passing test or scan asserting the
    call existed.
- **The turn's second half gets its own xform and its own `orig`, and the slot is
  its *transformed* footprint.** `edits` is keyed by path precisely so turning
  one page does not turn the next, so reading the partner's transform off the
  cursor is two pictures under one transform. And a slot sized from *stored*
  dims with a turned page painted into it spills `|w - h| * scale / 2` over the
  seam — measured at ~234 points on a 1000x1500 page. Hand `xformed_footprint`
  to `spread_rects`; the paint rect stays in unrotated image space, because
  passing the *slot* to `paint_tex` paints the transform twice.
- **Layout comes from dimensions; paint comes from textures.** `turn_pages` looks
  dimensions up in the shape cache, and a partner nothing has measured yet falls
  back to the cursor page's own dims. If layout waited on the partner's *texture*
  instead, the spread would reflow when that texture landed, under the reader,
  mid-read. A wrong placeholder is only a sliver at the seam; a reflow is a
  picture that moves by itself. `MAX_TILES` stays one global bound, counted
  across both pages, because cuts are on demand.
- **A lone page is not one rule, it is two, and the split is the *shape* and not
  the geometry.** A lone **portrait** page keeps its half, hanging off the seam —
  a book's cover genuinely is one page wide. A lone **spread** — a page above
  `SPREAD_ASPECT`, which `pairable` never pairs so it takes the whole turn
  *because* it is the whole spread — **fills the turn**: fitted against the
  panel's whole width and centred on the seam, which is the single-image layout.
  Confirmed on a real volume (pages 1350x1920, one of them 2700x1920 = 1.41),
  where the wide one alone rendered in the right half at 960x682.67 on a
  1920x1080 panel and now gets 1518.75x1080 centred, in both `rtl` values. The
  two claims were both in this section and contradicted, and the half was applied
  unconditionally. `spread_fit` computed `half_w = panel.0 / 2.0` and
  `spread_rects` put every lone page on one side of the seam.
  * **The caller decides, and `turn_is_a_lone_spread(page_count, shape)` is the
    decision as a pure function.** Not `w > h`, in both directions: a near-square
    page (1.05) is wider than tall, is a `Single`, and must keep its half; and
    `X` can force a *portrait* page to `Double`, and "treat this as a spread" has
    to mean *give it the whole turn*, or the key re-pairs the book and then draws
    the result at half size.
  * `lone_spread_fills_turn` is threaded through `spread_fit`, `spread_rects`,
    **`spread_centre` and `spread_pan_bounds`** — all four. Passing it to three of
    them is a plausible picture with no error: a full-panel page zoomed about the
    half-width centre creeps under the cursor, and one clamped by a pan bound
    derived from a half-width box cannot be panned at all, so the zoom's wanted
    offset is silently truncated. Both re-check `pages.len() == 1` rather than
    trusting the flag, because honouring it on a turn of two would stack both
    pages on the centre.
  * **And it is also what the turn is *decoded* for — five consumers, one answer.**
    `decode_panel_px(panel_px, manga_mode, lone_spread_fills_turn)` halves the
    panel's width for every turn except a lone spread, and it is the same `bool`
    the four layout calls are handed, read from one binding. A lone spread is
    drawn as the single-image layout, so decoding it for half the panel hands
    `spread_fit` a page wider than the texture it is about to be drawn from. The
    invariant is **the request is never smaller than the picture it will be drawn
    as**: on a 2700x1920 page drawn 1518.75x1080 on a 1920x1080 panel, the request
    went 1080 → 1920 at `ppp` 1 and 2160 → 3840 at `ppp` 2, either way from **0.711
    of the drawn long side to 1.264** — and because `decode_jpeg_scaled` picks the
    smallest of 1/8, 1/4, 1/2, 1 that still covers the request, the texture at
    `ppp` 1 went **1350x960 → 2700x1920**, a texture 0.889 of the width it was
    painted across. The number is one function because `panel_px` feeds everything
    downstream: `preview_target`, `display_dim`, `schedule`'s `target_dim` and the
    resize machine.
  * **Which rung a source lands on is the source's, not the rule's.** At `ppp` 2
    both requests are past a 2700-long-side page's own size, both land on 1/1, and
    that fixture's texture is the same 2700 wide either way — so the *texture* at
    `ppp` 2 did not change for it while the *request* was wrong at both ratios. A
    6000-long-side page leaves 1/2 at `ppp` 2 as well; a 1600-wide page never leaves
    it. Pin the request, not the rung: the rung is a property of the file.
    `a_lone_spread_is_decoded_at_the_size_it_is_drawn_at` pins both, and says which
    is which.
  * **The flag is asked once, at the top of the frame, from the position** —
    `lone_spread_fills_turn(Some(pos), lead_lone, ctx)` in `lib.rs`, which is
    `turn_is_a_lone_spread(turn_len(pos, ..), shape_of(pos, ..))`. Not
    `spread_dims.len()` and not `shown_partner.is_none()`: those are *readings* of
    the rule, and a reading is a frame behind the rule, which is how a value
    latched at one event and read at another keeps producing correct-looking
    screens. It is answerable before `pages` exists because it asks the **pairing**
    rather than the paint: `turn_len` and `shape_of` are the rule's own inputs, so
    the query cannot be a frame behind the rule the way a reading of the laid-out
    pages can. `main.rs` is untestable, so the wiring is pinned by a source
    scan (`central_decides_and_threads_the_lone_spread_flag`) — including that
    `central` binds the flag **exactly once** and decides `panel_px` before
    `preview_target` reads it — not by the library tests: the library is correct
    and the app still wrong if the flag is not passed, and verified, every
    behavioural test in the suite stayed green with the app's wiring reverted.
  * **Two consequences of getting it right, both worth knowing.** Toggling `M` on a
    lone spread is now *inert* — both modes decode the full panel and lay out the
    same single-image rectangle — so it costs no refine, where the half-panel
    answer charged one each way for a picture that did not change. And arriving at
    a spread nobody has measured is a target change **once**, when `goto`'s
    `learn_shapes` reads its header and the flag becomes true: one `needs_refine`,
    one replaced preview, then stable, because a shape only ever resolves from
    unknown to measured.
- **The content of a spread is not centred on the panel's centre; the *seam*
  is.** Pages extend outward from the seam and the turn is top-aligned, so the
  content's centre is displaced on both axes — horizontally by half the width
  difference, vertically by half the tallest page. Both are zero for two equal
  pages, for a single image, **and for a lone spread** (it is centred, and its
  height is the panel's) — **which is why a bug here hides.** The zoom
  anchor (`spread_centre`, asked at two scales, or the point under the cursor
  moves) and the pan clamp (`spread_pan_bounds`, the content's own edges against
  the panel's, per axis and not symmetric) both measure from the content. The old
  symmetric clamp truncated **36 of 41** zoom steps that still had room to pan,
  by up to 67 points. **One shared scale, one zoom level, one pan** — the two
  halves cannot zoom independently, and that is the price of sharing `view`.
- **The wheel deliberately does not follow the arrows, and every navigation key
  goes through one rule.** `nav_is_forward` in `lib.rs` is the only thing in the
  codebase that knows which way forwards is; `NAV_KEYS` pairs each physical key
  with what it *means* and `nav_direction` resolves them into `Option<bool>`.
  OpenComic ships a live bug from omitting `Space` from its inversion set, so
  Space silently becomes "previous page" in manga mode: nothing errors and the
  arrows are right. With two booleans instead of `Option<bool>`, every caller
  decides which wins and a caller that writes that decision is a second opinion
  about direction — the entire defect. The wheel is the one exception, always
  forwards, because it is the one input with no side to preserve; asking the
  rule rather than spelling `true` is what keeps it from drifting.
- **`SPREAD_ASPECT` is 1.2 and every reader in the field uses 1.0.** A real page
  is about 0.707 and a real spread about 1.414, so a threshold of 1.0 sits on the
  midpoint and calls a square-ish single page a spread. Above the threshold, not
  at or below it. **And the dimensions must be the ones the page *displays* at:**
  a phone scan stored 1600x900 with EXIF orientation 6 has a stored ratio of
  1.78, above the threshold, so judging by stored size makes *every* page in the
  volume a spread and nothing ever pairs — silent, and it lands on the largest
  category of real manga library there is.
- **`lone` decides a turn by asking about `anchor + 1`, so the next page's
  dimensions are load-bearing.** Learning only the page under the cursor gets it
  wrong in the worst way: arriving at page 5 changes page 5's shape, which
  changes `lone`, which changes the turn the reader is already looking at. A
  spread that re-flows mid-read is worse than one that was wrong from the start,
  because the reader watched it change. `learn_shapes` therefore reads the next
  page too, bounded to *the same document* — one page, and only for a file known
  to have more than one. A partner's *decode* landing is also a re-plan trigger
  (`arrival_reschedules`), because `pairable` treats unknown dims optimistically
  and a wide page that was planned as a partner collapses the turn to one page.
- **The pairing tests are random, not exhaustive.** `tests/spreads.rs` runs 256
  proptest cases over galleries of 1..=12 pages in one document, a leading-lone
  offset in 0..=2 and both `loop_back` settings; 2^12 shape patterns across twelve
  lengths is orders of magnitude more than 256 draws, and a handful of named
  tests carry the cases that matter. Say "random over N cases" and never
  "exhaustive" — three doc comments and this feature's own spec said
  "exhaustive" for a property test that is not, and each correction was a fix
  round of its own.
- **Overrides live in `%LOCALAPPDATA%\image-viewer\spreads.json`, keyed by the
  original archive path.** Never the extraction directory: its name is a hash of
  the archive's path, mtime and length, so it changes on every re-extract and an
  override keyed by it would work until the `.cbz` was touched and then lose
  every one. The store is a **read-modify-write**, so a parse failure is not a
  lost session, it is a deleted file: an unparseable store is replaced with one
  gallery's entry. A **UTF-8 BOM** therefore had to be stripped in three places
  — `Config::load`, `load_spread_overrides` and `save_spread_overrides` — and in
  the save path it was the difference between "settings reverted" and "every
  other gallery's decisions deleted". An over-cap store is honoured as empty by a
  load and left *untouched* by a save, because the two resources differ and the
  save's read is the first half of the write.
- **What is *not* claimed**: `main.rs` is unreachable from every test, so the
  paint and navigation wiring is covered by **source scans and one-off geometry
  dumps**, not by the suite. Every pairing rule and all four layout functions
  are pinned behaviourally in `tests/spreads.rs`, and the scans cover the
  navigation wiring (`step`, `turn_forward`, `turn_backward`, `goto_turn`,
  `goto`'s snap), the zoom anchor's two-scale call, the tile loop over both
  pages, a non-cursor `Kind::Full` being kept, and `invalidate_path`'s clearing.
  What they cannot tell is whether any of it *works* — a scan catches a call
  being reverted, not a rect being wrong. The sub-pixel slide that closes the
  seam, the per-page tile rects and the displaced-centre numbers were checked by
  instrumenting the running app and dumping its own geometry, and that evidence
  is not a test. **Nobody has looked at a spread.** Screen capture cannot see
  this app's swapchain on this box — three routes, all blank, including for the
  untouched single-image path — so an eyeball is still owed and is recorded in
  `TODO.md`.
- **The eyeball checklist had to be extended, because the two critical findings
  were both invisible to it.** Every check on it was *visual* — seam position, a
  cover where a cover belongs, sharpness, rotation — and both criticals produced a
  screen that is perfectly laid out and shows the **wrong picture**: `M` on a
  stationary cursor laid out one page where a spread belongs (and a lone page at
  half width beside an empty half is *exactly* what the seam rule prescribes, so the
  existing checks pass on it), and page 0's tiles were cut for page N's rectangle. A
  latch produces a stable, plausible screen, which is precisely what a screenshot
  judges well. The three added checks are "a spread **appeared**", "the tiles are
  *that page's*", and "`Ctrl+C` on page 2 pastes page 2" — named after the failure
  each would have caught, and the last one is outside the window entirely.
  `tests/probe_chords.ps1` cannot see it either: it uses a real `.jpg`, which has one
  page.

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
- **The frame you see is the frame that gets saved, and the frame that gets
  copied.** Chain: screen points →
  displayed pixels → stored pixels (`Crop::from_display_rect`) →
  `snapped_to_mcu` → back to the screen. `crop_from_screen` and
  `crop_screen_rect` in `main.rs` are two halves of one conversion and must stay
  in step. The origin lands on the MCU grid (8 px for 4:4:4, 16 for the usual
  4:2:0) and the rectangle is **grown** to fit, so the user gets up to one block
  of *extra* picture rather than one block cut off the edge they framed.
- **A frame can only be drawn, not moved.** Tried: a press *inside* the frame
  translates it, a press *outside* draws a new one, with `Crop::moved_by` doing
  the arithmetic through the same three steps a frame is built with. Reverted
  because it was wrong in the hand in a way the tests did not catch. **If it is
  attempted again, the trap to design around is the frame that covers the whole
  picture**: every press is then "inside" it, so there is no way to start a new
  frame at all — a state the old behaviour simply could not reach. That needs an
  explicit way out (a handle, a modifier, or `Clear crop` as the escape), not
  only a hit test.
- **A drag can be held to a shape** (`constrain_to_aspect`, `CropAspect`). The
  frame is built from the **press point**, and the axis the pointer leads is kept
  while the other one gives — so `crop_from_screen` takes anchor and pointer, not
  a finished min/max rect. A rect has already thrown the anchor away, and without
  it the frame grows out of the middle of the drag. Three decisions that will look
  wrong to somebody tidying:
  * **Shift means 1:1, not "ignore the setting"** — the other common convention.
    The default setting is `Free`, so a Shift-means-override shortcut would do
    nothing at all out of the box.
  * **The shape is decided per frame, not latched on press.** Letting go of Shift
    mid-drag is how you get out of a shape you did not want.
  * **`constrain_to_aspect` has no guard for a flat drag**, and that is not an
    oversight. `dx / dy` really is a division by zero, but every branch lands on
    the same answer — a zero on one side zeroes the other — so a guard would be a
    branch that cannot change an output, which is worse than none because it reads
    as protection that is not there. `a_drag_with_no_area_is_not_given_a_shape`
    pins the user-visible property instead.
  * **`CropAspect::Source` resolves against the *displayed* size**, not the file's.
    A 3:2 photo turned on its side is 2:3 on screen, and a frame that insisted on
    3:2 would not fit it.
- **`Ctrl+C` and `Ctrl+S` are one feature with two outputs**, and they reach the
  same picture by *different* routes, which is the whole difficulty.
  `save_transformed` cuts the rectangle out of the **stored** pixels and then
  applies `display_xform(exif, user)`. `region_for_save` (clipboard) is handed a
  buffer that is already EXIF-oriented, so it moves the rectangle with
  `to_display_rect(exif, stored)` — the *forward* direction, exif part alone — and
  then applies the user's turn by itself. Both are crop-then-turn, so both agree.
  `the_clipboard_region_is_the_picture_the_save_would_write` builds the save's own
  formulation independently and compares over every exif tag × all eight xforms ×
  four rectangles; it fails if the rect moves backwards, if the exif part is
  dropped or inverted, and if the two transforms are composed in the wrong order.
  **The trap is that every one of those mistakes yields a valid picture of the
  wrong part of the photo** — nothing about the result looks broken. Three
  details are load-bearing and easy to undo:
  * animation frames are **stored** pixels (the animation path reports `exif_tag`
    and never calls `apply_exif_orientation`), so the clipboard passes `0` there.
    Passing the reported tag transposes a GIF a second time.
  * `FullImage` carries `exif_tag` because that is a property of the buffer, not
    of the preview it happened to arrive with.
  * `set_clipboard_rgba` takes the buffer **by value**: 24 MP is 96 MB, and the
    copy is unavoidable, so it happens once.
  The clipboard does not re-encode at all, which is why it needs neither the
  lossless DCT path nor the re-encode fallback `save_transformed` has.
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

## Measuring

**Producing a number here is a procedure with preconditions that have each already
produced a wrong conclusion. Load the `measuring-runtime` skill before you
benchmark, run a `probe_*.ps1`, A/B two builds, or write that a change was
faster.** The short form: `quser` must say `console` (over RDP a 30 fps animation
measured 12.7 and four conclusions were wrong); a hidden window gets no
paint/vsync delivery; arms must interleave and both must be warmed up before
either is measured; `SetForegroundWindow` is refused unless you own the foreground
window; and a *killed* process writes no perf report at all.

Reading a report that already exists is the other half, and it stays here:

- **A killed process produces no perf report, and no report looks exactly like a
  broken feature.** The report is written by `on_exit`, so a harness must
  `CloseMainWindow` and wait; `Kill` is the fallback for a *hang*.
- **"First visible pixels" is committed in `frame_prelude`**, i.e. after the
  painting frame was presented — never when the worker message arrives, which is
  not on screen yet. A harness that timed "the message arrived" produced a
  plausible wrong number for a hang that was three steps away.
- **A frame count does not mean the app is spinning.** Presentation can set the
  cadence; a 20× smaller window produced the same one. Read `frames with input
  events` (0 while idle) before blaming CPU. On a static photo the only source is
  the 2 s watcher and ~2 fps is correct — which is why **`ms/frame`, CPU amortised
  over whatever frames happened, is meaningless**. Use total CPU%.
- `display-gap frames` (wanted to paint, no texture) and `stalls (>5s to first
  pixels)` must stay 0. The watchdog fires only when nothing was painted at all —
  check `recorded_first`, not just age, or it cries wolf on cache hits. Keep a
  worker panic's payload; a bare "decoder panicked" hid the cause (it is usually
  OOM).
- **Read the config back after writing it and assert the value.** A run that set
  `renderer.choice = gl` reported identical memory for both backends, which would
  have said the backend is irrelevant.
- **Measure the step you suspect, not the step you can see.** Timing the frame,
  then the decode, then adding a per-step timer inside the viewer is how the
  truncated-GIF hang was localised; the harness and the raw decoder both said
  "fine".

**A test that passes on the broken code is decoration.** For every trap here that
has a test, the test was verified to *fail* with the bug reintroduced. Where a
premise is an assumption about a third party (that a chord never arrives as a `Key`
event, that a probe is contained), the test checks the premise against a real
`egui::Context` / a real queue instead of asserting it in a comment. **Load the
`verifying-a-change` skill before committing anything.**

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

**Load the `adding-a-decoder` skill before adding a format, changing a decoder, or
fixing a file that decodes to the wrong pixels.** It sequences the checks below
into an order, and carries the per-format lookup (which decoder, whether it
animates, which primaries it reports). What follows is the part that constrains
an edit.

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
- **Wide-gamut to sRGB is a setting, off by default, and the matrix is derived
  rather than transcribed.** `convert_wide_gamut` in `Config`, applied at decode
  time through `set_wide_gamut_conversion` like `set_linear_resample`. Two things
  about it are easy to get wrong and neither shows up as an error:
  - **The matrix is built from the two spaces' chromaticities** (`P3_XY`,
    `SRGB_XY`, `D65_XY` → `rgb_to_xyz`), not copied out of a spec. It comes out
    identical to the published `lin_display_p3_to_lin_srgb`, and
    `test_the_derived_matrix_reproduces_the_icc_primaries_we_measured` checks the
    derivation against the D50-adapted primary read out of 192 real P3 profiles —
    agreement is 5 units in 15-bit fixed point, where the published primaries, the
    adaptation and this code all have to be right at once.
  - **Two transpositions, both invisible.** The first `rgb_to_xyz` stored the
    primaries as `[primary][component]` and then solved the system as if it were
    `[component][primary]`, so it solved the *transposed* equation: `invert3`
    still round-tripped to the identity, the numbers were plausible, and white
    ended up somewhere other than white. The second was the Bradford product
    order in the test — `M_A⁻¹ · scale · M_A`, not `M_A · scale · M_A⁻¹`. Same
    failure shape as `Xform::Transverse` and the wgpu backend bits: it compiles,
    it type-checks, and it is wrong.
  - `test_out_of_gamut_colours_clip_rather_than_wrap` exists because of the other
    trap in this feature: **pure primaries convert to themselves once clipping is
    applied**, so a fixture built from primaries asserts nothing. The value tests
    use mid-tones, where P3 and sRGB genuinely disagree.
- **`apply_wide_gamut_conversion` is a wrapper around `decode_page`, not a step
  inside it.** `load_page_preview_full` has five return points — preview, full,
  the DCT path, the embedded EXIF thumbnail, animations — and a conversion applied
  at each is a conversion one of them forgets. Tiles are cut from the converted
  `full` and the clipboard reuses the same buffers, so the wrapper covers both.
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
- **`linear_resize` is the hot path for big non-JPEG sources**: **linear-light
  kernel** pre-reduction to ≤2× the target (`prefilter_dim`) → 12-bit linear
  conversion of *that* buffer → two kernel passes. The conversion must never run
  over the full-resolution source, a reciprocal **division inside the inner loop
  cost more than everything else combined**, and **overshoot must clamp LUT
  indices to 4095** (index 4097 was a worker panic). Toggling `linear_resample`
  does not re-decode cached previews (the filter dropdown does — see `refilter`;
  and so does the new `Smooth` toggle, deliberately, which is the fix and not the
  status quo).
  - **The prefilter is a kernel, not a box, and it averages in *linear light*.**
    Both halves were measured on a 2223×3109 page at the real 2.879× reduction
    (the manga half-panel target of 1080), with a harness mirroring
    `linear_resize` verbatim and byte-exact against `downscale_image`.
    * A box's sinc sidelobes sit near −13 dB and do not roll off, so the **box was
      aliasing more than the Hermite pass it fed**: a 100-code sinusoid left
      **29/25/21/29 codes** at source periods 2.5/3/3.5/4 px, where a widened
      kernel leaves **4/2/5/11**. The two tie above source period ~6 px, and at
      every reduction from 1.3× to 16× the kernel is lower or equal (below 2× the
      pre-reduction does not fire at all and the two paths are bit-identical).
      The one cell where the box looked better — reduction 2.5, output period
      exactly 2.00 — is a **one-point notch in the box**: 49 codes at source
      period 4.9, **15** at 5.0, 50 at 5.1.
    * A weighted average preserves exposure **only in the space it averages in**.
      An sRGB-space kernel prefilter came out **5–7 codes darker per page** than
      the box it replaced, and 130.0 on a 50% near-Nyquist dither where the
      tone's true linear-light average is ~187. In linear light the same
      prefilter lands within 1.4 codes of what a single kernel pass over the whole
      source produces — measured on real pages, mean R **158.2 → 165.2** on
      32.jpg, and a 50% dither **162 → 187.2**. *Pages get slightly lighter, and
      that is the point*: the gamma-space box was under-reading dithered tone.
    * **The intermediate size is the memory bound and it did not move.** Peak
      bytes inside `linear_resize` are the same as the box's (u8 full-height
      scratch, u8 pre-size scratch, u16 pre-size linear), and the final pass still
      sees `dFactor` 2.0000 with Hermite's 4–5 taps. Deleting the pre-reduction
      would double the conversion cost for a quality gain of **zero** — a single
      kernel pass scores the same or better on every probe above — so
      `prefilter_dim` is a public function with a table test, and
      `linear_resize_keeps_its_two_step_shape` is a **source scan** (the
      established pattern) because nothing observable from outside says how big
      the intermediate is.
    * It came out **2.4× faster** (debug, 32.jpg → 772×1080: 1096 → 464 ms), and
      **not** because the kernel is cheaper than the box — per output pixel it is
      3–4 f32 taps against 1.4 integer adds. The box's vertical pass summed a
      sliding window down a column accumulator and so ran on **one thread**; the
      kernel's is row-parallel. Converting each row into a per-worker `u16` row
      buffer first, to cut the LUT lookups, was measured and is *slower*
      (565–707 ms), so the LUT stays in the inner loop.
  - **What this does not do.** It reduces the artefact; it cannot abolish it.
    Content at the source Nyquist cannot survive a 2.879× downscale without
    aliasing, and the artefact is a hard 1–2 px sparkle, not a low-frequency
    beat. Measured on real pages, mean |Δ| between adjacent output pixels inside
    flat tone drops 17.996 → 13.533 on 32.jpg — better, not gone.
    - **The "dot lattice at output pitch 3–6 beats by 0.00 codes" note above was
      true and was also measuring the wrong thing.** Those were **1-D** probes —
      `probe_sinusoid` and `probe_dots` both vary along x only — and a stripe
      pattern cannot produce a lattice in the first place. It takes **two
      axis-aligned frequencies beating in both axes**, which is why "the alias
      band is down to 4 codes" was reported as *fixed* while a square grid was
      plainly visible in the app. `probe_square_lattice` is the probe that can
      show it, and the conclusion from it is much less comfortable (below).
  - **The downscale low-pass has a strength axis now, and the `DownscaleFilter`
    shape dropdown is not it.** The square-lattice probe at the real 2.880×
    reduction, residual in codes (Fast → Smooth):
    - source pitch 2.5 → 3.0 → 3.5 → 4.0 → 5.0 → 6.0 → 8.0 px
    - Fast: **10 / 2 / 14 / 10 / 19 / 22 / 23**
    - Smooth (2.0): **1 / 0 / 1 / 1 / 0 / 0 / 4**
    - and as the lattice's **own** output period, which is the artefact rather
      than broadband spread (pitches 3.5 / 4.0 / 5.0 / 6.0): Fast **1.24 / 2.74 /
      5.75 / 7.09**, Smooth **0.08 / 0.06 / 0.10 / 0.04**.
    - **The residual does not fully vanish, and that is the honest limit.** At
      pitch 8 (2.8 output px, i.e. *above* the output Nyquist) Smooth still
      leaves 4 codes — content there is legitimate and a low-pass is entitled to
      keep some of it. On the worst real page, 32.jpg, flat-tone high-frequency
      energy falls only 13.38 → 7.37 codes/px (0.55×) and at strength 3.0 it is
      still 0.39×. **So the setting reduces the artefact substantially on a clean
      lattice and only about half of it on real screentone; it does not abolish
      either.**
      - **~~The residue is broadband, not a lattice: a 2-D spectrum of the
        flat-tone pixels has no peak at any output period from 1.5 to 8 px at any
        strength (all amplitudes ≤ 0.002 codes/px).~~ RETRACTED.** The metric
        that sentence came from divided a coherent amplitude by `sqrt(n)` and
        could not have resolved a coherent peak at any period; see
        `smooth_is_not_the_default_because_it_moves_energy_between_pitches` below
        for the defect and for what replaced it. A coherent peak **does** exist,
        and at some pitches it is **larger** under `Smooth`. Do not cite the
        retracted line, and re-measure the spectrum with
        `the_tone_spectrum_metric_reports_codes` in place before saying anything
        about whether the residue is broadband.
    - **The cost is real and it is the reason this is a toggle.** A hard step
      edge's 10–90% rise at 2.880×: Fast **1.607** → Smooth **2.747** output px
      (**1.71×**). 3-source-px bars: 14.118 → 7.634 codes/px (**0.54×**). On real
      pages, edge-masked gradient falls to 0.37–0.56× depending on the page.
    - **Wall clock, release, 2223×3109 → 1080: Fast 73–77 ms median → Smooth
      95–100 ms median** (interleaved one resize at a time, alternating order,
      both arms warmed). This is a **one-off per resize, not a per-page cost**:
      previews are cached and the cache is what `refilter` drops when the toggle
      flips, so the extra ~25 ms is paid once per image per target size.
      **Measure the arms interleaved.** Run arm-after-arm instead, and the two
      `Fast` blocks bracketing `Smooth` disagreed by 38% (76 ms then 105 ms) —
      the drift is larger than the effect.
    - **`FAST_STRENGTH` is 1.0**, the identity multiplier, so `Fast` is
      *exactly* the pre-existing code path and not a third flavour of it.
      `SMOOTH_STRENGTH` is **still 2.0** and was deliberately **not** re-tuned:
      it is no longer the default, so the question it was chosen to answer no
      longer decides what everyone gets, and lowering it would have been tuning a
      setting whose per-file divergence is not a tuning failure. Pinned by
      `fast_is_exactly_the_pre_existing_kernel_width` and by the bounded cost in
      `smooth_costs_edge_sharpness_and_the_cost_is_bounded`. The 1.71× step-edge
      cost quoted above is therefore still the cost of the `Smooth` **position**,
      which is what that test measures — it is no longer a cost paid by default.
  - **A strength multiplier is not a tap-count change, and the earlier "widening
    is a no-op" note is about the other one.** `contributions` already divides
    the kernel argument by `filter_scale`, so widening only the *span* evaluates
    every extra tap at exactly zero and is bit-identical — that stays true. The
    strength multiplies **`filter_scale`** itself, so the argument and the span
    widen together and the extra taps are real. Both widenings "add taps"; only
    one of them does anything.
  - **One value, read once, threaded to both halves.** `linear_resize` reads
    `downscale_strength()` exactly **once** and hands it to `kernel_reduce_linear`
    and to both `contributions` calls; `contributions` takes it as a parameter
    and does not read the setting. Two halves that read it separately would let
    a toggle pressed between them produce a prefilter of one width feeding a
    final pass of another — a picture that is neither setting, with no error.
    That is the same class as the two manga-mode sites that were each correct
    alone, so `one_strength_reaches_both_halves_of_the_resize` checks the
    **count** (exactly 1) and not just presence.
  - **`Config::smooth_downscale` now defaults to `false`, and that is the fix.**
    It shipped default-**on** (`1b5bdbe`) and was a regression on real files.
    Judging rendered output on six 2223×3109 pages at the manga half-panel
    target, strength 2.0 was better on `32.jpg`, better on `30.jpg`, unchanged on
    `52.jpg`, and **worse on `12.jpg`, `21.jpg` and `44.jpg`** — all three reported
    as a black dot pattern appearing in grey screentone. Strength 1.0 was better
    than `master` on **every** file. **The improvement on all six comes from the
    earlier, unconditional kernel prefilter (`cb0394c`), not from the strength
    toggle**, which is what makes `Fast` the safe default rather than merely the
    less interesting one.
    - **Why the sweep that chose 2.0 concluded the opposite.** It measured a
      **uniform synthetic square lattice**, where more strength is better at every
      pitch it tried. Real screentone is not that, and the sweep's own last
      section said so and then drew the wrong conclusion from it (see the metric
      defect below).
    - **What widening actually does: it moves energy between pitches.** In one
      grey field carrying a dot grid, a second screen at 30° and a dither, scanning
      output period 1.05→4.00 px in 0.05 steps, Fast → Smooth:
      - at **1.389 px** (the grid's own pitch) 1.853 → **0.629** codes — the
        effect the setting is for;
      - at **3.60 px** (a longer period the same image carries) 0.295 → **0.564**
        — widening **nearly doubles** it. 1.4 px goes 0.468 → 0.550, 3.5 px the
        same.
      So it is not "better low-pass, softer edges". It is a redistribution whose
      sign depends on pitch, which is why the per-file results diverge.
    - **The obvious explanation is wrong, and it is the one to reach for.** Not
      multi-pitch content: delete the second screen and the dither and a **pure
      single-pitch 0/100 lattice** at source pitch 4.0 still shows 1.25× at 4.4
      output px and 1.53× finer. It is the filter's response meeting **foldover** —
      the promoted component sits at a *multiple* of the pattern's own period,
      i.e. an image of its own frequency in the output. The previous sweep only
      ever measured at the pattern's own output pitch, where widening wins
      monotonically. `smooth_is_not_the_default_because_it_moves_energy_between_pitches`
      pins the finding as a **scan over pitches** (some pitch must improve, some
      must worsen) rather than one frequency, because a pinned pitch is a number
      that goes stale on the next kernel change and a single-pitch sweep is what
      produced the wrong default in the first place.
    - **Two defects found in the measurement, both of which had already produced
      a wrong conclusion.**
      - **`tone_spectrum_amp` divided a coherent amplitude by `sqrt(n)`.** For a
        *coherent* pattern — the only kind it is asked about, because a dot screen
        is coherent by construction — the least-squares fit already returns the
        amplitude, so the extra division divided a real signal by the square root
        of the pixel count (~700× at a 772×1080 output). Every per-pitch number
        above was produced by it, **including "no peak at any output period from
        1.5 to 8 px at any strength, all amplitudes ≤ 0.002 codes/px"** — the
        sentence that said the residue was broadband and that the lattice was
        "reduced, not abolished". A metric that cannot resolve a coherent peak
        cannot support that. `the_tone_spectrum_metric_reports_codes` calibrates it
        against `beat_amp`, which never had the division, and fails with the bug
        reintroduced.
      - **The probes answered the wrong question.** `masked_grad` over flat tone
        and a whole-image p95−p5 both *fall* at every strength on every file, and
        both therefore say "works" on all six — including the three that got
        worse. A mean cannot see a trade that is defined across pitches.
    - **Unmeasured, and recorded as such:** whether a **Gaussian** (the only
      compactly-supported family whose transform falls monotonically to zero, so no
      sidelobe can beat against anything, and no negative lobe to undershoot) is
      better here. A widened Gaussian was implemented as a throwaway probe and
      measured before the user settled the default, and the per-pitch numbers were
      discarded when the metric defect above invalidated the metric they came
      from. **It is the obvious next experiment and it is not done.** Note also
      that the prefilter and the final pass are both kernel passes, so a pattern
      can beat once against each; whether a single-pass resize beats the two-step
      path on these files is likewise **un-measured**. Do not cite either as a
      result.
    - **What was ruled out, with the mechanism rather than by argument.** The
      overshoot hypothesis — Hermite's negative lobes pulling dark detail below
      zero in the 12-bit buffer and clamping to black — is **refuted**.
      Instrumenting every pre-clamp weighted sum in `linear_resize` and
      `kernel_reduce_linear` and recording the true minimum (not the clamp count,
      which is per-resize and moves with page content): the **pre-clamp minimum is
      0.00 at every strength 1.0–3.0**, i.e. there is no undershoot to clamp.
      Hermite's cubic-B-spline weights are non-negative on `|x| < 1` by
      construction, `contributions` renormalises them to sum to 1, so no weighted
      sum of non-negative samples can go negative — the clamp can never fire from
      the low-pass at all. The user also placed the artefact in **grey screentone,
      not linework**, which is the opposite of what that hypothesis predicted.
  - **The toggle invalidates the previews on the same frame it flips** (the filter
    dropdown's `refilter`, pinned by
    `the_smooth_toggle_is_wired_and_invalidates_the_previews` — a source scan,
    since `main.rs` is unreachable from every test). Toggling `linear_resample`
    does *not*, and that asymmetry is the documented counter-example: a
    decode-time setting that leaves the cache alone flips the box, changes nothing
    visible, and reads as a broken feature.
  - **The filter *shape* dropdown is kept, and its irrelevance to this artefact
    was only measured on screentone.** Hermite had the lowest lattice residual of
    the three and Lanczos3 the highest at 1.93× the cost, which is why shape has
    no useful dynamic range here — but that sweep was on a synthetic lattice and
    **not** on photographic edges, where shape plausibly still matters. **Follow-up
    worth doing: measure shape on real photographic content.** If it turns out to
    be irrelevant there too, collapsing the three shapes into the strength axis
    would be the simplification — **but do not remove a shipped setting on the
    strength of a screentone-only measurement.**
- The old reciprocal-multiply box average (`box_spans` / `box_reciprocal` /
  `ROUND`) and its running column accumulator are **gone**; the notes above about
  integer division and cache misses in those loops no longer apply to this path.

- Known waste, not worth fixing: `exif_orientation(path)` re-reads the file with
  `nom_exif::read_exif` although the bytes are already in memory. That is
  0.1–0.2 ms against a 10–17 ms `fs::read` — ~0.02% of a preview decode.
- **The metadata *job* is the one whole-file read left in the pipeline, and it
  is now gated on the panel being visible** (see Decode pipeline). `color_info`
  needs the whole file because the ICC profile has to be found in it, so unlike
  `exif_orientation` above this one is not 0.1 ms: 8–12 ms on a 17–27 MB photo.
  It is the obvious next thing to fix properly (hand the colour read the bytes
  the decode already has), and it is deliberately *not* fixed by dropping the
  panel's colour line — that line is why a screenshot with no EXIF still says
  something useful.

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
- **A cache of any sort is `%LOCALAPPDATA%`, not `%APPDATA%` and not beside the
  config.** `thumb_cache_root` is the only cache on disk today, and the reason is
  that its contents are *this machine's* decodes of files that may be on a network
  share, are worthless on another machine, and are 256 MB. A roaming profile is
  the wrong place for that, and syncing it would be a bug, not a feature.
  `Config.thumbnail_cache` is the kill switch (`set_thumbnail_cache`, mirroring
  `set_linear_resample`), defaulting **on** with `#[serde(default =
  "default_true")]`, and the settings panel states the cap and the path next to
  the toggle — a cache that silently takes a quarter of a disk should not be
  enabled quietly.
- **A dropped file's copy is under `%TEMP%`, not `%LOCALAPPDATA%`, and is not a
  cache.** `drop_copy_dir` writes `%TEMP%\image-viewer\drop\<16 hex>`. It is not
  `%LOCALAPPDATA%` because it is not reusable: it is the picture the app is
  showing *right now*, released by `close_transients` on the next open and on
  exit, and a stale copy of a file the user has moved on from is worth nothing on
  the next launch. It is on the same root as the archive's temp dir for the same
  reason the user's own files are not: it is a working copy, and `%TEMP%` is what
  the platform reclaims.
- **A member of an archive caches under its temp path.** `%TEMP%\image-viewer\
  <hash(path, mtime, size)>` changes whenever the archive does, so editing a
  `.cbz` orphans every member's cached cell. They age out with everything else;
  no special case was written, and that is a decision rather than an oversight.
- **"The extracted temp dir is stable" is FALSE, and the watcher is disabled
  because of that claim.** `check_changed` and `rescan` both return early when
  `zip_archive.is_some()`, and the comment says why. Measured false twice over
  (IV_TRACE, debug build, `vol1.cbz` with 40 members): `extract_zip` truncates and
  rewrites **every member on every open**, and `close_zip` **deletes the whole
  directory while the app is running** — on `Esc`, on any folder/file open, and on
  `on_exit`. A run that rewrote all 40 members and then deleted the directory got
  `files=0 gallery=40` from the app's own poll, and ten consecutive 2 s polls all
  logged `check_changed SKIPPED`: **it cannot notice, and `failed` is permanent,
  so the first navigation after the bytes go away is a "Loading…" with no cure but
  a restart.** Two amplifiers: there is **no single-instance guard**
  (`CreateMutex` appears nowhere in `src/`), and the directory is content-addressed,
  so **two instances of the same archive share one directory** — the second's open
  rewrites the members the first is displaying and its exit deletes them (measured;
  see `.superpowers/sdd/2026-10-02-manga-spread-mode/archive-member-disappears.md`).
  And `close_zip` **discards `remove_dir_all`'s error**, so on exit — when a worker
  almost always holds a member open — the delete fails and the directory leaks;
  five zero-file directories were sitting in `%TEMP%\image-viewer` on this box.
  **Do not "fix" this by enabling the watcher for archives**: it costs a stat per
  member per 2 s on a 200-page volume, which is the trade this crate exists not to
  make, and it would introduce blanking where there is none today.
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
  - `bench_browse::real_scan_cells`, called from `thumb_cache_ab`, which returns
    early when the directory is absent. It prints the real per-cell cost of a
    large scan because the synthetic fixture deliberately is not that expensive
    and the absolute number should not be extrapolated from it.
- **`thumb_cache.rs` is the model for testing something with four independent
  ways to be wrong** — one test per guard, each verified to fail with the guard
  removed; a damaged-entry test that *enumerates* damage rather than asserting one
  case (which is how the truncated-PNG-as-a-hit bug surfaced); the cache root
  passed in explicitly so no test depends on an env var; and **no timing
  assertion**, with the reason written down rather than discovered later (the first
  two attempts at one were pure warm-up artefacts). Load the
  `verifying-a-change` skill before writing a test for something that can be wrong
  in several ways at once.
- Four suites assert against **source text**, deliberately, and say why in their
  module docs: `keybindings.rs` (a shortcut that does not exist is otherwise
  unobservable), `renderer.rs` (the probe's early exit is in a binary),
  `spreads.rs` (manga mode's `invalidate_path` wiring, that a partner's
  arrival re-plans, and the five fix-wave scans), `unit.rs` (the glyph scan).
  `drops.rs` is a fifth, and is the one to copy for a **lifetime** rule: its scans
  are byte-offset comparisons inside one `fn_body` (`close_transients` before
  `own_drop`), because presence is exactly what was true of the defect it guards.
  They check the *premise* against a real context or queue, not a comment — and
  none of them verifies that a bound key actually works, because that needs a
  synthetic keystroke.
- **A property test can pass on the broken code, and this one did.** With a hard
  link, `a_dropped_file_survives_its_source_being_deleted` was **green** — the
  link genuinely survives an unlink, which is what that test asserts — while the
  feature was still wrong, because a link shares the file rather than copying it.
  It was `a_changed_source_does_not_land_on_top_of_the_previous_copy`, asserting
  the *other* direction (a source rewritten must not mutate a copy the app already
  holds), that failed. **So for anything that has to be a snapshot, assert both
  directions**: survives the source going, and unaffected by the source changing.
  One direction is the deletion half of the problem and knows nothing of the
  aliasing half.
- **A source scan that checks a call's *presence* has already failed here, and the
  fix wave added the check that works.** The `shown_partner` latch had its
  assignment present, in the right function, doing the right call; what was wrong
  was *which block* it was in. So
  `the_turns_second_half_is_derived_not_latched` brace-matches the adopt gate with
  a `block_after` helper and asserts the derivation is **not** inside it, plus a
  byte-offset check that the call really is after the block — and it was verified
  by moving the call back inside, where it fails. **When the defect is a value
  computed at the wrong time, presence is not the property; position is.**
- `main.rs` is unreachable from any test. Every rule that has bitten us has been
  moved into `lib.rs` for that reason: the job planner, the resize policy, the
  transform algebra, `dim_bands`, the page cursor, the preview-cache eviction
  policy, `startup_index`, `path_arg`, the whole spread-pairing core and its
  geometry. If you are about to write a decision in the binary, check whether it
  can be a pure function first.
