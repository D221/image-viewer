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

Not every format the `image` crate can decode is enabled, and the reasons are
worth stating because they are not all "we forgot":

| Skipped | Why |
|---|---|
| Targa (`.tga`) | **No magic bytes at all.** Detection here is by content, so a Targa is unidentifiable — it fails as `Unknown`. Supporting it would mean putting the file extension back into the decode path, which is the one thing this design avoids |
| DDS | Decode-only in the crate, so no fixture can be generated with the crate that decodes it — untestable with it. Also a game-texture container |
| OpenEXR, Radiance HDR | Linear-light floating point. Through an 8-bit sRGB surface with no tone mapping they come out washed out and blown out, which is *worse* than refusing: the file is fine and the viewer is wrong, and you cannot tell which |
| PNM (`.pbm`/`.pgm`/`.ppm`/`.pam`), farbfeld | A bitmap-mask family and a format that mostly lives in Rust demos. They would pad the file-association list for no real use |
| AVIF (via `image`) | Already handled, and better, by the `heic` crate, which also reads the container's `colr` box. A second, weaker decoder for a format already covered |

The line is not "can the crate decode it" but "can we display it *correctly*, and
can we still test it". QOI clears both.

| Format | Decoder |
|---|---|
| JPEG | `turbojpeg`, with a DCT-scaled fast path for large images |
| PNG, WebP, GIF | `image` crate (animated GIFs included) |
| TIFF, BMP, ICO, QOI | `image` crate |
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
- **Rotate ⟲ / ⟳** — quarter turns, re-fits each time. A view transform by
  default: nothing is written to the file. `Ctrl+S` writes a new file beside the
  original with the rotation baked in, losslessly (see below)
- Folder watcher: new and deleted files are picked up automatically (every 2s)

### Gallery grid (`G`)

A scrollable thumbnail grid of everything in the current folder or archive.

- Thumbnails decode lazily, only for cells that are actually visible, at the
  lowest job priority — scrolling a 2000-image folder stays responsive
- Where the camera wrote an **embedded EXIF thumbnail**, that is used instead of
  decoding the file at all — measured **16–40× faster** on real JPEGs
  (631 ms → 16 ms). The embedded copy is ~160 px, so cells are marginally softer
  than a real decode, which is the trade every photo browser makes. Files whose
  thumbnail rotation can't be determined from the aspect ratio just decode
  normally, rather than risk showing sideways photos
- **Arrows** move a selection (Left/Right by one cell, Up/Down by one row,
  Home/End to the ends); **Enter** opens the highlighted image; a click opens
  whatever you clicked
- Browsing the grid does *not* change the image on screen, so arrowing through
  a big folder costs no re-decodes
- The selection scrolls itself into view only when it would otherwise be
  off-screen
- `Esc` closes the grid

### Filtering (`/`)

Press `/` to focus a filter box and narrow the folder to matching file names — a
case-insensitive substring match, and the folder is scanned flat, so there is no
path component to match. `Esc` clears it. The filter applies to **the whole
viewer, not just the grid** — next/previous walk only what is left, so a filtered
folder is a gallery of its own. Single-key shortcuts stand down while you type,
so `g` in the filter box types a `g` rather than opening the grid.

The filter is deliberately **not** remembered between sessions. One that
silently survived a restart would hide files with no obvious way back.

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

### Saving a rotation (`Ctrl+S`)

Rotation is a view transform by default, so nothing you do to an image touches
the disk. `Ctrl+S` is the one action that writes, and it writes a **new file
beside the original** — `photo.jpg` becomes `photo_rotated.jpg`, or
`photo_rotated2.jpg` if that exists. The original is never modified, so the
action is reversible and saving twice accumulates instead of compounding.

The saved file is the picture exactly as you see it: the camera's EXIF
orientation *and* your rotation, combined, baked into the pixels, with the
orientation tag reset so nothing applies it a second time.

For JPEG this is genuinely lossless. Rather than decode and re-encode, the
rotation is applied to the **DCT coefficients** — the compressed data itself is
moved, not recompressed. Measured on generated images at quality 90:

