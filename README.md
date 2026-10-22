# Image Viewer
  image stays sharp, which briefly costs some CPU.
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo the last turn, mirror or crop. **Per image** - undo means undo on the picture you are looking at, not walk back through earlier ones. The Undo/Redo entries in the bottom bar's Transform menu disable themselves when there is nothing to undo |
# Image Viewer
  image stays sharp, which briefly costs some CPU.
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo the last turn, mirror or crop. **Per image** - undo means undo on the picture you are looking at, not walk back through earlier ones. The Undo/Redo entries in the bottom bar's Transform menu disable themselves when there is nothing to undo |
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
| PNG, WebP, GIF | `image` crate, including APNG and animated WebP |
| TIFF, BMP, ICO, QOI | `image` crate (multi-page TIFF past page 1 via the `tiff` crate) |
| JPEG XL | `jxl-oxide` |
| HEIC, HEIF, AVIF | `heic` |

EXIF orientation is applied on load (JPEG only — the `heic` crate already
applies HEIF container transforms, so applying the tag again would
double-rotate).

### Viewing

- Fit-to-window on load, `F` to re-fit, `1` for exactly 1:1
- Zoom with the wheel, pan by dragging. The status bar shows the zoom as a
  percentage of 1:1, so it means the same whatever the window is doing
- Progressive refinement: a cheap preview first, then full-resolution
  **1024px tiles** streamed in over it (up to 8 new tiles per frame) so
  zooming in never blocks
- Neighbour preloading in both directions
- Animated playback with per-frame delays — GIF, APNG and animated WebP
- **Multi-page TIFF**: `←` / `→` walk the pages of a document before moving to the
  next file, and the position shows in the title bar and the status line
  (`page 3/12`). Stepping back from page 1 lands on the **last** page of the
  previous file, so going backwards never costs two presses per document
- Transparency checkerboard backdrop
- **Rotate ⟲ / ⟳**, **Mirror H / V**, **Reset turn** — quarter turns and mirrors,
  **per image**, re-fitting each time. Turning one photo does not turn the next,
  and going back finds yours still turned — the behaviour every real viewer has.
  Turn and mirror *compose* rather than living in separate counters, because they
  do not commute: turning then mirroring is a diagonal mirror, which is neither
  operation. Mirroring twice gets you back where you started. A view transform by
  default: nothing is written to the file. **Save transformed…** (bottom bar's
  Save menu, or `Ctrl+S`) writes a new file beside the original with the transform
  baked in, losslessly (see below)

**Undo (`Ctrl+Z`/`Ctrl+Shift+Z`)** covers the view transforms only - turn, mirror, reset-turn, crop and clear-crop. It is a per-image history (undo means "undo on this picture"), and it never writes to disk. Zoom, pan, settings changes and ratings are not undoable.
- **Crop** (`C`) — drag on the image to frame a crop. The frame is drawn over the whole
  picture rather than the picture being cut on screen, so turning and mirroring
  still apply to the full frame, and the frame follows the image if you turn it
  afterwards. It is snapped to the JPEG block grid (8 or 16 px) because that is
  where a lossless crop can actually land — you get up to one block of *extra*
  picture rather than up to one block cut off the edge you framed, and the frame
  you see is the frame that gets saved. **Clear crop** removes it and keeps any
  rotation, and so does leaving crop mode — press `C`, drag, press `C`, and you are
  back where you were. While crop mode is on, dragging pans no longer
- **Holding `Shift` frames a perfect 1:1 square**, whatever the drag was doing.
  **Settings → Crop shape** picks a shape for every drag instead: 1:1, 16:9, 9:16,
  4:3, 3:4, 3:2, 2:3, or **Same as image**, which follows the picture as displayed —
  so a 3:2 photo you have turned on its side gets a 2:3 frame. The default is
  Free, and an existing frame keeps whatever shape it has

- Folder watcher: new, deleted and re-saved files are picked up immediately
  (OS notifications; a folder that refuses a watch falls back to a 2 s check)

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
- **If you have already browsed the folder in Explorer, its cells come from
  Explorer's own thumbnail cache** — 6–7 ms each against 595–709 ms to decode the
  same photographs. The app only *reads* that cache: it never asks Windows to
  make a thumbnail, so a folder Explorer has never seen simply falls back to
  decoding (after a few quick misses, so the fallback costs nothing). A cached
  cell that is too small for the cell is refused rather than stretched
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

Opening a `.zip`, `.cbz`, `.7z` or `.rar` extracts the image entries to a temp
folder and opens it as a normal gallery, so paging, zooming and the grid all work
unchanged. The temp folder is reused while the archive is unchanged and
cleaned up on exit, and on stepping out at either end. Comics are read-only:
delete/move/copy are refused rather than silently writing into the zip.

