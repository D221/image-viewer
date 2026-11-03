//! How many decodes does one window drag cost?
//!
//! Not a timing benchmark, and deliberately so: the resize decision is a
//! comparison and a boolean, so timing it would measure nothing. The quantity
//! that matters is *how many decodes a drag triggers*, because one preview
//! decode of a large photo is hundreds of milliseconds of a worker thread and
//! the number of them is unbounded in principle — a drag delivers a stream of
//! sizes and the old code re-decoded on every one.
//!
//! It drives the real `ResizeTracker` and the real `needs_refine`, so this is
//! the shipped policy rather than a restatement of it, and prints the old
//! behaviour beside the new one so the change is legible as a number.
//!
//! `#[ignore]`d because it prints a report rather than asserting anything; the
//! assertions that matter live in `flip_through.rs`.

use image_viewer::{RESIZE_PX_FLOOR, ResizeAction, ResizeTracker, needs_refine};

/// Preview state the drag acts on, so each refine is reflected immediately.
#[derive(Clone, Copy)]
struct Shown {
    preview: u32,
    source: u32,
    decoded_for: u32,
}

impl Shown {
    fn new(source: u32, target: u32) -> Self {
        // A single-pass decode returns the source capped to the target.
        Self { preview: source.min(target), source, decoded_for: target }
    }
}

/// The old policy, for the comparison column: re-decode on every frame whose
/// size differs from the last, for the current image and every preloaded
/// neighbour. `neighbours` is the preload radius on each side.
fn old_policy_decodes(sizes: &[u32], source: u32, neighbours: usize) -> usize {
    let mut decodes = 0;
    let mut prev: Option<u32> = None;
    let mut shown = Shown::new(source, sizes.first().copied().unwrap_or(0));
    for &t in sizes {
        if prev != Some(t) {
            decodes += 1 + 2 * neighbours; // current + both directions
            let g = t > shown.preview;
            shown.decoded_for = t;
            if g {
                shown.preview = source.min(t);
            }
        }
        prev = Some(t);
    }
    decodes
}

/// The new policy, through the shipped state machine.
///
/// `decode_frames` is how many frames a decode occupies. This matters more than
/// anything else in the model: a preview decode of a large photo is hundreds of
/// milliseconds, and a drag delivers frames continuously, so a refine issued
/// early in a drag is still running when the drag ends. That is precisely the
/// window in which the intermediate sizes are coalesced away, so assuming an
/// instant decode would report one decode per size step and hide the entire
/// effect being measured.
fn new_policy_decodes(sizes: &[u32], source: u32, decode_frames: usize) -> usize {
    let mut tr = ResizeTracker::new();
    let mut shown = Shown::new(source, sizes.first().copied().unwrap_or(0));
    let mut decodes = 0;
    // Frames left before the in-flight decode returns.
    let mut inflight: Option<(usize, u32)> = None;

    for &t in sizes {
        if let Some((left, for_target)) = inflight {
            if left == 0 {
                shown.decoded_for = for_target;
                shown.preview = source.min(for_target);
                tr.landed();
                inflight = None;
            } else {
                inflight = Some((left - 1, for_target));
            }
        }
        let want = needs_refine(shown.preview, shown.source, shown.decoded_for, t, RESIZE_PX_FLOOR);
        if tr.frame(t, want) == ResizeAction::Refine {
            decodes += 1;
            inflight = Some((decode_frames, t));
        }
    }
    decodes
}

/// A drag as the window actually delivers it: many small steps, and the size
/// repeated while the pointer is momentarily stationary.
fn drag(from: u32, to: u32, steps: usize, repeats: usize) -> Vec<u32> {
    let mut v = Vec::new();
    for i in 0..=steps {
        let t = from + (to.saturating_sub(from) * i as u32) / steps as u32;
        for _ in 0..repeats {
            v.push(t);
        }
    }
    v
}

/// A window snapped to its new size in one step: the F11 / maximise case.
fn snap(from: u32, to: u32) -> Vec<u32> {
    vec![from, to, to, to]
}

/// Frames a preview decode occupies. A large photo decodes in ~800 ms and a
/// display frame is ~16 ms, so ~50 is the realistic figure — and the drag has to
/// outlast it for the coalescing to matter, which a real drag does.
const DECODE_FRAMES: usize = 50;

fn report(name: &str, sizes: &[u32], source: u32) {
    let old = old_policy_decodes(sizes, source, 3);
    let new = new_policy_decodes(sizes, source, DECODE_FRAMES);
    eprintln!(
        "  {name:<34} {:>4} frames  old {old:>3} decodes   new {new:>2}   \
         ({:.0}x less, {} vs {})",
        sizes.len(),
        old as f64 / new.max(1) as f64,
        old,
        new
    );
}

#[test]
#[ignore = "prints a report; the assertions live in flip_through.rs"]
fn decodes_per_drag() {
    eprintln!("\n  one window drag, by how the size is delivered");
    eprintln!("  (old = re-decode on every changed frame, current + 3 neighbours each side)");
    eprintln!("  (new = one decode per settled size; a decode occupies {DECODE_FRAMES} frames)\n");

    // A 28 MP photo, previewed into a ~1900px-wide window. The realistic case.
    const BIG: u32 = 3743;
    report("small drag, 1900 -> 2600, 20 steps", &drag(1900, 2600, 20, 2), BIG);
    report("big drag, 1200 -> 3800, 40 steps", &drag(1200, 3800, 40, 2), BIG);
    report("one-step snap, 1900 -> 3800", &snap(1900, 3800), BIG);
    report("maximise + F11, 1900 -> 3840", &snap(1900, 3840), BIG);

    // A source smaller than the display can ever be satisfied: the case where a
    // pixels-vs-target rule would re-decode on every single frame, forever.
    eprintln!("\n  a source too small for the display (1200px on a 3800px window)");
    report("small drag, 1200 -> 2600, 20 steps", &drag(1200, 2600, 20, 2), 1200);
    report("big drag, 1200 -> 3800, 40 steps", &drag(1200, 3800, 40, 2), 1200);

    // A source just big enough to satisfy the target exactly: the most work the
    // new policy can legitimately ask for.
    eprintln!("\n  a source just big enough (2600px source, window reaches 2600)");
    report("small drag, 1900 -> 2600, 20 steps", &drag(1900, 2600, 20, 2), 2600);

    eprintln!();
}