| image | lossless | decode + re-encode | size in → out |
|---|---|---|---|
| 1920×1080 | 42 ms | 67 ms | 1.02× |
| 4000×3000 | 185 ms | 384 ms | 1.00× |
| 6000×4000 | 381 ms | 911 ms | 1.00× |

The size column is the point: a re-encode makes a fresh rate-distortion decision
per block and comes out a different size, while a coefficient transform lands
within a percent. It is also about 2.4× faster, because it never fully
decompresses.

PNG, BMP and TIFF are also saved losslessly, by decoding and re-encoding into
their own containers — those formats have no lossy step, so nothing is lost.
**WebP, GIF, HEIC and JXL are refused** with a message rather than re-encoded:
that would be a second generation of compression, and a button that says "save"
should not quietly do that.



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
| `/` | Focus the filter box |
| `F11` | Fullscreen |
| `Esc` | Clear the filter (if typing) → close the grid → leave fullscreen → close the window |
| `Del` `M` `C` `E` `W` `R` | Triage, see above |
| `Ctrl+C` | Copy image to clipboard |
| `Ctrl+S` | Save a new file beside the current one, rotation baked in losslessly |
| Mouse wheel | Zoom — or flip images, if that setting is on (`Ctrl`+wheel always zooms) |
| Mouse side buttons | Previous / next image |
| Drag & drop | Open a file, folder or archive |

---

## Settings

Stored as JSON in `%APPDATA%\image-viewer\config.json`:

| Setting | Default | Meaning |
|---|---|---|
| Load all images in folder | on | Build the queue from the whole folder on startup |
| Linear-light downsampling | on | Resample in linear light instead of gamma space — noticeably cleaner on line art |
| Downscale filter | Hermite | Hermite / Catrom / Lanczos3, applied live |
| Wait for image when holding arrows | off | Hold-to-browse that never outruns the decoder, so no image is skipped |
| Mouse wheel flips images | off | Wheel pages instead of zooming (`Ctrl`+wheel still zooms) |
| Loop back at folder ends | on | Wrapping past the last image returns to the first. On by default so older configs keep their existing behaviour |
| Sort mode | Name | Name (natural order) / Date / Size / Random |

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
all sixteen extensions. It deliberately does **not** claim the default-handler
role — Windows requires the user to confirm that themselves.

Registration includes a `DefaultIcon` pointing at the icon embedded in the exe,
which is what makes Explorer show a picture rather than the generic application
icon for these types. It deliberately does **not** register a
`ShellEx\ThumbnailHandler`: the Windows Imaging Component already supplies real
photo thumbnails, and claiming that key would displace WIC and replace actual
thumbnails with an icon. Explorer caches icons per user, so if nothing appears to
change, run `ie4uinit.exe -show` (or restart Explorer).

### Environment variables

| Variable | Effect |
|---|---|
| `IV_WGPU_BACKEND=vulkan` | Render with Vulkan instead of the default DX12. The chosen set is printed at startup |
| `PERF=1` | Turn on the latency report without passing `--perf` a folder |
| `PERF_PHASE=1` | Per-phase timing inside the linear-light resampler (development) |

The renderer is **wgpu on DX12**. DX12 is not a preference — wgpu enumerates
every backend you enable *before* choosing one, and letting it probe Vulkan on a
machine where wgpu's Vulkan path faults takes the process down during adapter
discovery, before the window ever opens. So Vulkan is opt-in only. If a machine
genuinely has no DX12, set `IV_WGPU_BACKEND=vulkan` and check the printed
backend line.

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
- **Neighbours are preloaded.** A fixed window of 3 images either side of the
  current one, with "next" prioritised slightly over "previous".
- **Previews are sized to your display**, not a fixed cap, so the texture is
  exactly as sharp as the screen can show.
- **Decoding never blocks the UI.** A 2–4 thread worker pool with priorities:
  the image you are looking at first, its neighbours next, grid thumbnails
  last. Thumbnail requests re-arm every frame, so they survive the queue being
  rebuilt while you navigate. A job that has started always runs to
  completion — nothing is abandoned halfway.