Archives are also *listed* in a folder — see [Archives](#archives) below.

### Triage

| Key | Action |
|---|---|
| `Del` | Move to the Recycle Bin (never a hard delete) |
| — | Move / copy the current image to a chosen folder (buttons; no key, see below) |
| `E` | Reveal the containing folder in your default file browser |
| `W` | Set as desktop wallpaper |
| `Ctrl+C` | Copy the image to the clipboard, cropped and turned as it appears |
| `R` | Rescan the folder / reshuffle the sort order |

Move and Copy are buttons rather than keys because they write to the filesystem.
`M` and `C` used to be bound to them; `C` is now Crop, which is a pure view
transform until you explicitly save. The rule the app now follows is that a bare
letter may only do something that cannot lose data — `Del` is the exception, and it
goes to the Recycle Bin.

### Metadata

Left panel shows camera make, model and lens, capture date, aperture,
shutter speed, ISO, focal length, metering mode, flash, colour space and
orientation. GPS-tagged images also show coordinates and altitude, with a
clickable **Open in Google Maps** link.

It is read only while that panel is on screen. Reading it means reading the whole
file (the colour profile lives in a segment that has to be found) and it happens
on the same worker that decodes the picture, so with the panels hidden — F11, or
`I` — the viewer stops asking for it entirely and gives the time back to the
image. Leaving fullscreen starts the read again, so the panel is never empty
when you can see it.

Below the file's own data, a **Histogram** section shows per-channel levels
for the picture on screen plus black- and white-clipped pixel counts with
percentages. The levels are counted on the decode worker from the preview
buffer and carried with it, so the panel draws numbers rather than pixels.

### Ratings

Each image carries three rating attributes: a star rating (0-5), a pick flag,
and a tag list. They are stored in an XMP sidecar beside the file —
`photo.jpg` keeps its rating in `photo.jpg.xmp` — so the image bytes are never
touched and the write is atomic. The left pane shows `*` buttons for the stars
and a Flag checkbox — clicking the lit star clears back to 0, since the row
has no zero star to press; `+` / `-` nudge the stars and `B` flips the flag (tags are
edited by other tools and only read here). Ratings are refused inside archives,
which are read-only. Undo (`Ctrl+Z`) deliberately does not cover ratings: undo
is per-image over turns, mirrors and crops, while a rating is a value with no
wrong answer rather than an edit to take back.

The filter box understands ratings as well as names: `stars:N` and `stars:N+`
(both mean at least N), `flagged` / `unflagged`, and `tag:NAME` (case-insensitive
substring of the tag list). Anything else stays a file-name substring match.

### Interface

- Browse Folder… / Browse File… dialogs, plus drag-and-drop of a file, folder
  or archive
- **A dropped file is copied** into `%TEMP%\image-viewer\drop\` before it is
  opened, and the copy is deleted when you open something else or quit. This is
  because a drag out of a zip is a *hand-off*: Explorer extracts the member to a
  temp file, gives the viewer that path, and deletes it a moment later. Without
  the copy the picture blanks about a second later, when the folder watcher
  notices the file it was showing no longer exists. The status bar says where the
  copy went. Nothing else is copied — a file opened from the command line or the
  dialog is used where it is.
- F11 for fullscreen (hides all chrome), `I` to toggle the panels
- Sort by name (natural order, so `IMG_2` precedes `IMG_10`), date, size or
  random
- Remembers the last image you were on, per folder
- Optional per-user file-type registration

### Graphics backend

Settings → **Graphics** picks the wgpu backend: **Automatic**, **DirectX 12**,
**OpenGL** or **Vulkan**. Changing it restarts the renderer immediately — the
window closes and reopens on the new backend, which keeps your folder, sort mode
and settings but starts with an empty preview cache. **There is no Direct3D 11
option**, because wgpu removed its DX11 backend upstream; DX12 and OpenGL are
what it offers on Windows.

Automatic probes each backend and keeps the first that works, **in a separate
process for each**. That is not ceremony: wgpu enumerates every backend in the
enabled set during adapter discovery, and on some drivers that fails by
**access-violating** — `0xC0000005`, before any window exists, with nothing on
stderr. A process that has crashed cannot catch its own way out, so "try DX12
and fall back to Vulkan" is not writable inside one process. Probing **stops at
the first backend that works** — the others exist only to be tried when it
fails — and the answer is cached, so it normally costs one child launch, and
only on the first run after you change the setting. **Probe backends now**
re-runs it on demand.

The probe drives the driver as far as an adapter and a **device**, not just
adapter enumeration — enumeration only walks the loader's list, and a probe that
stopped there reported Vulkan as working on a machine where the app crashes on
it. It does not create a window, so a backend that fails only at surface
creation would still pass.

The order is **OpenGL, then DirectX 12, then Vulkan** — measured on the
development machine (Intel UHD 620, driver 30.0.100.9865), not assumed. Adapter
and device creation costs 186 ms on GL against 455 ms on DX12, and an empty
eframe window (no viewer code at all) holds 84 MB on GL against 321 MB on DX12,
so GL is the same speed for 154 MB less. DX12 is second because it goes through
a different driver stack, and so is the one most likely to work where GL does
not. Automatic probing stops at the first success, so on a machine where GL
works you see one line:

```
Probing for a working graphics backend...
       gl: works
```

and where it does not, the rest of the order is tried and reported in turn:

```
Probing for a working graphics backend...
       gl: unavailable
     dx12: works
   vulkan: unavailable
```

### Saving as (`Ctrl+Shift+A`)
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo the last turn, mirror or crop. **Per image** - undo means undo on the picture you are looking at, not walk back through earlier ones. The Undo/Redo entries in the bottom bar's Transform menu disable themselves when there is nothing to undo |

`Ctrl+S` always picks the name and the size. `Ctrl+Shift+A` lets you choose
them: a **file** path, a **format** (`.jpg`, `.png`, `.webp`, `.bmp`, `.tiff`),
a **JPEG quality** and a **maximum long side**. It opens pre-filled with exactly
what `Ctrl+S` would have written — the same `_rotated` / `_cropped` name, quality
85, no resize — so opening the panel and pressing Save produces the same file
`Ctrl+S` would have.

- **The format follows the destination, not the source.** A PNG saved to
  `out.jpg` is written as JPEG, because the alternative is a file whose
  extension lies about its contents.
- **Quality is JPEG's alone.** `image` has one lossy encoder; PNG, BMP and TIFF
  are written losslessly and the panel says so rather than offering a slider that
  does nothing. WebP is also lossless here for the same reason.
- **A maximum long side never upscales.** A 256px image asked for 1000 stays
  256px; there is no point inventing pixels nobody had. Blank means original size.
- **Same refusals as `Ctrl+S`**: animations and pages of a multi-page document.
- Saving over an existing file is not prevented — you named the destination, and
  an existing file is not overwritten silently by a name you did not choose.

### Saving a transform or crop (`Ctrl+S`)

Turning, mirroring and cropping are view transforms by default, so nothing you do
to an image touches the disk. `Ctrl+S` is the one action that writes, and it
writes a **new file beside the original** — `photo.jpg` becomes `photo_rotated.jpg`
(or `photo_cropped.jpg` if you cropped), with a `2`, `3`… if that exists. The
original is never modified, so the action is reversible and saving twice
accumulates instead of compounding.

The saved file is the picture exactly as you see it: the camera's EXIF
orientation, your turn and mirrors, and your crop, combined, baked into the
pixels, with the orientation tag reset so nothing applies it a second time. The
turn and the mirrors are composed into a single transform on the way in, so there
is no case where the save has to work out "a turn and a flip" at the last moment.

`Ctrl+C` gives you the same picture, on the clipboard, and it is the same edit
rather than a lookalike: the rectangle you framed, cut out of the pixels already
decoded, and turned. Nothing is re-compressed on the way — the clipboard takes
pixels, so there is no encode step and no second generation of JPEG artefacts.
Paste it into anything and it is the crop you drew, at full resolution.

For JPEG this is genuinely lossless. Rather than decode and re-encode, the
transform is applied to the **DCT coefficients** — the compressed data itself is
moved, not recompressed. Measured on generated images at quality 90, and
reported per transform because a mirror is not obviously cheaper than a turn and
that is worth seeing rather than assuming:

| image | transform | lossless | decode + re-encode | size in → out |
|---|---|---|---|---|
| 1920×1080 | rotated 90° CW | 43 ms | 84 ms | 1.02× |
| 1920×1080 | mirrored left-right | 33 ms | 84 ms | 1.00× |
| 4000×3000 | rotated 90° CW | 212 ms | 466 ms | 1.00× |
| 4000×3000 | mirrored left-right | 166 ms | 466 ms | 1.00× |
| 6000×4000 | rotated 90° CW | 456 ms | 1041 ms | 1.00× |
| 6000×4000 | mirrored left-right | 362 ms | 1041 ms | 1.00× |
| 6000×4000 | transversed | 546 ms | 1041 ms | 1.00× |

All seven transforms are in the same cost class, and about 2.4× faster than the
obvious alternative, because it never fully decompresses.

The size column is the point: a re-encode makes a fresh rate-distortion decision
per block and comes out a different size, while a coefficient transform lands
within a percent.

**A crop and a turn are two passes, not one**, and that is forced rather than
chosen. TurboJPEG reads a crop region in the transform's *destination* space — the
rectangle as it lands after the turn, not before it — and refuses the four
axis-swapping operations outright. So the crop is taken first, with no rotation,
and the turn is applied to the cropped result. Both passes rearrange
coefficients and neither decompresses to pixels, so it is still one generation:

| image | crop | crop + turn | turn only | size ratio |
|---|---|---|---|---|
| 1920×1080 | 31 ms | 39 ms | 54 ms | 1.01× |
| 4000×3000 | 103 ms | 170 ms | 323 ms | 0.99× |
| 6000×4000 | 195 ms | 318 ms | 586 ms | 1.00× |

The ratio is crop-plus-turn output over crop-only output: ~1.00 means the second
pass moved coefficients and left the entropy-coded data alone. The 0.99× is a
second Huffman rewrite, not a re-encode.

PNG, BMP and TIFF are also saved losslessly, by decoding and re-encoding into
their own containers — those formats have no lossy step, so nothing is lost.
**WebP, GIF, HEIC and JXL are refused** with a message rather than re-encoded:
that would be a second generation of compression, and a button that says "save"
should not quietly do that.



| Key | Action (shipped default — every row rebindable in Settings → Keys) |
|---|---|
| `←` `→` | Previous / next image, or previous / next **page** of a document (also `PgUp` / `PgDn`) — in **manga mode**, previous / next **turn**, and right-to-left flips the arrows |
| `↑` `↓` | In the grid: move the selection by a row |
| `Enter` | In the grid: open the selected image |
| `Home` / `End` | First / last image (in the grid: first / last cell) |
| `G` | Toggle the gallery grid |
| `F` | Toggle fit-view mode (every image fitted; mutually exclusive with `T`) — also `Space`, which is JPEGView's own Space binding |
| `Shift+F` | Fill window (crop overflow, single image only) |
| `1` | Zoom preset 1 (100% by default; each of `1`–`4` configurable in Settings) |
| `1` | Zoom preset 1 (100% by default; each of `1`–`4` configurable in Settings) |
| `2`-`4` | Zoom presets 2-4 (2x, 4x, 8x by default; one-shot jumps) |
| `T` | Fit window to image as a mode: always 100%, window hugs (clamped to monitor). On fits now, arrivals refit, zooms re-hug |
| `,` / `.` | Step the animation one frame back / forward (pauses it, wraps) |
| `K` | Pause / resume the animation |
| `Space` | Toggle 100% (actual size) ↔ fit, like `F` (JPEGView's Space key) — in the grid: move the selection right |
| `I` | Toggle the side and bottom panels |
| `L` / `P` | Toggle the left (EXIF) / right (settings) pane alone |
| `/` | Focus the filter box |
| `F11` | Fullscreen |
| `Esc` | Clear the filter (if typing) → close the grid → leave fullscreen → close the window (the last step-offable in Settings; `Esc` itself is not rebindable) |
| `C` | Toggle crop mode |
| `+` / `-` | Rating up / down one star (0-5, XMP sidecar) |
| `B` | Flag / unflag the current image (XMP sidecar) |
| `Del` | Move to the Recycle Bin (never a hard delete) |
| `E` `W` `R` | Explore folder, wallpaper, reload folder (`Shift+W`: wallpaper as displayed, crop and turn baked in) |
| `M` | Toggle **double page mode** — show images as turns of one or two pages |
| `D` | Toggle **right-to-left** pages (only while double page mode is on) |
| `O` / `Shift+O` | One fewer / one more page opening alone at the front of the gallery |
| `X` | Toggle the current page between single and double (takes a whole turn to itself) |
| `Ctrl+C` | Copy the image to the clipboard, cropped and turned as it appears (refused on page 2+ of a document) |
| `Ctrl+Shift+C` | Copy the current file's path as text |
| `Ctrl+S` | Save a new file beside the current one, crop and transform baked in losslessly (refused on page 2+ of a document). Named for what it is: `*_rotated`, `*_cropped`, or `*_copy` when nothing changed |
| `Ctrl+Z` / `Ctrl+Shift+Z` | Undo / redo the last turn, mirror or crop. **Per image** - undo means undo on the picture you are looking at, not walk back through earlier ones. The Undo/Redo entries in the bottom bar's Transform menu disable themselves when there is nothing to undo |
| `Ctrl+Shift+A` | Save as — a panel to pick the file, format, JPEG quality and a maximum long side. Opens pre-filled with exactly what `Ctrl+S` would write, so pressing Save without touching anything gives the same file |

| `Ctrl+Shift+S` | Same edit at display size (`*_screen.jpg`; JPEG is re-encoded, PNG/BMP/TIFF stay lossless) |
| `Ctrl+1`..`Ctrl+9` | Run a user command against the current file (Settings → User commands; `{file}` `{dir}` `{name}` expand) |
| `Ctrl+P` | Print the file on screen through its registered handler (crops/turns not applied) |
| Touch buttons | Set the file date to now, its EXIF date, or the whole visible folder to EXIF dates |
| Copy original | The stored picture EXIF-oriented, no crop or turn (Ctrl+C copies framed) |
| Portable mode | Empty `portable.txt` beside the exe keeps config, thumbs, spreads and wallpaper there |
| Arrow keys / `PgDn` / `PgUp` | Navigate (all rebindable like every other key) |
| Mouse wheel | Zoom — or flip images, if that setting is on (`Ctrl`+wheel always zooms) |
| Mouse side buttons | Same as `←` / `→` (still hardcoded — mouse is not rebindable yet) |
| Drag & drop | Open a file, folder or archive. A dropped **file** is copied to `%TEMP%` first, so it survives the source disappearing |

### User commands (beta)

> **Beta.** `Ctrl+1`..`Ctrl+9` runs an external program against the picture on
> screen, configured in Settings → User commands. `mspaint.exe` with args
> `{file}` opens the current picture in Paint. Empty args passes the file;
> no quotes needed (there is no shell — quotes would arrive as literal `"`
> characters). Spawns are detached, never waited on, and rate-limited to one
> a second; chords fire on physical presses only, so holding one cannot
> spawn a storm. One thing is open, written up in `TODO.md` §7:
>
> - **A hard freeze opening Paint was reported twice and is parked.**
>   Spawning itself is exonerated (a `cmd.exe` command runs exactly once, and
>   the same file double-clicked in Explorer opens fine), and every freeze
>   came from a build predating the repeat and cooldown guards — but it was
>   never confirmed fixed on a current build, so the feature stays beta
>   until it is.

