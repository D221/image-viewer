# AGENTS.md

Rust workspace (single crate, edition 2021): an egui/eframe-based speed-first image viewer.

## Commands

- `cargo test` — run all tests
- `cargo test --test unit` — run a single test file (`unit`, `property`, `integration`, `standard`)
- `cargo test <name>` — run one test by name (e.g. `cargo test zoom_pan`)
- `cargo bench` — criterion benches in `tests/bench.rs` + `tests/bench_variants.rs` (not `cargo test`; benches are separate targets)
- `cargo run` — launch the GUI viewer

## Gotchas

- **`test-images/` is gitignored but tests/benches hardcode exact filenames** (`test-images\booklet_front.jpg` in `tests/standard.rs`, `tests/integration.rs`, both bench files). These files are not committed and not guaranteed present — `cargo test --test standard` currently fails with NotFound unless someone manually supplies the images. Unit + property tests (`tests/unit.rs`, `tests/property.rs`) use `tempfile` and pass without any fixtures; run those to verify logic-only changes.
- **JPEG decode uses `turbojpeg`** (libturbojpeg, via the `zmij` build script) for DCT-domain scaled decoding, with a fallback to the `image` crate if turbojpeg fails at runtime. A full native libturbojpeg install is needed for a clean build on a fresh machine.
- **Architecture split:** `src/lib.rs` holds all testable logic (decode, `Gallery`, `Config`, `ZoomPan`); `src/main.rs` holds the eframe GUI and the decode pipeline (worker pool → stage-1 preview → stage-2 full decode → lazy 1024px tile uploads). Keep logic in `lib.rs` so it stays unit-testable; `main.rs` is hard to test.
- **Config location is platform-dependent:** `%APPDATA%\image-viewer\config.json` on Windows, `~/.config/...` elsewhere — see `Config::default_path()` in `src/lib.rs`.
- `PLAN.md` is a feature backlog/plan, not documentation of current behavior — verify claims against code before trusting it.
