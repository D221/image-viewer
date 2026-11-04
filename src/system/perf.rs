//! Lightweight internal performance instrumentation.
//!
//! Design constraints (from the project's "speed first" goal):
//!
//! * Recording a sample must be cheap enough to leave in release builds. Each
//!   metric is a fixed-size ring of relaxed atomics: one `fetch_add` for the
//!   write index and one `store` for the value. No locks, no allocation, no
//!   formatting. Events are per image transition, not per frame, so the cost is
//!   irrelevant next to a decode.
//! * Percentiles come from a 256-entry ring per metric. That is plenty for
//!   interactive sessions (you do not press Right 1000 times in a row) and
//!   bounds memory at a few KB total.
//! * Counters are *always* on: they are the cheap regression guards (blank
//!   frames, dropped jobs, texture uploads) and cost one relaxed add.
//!
//! Use [`set_enabled`] (or `--perf` / `PERF=1`) to turn latency sampling on, and
//! [`report`] at shutdown to print a table.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};

/// Process start, stamped as early as possible in `main`.
static BOOT: OnceLock<std::time::Instant> = OnceLock::new();

/// Stamp the process start. Call first thing in `main`.
pub fn mark_boot() {
    let _ = BOOT.set(std::time::Instant::now());
}

/// The process-start instant, if [`mark_boot`] was called.
pub fn boot() -> Option<std::time::Instant> {
    BOOT.get().copied()
}

/// Number of latency metrics (see [`METRICS`]).
const N_METRICS: usize = 16;
/// Samples retained per metric.
const CAP: usize = 256;

/// Latency metrics, by index. Keep `METRICS` in the same order.
pub const STARTUP_TO_FIRST_FRAME: usize = 0;
pub const SELECT_TO_FIRST_PIXELS: usize = 1;
pub const SELECT_TO_PREVIEW: usize = 2;
pub const SELECT_TO_FULL: usize = 3;
pub const NAV_TO_FIRST_PIXELS: usize = 4;
pub const NAV_TO_FULL: usize = 5;
pub const ZOOM_TO_HIRES: usize = 6;
pub const PAN_TO_TILES: usize = 7;
pub const GALLERY_TO_THUMBS: usize = 8;
pub const SCAN_TO_GALLERY: usize = 9;
pub const RESIZE_TO_USABLE: usize = 10;
/// How long a single worker job ran, by kind (Preview / Full / Thumb / Exif).
pub const JOB_PREVIEW_MS: usize = 11;
pub const JOB_FULL_MS: usize = 12;
pub const JOB_THUMB_MS: usize = 13;
pub const JOB_EXIF_MS: usize = 14;
/// Wall-clock gap between consecutive frames. The *spread* is the signal, not
/// the value: a tight cluster around a constant period means something outside
/// the app is pacing presentation (vsync / compositor / remote display), while
/// a wide spread means the app is the one asking for frames as fast as it can
/// build them. Those need opposite fixes.
pub const FRAME_INTERVAL_US: usize = 15;

const NAMES: [&str; N_METRICS] = [
    "startup -> first frame",
    "select -> first pixels",
    "select -> fit preview",
    "select -> full quality",
    "next/prev -> first pixels",
    "next/prev -> fully refined",
    "zoom -> higher-res content",
    "pan -> required tiles",
    "gallery open -> first thumbs",
    "folder scan -> usable gallery",
    "resize -> usable",
    "worker: preview decode",
    "worker: full decode",
    "worker: thumbnail decode",
    "worker: exif",
    "frame interval",
];