### Manga mode (beta)

> **Beta.** The pairing, the layout, the keys and full-resolution tiles on both
> halves have been run and checked on real volumes. Two things are open, both
> written up in `TODO.md` with their measurements:
>
> - **1440p-class images can still show moire.** Turning it down is a straight
>   trade against sharpness, and no single setting is right for both photographs
>   and grey screentone — a wider low-pass that helps one makes the other worse,
>   because it moves energy between dot pitches rather than simply removing it.
>   The setting is in the panel (`Smooth`); `Fast` is the default. `TODO.md` has
>   the numbers.
> - **Fullscreen (`F11`) has one unreproduced report** of moire appearing on
>   every page and clearing when the mode is toggled. It may be the same
>   threshold effect as the line above; `TODO.md` records what has been ruled out.

`M` shows images as **turns** — one page, or two meeting at a seam — laid out the
way a book is: a scan that is itself two printed pages takes a whole turn to itself,
and everything after it is re-paired. `D` swaps which side a turn opens on and which
way the arrows go; **it is on by default**, because someone who has asked for a manga
reader has asked for the one thing that makes it one. Both are in the settings panel
too, under the same names.

The **seam is the middle of the picture** and there is **no gutter** between the two
pages, so facing pages touch and a drawing that crosses the fold is unbroken.

