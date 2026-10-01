# TODO

**Open work only.** Anything finished is in the git history, and the durable
lessons from it are in `AGENTS.md` — neither belongs in this file. Each item
states what is *known* (verified), what is *undecided* (needs a human) and what
is *unverified* (an honest gap), so the investigation does not have to be
repeated. `PLAN.md`, the early-project feature list, was deleted once its work
landed.

---

## 1. Display P3 → sRGB conversion — blocked on 2 decisions

### Undecided — needs you

1. **Default on or off?** Argument for on: 20% of a real library is P3 and
   displays wrong today; converting is strictly better on an sRGB display.
   Argument for off: it is a visible change to how your photos look, and you
   raised (correctly) that it is a downgrade on a wide-gamut display.
2. **Ground truth for "the colours are now right"?** The matrix running and the
   output staying in gamut are testable. Correct *appearance* is not, without a
   P3 file whose right answer is known: `test-images/color-p3-named.jpg` and
   `color-p3-anon.jpg` are the candidates — compare against JPEGView_L (in
   `test-images`), which can convert.

### Known, verified

- **200 of 1001** images in `phone_backup\Download\Ttg` were Display P3 when this
  was first measured; a re-run after HEIC support landed says **229**, the
  library having grown. Another 111 are sRGB-profiled, 641 untagged sRGB, 0
  unreadable. Re-measure with `--icc-scan` rather than trusting these.
- The primaries never mix: **192 files at exactly `(33759,15807,-69)`** (P3) and
  111 at `(28578,14581,912)` (sRGB), both D50-adapted. These are *measured* —
  do not "correct" them from memory, a guessed P3 value was wrong once already.
- **170 of the 200 P3 files have no `desc` tag at all**, so classification must
  work from primaries. That part is done (`icc_tags_valid`, `ColorSpace`).
- Display P3 is the *only* wide-gamut space in a real library. AdobeRGB, Rec.2020
  and "RGB-to-GBR" appear only in the synthetic charts in `test-images`, so a
  fixed 3×3 is enough and **lcms2 is not needed**.
- (Per-file numbers came from a throwaway probe; see git history. The detector
  itself is `image_viewer::color_info`.)

### Do it like this, so it is not written wrong

Convert primaries **in linear light**, not in gamma space — doing it in gamma
space is the classic mistake that yields dark, muddy images: decode sRGB→linear
with the sRGB curve, apply the P3→sRGB matrix, re-encode linear→sRGB.

**No transfer-function change is needed on the output side.** The destination
framebuffer is already sRGB-encoded 8-bit (egui wants a gamma-space surface), so
the sRGB curve in and out cancels; what is actually needed is only the primaries
matrix.

### Where to apply — undecided

- **In `load_image_preview_full`**, on the worker: the texture, the tiles and
  the clipboard's cached-full fast path come from this decode, so most consumers
  are correct automatically — at the cost of one LUT+matrix per pixel on the hot
  path, which is where the earlier instability came from. **Two are not**: the
  `Ctrl+C` fallback decodes with `load_image_full` on its own thread, and
  wallpaper never goes through our decode at all (`set_wallpaper` hands the OS a
  file *path*, so it needs a converted file, not a converted buffer).
- **As a separate post-pass**: zero risk to the decode path, but it has to cover
  the same five places, and the wallpaper one is not a pixel buffer.

### Safety requirement

The transform must fire **only** on a structurally-validated P3 profile.
Converting an sRGB image through a P3 matrix visibly wrecks it, which is the same
class of error as the reverse. `ColorSpace::WideGamut` alone is not sufficient
signal — confirm `red_primaries == Some(P3_D50_RED)` before touching pixels.

---

## 2. WebP and GIF colour — the blind spot is not the one that was assumed

This item was written as "HEIF that states its colour with `nclx` instead of a
profile". **Running `--icc-scan` over the real library says otherwise.** Of 1001
images in `phone_backup\Download\Ttg`:

| | |
|---|---|
| wide gamut | **229** |
| sRGB profiled | 111 |
| sRGB untagged | 641 |
| unknown profile | 0 |
| not inspected | **20** — and all 20 are **13 WebP and 7 GIF** |

**Zero HEIF.** Every HEIF/AVIF in that library carries an ICC profile, so the
`nclx` case this item was about does not occur in it at all. The `nclx` handling
is still correct to keep — `None` must not be read as sRGB — it is simply not
where the remaining gap is.

So the real remaining work is two formats, and it is smaller than this item was:

- **WebP (13 files).** A WebP carries its profile in a RIFF `ICCP` chunk, and
  nothing here parses it. This is the same shape as the JPEG `APP2` chunk
  reassembly that already exists, ~20 lines of RIFF walking. There is a fixture
  in `test-images` (`color-webp-uninspected.webp`) and 13 real files to check
  against.
