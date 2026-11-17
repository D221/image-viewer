# AGENTS.md

Windows-only Rust desktop app: egui/eframe 0.37 on glow (OpenGL). One crate, no workspace, CI in GitHub Actions. One rule shapes everything: **the UI thread never decodes** — every decode, thumbnail and tile runs on a worker pool; the UI only paints textures already in memory.

## Commands

- `cargo nextest run --no-fail-fast` — the whole suite, test binaries in parallel. Plain `cargo nextest run` stops at the first failing test. Nextest does not run doctests; this crate has none.
- `cargo fmt` — the tree is rustfmt-clean under `rustfmt.toml`; `cargo fmt --check` shows what it would change. Run it before committing.
- `cargo nextest run --test <file>` — one test file; `cargo nextest run <test_name>` — one test.
- `cargo bench` runs nothing. The benchmarks are `#[ignore]`d test targets:
  `cargo nextest run --release --test bench_browse --run-ignored ignored --no-capture`
  (same shape for `bench_resize`, `bench_rotate_save`, `bench_startup`, `pathological`).
- `cargo run` — the GUI. Flags: `--register` / `--unregister` (HKCU, no admin), `--icc-scan <folder>`, `--perf <folder>` (latency report; release has no console, output goes to `%TEMP%\image-viewer-perf.txt`). `PERF=1` samples without a folder.
- `cargo run --release --example make_icon` regenerates `assets/app.ico`.
- PowerShell probes: `powershell -File tests\<script>.ps1`, console session only (`bench_launch_ab.ps1`, `probe_chords.ps1`, `probe_thumb_cache.ps1`, `probe_watcher.ps1`, …).
- Needs the MSVC toolchain and CMake (turbojpeg-sys builds libjpeg-turbo from source).

## Source layout — the 1000-line rule

Enforced by `tests/source_size.rs`:

- Every source file under `src/`, `tests/` and `examples/` must be under 1000 lines.
- Files over the target are ceilings in that test's `OVER_TARGET` list; they may be edited but may not **grow**. The list only ever shortens.
- New code goes in a module of its own — `src/<topic>.rs` or `src/<topic>/mod.rs` — declared with `pub mod` at the crate root and re-exported (`pub use <topic>::*;`) so `image_viewer::name` keeps working.
- To split an over-target file: move a section into its own module, declare it where the old code was, and lower its ceiling in `tests/source_size.rs`.

Raising a ceiling is a decision, not a default — don't do it by accident.

## Architecture traps — these fail silently if you don't know them