A lone page depends on what it is. A lone **portrait** page - a cover, which
genuinely is one page wide - is not centred: it hangs off the seam on its own
side, at no more than half the window's width. A lone **spread** is the other
case, because it is not a page but the whole spread: it takes the full width of
the window and is centred across the seam, in both reading directions.

A lone **portrait** page and a page **whose partner has not arrived yet look the
same**: one slot is laid out in half the window, and the far half is empty either
way. That is deliberate - a half-width page in an empty half is the cover, and
centring it instead would stop it sharing the seam every other turn's pages meet
on - but it means a blank half is not by itself a sign of a problem. Wait a
moment: the partner is in the decode queue and will appear on its own. If it does
not, the pairing is what changed, and the notice line will say so. A lone
**spread** is not in this class at all: it fills the width, so it never leaves a
half empty.

**Every navigation key moves as one unit.** The arrows, `PgUp` / `PgDn` and
both mouse side buttons are all resolved through one rule, so under right-to-left a
rightward press is *backwards* and nothing can disagree with anything else. The wheel
is the one exception and always moves forward: it has no side to preserve.

`O` and `Shift+O` change how many pages open alone at the front — the cover, and
usually one page of front matter. `X` overrides what the app thinks the current page
is, which is the escape hatch for a scan whose proportions lie about it; press it
again to undo. Both are stored per folder in
`%LOCALAPPDATA%\image-viewer\spreads.json` — a file this app owns, holding nothing
but pairing decisions. **Deleting it clears every override** and goes back to the
detected shapes.

