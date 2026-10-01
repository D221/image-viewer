# Test fixtures

Binary fixtures, which is the one kind of test input this repository does not
generate for itself. Everything else is built in a `tempfile::tempdir()` at run
time, because a fixture that reads from the gitignored `test-images/` passes on
one machine and cannot pass in a fresh clone.

A file is only vendored here when **nothing in the dependency set can write
it**, which makes it the same situation as the hand-assembled APNG in
`tests/multiframe.rs` — just with bytes rather than code. If a crate can produce
the format, it must, and the fixture does not go here.

## `animated.webp`

| | |
|---|---|
| Source | `webp-animation` crate, `data/animated.webp` |
| Version | 0.9.0 |
| Upstream | https://github.com/blaind/webp-animation |
| Licence | MIT OR Apache-2.0 |
| What it is | 10 frames, 400×400, 40 ms per frame (400 ms loop) |

**Why it is here:** `animated.webp` decoding is the one decode path in the app
that no amount of synthesis could reach. `image` 0.25 writes lossless VP8L
*stills* only, `image-webp` 0.2.4 has an encoder but it is single-frame, and
there is no libwebp binding in the tree. The viewer reached
`load_animation` with a `WebPDecoder` and the code was never wrong — it was
never *run*, which is a different and much more dangerous state, since an
untested path that works and an untested path that is broken look identical from
the outside.

The still case needs no file: `WebPEncoder::new_lossless` writes one, which is
what `a_still_webp_is_recognised_as_webp_but_not_as_an_animation` uses. The
distinction that test protects is real — `WebPDecoder` over a still WebP yields
one *decoded* frame that the caller throws away and then decodes a second time —
and none of it needs an animated container.