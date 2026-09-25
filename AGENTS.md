# AGENTS.md

Single Rust crate (edition 2021): a speed-first egui/eframe image viewer with a background worker-pool decode pipeline.

## Commands

- `cargo test` — run all tests; targets: `unit`, `property`, `integration`, `standard`
- `cargo test --test unit` — one test file; `cargo test <name>` — one test by name
- `cargo bench` — criterion benches in `tests/bench.rs` + `tests/bench_variants.rs` (separate targets, not part of `cargo test`)
- `cargo run` — launch the GUI viewer

## Gotchas

- **`test-images/` is gitignored but `tests/standard.rs`, `tests/integration.rs`, and both bench files hardcode `test-images\booklet_front.jpg`.** In a fresh clone `cargo test --test standard` fails with NotFound until that file is supplied. `unit` + `property` tests are fixture-free (`tempfile`) — run those to verify logic-only changes.
- **`build.rs` is load-bearing**: it passes `/NODEFAULTLIB:LIBCMT(D)` because turbojpeg is built with the static CRT while Rust links the dynamic one. Delete it and every build emits LNK4098 warnings.
- **Format handling is content-based, never extension-based.** Magic-byte sniffing picks the decoder (JPEG/PNG/WebP/GIF via the `image` crate; JXL via `jxl-oxide`; HEIC/HEIF/AVIF via the `heic` crate). Misnamed files (a PNG saved as `.jpg`) must keep loading — there is a regression test for it. `image_dims_bytes` probes header dims per format; the `image` crate cannot open JXL/HEIF at all.
- **EXIF orientation is applied only to JPEGs.** For HEIF/AVIF the `heic` crate already applies container transforms (`irot`/`imir`); applying the EXIF tag on top would double-rotate. Test files like `IMG_xxxx.HEIC` in this repo's dumps often have no EXIF at all (exiftool confirms) — that's correct, not a bug.

## Architecture (things that are easy to break)

- **Decode is single-pass** (`load_image_preview_full` in `src/lib.rs`): the file is read and decoded once; the preview is derived from the full decode. Large JPEGs (> 12 MP, `FAST_FULL_MAX_PIXELS`) are the exception — cheap DCT-scaled preview (`PREVIEW_DIM` = 256 in `main.rs`, despite a doc comment saying 3840), then a second full decode after a 120 ms dwell.
- **The full image rides on the preview cache.** `Preview` carries `full: Option<Arc<ImageData>>` (and `anim` for GIFs); `sync_shown(ctx)` adopts it for the current image. `needs_full()` checks data availability, *not* texture size vs. source size — the size heuristic was a bug that permanently skipped full-res for preloaded neighbors. Don't reintroduce it.
- **Small/fast files (< 12 MP) skip the preview stage entirely**; the display texture is the full image capped at `PREVIEW_TEXTURE_DIM` (4096). At fit-to-window it must be sharp — a 256px texture there reads as "blurry preview" to the user.
- **Animated GIFs** decode to all composited frames (`Animation`, caps: 240 frames / 256 MB), ride the preview cache like `full`, never get tiles (`needs_full` returns false when `pv.anim.is_some()`), and play back through a single re-uploaded texture with per-frame delays floored to 20 ms. `request_repaint_after` fires at frame boundaries, not every frame.
- **Never decode on the UI thread.** All decode runs on the worker pool (`Loader` in `main.rs`); the UI only paints textures. Tile uploads are budgeted (`TILES_PER_FRAME`) to keep frames smooth.
- **Folder watcher**: `maybe_rescan` polls `scan_folder` every 2 s (`RESCAN_INTERVAL`) and keeps the user's position. A 2 s UI-thread `read_dir` is acceptable; don't make it more frequent.
- **Triage keys** (bound in `update()`): Del = recycle bin (`trash` crate), M = move, C = copy, E = open folder in the system's default file browser (`open` crate — respects Directory Opus, not just Explorer), R = force rescan.

## Misc

- **Config location is platform-dependent:** `%APPDATA%\image-viewer\config.json` on Windows — see `Config::default_path()` in `src/lib.rs`.
- **`test-images/JPEGView_L` is a checked-out reference viewer** (JPEGView fork, C++) used for feature ideas, not part of the app.
- `PLAN.md` is a feature backlog, not documentation of current behavior — verify claims against code before trusting it.
