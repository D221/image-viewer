# Per-format notes

## Decoder assignment

| Format | Decoder | Animates | Notes |
|---|---|---|---|
| JPEG | `turbojpeg` | no | DCT preview then full decode above 12 MP after a 120 ms dwell; the only format EXIF orientation is applied to; lossless save rearranges DCT coefficients |
| PNG | `image` | APNG via `ApngDecoder` | `iCCP` is zlib-compressed — inflating is not optional or the parser finds no `acsp` and reports "unknown" |
| WebP | `image` | yes, via `WebPDecoder` | sniff `"RIFF"` **plus** the form type at offset 8; WAV and AVI start the same way |
| GIF | `image` | yes, via `GifDecoder` | `is_animated` = two `next()` calls; `first_frame` = one, no loop |
| TIFF | `tiff` + `image` | no | direct dependency because `image` 0.25's `TiffDecoder` implements only `ImageDecoder` and **cannot be pointed at page 2**; it also writes only single-page TIFFs, so a fixture must chain IFDs via `write_image` |
| BMP, ICO, QOI | `image` | no | |
| JXL | `jxl-oxide` | — | colour is inside the codestream, so it can be classified |
| HEIC/HEIF/AVIF | `heic` | no | container transforms `irot`/`imir` already applied — do **not** add the EXIF tag |
| Targa | — | — | **no signature at all**, so unidentifiable here. Sharpest entry in `IMAGE_CRATE_DECODERS_WE_SKIP`. |

## Colour

- **Classify by primaries, not the `desc` string** — 170 of 200 real profiles have
  no `desc` at all. The two primaries that occur are sRGB `(28578, 14581, 912)`
  and Display P3 `(33759, 15807, -69)`, both D50-adapted, and they never mix.
  Those are *measured*; a remembered P3 value was wrong once.
- `icc_tags_valid` refuses corrupt profiles outright. Both wrong answers are
  harmful: claiming sRGB skips a needed conversion, claiming wide gamut pushes a
  correct image through a P3 matrix.
- HEIC/AVIF report `NotInspected` when `colr` carries **`nclx` parameters instead
  of a profile** — which is how a phone HEIC often says Display P3.
  `heic::DecoderConfig::extract_icc` returns the raw `colr` bytes, so reading a
  profile is one call.
- Assuming sRGB in either case is how a wide-gamut photo looks flat and nobody
  notices, because everything "works".

## The resampler is the hot path for big non-JPEG sources

`linear_resize`, in order: sRGB box pre-reduction to <=2x the target (integer
running sums, rayon, precomputed reciprocals) -> 12-bit linear conversion of
*that* buffer -> two kernel passes.

- The conversion must never run over the full-resolution source.
- A reciprocal **division inside the inner loop cost more than everything else
  combined**.
- The vertical pass needs its running column accumulator or it misses cache on
  every pixel.
- **Overshoot must clamp LUT indices to 4095** — index 4097 was a worker panic.

`dynamic_to_image_data` still decodes at source size and resizes for PNG, HEIC,
JXL and TIFF, so the transient full-size buffer is allocated for those. That is
known, not fixed.

## Things this tree cannot encode

Nothing here can write an **APNG** or an **animated WebP**: `image` writes
lossless VP8L stills and has no `write_frames`; `image-webp`'s encoder is
single-frame. So both fixtures are the exception `tests/fixtures/` exists for —
the APNG is assembled chunk by chunk in `tests/multiframe.rs`, and the animated
WebP is a vendored 4.9 KB file (MIT/Apache, `webp-animation` 0.9.0) attributed in
that README.

That gap closed by finding a file rather than writing code: the animated WebP
path had never been *run*, which from the outside is indistinguishable from
broken.