# AGENTS.md

Single Rust crate (edition 2021): a speed-first egui/eframe image viewer with a background worker-pool decode pipeline.

## Commands

- `cargo test` — run all tests; targets: `unit`, `property`, `integration`, `standard`
- `cargo test --test unit` — one test file; `cargo test <name>` — one test by name
- `cargo bench` — criterion benches in `tests/bench.rs` + `tests/bench_variants.rs` (separate targets, not part of `cargo test`)
- `cargo test --release --test pathological -- --ignored --nocapture` — generate and time 40/60 MP images; the only way to see where big-image time actually goes
- `cargo run -- --perf <folder>` — turn on latency sampling; the report prints on exit and is written to `%TEMP%\image-viewer-perf.txt` (a release build has no console, so the file is the real output)
- `PERF_PHASE=1` — per-phase timing inside the linear-light resampler
- `cargo run` — launch the GUI viewer; `cargo run -- --register` / `--unregister` manage file-type registration (HKCU, no admin) and exit
- Release builds hide the console (`windows_subsystem = "windows"`); debug keeps it for `eprintln!` diagnostics — don't "fix" this by removing the attribute

## Performance (the core goal — read before touching the pipeline)

- **Measure, don't guess.** `src/perf.rs` samples 16 latency metrics into fixed
  256-entry atomic rings (p50/p95/p99) plus 8 always-on counters. Enable with
  `--perf` or `PERF=1`. "First visible pixels" is committed in `frame_prelude`,
  i.e. *after* the painting frame was presented — never record it when the
  worker message arrives, that is not on screen yet.
- **`display-gap frames` must stay 0.** It counts frames that wanted to paint the
  current image and had no texture. Nonzero means something threw away the
  texture the user is looking at.
- **Never drop the displayed texture to re-decode.** On a larger window the
  existing texture keeps being painted (the GPU scales it) and a sharper one is
  requested, debounced by `RESIZE_SETTLE` (180 ms) so a drag-resize decodes once.
  The old code dropped `below_target` previews here, which blanked the image.
- **The resampler is the hot path for big non-JPEG sources.** `linear_resize`:
  sRGB box pre-reduction to ≤2x the target (integer running sums, rayon,
  precomputed reciprocals) → 12-bit linear conversion of *that* buffer → two
  kernel passes. Hard-won: the conversion must never run over the
  full-resolution source; a reciprocal **division inside the inner loop cost more
  than everything else combined**; the vertical pass needs its running column
  accumulator or it misses cache on every pixel.
- **Cancellation is a set of wanted paths, never a global epoch.** `Loader.wanted`
  is published by `schedule()`; a job is abandoned when its path leaves the set
  (thumbnails are never cancelled — cheap, and the grid re-requests them). Two
  rules an earlier epoch-based design got catastrophically wrong: re-planning
  the *same* work must cancel nothing, and `JobErr::Cancelled` must never be
  shown, logged, or recorded in `failed` — it means "ask again later", not
  "broken file". Conflating the two poisoned every image the user passed, and
  the image was then never retried. `schedule()` always puts the current image
  in the set, so it can never be cancelled. Third-party decoders
  (turbojpeg/libheif/jxl-oxide) are *not* interrupted; they expose no safe
  cancellation.
- **Preloading follows travel direction** (`preload_radii` in lib.rs): slow
  browsing gets ±2, rapid one-directional browsing gets 6 ahead / 1 behind (or
  mirrored), and anything without a clear 2/3 majority stays symmetric — lean
  only on 2/3 or alternating input ends up betting on one side. The current
  image is always `PRIO_CURRENT_PREVIEW` (0) and every prefetch job is ≥ 10, so
  prefetch can never starve it.
- **`queued jobs dropped by rebuild` is the coalescing signal.** Large values mean
  the user outran the decoder — tune the preload window, don't add workers.
- **Texture churn is already low**: previews upload once per decode, tiles are
  created once and reused, animated GIF frames update in place via `set`. Count
  uploads (`GPU texture uploads`) before "optimising" this.

## Gotchas

- **`test-images/` is gitignored but `tests/standard.rs`, `tests/integration.rs`, and both bench files hardcode `test-images\booklet_front.jpg`.** In a fresh clone `cargo test --test standard` fails with NotFound until that file is supplied. `unit` + `property` tests are fixture-free (`tempfile`) — run those to verify logic-only changes.
- **`build.rs` is load-bearing**: it passes `/NODEFAULTLIB:LIBCMT(D)` because turbojpeg is built with the static CRT while Rust links the dynamic one. Delete it and every build emits LNK4098 warnings.
- **Format handling is content-based, never extension-based.** Magic-byte sniffing picks the decoder (JPEG/PNG/WebP/GIF via the `image` crate; JXL via `jxl-oxide`; HEIC/HEIF/AVIF via the `heic` crate; misnamed files like a PNG saved as `.jpg` must keep loading — regression test exists). `image_dims_bytes` probes header dims per format.
- **`normalized()` (lib.rs) guards every decode exit.** Decoders can return buffers that disagree with declared dims (truncated files, OneDrive hydration races); without normalization the epaint size assert panics a worker ("decoder panicked"). Keep it.
- **EXIF orientation is applied only to JPEGs.** For HEIF/AVIF the `heic` crate already applies container transforms (`irot`/`imir`); applying the EXIF tag on top would double-rotate. Some HEICs in test dumps have no EXIF at all (exiftool-verified) — that's correct, not a bug.