In manga mode `Home` and `End` go to the first and last **turn** rather than the first
and last file, which matters for a folder of documents. The grid (`G`) is unchanged —
one cell per file — and opening a cell lands on the turn at that file's first page.

Both pages of a turn share **one scale**, so zooming and panning apply to the turn and
the halves cannot be zoomed apart. A page you have turned keeps its own rotation
inside the turn, and full-resolution tiles are cut for each page separately, so a
zoomed spread is sharp on both sides.

#### Three things this cannot do, so you do not have to find out

- **A single landscape page and a scan of two printed pages are geometrically
  identical.** Both are about 1.414 wide, so no aspect-ratio detector can tell them
  apart — every reader in this field has the same problem. What bounds it here is the
  tolerance check: a landscape page will not pair with a portrait one, so it takes a
  turn of its own instead of being squashed into a half. `X` ends it: press it on the
  page and it shares.
- **A folder of 3:2 photographs in manga mode is one photo per turn.** 3:2 is 1.5,
  above the 1.2 threshold, so every photograph is read as a two-page scan. Manga mode
  is for pages; leave it off for photographs.
- **The first visit to a cold book can shift the pairing once**, as each page's
  dimensions arrive, and it corrects itself on the second visit. This cannot be
  avoided: deciding whether a page pairs means knowing how wide it is, and that answer
  comes from reading the file. A spread that settles once, at the front of a book
  nobody has read yet, is the cold-start cost of the feature rather than a defect.

**In the grid, the navigation keys drive a selection instead.** `←` `→` `↑` `↓`
move by cell and by row, `PgUp` / `PgDn` move a viewport at a time, `Home` /
`End` jump to the ends, and `Enter` opens what is highlighted. Walking the grid
does not change the image on screen, so arrowing through a large folder costs no
re-decodes, and the selection stops at the ends rather than wrapping.

**Move and Copy have no keyboard shortcut**, deliberately. Both write to the
filesystem, and a bare letter is one stray keystroke away from doing that. `C` used
to be Copy and is now Crop, which touches nothing on disk until `Ctrl+S` — the
single-key budget goes to actions that cannot lose data. `M` was Move and is now
manga mode, which is the same kind of trade: a letter worth having for something that
cannot lose a picture. `Del` keeps its key because it is the one people reach for
while culling, and it goes to the Recycle Bin rather than deleting. If Move and Copy
are wanted back on the keyboard, a modifier is the shape to add rather than a bare
letter.

**A UTF-8 BOM in `config.json` is ignored** rather than read as an empty file. Some
editors and PowerShell's `Set-Content -Encoding utf8` write one, `serde_json` rejects
it, and the alternative is that every setting silently reverts to its default with
no error. The same applies to `spreads.json`, where it would be worse: the store is a
read-modify-write, so a BOM that made it unreadable would turn the next `O` or `X`
into a delete of every other folder's pairing decisions.

---

## Settings

Stored as JSON in `%APPDATA%\image-viewer\config.json`:

| Setting | Default | Meaning |
|---|---|---|
| Load all images in folder | on | Build the queue from the whole folder on startup |
| Linear-light downsampling | on | Resample in linear light instead of gamma space — noticeably cleaner on line art |
| Downscale filter | Hermite | Hermite / Catrom / Lanczos3, applied live |
| Smooth | off | Low-passes a downscaled page harder than the minimum, which hides a dot grid on some pages and costs edge sharpness. **Off by default** — it depends on the page. See the note below |
| Pixel art | off | Magnified drawing samples nearest, so a sprite zoomed past 100% stays squares of its own pixels instead of smearing. Zoomed out, the averaging filter still runs (fit a photo and it is unchanged), so the minification never drops pixel rows. Applies to the picture, its zoom tiles and animation frames — grid cells stay as they are. Flipping it re-requests the previews |
| Convert Display P3 to sRGB | off | See the wide-gamut note below. Off by default because it changes how photos look |
| Cache grid thumbnails on disk | on | Keep decoded grid cells in `%LOCALAPPDATA%\image-viewer\thumbs`, capped at 256 MB. See the note below |
| Grid cell size | 150 pt | Side of one gallery-grid thumbnail (64–400 pt). Applies live, no re-decode |
| Wait for image when holding arrows | off | Hold-to-browse that never outruns the decoder, so no image is skipped |
| Mouse wheel flips images | off | Wheel pages instead of zooming (`Ctrl`+wheel still zooms) |
| Loop back at folder ends | on | Wrapping past the last image returns to the first. On by default so older configs keep their existing behaviour |
| Manga mode | off | Show images as turns of one or two pages rather than one at a time. `M`. See the note below |
| Right-to-left pages | **on** | A turn opens on the right, so `←` reads forwards. `D`. On by default because it is only read while manga mode is |
| Sort mode | Name | Name (natural order) / Date / Size / Random |
| Descending sort | off | Reverse the sort order (Z→A, oldest/smallest first) |
| Slideshow | off, every 3 s | Start/stop button plus pace (1-60 s) and a Shuffle checkbox. Advances until toggled off, or stops at the end with looping off. Shuffle reshuffles every show and restores the folder order on stop |
| Animation speed | 1× (file timing) | GIF/WebP/APNG playback multiplier, 0.1–8×, live |
| Esc closes the app | on | Esc in crop mode drops the frame first; with this off Esc never quits |
| Worker threads | 0 (auto) | Decode pool size. 0 picks 2-4 from your CPU; 1-8 overrides. Applies on restart |
| Zoom keys 1-4 | 1× toggles 1:1 mode, 2×-8× one-shot jumps (all configurable) |
| Fit view | off | Sticky fit-to-window (`F`); mutually exclusive with `T`. Photos open at 100% with everything off |
| Remember zoom and pan per image | off | An image you zoomed or panned comes back the way you left it while you browse; others keep the zoom carried over. Per page of a document, and the memory is session-only — it is gone when the app closes |
| Show panels | on | Side panels (`I` to hide/show, `L`/`P` per pane). **Remembered between launches**: hide once with `I` and every launch opens chrome-free, JPEGView's minimal state. A fullscreen detour does not touch it — leaving fullscreen restores what you had |
| Allow only one window | on | A second launch exits instead of opening another window. Applies on restart |
| Show archives | on | Lists `.zip`/`.cbz`/`.7z`/`.rar` in a folder beside the images. Off hides them from every scan and the grid |
| Include subfolders | off | One gallery over the whole tree (sorts apply globally); the watcher covers it too |

## Archives

`.zip`, `.cbz`, `.7z` and `.rar` files are **listed in a folder** alongside images,
in every sort mode and in the grid, with no setting to turn on. Arrow onto one and
it *opens* rather than decodes, and **both** ends of an archive step back out into
the folder:

- `←` at the archive's first page returns to the folder, cursor on the archive.
- `→` at its last page *continues* — on to the next entry in the folder, which is
  usually the next archive and so opens it. A shelf of volumes is one press per
  volume rather than an unreachable list. With looping off, an archive that is the
  folder's last entry stays put rather than dropping you in with nowhere to land.

Both ends are deliberate. `→` at an archive's last page would otherwise be the
ordinary end-of-list wrap, which inside an archive wraps to that archive's *own*
first page — so you cycle through one volume forever and never reach the second.

- The status line reads `vol1.cbz - 42 images, press right to open`, and the grid
  cell shows the member count. Both come from a header read, not a decode.
- `Del` refuses on an archive. So does move/copy.

To pick and choose without turning them off, the filter box matches on file name:
`cbz` shows only the `.cbz` files, `7z` only the `.7z`, `zip` matches `.zip` and
`.cbz` together. "Show archives" in Settings makes that permanent.
- Members are flattened: nested folders do not become folders, non-images are
  skipped, and two members that would flatten to the same file name are counted
  once.
- **7z and zip read fully. RAR5 reads only uncompressed members** — the Rust RAR
  reader cannot decompress, so compressed pages are counted as unreadable and
  skipped rather than shown as broken images. RAR4 is not supported and says so.
  Archives are extracted to `%TEMP%\image-viewer`, released on leaving and on exit.
| Always on top | off | Keep the window above all others (applies live, kept on restart) |
| Hide title bar | off | No title bar or borders; the window's picture stays in place through the toggle and only the chrome arrives or leaves (move with Win+arrows; applies live, kept on restart) |
| Keys | defaults | Click any chord, press replacement keys. Bare letters stay bare, chords stay exact, navigation keeps its direction rule, taken chords show red `also:` markers instead of refusing. `Esc` and mouse buttons are not rebindable |
| User commands | none | External programs run with Ctrl+1..9 against the current file. `{file}` `{dir}` `{name}` expand in program and args (empty args just passes `{file}`); spawned detached, never waited on, rate-limited to one a second |

The downscale filter, **Smooth** and sort mode re-decode immediately; the
linear-light toggle applies to newly decoded images (see the known caveat below).

**Smooth is off by default because it depends on the page, not because it does
not work.** It does work, and on some pages it works well — but a wider low-pass
does not simply remove detail, it *moves* it. On a grey tone carrying a dot
grid, widening kills the grid's own beat and at the same time strengthens a
longer-period beat that was nearly invisible, so one grid can disappear and
another appear. Measured on six 2223x3109 pages at the manga half-panel target:
better on two, unchanged on one, and worse on three, where the report was a black
dot pattern in grey screentone that was not there before.