/// Cheap counters, always recorded.
pub const C_DISPLAY_GAP_FRAMES: usize = 0; // frames that could not paint the wanted image
pub const C_JOBS_DROPPED: usize = 1; // queued jobs discarded by a queue rebuild
pub const C_TEXTURE_UPLOADS: usize = 2; // textures created or updated on the GPU
pub const C_TILES_CREATED: usize = 3; // full-res tiles cut and uploaded
pub const C_DECODE_CALLS: usize = 4; // file reads + decodes started
pub const C_PREVIEW_REUSED: usize = 5; // previews served without a new decode
pub const C_RESIZE_REDECODES: usize = 6; // resize-driven re-decodes of stale textures
pub const C_DECODE_PANICS: usize = 7; // worker panics caught (usually OOM)
pub const C_STALL: usize = 8; // times an image took absurdly long to appear
/// Frames actually built. Repaints are on demand, so this is also the honest
/// denominator for "CPU per frame": a config can look cheap simply by
/// repainting less often.
pub const C_FRAMES: usize = 9;
/// Frames that arrived carrying winit events. Against `C_FRAMES` this
/// separates "the user is driving" from "something is asking for frames by
/// itself" — the question to ask before concluding the app burns CPU idly.
pub const C_FRAMES_WITH_EVENTS: usize = 10;
/// Grid thumbnails served from the on-disk cache, and grid thumbnails that had
/// to be decoded. Their ratio is the only honest measure of whether the cache is
/// doing anything: a cache that always misses looks fine in a latency histogram
/// and is worthless, and a benchmark that fills the cache before it starts will
/// happily report 100% hits forever.
pub const C_THUMB_CACHE_HIT: usize = 11;
pub const C_THUMB_CACHE_MISS: usize = 12;
/// Files the 2 s watcher saw change on disk *while the app holds state for
/// them* — the funnel `invalidate_path` clears everything through. This is the
/// counter that says "the reload machinery fired", to be set against
/// `C_LOAD_FAILED` and the decode counts: a probe that mutates a file under a
/// running viewer can assert the machinery acted on it, without a screenshot.
pub const C_WATCHED_CHANGES: usize = 13;
/// Decodes that landed in `failed` (Preview or Full). A permanently poisoned
/// image shows only here — a session-long failure otherwise looks identical to
/// a slow file. The self-healing rule is: the next stamp change clears the
/// entry, so this counter may tick up mid-write (a half-written file is a
/// legitimate failure) but a *healed* change keeps moving; a file that failed
/// once on a toe and gets no further transaction is a `failed` entry that
/// never leaves.
pub const C_LOAD_FAILED: usize = 14;

/// `ReadDirectoryChangesW` records the OS delivered, ever — **including the
/// ones the app chose to ignore**. It is the other half of
/// `C_WATCHED_CHANGES`: a probe that mutates a file can tell "the OS never
/// told us" (this stays 0) from "we were told and did nothing" (this moved,
/// `C_WATCHED_CHANGES` did not). A watcher that is silently broken otherwise
/// looks exactly like a quiet folder.
pub const C_WATCH_EVENTS: usize = 15;
/// Folder watches that could not be started, so the 2 s poll is standing in.
/// One per folder open in the normal case is 0; a non-zero value on a
/// particular folder (a network share) is the explanation for why that folder
/// is still polled.
pub const C_WATCHER_FALLBACK: usize = 16;
/// Grid cells painted from **Explorer's** thumbnail cache instead of decoded
/// here (`SIIGBF_INCACHEONLY`, so no extraction is ever caused). The one counter
/// that can tell where a cell came from: without it, a grid served by the shell
/// and a grid served by our own cache look identical in a perf report, and
/// `tests/probe_thumb_cache.ps1` could not tell them apart either.
pub const C_SHELL_THUMB: usize = 17;
/// Grid-sized asks that the shell's cache did not have.
///
/// Two counters and not one because they answer different questions: the hits are
/// the win, and the misses are what a folder pays for the chance at one — and,
/// with `SHELL_MISS_GIVEUP`, how far the asking went before it stopped.
pub const C_SHELL_MISS: usize = 18;
/// Previews served from the **preview store on disk** instead of decoded.
///
/// The counter that says whether the store is doing anything at all: without it
/// a report cannot tell "the store is empty" from "the store is never asked".
pub const C_PREVIEW_STORE_HIT: usize = 19;
/// Times the store was asked and did not have the picture.
///
/// Counted even for a source too small to be stored, because that is the shape
/// of the tax: every preview decode asks.
pub const C_PREVIEW_STORE_MISS: usize = 20;

const N_COUNTERS: usize = 21;
const COUNTER_NAMES: [&str; N_COUNTERS] = [
    "display-gap frames (regression guard)",
    "queued jobs dropped by rebuild",
    "GPU texture uploads",
    "full-res tiles created",
    "decode calls started",
    "previews reused (no decode)",
    "resize-driven re-decodes",
    "worker decode panics",
    "stalls (>5s to first pixels)",
    "frames built",
    "frames with input events",
    "grid thumbnails from cache",
    "grid thumbnails decoded (cache miss)",
    "files reloaded (watcher saw a change)",
    "loads failed into `failed`",
    "folder-watch events received",
    "folder watches that fell back to polling",
    "grid cells from the Windows shell cache",
    "shell cache misses (grid-sized asks)",
    "previews served from the on-disk store",
    "store asks that did not have the picture",
];

