# AGENTS.md

Single Rust crate (edition 2021): a speed-first egui/eframe image viewer with a background worker-pool decode pipeline.

## Commands

- `cargo test` — run all tests; targets: `unit`, `property`, `integration`, `standard`, `embedded_thumbs`, `filter_and_paging`, plus the in-crate `--lib` perf tests. It **stops at the first failing target**, so use `cargo test --no-fail-fast` to see everything.
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
- **`build.rs` is load-bearing**: it passes `/NODEFAULTLIB:LIBCMT(D)` because turbojpeg is built with the static CRT while Rust links the dynamic one. Delete it and every build emits LNK4098 warnings.
- **Format handling is content-based, never extension-based.** Magic-byte sniffing picks the decoder (JPEG/PNG/WebP/GIF via the `image` crate; JXL via `jxl-oxide`; HEIC/HEIF/AVIF via the `heic` crate; misnamed files like a PNG saved as `.jpg` must keep loading — regression test exists). `image_dims_bytes` probes header dims per format.
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
  is the first text input this app has, so `update()` reads
  `ctx.wants_keyboard_input()` and gates every single-letter and navigation key
  on it. Without that, typing "g" into the filter toggles the grid and "r"
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
- `PLAN.md` is a feature backlog, not documentation of current behavior — verify claims against code before trusting it.
