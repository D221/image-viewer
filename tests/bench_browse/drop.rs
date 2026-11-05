//! What one drag-and-drop costs, split into its copy and its open.

use crate::harness::*;
use std::path::PathBuf;
use std::time::Instant;

/// Sizes the drop-copy arm measures, in megabytes. Synthetic files in a
/// `tempfile::tempdir()` — never `test-images/`, which is gitignored, and a
/// missing file there must not read as a fast copy.
const DROP_SIZES_MB: [u64; 3] = [5, 50, 500];

/// A file of `mb` megabytes in `dir`, written once and reused by every round.
///
/// Not a JPEG: nothing here decodes it, and the cost being measured is
/// `fs::copy`'s read-and-write, which is a function of the byte count and not of
/// what is in the bytes.
fn drop_fixture(dir: &std::path::Path, mb: u64) -> PathBuf {
    let path = dir.join(format!("drop-{mb}mb.bin"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == mb * 1024 * 1024) {
        return path;
    }
    let f = std::fs::File::create(&path).expect("create the drop fixture");
    let mut w = std::io::BufWriter::new(f);
    // One 1 MiB block of varied bytes, written `mb` times: a run of zeroes would
    // let the filesystem's compression or sparse handling make the copy look
    // faster than a photograph's bytes would.
    let block: Vec<u8> =
        (0..1024 * 1024u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect();
    for _ in 0..mb {
        std::io::Write::write_all(&mut w, &block).expect("write the drop fixture");
    }
    std::io::Write::flush(&mut w).expect("flush the drop fixture");
    path
}

/// What one drop of a file costs, split into "must copy" and "the same unchanged
/// file again".
///
/// **Both arms matter and the point of the benchmark is their ratio.** A first
/// drop of a file has to read and write it, and nothing can make that cheaper —
/// the same "a baseline JPEG has no cheaper full-image read" limit as a grid
/// cell. A *repeated* drop of an unchanged file does not: the copy is
/// content-addressed by path + mtime + length, so a valid one is already sitting
/// in `%TEMP%` and the only honest work is a `stat`. A benchmark that printed only
/// the first number would be how the second got forgotten.
///
/// **The cold arm *is* what the UI thread used to pay**, because it is the same
/// work `own_dropped` did when it was called from the drop handler: one
/// `metadata`, one `create_dir_all`, one `remove_file`, one `fs::copy`. Moving the
/// copy to a thread moves *when* it is paid, not how much — so this column is the
/// answer to "how long was the UI frozen", and after the move it is the answer to
/// "how long is the drop invisible", which is a different cost and not a smaller
/// one.
///
/// The cold arm's copy is released **outside** the timed region, so the arm is
/// cold by construction (the destination is gone) without paying a delete inside
/// the measurement.
///
/// `ctl` is a second, identical cold block: two arms of the same measurement,
/// interleaved, so the run reports **its own drift floor** rather than asking
/// anyone to take the difference between two blocks for one. On this box that
/// floor is 1-5%, which is what makes the cold/repeat ratio readable at all.
#[test]
#[ignore = "benchmark; run with --ignored --nocapture"]
fn drop_copy_ab() {
    let runs: usize = std::env::var("IV_BENCH_RUNS").ok().and_then(|v| v.parse().ok()).unwrap_or(7);
    let dir = tempfile::tempdir().expect("temp dir");
    eprintln!(
        "\n  drop copy A/B: {} MB files, median of {runs} interleaved rounds, \
         each arm warmed first\n",
        DROP_SIZES_MB.len()
    );

    struct Arms {
        cold: Vec<f64>,
        repeat: Vec<f64>,
        ctl: Vec<f64>,
    }

    for mb in DROP_SIZES_MB {
        let src = drop_fixture(dir.path(), mb);
        // Both arms return what the copy reported alongside the time, so the
        // loop below can assert the arm took the path it claims without the
        // warm-up calls tripping the same assertion.
        //
        // The cold arm's directory is removed **before** the clock starts, not
        // after it stopped: forgetting afterwards left the copy on disk for the
        // reuse arm's warm-up, and the next "cold" call then reused it and
        // measured nothing — an arm that reported a reuse while asserting it had
        // not, which is what the assertion below is for.
        let forget = || {
            let m = std::fs::metadata(&src).expect("stat the source");
            // The library's own path function, so the arm cannot drift from where
            // the app puts the copy.
            let _ = std::fs::remove_dir_all(image_viewer::drop_copy_dir(&src, &m));
        };
        let copy = |_round: usize| -> (f64, bool) {
            forget();
            let t = Instant::now();
            let c = image_viewer::own_dropped(&src).expect("own the dropped file");
            (t.elapsed().as_secs_f64() * 1000.0, c.reused)
        };
        let reuse = |_round: usize| -> (f64, bool) {
            let t = Instant::now();
            let c = image_viewer::own_dropped(&src).expect("own it again");
            (t.elapsed().as_secs_f64() * 1000.0, c.reused)
        };
        // Warm both arms: the first `fs::copy` of a file faults in its pages and
        // is not what a drop in the middle of a session pays. `reuse` twice,
        // because its first call after a cold arm has nothing to reuse and so
        // takes the path it exists not to measure.
        let _ = copy(usize::MAX);
        let _ = reuse(usize::MAX);
        let _ = reuse(usize::MAX);

        let mut a = Arms { cold: Vec::new(), repeat: Vec::new(), ctl: Vec::new() };
        for i in 0..runs {
            // Alternate which arm goes first, so neither systematically inherits
            // the other's warm-up — and put the control between them, so a drift
            // shows up as cold-vs-ctl disagreeing within the same round.
            let (c, r, k) = if i % 2 == 0 {
                let c = copy(i);
                let r = reuse(i);
                let k = copy(i);
                (c, r, k)
            } else {
                let k = copy(i);
                let r = reuse(i);
                let c = copy(i);
                (c, r, k)
            };
            assert!(!c.1, "the cold arm must really copy, or it is not measuring that");
            assert!(r.1, "an unchanged source must reuse its copy");
            assert!(!k.1, "the control must really copy, or it is not a control");
            a.cold.push(c.0);
            a.repeat.push(r.0);
            a.ctl.push(k.0);
        }
        eprintln!("  {:>5} MB   {:>10}  {:>10}  {:>10}   (ms)", "size", "cold", "repeat", "ctl");
        eprintln!("  {}", "-".repeat(46));
        for i in 0..runs {
            eprintln!(
                "  run {i:<2}   {:>10.2}  {:>10.2}  {:>10.2}",
                a.cold[i], a.repeat[i], a.ctl[i]
            );
        }
        let (cold, repeat, ctl) = (med(a.cold.clone()), med(a.repeat.clone()), med(a.ctl.clone()));
        let drift = (ctl - cold).abs() / cold.max(1e-9) * 100.0;
        eprintln!(
            "  {:>5} MB   {:>10.2}  {:>10.2}  {:>10.2}   <- median  \
             (control drift {drift:.1}%)",
            mb, cold, repeat, ctl
        );
        if mb == DROP_SIZES_MB[0] {
            eprintln!(
                "\n  `cold` is what a drop cost the UI thread before the copy moved off\n  \
                 it — and what it costs a worker now, so the drop is invisible for\n  \
                 this long instead. `repeat` is the same drop of an *unchanged*\n  \
                 file: the common case, and two stats."
            );
        }
        eprintln!();
    }
}