struct Metric {
    count: AtomicU64,
    write: AtomicUsize,
    /// `micros + 1`, so 0 means "never written".
    ring: [AtomicU32; CAP],
}

impl Metric {
    const fn new() -> Self {
        #[allow(clippy::declare_interior_mutable_const)]
        const Z: AtomicU32 = AtomicU32::new(0);
        Self { count: AtomicU64::new(0), write: AtomicUsize::new(0), ring: [Z; CAP] }
    }

    fn push(&self, micros: u64) {
        let w = self.write.fetch_add(1, Relaxed);
        self.ring[w % CAP].store(micros as u32 + 1, Relaxed);
        self.count.fetch_add(1, Relaxed);
    }

    /// Copy the ring into a sorted `Vec` of microsecond values.
    fn snapshot(&self) -> (u64, Vec<u32>) {
        let n = self.count.load(Relaxed);
        let mut v: Vec<u32> =
            self.ring.iter().map(|a| a.load(Relaxed)).filter(|&s| s != 0).map(|s| s - 1).collect();
        v.sort_unstable();
        (n, v)
    }
}

static METRIC: [Metric; N_METRICS] = [const { Metric::new() }; N_METRICS];
static COUNTERS: [AtomicU64; N_COUNTERS] = [const { AtomicU64::new(0) }; N_COUNTERS];
static ENABLED: AtomicU32 = AtomicU32::new(0);

/// `file:line` of every `request_repaint` egui saw, counted.
///
/// egui tags each repaint request with the call site that made it, so this
/// answers "why is this app building frames when nothing is happening?" without
/// guessing. It is how the two repaint sources in this app were told apart: the
/// 2 s folder rescan, and GIF playback.
///
/// Only collected when sampling is enabled — it allocates and formats a string
/// per request, which is not something to pay for on a frame hot path.
static REPAINT_CAUSES: std::sync::Mutex<Option<std::collections::HashMap<String, u64>>> =
    std::sync::Mutex::new(None);

/// Tally one repaint request. Pass egui's `RepaintCause` straight through.
pub fn count_repaint_cause(cause: &egui::RepaintCause) {
    if !enabled() {
        return;
    }
    let mut guard = REPAINT_CAUSES.lock().unwrap();
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    let key = if cause.reason.is_empty() {
        format!("{}:{}", cause.file, cause.line)
    } else {
        format!("{}:{} ({})", cause.file, cause.line, cause.reason)
    };
    *map.entry(key).or_insert(0) += 1;
}

/// The repaint tallies, most-requested first. Empty unless sampling is on.
pub fn repaint_causes() -> Vec<(String, u64)> {
    let guard = REPAINT_CAUSES.lock().unwrap();
    let Some(map) = guard.as_ref() else {
        return Vec::new();
    };
    let mut v: Vec<(String, u64)> = map.iter().map(|(k, n)| (k.clone(), *n)).collect();
    v.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    v
}

/// Turn latency sampling on/off. Off by default; `--perf` or `PERF=1` turns it
/// on. Counters are always recorded regardless.
pub fn set_enabled(on: bool) {
    ENABLED.store(on as u32, Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Relaxed) != 0
}

/// Record a latency sample in microseconds. No-op when sampling is disabled.
pub fn record(idx: usize, micros: u64) {
    if ENABLED.load(Relaxed) != 0
        && let Some(m) = METRIC.get(idx)
    {
        m.push(micros);
    }
}

/// Record a latency sample from a duration.
pub fn record_since(idx: usize, start: std::time::Instant) {
    record(idx, start.elapsed().as_micros() as u64);
}

/// Increment a counter (always on).
pub fn count(idx: usize, by: u64) {
    if let Some(c) = COUNTERS.get(idx) {
        c.fetch_add(by, Relaxed);
    }
}

/// Read a counter (for diagnostics).
pub fn counter(idx: usize) -> u64 {
    COUNTERS.get(idx).map(|c| c.load(Relaxed)).unwrap_or(0)
}

