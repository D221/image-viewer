# Undo and ratings/tags — design

Two independent features, specified together because they are two answers to the
same question: *what does this viewer let you do to a picture that a viewer
otherwise does not?* They are built separately and share nothing but that.

The third item in the original survey — comparison / light-table — is **not**
here. It was declined.

---

## 1. Undo

### What it covers, and what it deliberately does not

Covers: **view transforms**, which are exactly the contents of `ImageEdits` —
turn, mirror (H and V), reset turn, crop, clear crop.

Does not cover: zoom and pan (pan is continuous — every mouse move would be a
step, and the history would be unreadable within seconds), settings changes
(already one click back in the panel), ratings and tags (see §2), and file
operations like `Del`.

**Undo is a thing that cannot lose data**, and that property is the reason for
both exclusions that are not about noise. Ratings are written to disk; a `Ctrl+Z`
that sometimes writes to your disk and sometimes does not is a chord you cannot
predict, and the app's existing rule — a bare letter may only do something that
cannot lose data — is best kept true rather than bent for one feature.

### Keys

`Ctrl+Z` and `Ctrl+Shift+Z`, as rebindable `KEY_ACTIONS` rows (`undo`, `redo`).
Every other action in the app is rebindable and these are no exception.

Buttons too, placed with the transform buttons in the panel beside *Reset turn*
— its natural sibling — and **disabled when there is nothing to undo**. The
disabled state is the feature being visibly alive: an undo button that is always
enabled and sometimes does nothing reads as broken, and one that is always
disabled reads as absent.

### Shape of the history

In `lib.rs`, so it is unit-testable without a GUI:

```rust
pub struct EditHistory {
    past: HashMap<PathBuf, Vec<Edit>>,
    redo: HashMap<PathBuf, Vec<Edit>>,
    cap: usize,          // per path
}
```

`undo(path) -> Edit` restores the previous edit and pushes the current one onto
the redo stack; `redo(path)` is the mirror. Both are pure over the store.

**Per-path, not one global list.** Undo means "undo on *this* picture". Someone
who turns three photos and then walks back to the first expects undo there to
undo that photo's turn, not to walk them through all three.

### The interface: one call, before the change

```rust
pub fn record(&mut self, path: &Path)
```

Called immediately **before** an edit is applied. This is the whole interface,
and it is chosen so that the four transform sites need one line each rather than
an edit-aware wrapper.

`apply_to_current` in `main.rs` is already the shared tail for all four transform
buttons, so the call goes there once rather than four times.

### The one place the rule needs care: crop

`set_crop` is called **during the drag**, every frame the pointer moves. Recording
each call would push ~200 entries for one gesture and make undo useless.

So crop records **once, on press**, not per frame. The crop site is therefore the
one place where "record before" is not adjacent to the mutation, and the source
scan has to check the *call site* rather than the presence of `record` anywhere
nearby — the same lesson `sync_shown_partner` and `display_dim` paid for.

### Redo is cleared by a new edit

Undo, then turn again, and redo is gone — the future has been rewritten. Standard,
and it needs no special case beyond one `clear` in `record`.

### Bounded

`cap` per path (50 is generous; a gesture is one step). When a path's stack is
full, the **oldest** step is dropped rather than refusing the newest — losing the
deepest history beats losing the edit you just made.

### Tests

- Truth table over the history: record/undo/redo, redo cleared by a new edit,
  undo at the bottom is a no-op, cap drops the oldest.
- Per-path isolation: recording on two paths leaves both intact.
- Scans: `record` is called before the edit in `apply_to_current`, and the crop
  path records on press rather than per frame.

---

## 2. Ratings and tags

### The model

- **Rating**: 0–5 stars. Ordinal, and people compare them.
- **Flag**: boolean. The culling decision — "this one is the sharp one".
- **Tags**: free-form labels.

Stars and a flag are separate attributes rather than one scale because they are
different kinds of judgement: a flag does not imply a rating, and a grid that
showed a flag as "3 stars" would be making a claim the user never made.

### Storage: XMP sidecars

`photo.jpg.xmp` beside the image, holding the rating, the flag and the keywords.
Chosen over the alternatives for a specific reason:

- **Ratings that do not travel with the folder are close to useless** for a photo
  library. An app-owned database under `%LOCALAPPDATA%` is cheap and adds no
  files beside the images, but it is per-machine, per-install, and unreadable by
  anything else.
- **Writing EXIF in place is the option to avoid.** `nom-exif` is read-only in
  this tree, so it would mean a hand-written APP1 segment writer on the exact
  path that already does `neutralize_exif_orientation` for the lossless transform.
  A second segment writer next to that machinery, on the path where a mistake is
  invisible in this viewer and fatal everywhere else, is a bad trade for a
  convenience.

XMP is what Lightroom, Capture One, Bridge and darktable already agree on, so
writing it makes the files useful to other tools rather than only to us.

**Atomic write**: `.tmp` then rename, so a half-written sidecar is never read
back. Same trap, same defence, as the thumbnail cache's `IEND` check — a partial
file that reads as *valid but wrong* is worse than one that fails.

### Where it shows

- **Grid cell** — the label strip under the thumbnail gets the stars; a flagged
  image gets a marker. Visible without opening anything, which is the point of
  rating while culling.
- **EXIF panel** — a row above the file's own metadata. A rating is *your*
  metadata, not the camera's, so it does not belong interleaved with shutter speed
  and ISO.
- **Filter box** — `stars:4`, `flagged`, `tag:holiday`. This is what makes ratings
  worth having at all: rating 2000 photos is pointless if you cannot then navigate
  by the ratings.

### Keys

A bare-letter budget entry per the app's rule. Both must be **non-destructive or
trivially reversible**:

- **Flag** — a bare letter, the single most useful culling action.
- **Rating up / down** — `+` / `-`, stepping 0→5, holding to repeat is fine since
  each step is a bounded write to a sidecar this app owns.

A bare letter that writes a sidecar next to the user's image is *not* in the
"cannot lose data" class by itself — but a rating is a value with no wrong answer
and no failure that touches the image, and the sidecar is rewritten atomically.
This is a genuine extension of the rule and is called out as one in
`tests/keybindings.rs`'s `BARE_LETTERS`, with the reasoning, rather than slipped
in.

### Not doing

- **No tag renaming or bulk operations** across files.
- **No sidecar merging**, and no reading XMP beyond the attributes above.
- **A partial XMP parser is required**, because XMP is RDF/XML and writing a
  valid document means either understanding enough of it or carefully rewriting
  only our own block and preserving every other packet byte-for-byte. The second
  is the plan: parse enough to find `xmp:Rating`, `xmp:Label`, `xmp:Rating`
  equivalents and our keywords, and **leave every byte we did not write alone**.
  A round-trip that drops a Camera Raw packet is worse than not supporting ratings.

---

## What is not here

**Comparison / light-table** was declined. **Histogram and exposure readout**,
**metadata editing** (caption/artist/description), **grid cell size** and
**slideshow shuffle** are filed in `TODO.md`.

**A visual check on manga mode is still owed** and outranks all of this: per
`TODO.md` §6 it is "arithmetically correct and unseen", and screen capture cannot
see this app's swapchain on this box.