## Architecture (things that are easy to break)

- **Decode is single-pass** (`load_image_preview_full` in `src/lib.rs`): the file is decoded once; the preview texture is derived from the full decode and sized to the *display area* (`Job.target_dim`, the central panel in physical px). Large JPEGs (> 12 MP, `FAST_FULL_MAX_PIXELS`) are the exception — turbojpeg DCT preview, then a full decode after a 120 ms dwell.
- **The full image rides on the preview cache.** `Preview` carries `full: Option<Arc<ImageData>>` (and `anim` for GIFs); `sync_shown(ctx)` adopts it for the current image. `needs_full()` checks data availability, *not* texture size vs. source size — the size heuristic was a bug that permanently skipped full-res for preloaded neighbors. Don't reintroduce it.
- **Resize events**: growing the display drops `below_target` previews and re-decodes at the new target. Panel toggles and F11 set `suppress_resize_redecode` for exactly one transition so layout changes don't count as "window grew".
- **Hold-to-browse** (`block_while_loading` setting): `step()` is throttled while `waiting_load`; on preview arrival a `pending_advance` is queued and consumed at the *start of the next frame* — the arrived image is guaranteed to have been painted. Consuming it in the same frame skips the render (that was the original bug).
- **Animated GIFs** decode to all composited frames (`Animation`, caps: 240 frames / 256 MB), ride the preview cache, never get tiles, and play back through a single re-uploaded texture with delays floored to 20 ms.
- **Never decode on the UI thread.** All decode runs on the worker pool (`Loader` in `main.rs`); the UI only paints textures. Grid thumbnails use `Kind::Thumb` jobs (`Loader::push`, prio 45) and an LRU-capped `thumbs` cache — `grid_cell` requests them lazily for visible cells only.
- **Folder watcher**: `maybe_rescan` polls `scan_folder` every 2 s and keeps the user's position. Skipped while a cbz/zip archive is open.
- **Key bindings to know**: F11 = fullscreen + hides all chrome; I = panels; G = gallery grid; in the grid the arrows/Home/End move a *selection* (they do not change the shown image) and Enter opens it; Del/M/C = triage; E = open folder in default file browser; W = wallpaper; Ctrl+C = copy image; Esc = close grid → exit fullscreen → close window. Side mouse buttons = prev/next. Rotate ⟲/⟳ are bottom-panel buttons only (temporary UI; `self.rot` is a view transform, re-fits after each turn).

## Gotchas by feature

- **CBZ/ZIP**: opening an archive extracts supported entries to `%TEMP%\image-viewer\<hash(path, mtime, size)>` and opens it as a folder gallery. The temp dir is reused for unchanged archives, deleted on folder switch and exit. Triage (Del/M/C) is disabled for archives; wallpaper copies the member to `%APPDATA%\image-viewer\wallpaper.<ext>` first (temp dir dies). Don't persist the temp path into `config.folder`.
- **Resume (`last_by_folder`)** stores **file names, not full paths** (archives and folders share the format); the folder path is the key. An explicit CLI file argument beats resume — don't reintroduce the unconditional resume lookup.
- **File registration** (`src/fileassoc.rs`): per-user HKCU ProgID + `OpenWithProgids` for all 9 extensions; never claims default silently (Windows 8+ policy). ProgID embeds the exe folder name — moving the exe requires re-registering.
- **Linear-light resize** (`downscale_image` → `linear_resize`): 12-bit sRGB↔linear LUTs, runtime-selectable filter (`DownscaleFilter`: Hermite/Catrom/Lanczos3 via `set_downscale_filter`), custom separable resampler because the `image` crate's `Filter` is `pub(crate)`. Overshoot must clamp LUT indices to 4095 (index 4097 was a worker panic). Known wart: toggling `linear_resample` doesn't re-decode existing previews (the filter dropdown does) — see `refilter` in the settings handler if you touch this.

## Misc

- **Config location is platform-dependent:** `%APPDATA%\image-viewer\config.json` on Windows — see `Config::default_path()` in `src/lib.rs`. New `Config` fields need `#[serde(default)]` or old configs fail to load.
- **`test-images/JPEGView_L` is a checked-out reference viewer** (JPEGView fork, C++) used for feature ideas, not part of the app.
- `PLAN.md` is a feature backlog, not documentation of current behavior — verify claims against code before trusting it.
