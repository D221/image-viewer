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

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use std::sync::OnceLock;

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
pub const RESIZE_TO_REFINED: usize = 11;
/// How long a single worker job ran, by kind (Preview / Full / Thumb / Exif).
pub const JOB_PREVIEW_MS: usize = 12;
pub const JOB_FULL_MS: usize = 13;
pub const JOB_THUMB_MS: usize = 14;
pub const JOB_EXIF_MS: usize = 15;

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
    "resize -> refined",
    "worker: preview decode",
    "worker: full decode",
    "worker: thumbnail decode",
    "worker: exif",
];

/// Cheap counters, always recorded.
pub const C_DISPLAY_GAP_FRAMES: usize = 0; // frames that could not paint the wanted image
pub const C_JOBS_DROPPED: usize = 1; // queued jobs discarded by a queue rebuild
pub const C_JOBS_CANCELLED: usize = 2; // decodes abandoned via the cancel token
pub const C_TEXTURE_UPLOADS: usize = 3; // textures created or updated on the GPU
pub const C_TILES_CREATED: usize = 4; // full-res tiles cut and uploaded
pub const C_DECODE_CALLS: usize = 5; // file reads + decodes started
pub const C_PREVIEW_REUSED: usize = 6; // previews served without a new decode
pub const C_RESIZE_REDECODES: usize = 7; // resize-driven refinement requests

const N_COUNTERS: usize = 8;
const COUNTER_NAMES: [&str; N_COUNTERS] = [
    "display-gap frames (regression guard)",
    "queued jobs dropped by rebuild",
    "decodes cancelled mid-flight",
    "GPU texture uploads",
    "full-res tiles created",
    "decode calls started",
    "previews reused (no decode)",
    "resize-driven re-decodes",
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
        Self {
            count: AtomicU64::new(0),
            write: AtomicUsize::new(0),
            ring: [Z; CAP],
        }
    }

    fn push(&self, micros: u64) {
        let w = self.write.fetch_add(1, Relaxed);
        self.ring[w % CAP].store(micros as u32 + 1, Relaxed);
        self.count.fetch_add(1, Relaxed);
    }

    /// Copy the ring into a sorted `Vec` of microsecond values.
    fn snapshot(&self) -> (u64, Vec<u32>) {
        let n = self.count.load(Relaxed);
        let mut v: Vec<u32> = self
            .ring
            .iter()
            .map(|a| a.load(Relaxed))
            .filter(|&s| s != 0)
            .map(|s| s - 1)
            .collect();
        v.sort_unstable();
        (n, v)
    }
}

static METRIC: [Metric; N_METRICS] = [const { Metric::new() }; N_METRICS];
static COUNTERS: [AtomicU64; N_COUNTERS] = [const { AtomicU64::new(0) }; N_COUNTERS];
static ENABLED: AtomicU32 = AtomicU32::new(0);

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
    if ENABLED.load(Relaxed) != 0 {
        if let Some(m) = METRIC.get(idx) {
            m.push(micros);
        }
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

fn percentile(sorted: &[u32], p: f32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    // Nearest-rank on the sorted sample.
    let rank = (p * sorted.len() as f32).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1] as u64
}

fn ms(micros: u64) -> String {
    if micros >= 1000 {
        format!("{:.1}ms", micros as f32 / 1000.0)
    } else {
        format!("{micros}us")
    }
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
        out.push_str(&format!(
            "    {:<40} {}\n",
            COUNTER_NAMES[i],
            c.load(Relaxed)
        ));
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
    use super::*;

    #[test]
    fn records_and_reports_percentiles() {
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