- **Never cancel or abandon a decode.** A job runs to completion even after the user moves on. On finish, a job that produced nothing and is still wanted (per the queue's `wanted` map) is re-queued. The only question is "is this still wanted?".
- **`failed` is `HashSet<(ImgKey, Kind)>` and a failure is permanent for the session.** Only `invalidate_path`, triage removal, the resize drop and `refilter` clear entries. A transient error poisons the image until restart. If a load failure looks mysterious, suspect this first.
- **`ImgKey { path, page }` keys every cache that holds pixels** (preview cache, `failed`, loader `wanted`/`busy`, `Msg::Preview`/`Full`). A path-keyed cache silently hands back page 1 when page 3 was asked — only matters for multi-page TIFF, but it is the whole reason the `tiff` crate is a direct dependency (`image` 0.25 reads only page 0).
- **`main.rs` is unreachable from any test.** If you are about to write a decision in the binary, check whether it can be a pure function in `lib.rs` first. Every rule that has bitten us has been moved there (job planner, resize policy, transform algebra, page cursor, spread pairing).
- **Drag-and-drop is a hand-off, not a reference.** Explorer extracts a zip member to a transient temp file and deletes it, so the app copies the dropped file first — on a worker thread — to `%TEMP%\image-viewer\drop\<16 hex>` before opening it. Never open a dropped path in place.
- **Archives (`.zip`/`.cbz`/`.7z`/`.rar`) extract to `%TEMP%\image-viewer\<hash(path,mtime,size)>`**, are read-only (triage and move/copy are refused), and the temp dir is deleted on folder switch and exit. The folder watcher is **disabled for archives** — `extract_zip` rewrites every member on every open and `close_zip` deletes the directory while the app runs, so a watch would blank the gallery. Do not enable it for archives.
- **There is no single-instance guard** — `CreateMutex` appears nowhere in `src/`.

## Windows integration

- Config lives at `%APPDATA%\image-viewer\config.json`. **New `Config` fields need `#[serde(default)]`** or old configs fail to load; **never add `deny_unknown_fields`** — unknown keys are ignored, which is why an older config still starts a newer build.
- Caches live in `%LOCALAPPDATA%\image-viewer\` (`thumbs`, `previews`) — never `%APPDATA%` and never beside the config. Their contents are this machine's decodes of files that may be on a network share; syncing them would be a bug.
- File associations are per-user HKCU ProgID + `OpenWithProgids` for the 9 decoded extensions (`src/fileassoc.rs`). It never claims default silently (Windows 8+ policy). After a registration change, run `ie4uinit.exe -show` — Explorer caches icons per user. A registered type needs a `DefaultIcon` and must NOT have a `ShellEx\ThumbnailHandler` (that would displace WIC's real photo thumbnails).
- The window icon is set via `ViewportBuilder::with_icon`. Do not reach for `WM_SETICON` — it arrives too late and four attempts failed. Explorer reads the registered `DefaultIcon`; the title bar/taskbar read the window icon.
- **The renderer is glow (OpenGL), and wgpu is not in the build.** `Cargo.toml` sets eframe's `default-features = false` and asks only for `glow`, `default_fonts` and `accesskit`; `main.rs` passes `eframe::Renderer::Glow`. Do not restore eframe's defaults: that brings back wgpu (via `egui-wgpu`), the Wayland/X11 platform features and the web screen reader, none of which a Windows glow app uses. The reason is measured, not stylistic - on this machine wgpu cost 154 MB more at idle and 269 ms more to create a device for the **same** frame rate (76.8 fps on both, interleaved A/B on a console session), and its Vulkan backend access-violates during adapter enumeration even though `vulkaninfo` reports a healthy device. That crash is why an earlier version pinned one backend per process, probed each in a child, and restarted the window to change it; with glow there is no backend to choose, so there is no probe, no `RestartSlot` and no graphics setting.

## Testing

- Every suite builds its own fixtures in `tempfile::tempdir()`. `test-images/` is gitignored — a test reading it passes on your machine and dies with `NotFound` in a fresh clone. The only readers of `test-images/` are the criterion targets (which want a real large photo), `test_gps_decode` (skips if `geo.jpg` is absent) and `bench_browse::real_scan_cells` (returns early when the dir is absent).
- `tests/thumb_cache.rs` is the model for a cache that can be wrong in several ways: one test per guard, each verified to fail with the guard removed, a damaged-entry test that enumerates damage, the cache root passed in explicitly, and **no timing assertion**.
- The suites that assert against **source text** (`keybindings.rs`, `spreads_dims.rs`, `spreads_goto.rs`, `spreads_latches.rs`, `spreads_pan.rs`, `spreads_zoom.rs`, `unit.rs`) each compile their own copy of the helpers in `tests/common/spreads_fixtures.rs` - one test target cannot import from another. `drops.rs` is the lifetime-rule equivalent. A scan that checks a call's *presence* has already failed here - when the defect is a value computed at the wrong time, **position is the property, not presence**. Strip comments before scanning source; comments quote the very call being sought. A needle longer than one line must be compared against whitespace-squashed text (`squash` in `unit.rs`, `code_only` where it already strips comments), or the test silently encodes today's rustfmt wrap points and `cargo fmt` breaks it.
- **A test that passes on the broken code is decoration.** For every trap with a test, the test was verified to fail with the bug reintroduced. Check premises against a real `egui::Context` / real queue, not a comment. Load the `verifying-a-change` skill before committing.

## egui 0.36 / eframe quirks

- `Context::run_ui` (not `run`), `Ui::new_child(UiBuilder)` (never `scope_builder` — it advances the parent's cursor), `ComboBox::from_id_salt` (not `from_id_source`).
- Clipboard chords never arrive as `Key` events: `egui-winit` intercepts them into `Event::Copy`/`Cut`/`Paste`. Read `egui::Event::Copy` inside the `!typing` block. Check any new chord against `is_cut_command`/`is_copy_command`/`is_paste_command` before binding it.
- egui's default font is a subset; a missing glyph is a blank box, not an error. Only `… — · ° ⟲ ⟳` work beyond ASCII. `tests/unit.rs` scans `src/main.rs` and `src/lib.rs` against the real font.
- **A pinned panel is not clipped to its pin.** `Panel::right(...).exact_size(w)` whose content is wider than `w` grows the content `Ui` to the right past `max_rect`; `panel.rs` then clamps the *size* but re-anchors the rect at the grown right edge, so the pane is reported shifted by the overflow — drawn off the window, with the `CentralPanel` painting over its left end. That was the fit-window "only the right side of Settings is drawn" bug (the Back/Forward row needs ~401 pt against a 300 pt pin). Rows in `src/app/ui/settings.rs` and `settings_keys.rs` are therefore `horizontal_wrapped`, and a text edit sharing a row with a button is sized by `edit_width`; `tests/unit_uichrome.rs` enforces both.
- `RectShape` moved texture fields into `brush: Option<Arc<Brush>>`; a plain `filled` has `brush: None`. Don't hardcode egui defaults (`item_spacing` moves between releases).

## Formats, colour, resampling

- Format handling is **content-based, never extension-based** — magic bytes pick the decoder (JPEG/turbojpeg, PNG/WebP/GIF/`image`, TIFF/BMP/ICO/QOI/`image` + `tiff` for page 2+, JXL/`jxl-oxide`, HEIC/HEIF/AVIF/`heic`). A PNG saved as `.jpg` must load. `IMAGE_CRATE_DECODERS_WE_SKIP` lists the deliberately disabled `image` decoders (sharpest: Targa, which has no signature at all). Do not "fix" by passing the extension into the decode path.
- `normalized()` guards every decode exit and the downscale exit — a buffer that disagrees with its dims aborts a worker.
- EXIF orientation is applied to **JPEGs only** (the `heic` crate already applies HEIF container transforms).
- Never `into_frames().count()` — it decodes the whole animation and a truncated GIF keeps yielding `Err`. Take two frames for GIF (`is_animated`); read a header for PNG/WebP.
- Wide-gamut → sRGB is a setting, off by default, and the matrix is derived from chromaticities, not transcribed.
- `linear_resize` is the hot path; `Smooth` defaults to `false` and `MIN_FILTER_SCALE` is `0.0` (the floor is off). Toggling `linear_resample` does not re-decode cached previews; the filter dropdown and the `Smooth` toggle do (`refilter`).

## Measuring

If you are going to claim a number, load the `measuring-runtime` skill first. Preconditions that have already produced wrong conclusions:

- `quser` must say `console` — over RDP a 30 fps animation measured 12.7 fps and four conclusions were wrong.
- Arms must interleave and both be warmed up before either is measured; run arm-after-arm and the drift exceeds the effect.
- A **killed process writes no perf report** — the report is written by `on_exit`, so `CloseMainWindow` and wait; `Kill` is only for a hang.
- "First visible pixels" is committed in `frame_prelude` (after the frame was presented), not when the worker message arrives.
- `display-gap frames` and `stalls (>5 s to first pixels)` must stay 0; `ms/frame` amortised over whatever frames happened is meaningless — use total CPU%.
- Read the config back after writing it and assert the value.