- **GIF (7 files).** A GIF has no colour profile chunk at all — its colours come
  from a palette. The honest answer is `UntaggedSrgb` by the same convention
  untagged PNG and JPEG already get, not `NotInspected`. That is a one-line
  change plus a test.

Neither is on the conversion path (§1): both only make the report honest.

---

## 3. Animated WebP decode — written, never verified against a real file

The path is reached (`animated_format` → `animation_frame_count` →
`load_animation` with `WebPDecoder`) and the *detection* is tested: a still WebP
is recognised and reported as not an animation, which is the part that would
otherwise decode the file twice. The decode itself has never run against a real
animated WebP, because nothing in this dependency set can produce one. Ruled out:
`image` 0.25 writes lossless VP8L stills only; `image-webp` 0.2.4 has
`WebPEncoder::encode` but it is single-frame; there is no libwebp binding.

Hand-building one means emitting `VP8X` + `ANIM` + `ANMF` around a real VP8 or
VP8L bitstream, and VP8L is Huffman-coded — not worth writing by hand to test
three lines that call an upstream decoder.

**To close it: get one real animated WebP** (a 2 KB one is enough) and either
commit it as a fixture or add a decoder to a test-only helper. The APNG fixture
in `tests/multiframe.rs` is the model — assembled chunk by chunk because nothing
else could produce one.

---

## 4. Multi-page TIFF: the colour types still past page 0

Pages work, and **CMYK now works too** — `cmyk_via_image` re-wraps a page's
samples in a one-page TIFF and lets `image` do the conversion, because `image`
owns the maths and cannot be pointed at page 2. Verified for CMYK8 and CMYK16.

Still refused past page 0, each an error rather than an approximation: **YCbCr,
Lab, palette, CMYK+alpha, multiband.** The blocker is not the decoder — `image`
can convert YCbCr and Lab — it is that the re-wrap trick needs a `tiff`-crate
*encoder* type for the source colour type, and there isn't a usable one:

- `tiff::encoder::colortype::YCbCr8` has `Inner = u8` with `BITS_PER_SAMPLE =
  [8, 8, 8]`, which cannot carry three samples per pixel. Writing a YCbCr page
  through it is not possible as written.
- palette needs the `colormap` tag applied, and Lab is not a photometric
  interpretation the encoder offers at all.

So this item is now "find or write a way to express those types in a one-page
TIFF", not "make `image` do the conversion". Worth doing only if real scans need
it; a 24-bit CMYK scan is the common print case and it is already covered.

---

## 5. Smaller queued items

- **Grid thumbnails of large scans are the grid's real bottleneck.** Files with
  no embedded thumbnail take ~800 ms each even via the turbojpeg DCT path, which
  still decompresses every coefficient. Embedded EXIF thumbnails took everything
  else 15-40× faster; these are the residue and need a different approach.
  Related: a multi-page document's thumbnail is page 1 at 256 px through the
  full pipeline, which for a 25 MB scan dominates opening the grid.
- **The file-change watcher is unverified** (`check_changed`, `invalidate_path` —
  both in `main.rs`, so no test can reach them). Worth one manual pass: change a
  file on disk, confirm it reloads within a few seconds, *including while it is
  mid-write* — that is the case which would poison the file if `invalidate_path`
  ever stopped clearing `failed`.
- **Colour correctness has no ground truth** — blocked on decision 2 in §1.

---

## Deliberately not doing — don't re-open without a reason

- **HEIF / AVIF image sequences.** An ISO-BMFF `meta` box can hold several
  images (a burst saved as one HEIF), and `heic` 0.1.6 has no sequence support at
  all: primary image, thumbnail, depth, gain map, auxiliary images, and that is
  everything. Reading a sequence means hand-walking `meta`/`iinf`/`iref` for the
  item offsets, which is how you get a viewer showing item 1 of a 3-image burst
  with no sign that two more exist. Sequences are rare and the per-item work is
  guessable from the spec; if it is ever wanted, the walk belongs in `lib.rs`
  with tests, and "is this a sequence?" has to be asked while sniffing rather
  than after decoding the primary.
- **`Ctrl+S` on page 2+ of a document.** Refusing is the feature. The lossless
  path rewrites the whole file's coefficients and the re-encode fallback starts
  from page 1, so saving would write page 1's pixels under the document's name and
  call it a lossless save of what you were looking at. Supporting it means a new
  save mode and re-examining the "lossless" claim rather than assuming it.
- **One grid cell per file**, so a 200-page document is not a 200-cell row.