---
name: adding-a-decoder
description: Use when adding support for an image format, changing a decoder, or fixing a file that decodes to the wrong pixels or wrong dimensions - PNG, JPEG, WebP, GIF, TIFF, BMP, ICO, QOI, HEIC/AVIF, JXL. Also use when a decode panics on a worker, when an animation hangs or freezes on frame 0, or when writing a fixture for a format.
---

# Adding a decoder

The order matters: each step exists because the one before it was got wrong.

## 1. Sniff by magic bytes

`sniff_format` maps leading bytes to a format; the extension is never consulted,
so a PNG saved as `.jpg` must load. `"RIFF"` is not enough for WebP — WAV and AVI
start the same way, so check the form type at offset 8. Read
`IMAGE_CRATE_DECODERS_WE_SKIP` first; its sharpest entry is Targa, which has no
signature.

## 2. Guard every exit with `normalized()`

A buffer whose dims disagree with its declared dims **aborts a worker**. It
happens when a decoder mis-reports its size, and when `DynamicImage::resize`
returns *smaller* than asked because it preserves aspect ratio — 980x552 asked
for 300x168 returns 298x168. Report what the library produced.

## 3. Never `into_frames().count()`

The count's only job is "is this more than one?". `count()` decoded the whole
animation, and a truncated GIF yields `Err` forever, so the worker never answered
— no error, no panic, and the image you asked for never appears.

GIF: **two** `next()` calls, no loop. PNG/WebP: read a header. `image` 0.25
implements `AnimationDecoder` for `GifDecoder`, `ApngDecoder` *and*
`WebPDecoder`; using one and not the others froze APNG and animated WebP on frame
0. Ask the count *before* decoding.

## 4. Orientation is JPEG-only

`heic` already applies HEIF container transforms (`irot`/`imir`); the EXIF tag on
top double-rotates. **Never guess an embedded thumbnail's rotation** —
`embedded_thumb_rotation` returns `None` when ambiguous, and a wrong guess shows
sideways photos, which is worse than being slow.

`find_embedded_thumbnail` parses untrusted input: bounds-check every offset and
length, cap the IFD entry count, require `FFD8`/`FFD9`, return `None` rather than
panic. IFD1 is reached via IFD0's *next-IFD pointer* — the header's IFD0 offset
looks like it works and finds nothing.

## 5. Wrappers wrap, they do not step inside

`load_page_preview_full` has five return points, so anything applying to every
decode — the wide-gamut conversion — is a **wrapper around `decode_page`**, because
a conversion at each return point is one of them forgetting.

`load_image`'s `max_dim` is a **minimum long-side target, not a hard cap**:
`load_image(2400x1600, 1600)` returns 2400x1600. Assert the real contract.

## 6. Fixtures

Build them in a `tempfile::tempdir()`. `tests/fixtures/` holds a file **only when
nothing in the dependency set can write that format** — if a crate can, it must,
because `test-images/` is gitignored.

## Red flags

A test reading `test-images/`. Frame counting via `count()`. An extension
consulted in the decode path. A new `image` decoder without checking
`IMAGE_CRATE_DECODERS_WE_SKIP`. Colour from a `desc` string — 170 of 200 real
profiles have none; classify by primaries.

Per-format lookup: `references/formats.md`.