/// Publish a gauge (e.g. bytes currently cached) rather than adding to it.
pub fn gauge(idx: usize, value: u64) {
    if let Some(c) = COUNTERS.get(idx) {
        c.store(value, Relaxed);
    }
}

fn percentile(sorted: &[u32], p: f32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    // Nearest-rank on the sorted sample.
    let rank = (p * sorted.len() as f32).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1] as u64
}

fn ms(micros: u64) -> String {
    if micros >= 1000 { format!("{:.1}ms", micros as f32 / 1000.0) } else { format!("{micros}us") }
}

/// Render a human-readable report of everything recorded so far.
pub fn report() -> String {
    let mut out = String::new();
    out.push_str("\n  latency (us; p50 / p95 / p99 / max)\n");
    for (i, m) in METRIC.iter().enumerate() {
        let (n, v) = m.snapshot();
        if n == 0 {
            continue;
        }
        let p50 = percentile(&v, 0.50);
        let p95 = percentile(&v, 0.95);
        let p99 = percentile(&v, 0.99);
        let max = *v.last().unwrap_or(&0) as u64;
        out.push_str(&format!(
            "    {:<30} n={:<4} {:>8} {:>8} {:>8} {:>8}   [{}]\n",
            NAMES[i],
            n,
            ms(p50),
            ms(p95),
            ms(p99),
            ms(max),
            if enabled() { "sampled" } else { "OFF" }
        ));
    }
    if out.trim_end() == "latency (us; p50 / p95 / p99 / max)" {
        out.push_str("    (no latency samples recorded - pass --perf to enable)\n");
    }
    out.push_str("\n  counters\n");
    for (i, c) in COUNTERS.iter().enumerate() {
        out.push_str(&format!("    {:<40} {}\n", COUNTER_NAMES[i], c.load(Relaxed)));
    }
    let causes = repaint_causes();
    if !causes.is_empty() {
        out.push_str("\n  who asked for a repaint (egui call site, most requests first)\n");
        for (k, n) in causes.iter().take(12) {
            out.push_str(&format!("    {:<8} {}\n", n, k));
        }
    }
    out
}

/// Write [`report`] to a file, ignoring failures (best effort on exit).
pub fn write_report(path: &std::path::Path) {
    if let Err(e) = std::fs::write(path, report()) {
        eprintln!("perf: could not write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    /// `ENABLED`, `METRIC` and `COUNTERS` are process-wide statics, and
    /// `cargo test` runs these two tests concurrently. Both flip `set_enabled`,
    /// so without this they race: one disables sampling while the other is
    /// mid-loop, its `record` calls become no-ops, and it fails intermittently
    /// (observed ~40% of runs).
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    use super::*;

    #[test]
    fn records_and_reports_percentiles() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(true);
        for i in 1..=100u64 {
            record(SELECT_TO_FIRST_PIXELS, i * 10);
            record(JOB_PREVIEW_MS, i);
        }
        let (n, v) = METRIC[SELECT_TO_FIRST_PIXELS].snapshot();
        assert_eq!(n, 100);
        assert_eq!(v.len(), 100);
        assert_eq!(percentile(&v, 0.5), 500);
        assert_eq!(percentile(&v, 0.99), 990);
        assert_eq!(*v.last().unwrap(), 1000);
        count(C_TILES_CREATED, 3);
        assert_eq!(COUNTERS[C_TILES_CREATED].load(Relaxed), 3);
        set_enabled(false);
        // Disabled sampling must not record.
        let before = METRIC[JOB_EXIF_MS].count.load(Relaxed);
        record(JOB_EXIF_MS, 5);
        assert_eq!(METRIC[JOB_EXIF_MS].count.load(Relaxed), before);
        let text = report();
        assert!(text.contains("select -> first pixels"));
        assert!(text.contains("GPU texture uploads"));
    }

    #[test]
    fn ring_wraps_without_unbounded_memory() {
        let _guard = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_enabled(true);
        for i in 0..(CAP as u64 * 3) {
            record(NAV_TO_FULL, i);
        }
        let (n, v) = METRIC[NAV_TO_FULL].snapshot();
        assert_eq!(n, CAP as u64 * 3);
        assert_eq!(v.len(), CAP, "ring must stay bounded");
        set_enabled(false);
    }
}
