# Image Viewer

A fast desktop image viewer for Windows, written in Rust with
[egui/eframe](https://github.com/emilk/egui). Built around one rule: **the UI
thread never decodes anything** — every decode, thumbnail and full-resolution
tile happens on a small worker pool, and the UI only ever paints textures that
are already in memory.

```
cargo run --release
```

---

## Features

### Formats

Content-based detection by magic bytes, so misnamed files still open (a PNG
saved as `.jpg` is fine).

| Format | Decoder |
|---|---|
| JPEG | `turbojpeg`, with a DCT-scaled fast path for large images |
| PNG, WebP, GIF | `image` crate (animated GIFs included) |
| JPEG XL | `jxl-oxide` |
| HEIC, HEIF, AVIF | `heic` |

EXIF orientation is applied on load (JPEG only — the `heic` crate already
applies HEIF container transforms, so applying the tag again would
double-rotate).

### Viewing

- Fit-to-window on load, `F` to re-fit, `1` for exactly 1:1
- Zoom with the wheel, pan by dragging
- Progressive refinement: a cheap preview first, then full-resolution
  **1024px tiles** streamed in over it (up to 8 new tiles per frame) so
  zooming in never blocks
- Neighbour preloading in both directions
- Animated GIF playback with per-frame delays
- Transparency checkerboard backdrop
- **Rotate ⟲ / ⟳** — quarter turns, re-fits each time (view transform only,
  nothing is written to the file)
- Folder watcher: new and deleted files are picked up automatically (every 2s)

### Gallery grid (`G`)

A scrollable thumbnail grid of everything in the current folder or archive.

- Thumbnails decode lazily, only for cells that are actually visible, at the
  lowest job priority — scrolling a 2000-image folder stays responsive
- **Arrows** move a selection (Left/Right by one cell, Up/Down by one row,
  Home/End to the ends); **Enter** opens the highlighted image; a click opens
  whatever you clicked
- Browsing the grid does *not* change the image on screen, so arrowing through
  a big folder costs no re-decodes
- The selection scrolls itself into view only when it would otherwise be
  off-screen
- `Esc` closes the grid

### Archives

Opening a `.cbz` or `.zip` extracts the image entries to a temp folder and
opens it as a normal gallery, so paging, zooming and the grid all work
unchanged. The temp folder is reused while the archive is unchanged and
cleaned up on exit. Comics are read-only: delete/move/copy are refused rather
than silently writing into the zip.

### Triage

| Key | Action |
|---|---|
| `Del` | Move to the Recycle Bin (never a hard delete) |
| `M` / `C` | Move / copy the current image to a chosen folder |
| `E` | Reveal the containing folder in your default file browser |
| `W` | Set as desktop wallpaper |
| `Ctrl+C` | Copy the image to the clipboard |
| `R` | Rescan the folder / reshuffle the sort order |

### Metadata

Left panel shows camera make, model and lens, capture date, aperture,
shutter speed, ISO, focal length, metering mode, flash, colour space and
orientation. GPS-tagged images also show coordinates and altitude, with a
clickable **Open in Google Maps** link.

### Interface

- Browse Folder… / Browse File… dialogs, plus drag-and-drop of a file, folder
  or archive
- F11 for fullscreen (hides all chrome), `I` to toggle the panels
- Sort by name (natural order, so `IMG_2` precedes `IMG_10`), date, size or
  random
- Remembers the last image you were on, per folder
- Optional per-user file-type registration

---

## Keyboard

| Key | Action |
|---|---|
| `←` `→` | Previous / next image (also `PgUp` / `PgDn` / `Space`) |
| `↑` `↓` | In the grid: move the selection by a row |
| `Enter` | In the grid: open the selected image |
| `Home` / `End` | First / last image (in the grid: first / last cell) |
| `G` | Toggle the gallery grid |
| `F` | Fit to window |
| `1` | 100% (1 image pixel = 1 screen pixel) |
| `I` | Toggle the side and bottom panels |
| `F11` | Fullscreen |
| `Esc` | Close the grid → leave fullscreen → close the window |
| `Del` `M` `C` `E` `W` `R` | Triage, see above |
| `Ctrl+C` | Copy image to clipboard |
| Mouse wheel | Zoom — or flip images, if that setting is on (`Ctrl`+wheel always zooms) |
| Mouse side buttons | Previous / next image |
| Drag & drop | Open a file, folder or archive |

---

## Settings

Stored as JSON in `%APPDATA%\image-viewer\config.json`:

| Setting | Default | Meaning |
|---|---|---|
| Load all images in folder | on | Build the queue from the whole folder on startup |
| Sort mode | Name | Name (natural order) / Date / Size / Random |
| Linear-light downsampling | on | Resample in linear light instead of gamma space — noticeably cleaner on line art |
| Downscale filter | Hermite | Hermite / Catrom / Lanczos3, applied live |
| Wait for image when holding arrows | off | Hold-to-browse that never outruns the decoder, so no image is skipped |
| Mouse wheel flips images | off | Wheel pages instead of zooming (`Ctrl`+wheel still zooms) |

The downscale filter and sort mode re-decode immediately; the linear-light
toggle applies to newly decoded images (see the known caveat below).

---

## Command line

```
image-viewer.exe [file | folder | archive.zip | archive.cbz]
image-viewer.exe --register      # add this build to the "Open with" list
image-viewer.exe --unregister
```

`--register` writes a per-user (HKCU) ProgID and `OpenWithProgids` entries for
all nine extensions. It deliberately does **not** claim the default-handler
role — Windows requires the user to confirm that themselves.

---

## How it stays fast

- **One decode per image.** The file is read and decoded once; the on-screen
  preview is derived from that decode instead of re-reading the file, and the
  full-resolution pixels ride along in the cache for free.
- **Large JPEGs get a DCT-scaled preview** (turbojpeg scales in the DCT
  domain), so opening a 40 MP photo doesn't mean decoding 40 MP twice. The
  full decode starts only after you have actually looked at it for 120 ms.
- **Big images are resampled in two steps.** A cheap box pass reduces the source
  to at most twice the display size, and only that reduced buffer is converted
  to linear light and filtered — measured at **790 ms → 219 ms** for a 40 MP
  source, and it aliases less than filtering straight from full resolution.
- **Preloading follows how you browse.** Deliberate paging preloads ±2 images;
  running forward fast preloads 6 ahead and 1 behind (mirrored when you go
  back), so decode capacity goes where you are going.
- **Abandoned work stops early.** Every job carries an epoch; when you navigate
  away, the in-flight decode abandons its remaining phases instead of finishing
  work nobody will see. Decoder threads themselves are never interrupted.
- **Previews are sized to your display**, not a fixed cap, so the texture is
  exactly as sharp as the screen can show.
- **Resizing never blanks.** The current texture keeps being painted (scaled by
  the GPU) while a sharper one is decoded in the background, debounced so a
  window drag triggers a single re-decode.
- **Decoding never blocks the UI.** A 2–4 thread worker pool with priorities:
  the image you are looking at first, then previews along your direction of
  travel, grid thumbnails last. Thumbnail requests re-arm every frame, so they
  survive the queue being rebuilt while you navigate.
- **Bounded memory.** Preview cache capped at 16 textures, tile cache at 64
  (≈256 MB), thumbnails LRU-capped at 512, animated GIFs at 240 frames /
  256 MB.

### Measuring it

Built in, because the interesting numbers are about what you *see*, not about
how long a decode takes:

```
image-viewer.exe --perf <folder>     # latency report on exit
```

It records time from intent to useful pixels — startup, selection, next/prev,
zoom, pan, gallery open, folder scan, resize — at p50/p95/p99, plus counters for
blank frames, dropped jobs, cancelled decodes and GPU uploads. A release build
has no console, so the report is written to `%TEMP%\image-viewer-perf.txt`.

---

## Building and testing

```
cargo build --release      # the exe users run
cargo test                 # unit + property + integration
cargo bench                # criterion benchmarks
cargo test --release --test pathological -- --ignored --nocapture   # 40/60 MP timings
```

Requires the MSVC toolchain and CMake (turbojpeg builds libjpeg-turbo from
source). Tests are fixture-free — they build their own images in a temp dir —
and fast. `test-images/` is gitignored, so `cargo test --test standard` needs
`test-images\booklet_front.jpg` supplied locally; the `unit` and `property`
suites do not. The `pathological` suite generates its own 40/60 MP images and is
`#[ignore]`d because it takes seconds.

---

## Known caveats

- Toggling **linear-light downsampling** does not re-decode images that are
  already in the preview cache — the filter dropdown does. Restart or move to
  another image for the change to show everywhere.
- The **preview cache keeps a few images either side of the current one**, so
  jumping far away discards the old previews; scrolling back re-decodes.
- **Rotation is display-only.** It is not written to the file and not carried
  into copies, clipboard or wallpaper.
- **Comics (`.cbz`/`.zip`) are read-only** — triage actions are refused, and
  wallpaper support copies the member out to `%APPDATA%` first.
- Preview texture size follows the window: resizing larger re-decodes so the
  image stays sharp, which briefly costs some CPU.
