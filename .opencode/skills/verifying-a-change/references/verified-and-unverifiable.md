# What Rule 1 found here

Reintroducing the bug is how you find out a test is decoration — and it finds
more than the bug. Each of the four cache guards was removed in turn. The IEND
check, the stamp check, the path check, the header-dims check and the cell
downscale all failed as expected. Two results were not.

## The `full: None` arm could not fail any test

A grid cell's decode had an arm returning `full: None`, on the reasoning that a
cell has no use for the full image. Removing it failed **no test**, because
`load_thumbnail` returns only the preview, so the arm is unobservable from
outside.

That is not a reason to delete it — it is the proof that no caller can depend on
it, which is why the cell's result type `ThumbData` has neither a `full` nor an
`anim` field. **Not having the fields is the point: there is no way to fill them
by accident.** The comment says plainly what the absence does *not* save — the
transient full-size buffer is still allocated for PNG, HEIC, JXL and TIFF, because
`dynamic_to_image_data` still decodes at source size and resizes. What went away
is the retention and the second downscale.

## `dct_is_cheapest` was redundant

A helper existed to decide whether the DCT path was the cheapest way to get a
cell. Removing it changed nothing, because `decode_bytes` already takes the DCT
path for a JPEG at any `max_dim`. It was **deleted rather than kept as
insurance**, and the downscale moved out of that branch into `load_thumbnail`,
where one copy covers all five return points.

**A change whose only justification was "it looks right" is a change nobody has
measured.**

## The lesson generalises

The first draft of a fix routinely contains a piece that does nothing. Find it by
deleting each piece and watching for a test that does not fail — and do not then
keep it, because an untested no-op is a trap for the next reader.

## Three bugs that passed everything

All three compiled, type-checked and passed the suite:

- **`Xform::Transverse`** was hand-transcribed as `(h-1-oy, w-1-ox)`; it should be
  `(w-1-oy, h-1-ox)`. It underflowed the moment the image was wider than tall.
- **The wgpu backend bits** were asserted against hand-written literals that
  matched the same wrong assumption. wgpu 30 inserted `Noop = 0`, moving `Gl` from
  bit 1 to **bit 4**; the test could not fail because it repeated the code's own
  numbers. `tests/renderer.rs` now compares against `Backends::GL` from wgpu.
- **The wide-gamut matrix** stored primaries as `[primary][component]` and solved
  the system as if it were `[component][primary]` — the transposed equation, which
  has an answer, just not this one. `invert3` still round-tripped to the identity
  and every number was plausible.

Same failure shape each time: something transcribed rather than derived, which
compiles, type-checks, and is wrong. The tests caught all three only after they
were rewritten to assert against the real source of truth rather than against a
restatement of the implementation.

## And one test that asserted a comment

`tests/unit.rs` scans the source for UI strings and asks the real egui font at two
sizes whether it has the glyphs, rather than asserting in a comment which glyphs
it "should" have. Same for `keybindings.rs` (a shortcut that does not exist is
otherwise unobservable) and `renderer.rs` (the probe's early exit is in a binary).
Where a premise is an assumption about a third party, check the premise against a
real object.