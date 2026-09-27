# AGENTS.md

Single Rust crate (edition 2021): a speed-first egui/eframe image viewer with a background worker-pool decode pipeline.

## Commands

- `cargo test` — run all tests; targets: `unit`, `property`, `integration`, `standard`
- `cargo test --test unit` — one test file; `cargo test <name>` — one test by name
- `cargo bench` — criterion benches in `tests/bench.rs` + `tests/bench_variants.rs` (separate targets, not part of `cargo test`)
- `cargo run` — launch the GUI viewer; `cargo run -- --register` / `--unregister` manage file-type registration (HKCU, no admin) and exit
- Release builds hide the console (`windows_subsystem = "windows"`); debug keeps it for `eprintln!` diagnostics — don't "fix" this by removing the attribute

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
- **Key bindings to know**: F11 = fullscreen + hides all chrome; I = panels; G = gallery grid; Del/M/C = triage; E = open folder in default file browser; W = wallpaper; Ctrl+C = copy image; Esc = close grid → exit fullscreen → close window. Side mouse buttons = prev/next.

## Gotchas by feature

- **CBZ/ZIP**: opening an archive extracts supported entries to `%TEMP%\image-viewer\<hash(path, mtime, size)>` and opens it as a folder gallery. The temp dir is reused for unchanged archives, deleted on folder switch and exit. Triage (Del/M/C) is disabled for archives; wallpaper copies the member to `%APPDATA%\image-viewer\wallpaper.<ext>` first (temp dir dies). Don't persist the temp path into `config.folder`.
- **Resume (`last_by_folder`)** stores **file names, not full paths** (archives and folders share the format); the folder path is the key. An explicit CLI file argument beats resume — don't reintroduce the unconditional resume lookup.
- **File registration** (`src/fileassoc.rs`): per-user HKCU ProgID + `OpenWithProgids` for all 9 extensions; never claims default silently (Windows 8+ policy). ProgID embeds the exe folder name — moving the exe requires re-registering.
- **Linear-light resize** (`downscale_image` → `linear_resize`): 12-bit sRGB↔linear LUTs, runtime-selectable filter (`DownscaleFilter`: Hermite/Catrom/Lanczos3 via `set_downscale_filter`), custom separable resampler because the `image` crate's `Filter` is `pub(crate)`. Overshoot must clamp LUT indices to 4095 (index 4097 was a worker panic). Known wart: toggling `linear_resample` doesn't re-decode existing previews (the filter dropdown does) — see `refilter` in the settings handler if you touch this.

## Misc

- **Config location is platform-dependent:** `%APPDATA%\image-viewer\config.json` on Windows — see `Config::default_path()` in `src/lib.rs`. New `Config` fields need `#[serde(default)]` or old configs fail to load.
- **`test-images/JPEGView_L` is a checked-out reference viewer** (JPEGView fork, C++) used for feature ideas, not part of the app.
- `PLAN.md` is a feature backlog, not documentation of current behavior — verify claims against code before trusting it.
