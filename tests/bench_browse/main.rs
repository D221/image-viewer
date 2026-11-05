//! Browsing benchmark: what actually happens when you flip through a folder.
//!
//! This measures the things the re-introduced optimisations are supposed to
//! change, rather than decode throughput in isolation:
//!
//!   * **time to first pixels** per picture — the number a user feels
//!   * **wasted decode work** — worker time spent on pictures nobody wanted by
//!     the time the result arrived, which is what cancellation exists to remove
//!   * **cache hit rate on arrival** — how often the picture you stepped to was
//!     already decoded, which is what preloading exists to improve
//!   * **peak bytes held** — what the cache policy is supposed to bound
//!   * **jobs dropped by rebuild** — how far ahead of the decoder you are
//!
//! It drives the *real* `Loader` and the *real* `plan_jobs` from the library, so
//! it reflects the real scheduling decisions rather than a model of them. The
//! preview cache is a stand-in that tracks the same quantities the real one does
//! (bytes per entry, an LRU clock, a count cap); it becomes the real
//! `PreviewCache` when that moves into the library.
//!
//! Not criterion, on purpose: one session is seconds long, and what matters is
//! the distribution and the counters rather than a nanosecond figure. Run it
//! explicitly:
//!
//! ```sh
//! cargo test --release --test bench_browse -- --ignored --nocapture
//! ```
//!
//! Images are generated once into a temp dir and reused, so runs are comparable
//! to each other and to other commits. The size is chosen so one preview decode
//! takes tens of milliseconds: slow enough that the queue is under real
//! pressure, fast enough to flip through.
//!
//! Two accounting bugs were fixed here while measuring something else, and both
//! made the totals *under*-report, which is the direction that hides an effect:
//!
//!   * the final drain checked `outstanding() == 0` *after* taking delivery, so
//!     the last worker's result was thrown away. A result is sent before the
//!     worker frees its busy slot, so "idle" can be true while a message is still
//!     in flight. `outstanding()` is now read first. This is the same trap the
//!     app's queue has, and the same fix.
//!   * that drain never added to `decode_ms`, so at the shortest holds — where
//!     essentially all the work finishes after the last step — the `decode s`
//!     column read 0.00. **It is larger now, and correct**; a `decode s` of 0
//!     against a non-zero time-to-first-pixels was never possible to be right.
//!
//! The EXIF arm (`exif_ab`) exists because the metadata job is not free and was
//! not small: it used to call `color_info`, whose `fs::read` pulls the **whole
//! file**, and `nom_exif` then seeks out the APP1 segment. That read is gone
//! now — the colour profile is classified from the bytes the preview decode
//! already read (`PreviewAndFull::colour`), so the job's shape here mirrors the
//! app's: the EXIF seek plus the 256 KiB header read for the JPEG `COM`
//! segment. Modelled with the same calls rather than as a no-op — the question
//! "does not probing metadata while the panel is hidden make navigation
//! faster" has no answer if the benchmark pretends the job costs nothing —
//! but expect the per-read numbers an order of magnitude under the old ones.
//! It is the one job per navigation that exists only to fill a panel, and it
//! is queued *ahead* of every prefetch.
//!
//! The thumbnail arm (`thumb_cache_ab`) is the third kind of measurement here: not
//! a scheduling question but a **"can this be made faster at all"** one. A 95 MP
//! baseline JPEG has no cheaper full-image read — every pixel needs every DCT
//! coefficient, so the entropy decode is the floor for any conformant decoder — and
//! the grid's answer to that is a cache rather than a faster decode. Which means
//! the number that matters is the **cold** one, and printing only the warm number
//! would be how the limit of the change got forgotten.
//!
//! **The arms are sibling modules, split by question:** `harness` (the fixtures,
//! the session and the queue counters every arm drives), `baseline` (flipping
//! through a folder, and sitting still), `sweep` (the parametric sweeps),
//! `meta` (the metadata read, the thumbnail cache, Explorer's cache) and `drop`
//! (one drag-and-drop). One target directory rather than five targets, so the
//! command above stays the whole benchmark.

mod baseline;
mod drop;
mod harness;
mod meta;
mod sweep;