What you get on `main` is already better than before on every page, and that
comes from the linear-light kernel prefilter rather than from this toggle. Turn
`Smooth` on for a page whose grid is a single even dot pattern and it will clean
up noticeably; leave it off if your pages are screened at several angles or you
want the crispest line art. Edges are noticeably softer with it on — about 1.7x
wider on a hard edge.

### Moiré on 1440p photos, and what this build does about it

Screened or finely-halftoned content can show a moiré lattice after being
downscaled, because detail finer than the output can represent has to be removed
before it lands, and a resampler that removes too little folds it into a pattern
of its own. Two things in this build handle it.

The kernel never runs narrower than the pipeline's own minimum
(`MIN_FILTER_SCALE`, not a setting), which is what a downscaled photo needs at
any size. Before it existed the low-pass width **jumped** at one point in the
pipeline: above it, a two-stage resize landed on a wide enough kernel; below it,
a single pass narrowed to whatever the reduction was — **1.333x** for a
2560x1440 photo on a 1920-long panel. Every 1440p photo was below that point, so
every 1440p photo was under-filtered. Measured on a 100-code test pattern at
that size, the over-filter band (detail too fine to represent) drops from **51 to
19 codes** and **41 to 11**, while detail the output *can* represent barely moves
(58 → 37 at four source pixels, 99 → 97 at twenty-four).

Images are also resized for the rectangle they are drawn in, so a photo is
resampled once, at the size it will be displayed.

**What is still there.** This reduces the lattice substantially; it does not
remove it. Roughly a third of the original amplitude remains, and the remaining
part is content sitting exactly at the output's limit, which no resampler can keep
and not alias at the same time. `Smooth` cuts it further and costs edge sharpness,
so it remains a per-image choice rather than a default.

**And one case this cannot fix.** When the window is wider than the photo's own
aspect ratio, the photo is drawn smaller than the texture prepared for it, and the
**graphics card** does that last bit of shrinking — by a small amount on an
ordinary window and by the whole factor on an ultrawide one. That step cannot be
low-passed here, because the renderer draws with a linear sampler and no mipmaps.
If a photo moirés badly *only* when the window is very wide, that is what you are
looking at; making the window narrower or narrower-aspect will clear it.

### Why the grid thumbnail cache exists

A grid cell is a 150-point picture of the whole image, and for most files that
means decoding the whole file: a 95 MP scan takes **700-900 ms** here, and there
is no cheaper way to get the whole picture out of a JPEG — every pixel needs every
DCT coefficient, so the decode is the floor for any conformant decoder. DCT scaling
to 1/8 saves memory, not time.

What *is* cheap is decoding it a second time, so cells are cached on disk. The
first visit to a folder is as slow as it always was; every visit after it is a
small file read, about **1 ms** against 67 ms for the same cell without a cache.

Each entry is checked against the file's modification time and length on every
read, so a file edited on disk is re-decoded even if nothing remembered to
invalidate it, and entries are trimmed oldest-first once the cache reaches its cap.
Turning the setting off stops it being written at all; nothing else changes.

There is a second cache in the picture, one this app does not own: Windows'
own thumbnail cache, which Explorer fills as you browse. A cell may come from
there, and it is only ever *read* — the app never asks Windows to make a
thumbnail, so asking can never make the cold case slower. Cells from that cache
are never written into this app's own cache, because an entry there has to be the
decode it claims to be.

---

## Command line

```
image-viewer.exe [file | folder | archive.zip | archive.cbz | archive.7z | archive.rar]
image-viewer.exe --register      # add this build to the "Open with" list
image-viewer.exe --unregister
image-viewer.exe --icc-scan <folder>   # report the colour space of every image
```

`--icc-scan` prints a tally of what the colour detector found in a folder and
lists the files that need a look, then exits:

```
Colour audit of C:\photos  (1001 images)

  wide gamut              200   displayed without conversion - reads flat
  unknown profile           0   profile could not be read
  non-RGB                   0   not an RGB space
  sRGB                    111
  sRGB (untagged)         641
  not inspected            49   colour lives where this build does not look

Files that need a look (200):
  IMG_0001.heic
  ...
```

It exists because the detector is otherwise only reachable one file at a time
from the metadata panel, and auditing a library meant clicking through it. It
reads every file whole, so it takes a few seconds on a folder of a thousand
images.

`--register` writes a per-user (HKCU) ProgID and `OpenWithProgids` entries for
the nine extensions the app decodes (jpg, jpeg, png, webp, gif, jxl, avif,
heic, heif). It deliberately does **not** claim the default-handler role —
Windows requires the user to confirm that themselves.

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
| `PERF=1` | Turn on the latency report without passing `--perf` a folder |
| `PERF_PHASE=1` | Per-phase timing inside the linear-light resampler (development) |

The renderer is **wgpu**, and which backend it uses is a setting (below). The
backend is printed at startup.

---

## How it stays fast

- **One decode per image.** The file is read and decoded once; the on-screen
  preview is derived from that decode instead of re-reading the file, and the
  full-resolution pixels ride along in the cache for free.
- **Large JPEGs get a DCT-scaled preview** (turbojpeg scales in the DCT
  domain), so opening a 40 MP photo doesn't mean decoding 40 MP twice. The
  full decode starts only after you have actually looked at it for 120 ms.
