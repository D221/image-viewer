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
- **Texture churn is already low**: previews upload once per decode, tiles are
  created once and reused, animated GIF frames update in place via `set`. Count
  uploads (`GPU texture uploads`) before "optimising" this.
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