- **Nothing gets stranded.** The queue keeps the set of paths the viewer wants,
  so a job superseded while in flight is re-queued rather than dropped.
  Cancelling in-flight decodes looks faster and is not: dropping them was the
  cause of images that silently never loaded, because the event that would have
  asked for them again had already gone by. `tests/flip_through.rs` pins this.
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
blank frames, dropped jobs, GPU uploads, decode panics and stalls. A release
build has no console, so the report is written to
`%TEMP%\image-viewer-perf.txt`.

It measures the *app*. For the *pipeline* there is a benchmark that drives the
loader directly with no GUI at all:

```
cargo test --release --test bench_browse -- --ignored --nocapture
```

It flips through a generated folder at several press rates and prints
time-to-first-pixels, wasted decode share, peak MB and cache-hit rate, so any
change to the queue, the preloading window or the cache policy has a number to
move. `IV_BENCH_N` sets the image count, `IV_BENCH_RUNS` the repetitions.

---

## Building and testing

```
cargo build --release      # the exe users run
cargo test                 # unit + property + integration + flip-through
cargo test --no-fail-fast  # ... without stopping at the first failing target
cargo bench                # criterion benchmarks (the only thing needing test-images/)

cargo test --release --test bench_browse -- --ignored --nocapture   # browsing benchmark
cargo test --release --test bench_resize -- --ignored --nocapture    # decodes per window drag
cargo test --release --test bench_rotate_save -- --ignored --nocapture  # lossless save, size ratio
cargo test --release --test pathological -- --ignored --nocapture   # 40/60 MP timings
```

Requires the MSVC toolchain and CMake (turbojpeg builds libjpeg-turbo from
source).

Every suite is **fixture-free** — each builds the images it needs in a temp
directory — so a fresh clone passes with no setup at all. `test-images/` is
gitignored and no `cargo test` suite reads it. The two `cargo bench` targets are
the deliberate exception: they measure a real large photo, so they pick the
biggest JPEG in `test-images/` and print a "drop a photo in to benchmark"
message rather than failing.

`cargo test` stops at the first failing target, so pass `--no-fail-fast` when
you want the whole picture. The `pathological` suite generates its own 40/60 MP
images and is `#[ignore]`d because it takes seconds.

---

## Known caveats

- **Multi-page TIFF shows the first page only.** Page navigation is not
  implemented, so a 20-page document scan opens as one image with no indication
  that more exist. The `image` crate's TIFF *encoder* cannot emit a multi-page
  file, so this is not covered by a test — it is the library's documented
  first-page behaviour.
- **ICO needs an RGBA PNG entry or a BMP entry.** Windows writes the large sizes
  as RGBA PNGs and the small ones as BMP, both of which decode. A PNG-compressed
  entry that is *not* RGBA is rejected, which is a limitation of the underlying
  decoder rather than a choice here.
- **Wide-gamut images look flat.** The colour space is *detected* and shown in
  the metadata panel, but the pipeline is 8-bit sRGB end to end, so a Display P3
  photo is displayed without conversion and reads duller than it should.
  HEIC/AVIF/JXL report their colour space as "not inspected" rather than
  guessing. `TODO.md` records the investigation and the two open decisions.
- Toggling **linear-light downsampling** does not re-decode images that are
  already in the preview cache — the filter dropdown does. Restart or move to
  another image for the change to show everywhere.
- The **preview cache keeps a few images either side of the current one**, so
  jumping far away discards the old previews; scrolling back re-decodes.
- **Rotation is display-only by default.** It is not written to the file and not
  carried into copies, clipboard or wallpaper. Only `Ctrl+S` writes anything,
  and it writes a new file rather than modifying the original.
- **A rotated JPEG can lose up to one 16 px block off an edge.** Lossless
  transforms move whole 8×8 DCT blocks, so a width or height that is not a
  multiple of the MCU block size leaves partial blocks that cannot reach the new
  edge. Rather than leave a visible strip, the save discards them. This is a
  no-op when the dimensions already align, which is the common case.
- **Comics (`.cbz`/`.zip`) are read-only** — triage actions are refused, and
  wallpaper support copies the member out to `%APPDATA%` first.
- Preview texture size follows the window: resizing larger re-decodes so the
  image stays sharp, which briefly costs some CPU.