- **Big images are resampled in two steps.** A first pass reduces the source to at
  most twice the display size, and only that reduced buffer is converted to linear
  light and filtered — measured at **790 ms → 219 ms** for a 40 MP source. That
  first pass filters in linear light with the same kernel as the second, because a
  plain average aliases *more* than the filter it feeds: at the 2.9× reduction of
  a scanned page a box average left 21–29 codes of a near-Nyquist tone as 1–2 px
  sparkle, where the kernel leaves 2–11.
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
  (≈256 MB), thumbnails LRU-capped at 512, animations at 240 frames /
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
cargo test                 # every suite
cargo test --no-fail-fast  # ... without stopping at the first failing target
cargo test --test unit     # one suite; cargo test <name> for one test

cargo test --release --test bench_browse -- --ignored --nocapture   # browsing benchmark
cargo test --release --test bench_resize -- --ignored --nocapture    # decodes per window drag
cargo test --release --test bench_rotate_save -- --ignored --nocapture  # lossless save, size ratio
cargo test --release --test bench_startup -- --ignored --nocapture   # start-up per candidate
cargo test --release --test pathological -- --ignored --nocapture   # 40/60 MP timings
```

Requires the MSVC toolchain and CMake (turbojpeg builds libjpeg-turbo from
source).

The benchmarks are `#[ignore]`d `cargo test` targets, not `cargo bench`:
`tests/bench.rs` and `tests/bench_variants.rs` do use criterion, but they sit
in `tests/` as ordinary test targets with the default harness, so `cargo bench`
reports "running 0 tests" and prints nothing. Until they are moved to `benches/`
with `harness = false`, the four commands above are the benchmarks that produce
numbers.

Every suite is **fixture-free** — each builds the images it needs in a temp
directory — so a fresh clone passes with no setup at all. `test-images/` is
gitignored, and the only things that read it are the two criterion targets, which
want a real large photo and print a message rather than failing, and
`tests/unit.rs::test_gps_decode`, which skips if `geo.jpg` is absent.

`cargo test` stops at the first failing target, so pass `--no-fail-fast` when
you want the whole picture. The `pathological` suite generates its own 40/60 MP
images and is `#[ignore]`d because it takes seconds.

---

## Known caveats

- **Multi-page TIFF: three deliberate limits.** Pages work — arrow keys page
  through a document and the title bar and status line show which one — and CMYK
  pages convert correctly. But: pages in **YCbCr, Lab, palette, CMYK+alpha or
  multiband** are **refused with an error** rather than converted approximately,
  because a wrong page colour is silent; **`Ctrl+S` is refused** on any page but
  the first, since "save what I see" is a statement about a file and the lossless
  path rewrites the whole document; and **the grid shows one cell per file**
  (page 1), because a document is one thing to browse.
- **ICO needs an RGBA PNG entry or a BMP entry.** Windows writes the large sizes
  as RGBA PNGs and the small ones as BMP, both of which decode. A PNG-compressed
  entry that is *not* RGBA is rejected, which is a limitation of the underlying
  decoder rather than a choice here.
- **Wide-gamut photos are not converted by default.** The colour space is
  *detected* in all cases and shown in the metadata panel, but the pipeline is
  8-bit sRGB end to end, so a Display P3 photo is displayed without conversion and
  reads duller than it should. **Convert Display P3 photos to sRGB** in Settings
  turns the conversion on; it is off by default because it is a visible change to
  your pictures, it is a *downgrade* on a wide-gamut display, and colours outside
  sRGB are clipped to fit, which is lossy. Detection covers JPEG, PNG and — through
  the container's `colr` box — HEIC and AVIF; what is **not** read is a HEIF that
  states its colour with `nclx` parameters instead of a profile, and JXL, whose
  colour lives inside the codestream. Both report "not inspected" rather than
  guessing. `--icc-scan <folder>` reports the tally for a whole library.
  `TODO.md` records what is and isn't verified about the conversion.
- Toggling **linear-light downsampling** does not re-decode images that are
  already in the preview cache — the filter dropdown does. Restart or move to
  another image for the change to show everywhere.
- The **preview cache keeps a few images either side of the current one**, so
  jumping far away discards the old previews; scrolling back re-decodes.
- **Turning, mirroring and cropping do not touch the file.** Nothing is written
  until **Save transformed…** / `Ctrl+S`, and that writes a new file rather than
  modifying the original. Transforms are per image and last for the session only;
  they are not persisted across restarts.
- **The clipboard carries the edit; the wallpaper does not.** `Ctrl+C` puts the
  picture *as you framed it* on the clipboard — cropped, turned and mirrored —
  because that is the picture you were looking at and pasting it somewhere else
  should paste that one. **Set wallpaper** still copies the original file, so the
  desktop shows the photograph as the camera stored it.
- **A rotated JPEG can lose up to one 16 px block off an edge.** Lossless
  transforms move whole 8×8 DCT blocks, so a width or height that is not a
  multiple of the MCU block size leaves partial blocks that cannot reach the new
  edge. Rather than leave a visible strip, the save discards them. This is a
  no-op when the dimensions already align, which is the common case.
- **Comics (`.cbz`/`.zip`) are read-only** — triage actions are refused, and
  wallpaper support copies the member out to `%APPDATA%` first.
- Preview texture size follows the window: resizing larger re-decodes so the
  image stays sharp, which briefly costs some CPU.