//! The queue and the plan, driven directly.
//!
//! The bug these exist for: the viewer used to publish a "wanted" set and
//! abandon any in-flight decode whose path left it, while `replace` discarded
//! wanted jobs that were already running. A path could then end up with nothing
//! decoding it, nothing queued for it, and no re-request — the "random images
//! never load" report. Neither half was reachable from a test, because both
//! lived in a binary.

use image_viewer::{plan_jobs, ImgKey, Job, Kind, Loader, Plan};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What a fake decode reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Done {
    Delivered,
    /// The job gave up without producing anything (what cancellation will do).
    Abandoned,
}

struct Harness {
    loader: Loader,
    rx: std::sync::mpsc::Receiver<(PathBuf, Kind, Done)>,
    started: Arc<Mutex<Vec<(PathBuf, Kind)>>>,
    finished: Arc<Mutex<Vec<(PathBuf, Kind, Done)>>>,
    /// Every (path, kind) that reported `Delivered`.
    delivered: HashSet<(PathBuf, Kind)>,
    /// Every (path, kind) that gave up at least once. A repeat run of one of
    /// these is the retry working; a repeat run of anything else is a bug.
    abandoned: HashSet<(PathBuf, Kind)>,
    /// How many times each (path, kind) actually ran.
    runs: HashMap<(PathBuf, Kind), usize>,
}

impl Harness {
    /// `deliver` decides, per job, whether it produces a result or abandons.
    /// `hold_ms` keeps a job in flight long enough to be caught mid-run, which
    /// is what makes the busy/pending interleaving deterministic instead of a
    /// race we have to get lucky with.
    fn new<F>(workers: usize, deliver: F, hold_ms: u64) -> Self
    where
        F: Fn(&PathBuf, &Kind) -> Done + Send + Sync + 'static,
    {
        let (tx, rx): (Sender<(PathBuf, Kind, Done)>, _) = channel();
        let started = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(Mutex::new(Vec::new()));
        let s2 = started.clone();
        let f2 = finished.clone();
        let loader = Loader::spawn(
            workers,
            tx,
            move |job: &Job| {
                s2.lock().unwrap().push((job.path().to_path_buf(), job.kind));
                if hold_ms > 0 {
                    std::thread::sleep(Duration::from_millis(hold_ms));
                }
                let d = deliver(&job.key.path, &job.kind);
                f2.lock().unwrap().push((job.path().to_path_buf(), job.kind, d.clone()));
                ((job.path().to_path_buf(), job.kind, d.clone()), d != Done::Abandoned)
            },
            |job: &Job, err: String| {
                panic!("fake job {} panicked: {err}", job.path().display());
            },
            || {},
        );
        Self {
            loader,
            rx,
            started,
            finished,
            delivered: HashSet::new(),
            abandoned: HashSet::new(),
            runs: HashMap::new(),
        }
    }

    fn drain(&mut self) {
        for (path, kind, d) in self.rx.try_iter() {
            *self.runs.entry((path.clone(), kind)).or_insert(0) += 1;
            match d {
                Done::Delivered => {
                    self.delivered.insert((path, kind));
                }
                Done::Abandoned => {
                    self.abandoned.insert((path, kind));
                }
            }
        }
    }

    /// Wait until `n` jobs have finished, or panic. Generous: this must never be
    /// the thing that flakes.
    fn wait_finished(&mut self, n: usize, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.drain();
            if self.finished.lock().unwrap().len() >= n {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {n} finished jobs ({what}); got {}",
                self.finished.lock().unwrap().len()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn wait_until_idle(&mut self, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            self.drain();
            if self.loader.outstanding() == 0 {
                // A result is sent *before* the worker clears its busy slot, so
                // "idle" does not yet mean "all messages read". Drain once more
                // after the queue looks empty, or the last result is missed.
                self.drain();
                if self.loader.outstanding() == 0 {
                    return;
                }
            }
            assert!(
                Instant::now() < deadline,
                "loader never went idle ({what}); {} outstanding",
                self.loader.outstanding()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn runs_of(&self, path: &PathBuf, kind: Kind) -> usize {
        self.runs.get(&(path.clone(), kind)).copied().unwrap_or(0)
    }
}

fn files(n: usize) -> Vec<PathBuf> {
    (0..n).map(|i| PathBuf::from(format!("/photos/img{i:03}.jpg"))).collect()
}

fn plan<'a>(
    f: &'a [PathBuf],
    index: usize,
    cached: &'a HashSet<ImgKey>,
    failed: &'a HashSet<(ImgKey, Kind)>,
) -> Plan<'a> {
    Plan {
        files: f,
        index,
        current_page: 0,
        cached,
        failed,
        wants_exif: false,
        wants_full: false,
        current_stale: false,
        target_dim: 1024,
        radius: 3,
    }
}

fn delivered_always(_p: &PathBuf, _k: &Kind) -> Done {
    Done::Delivered
}

/// Abandon each path's *first* attempt only, so a retry succeeds. Abandoning
/// every attempt would make the queue retry forever, which is a different test
/// (and a real risk once cancellation exists, worth its own).
fn abandons_first_attempt_only() -> impl Fn(&PathBuf, &Kind) -> Done + Send + Sync + 'static {
    let seen: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    move |p: &PathBuf, _k: &Kind| {
        if seen.lock().unwrap().insert(p.clone()) {
            Done::Abandoned
        } else {
            Done::Delivered
        }
    }
}

#[test]
fn a_wanted_job_that_was_already_running_is_re_queued_if_it_gives_up() {
    // The exact stranding shape, made deterministic.
    //
    // 1. A is busy, decoding.
    // 2. The view moves to B, but the plan still wants A (it is a neighbour).
    // 3. `replace` must not queue a second A, and must not forget A either.
    // 4. A abandons. Because A was still wanted, it must run again — and that
    //    retry delivers, so the test terminates.
    let f = files(8);
    let mut h = Harness::new(1, abandons_first_attempt_only(), 120);
    h.loader.replace(vec![Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Preview,
        prio: 0,
        target_dim: 1024,
    }]);
    // Let the worker pick it up so `busy` is genuinely populated.
    h.wait_finished(0, "nothing");
    while h.started.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(2));
    }

    // A is now in flight and abandoning. Want A again (as a neighbour) + B.
    let jobs = vec![
        Job {
            key: ImgKey::first(&f[0]),
            kind: Kind::Preview,
            prio: 12,
            target_dim: 1024,
        },
        Job {
            key: ImgKey::first(&f[1]),
            kind: Kind::Preview,
            prio: 0,
            target_dim: 1024,
        },
    ];
    h.loader.replace(jobs);

    // A gave up, so it must be re-queued and run again. Waiting on the queue to
    // drain rather than on a job count: the count of *finished* jobs is bumped
    // before the result is sent, so waiting on it can return while messages are
    // still unread and under-report what ran.
    h.wait_until_idle("A re-queued after abandoning");
    assert!(
        h.runs_of(&f[0], Kind::Preview) >= 2,
        "A was still wanted and abandoned, so it must be re-queued: ran {} time(s)",
        h.runs_of(&f[0], Kind::Preview)
    );
    // B also gives up on its first attempt (the fake abandons every path's first
    // try), so it runs twice as well. The point of this test is A, not B.
    assert!(
        h.runs_of(&f[1], Kind::Preview) >= 1,
        "B was queued and must run"
    );
    assert!(
        h.delivered.contains(&(f[0].clone(), Kind::Preview)),
        "A's retry must deliver"
    );
}

#[test]
fn a_wanted_job_that_delivered_is_not_run_again() {
    // The other half of the contract, and the reason the re-queue is keyed on
    // *abandonment* rather than on completion: re-queueing a job that already
    // produced its result would be an endless decode loop.
    let f = files(4);
    let mut h = Harness::new(1, delivered_always, 30u64);

    h.loader.replace(vec![Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Preview,
        prio: 0,
        target_dim: 1024,
    }]);
    while h.started.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(2));
    }
    // f[0] is in flight and will succeed. Ask for it again while it runs.
    h.loader.replace(vec![Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Preview,
        prio: 12,
        target_dim: 1024,
    }]);

    h.wait_finished(1, "first run");
    h.wait_until_idle("after a delivered wanted job");
    assert_eq!(
        h.runs_of(&f[0], Kind::Preview),
        1,
        "a job that delivered must not be re-run just because it was wanted twice"
    );
    assert!(h.delivered.contains(&(f[0].clone(), Kind::Preview)));
}

#[test]
fn a_re_queued_job_keeps_its_priority_and_target_size() {
    // A re-request that loses `target_dim` silently decodes at the wrong size,
    // which shows up as a soft image much later than the bug that caused it.
    let f = files(4);
    let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let (tx, rx) = channel::<(PathBuf, Kind, Done)>();
    let loader = Loader::spawn(
        1,
        tx,
        move |job: &Job| {
            seen2.lock().unwrap().push((job.prio, job.target_dim));
            let abandon = seen2.lock().unwrap().len() == 1;
            (
                (job.path().to_path_buf(), job.kind, Done::Delivered),
                !abandon,
            )
        },
        |_j, e| panic!("{e}"),
        || {},
    );

    loader.replace(vec![Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Preview,
        prio: 12,
        target_dim: 2048,
    }]);
    while seen.lock().unwrap().is_empty() {
        std::thread::sleep(Duration::from_millis(2));
    }
    // Same job wanted again while in flight; it abandons, so it re-queues.
    loader.replace(vec![Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Preview,
        prio: 12,
        target_dim: 2048,
    }]);

    let deadline = Instant::now() + Duration::from_secs(10);
    while seen.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "re-queued job never ran");
        std::thread::sleep(Duration::from_millis(2));
    }
    while rx.try_iter().next().is_some() {}
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "expected exactly one re-run, got {seen:?}");
    assert_eq!(
        seen[1],
        (12, 2048),
        "re-queued job must keep its priority and target size, got {:?}",
        seen[1]
    );
}

/// The headline test: flip through a folder and require that every picture the
/// viewer still wants is actually on screen when everything goes quiet.
///
/// Two properties are asserted, and they are different:
///
///  1. **No strand.** When the loader is completely idle — nothing queued,
///     nothing running, nothing remembered — the plan for the current position
///     must be empty. If it is not, the viewer is wanting a picture that nobody
///     is going to fetch, which is the "random images never load" report.
///
///  2. **It terminates.** No hang, and the queue drains.
///
/// The ordering here is load-bearing, and getting it wrong makes the test
/// worthless in the *passing* direction. An earlier version re-planned in a
/// tight loop until convergence, which heals a strand by re-asking — so it
/// passed with the bug still in the code. The real app re-plans only when the
/// current image's preview arrives, on a step, when the full-res dwell expires,
/// or on a resize; a stranded *neighbour* is never re-asked while the user sits
/// still. So this test re-plans a bounded number of times, and then stops and
/// demands that the system be consistent with no further prompting.
#[test]
fn flipping_through_pictures_leaves_nothing_stranded() {
    const N: usize = 40;
    let f = files(N);
    let mut cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();

    // The first attempt at any given path walks away; a retry succeeds. That
    // makes abandonment deterministic per path rather than a coin flip, so the
    // test exercises the retry path every single run instead of most runs.
    //
    // One worker, held jobs: the bug needs a wanted path to be *already running*
    // when `replace` is called, and with a free pool that race is rare — which
    // is how the first version of this test passed with the bug present.
    let abandoned_once: Arc<Mutex<HashSet<PathBuf>>> = Arc::new(Mutex::new(HashSet::new()));
    let a2 = abandoned_once.clone();
    let mut h = Harness::new(
        1,
        move |p, _k| {
            let mut seen = a2.lock().unwrap();
            if seen.insert(p.clone()) {
                Done::Abandoned
            } else {
                Done::Delivered
            }
        },
        4,
    );

    // A browsing pattern with the awkward bits in it: forwards, a jump, back
    // again, and a wrap past the end of the folder.
    //
    // Deliberately *not* the whole folder. The final window has to be untouched
    // by the flip, because a strand needs the job that is in flight at the last
    // prompt to be on its *first* attempt — a path that already abandoned once
    // succeeds on its retry, and then there is nothing to strand. Leaving a
    // clean region at the end is what makes the ending deterministic instead of
    // dependent on which job happened to be running.
    const FLIP_UPTO: usize = 12;
    let mut steps: Vec<usize> = Vec::new();
    for i in 0..=FLIP_UPTO {
        steps.push(i);
        steps.push((i * 7 + 3) % (FLIP_UPTO + 1));
        if i > 0 {
            steps.push(i - 1);
        }
    }
    let final_index = N - 6; // window is N-9..=N-3, clear of everything above
    steps.push(final_index);

    let mut plan_at = |h: &mut Harness, cached: &mut HashSet<ImgKey>| {
        h.drain();
        for p in h.delivered.iter().filter(|(_, k)| *k == Kind::Preview).map(|(p, _)| p) {
            cached.insert(ImgKey::first(p));
        }
        plan_jobs(&plan(&f, final_index, cached, &failed))
    };

    let started = Instant::now();
    for &index in &steps {
        // Pump, then re-plan: the order a real frame does it in.
        h.drain();
        for p in h.delivered.iter().filter(|(_, k)| *k == Kind::Preview).map(|(p, _)| p) {
            cached.insert(ImgKey::first(p));
        }
        h.loader.replace(plan_jobs(&plan(&f, index, &cached, &failed)));
        // Step faster than a decode takes, so navigation lands mid-flight.
        std::thread::sleep(Duration::from_millis(1));
    }

    // Now the part that actually decides whether this test can fail.
    //
    // Browse, then stop. The stranding bug is not "work is lost" in the abstract;
    // it is that the viewer is left *wanting* a picture with nothing queued for
    // it, and the only thing that could re-ask is another `schedule()`. So:
    // prompt once, wait for a wanted job to be in flight, prompt again — and
    // then never prompt again. Looping until convergence, which two earlier
    // versions of this test did, heals the strand by re-asking and therefore
    // passes with the bug present. That is the trap this test has to avoid.
    let mut jobs = plan_at(&mut h, &mut cached);
    assert!(
        !jobs.is_empty(),
        "test setup: the final window is already cached, so there is nothing to strand"
    );
    h.loader.replace(jobs);

    // Wait for the interleaving the bug needs: a job of the final window
    // actually running.
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.loader.running() == 0 {
        assert!(
            Instant::now() < deadline,
            "no job ever started for the final window"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        h.loader.running(),
        1,
        "one worker means one job at a time; more than that is a setup mistake"
    );

    // The app's last prompt, landing while that job is still running. With the
    // stranding bug this is where the re-request is thrown away.
    jobs = plan_jobs(&plan(&f, final_index, &cached, &failed));
    assert!(
        !jobs.is_empty(),
        "test setup: nothing wanted at the last prompt, so nothing can be stranded"
    );
    h.loader.replace(jobs);

    // From here the app goes quiet: it re-plans on a step, when the *current*
    // image's preview arrives, when the full-res dwell expires, or on a resize.
    // A stranded neighbour triggers none of those, so nothing re-asks it.
    h.drain();
    for p in h.delivered.iter().filter(|(_, k)| *k == Kind::Preview).map(|(p, _)| p) {
        cached.insert(ImgKey::first(p));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.loader.outstanding() > 0 {
        assert!(
            Instant::now() < deadline,
            "loader never went idle: {} outstanding after browsing stopped",
            h.loader.outstanding()
        );
        h.drain();
        for p in h.delivered.iter().filter(|(_, k)| *k == Kind::Preview).map(|(p, _)| p) {
            cached.insert(ImgKey::first(p));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // One last drain. A result is sent *before* its worker frees the busy slot,
    // so "the queue is empty" happens while the last message can still be
    // unread — and the check below is computed from what has been read. Without
    // this the test reports a strand for a picture that was in fact delivered.
    h.drain();
    for p in h.delivered.iter().filter(|(_, k)| *k == Kind::Preview).map(|(p, _)| p) {
        cached.insert(ImgKey::first(p));
    }

    // Property 1: nothing wanted, nothing pending. This is the strand check.
    let outstanding_wanted: Vec<PathBuf> = plan_jobs(&plan(&f, final_index, &cached, &failed))
        .iter()
        .filter(|j| j.kind == Kind::Preview)
        .map(|j| j.path().to_path_buf())
        .collect();
    assert!(
        outstanding_wanted.is_empty(),
        "STRANDED: still wanted, but nothing is decoding, queued or remembered \
         for {:?} — this is the 'random images never load' bug",
        outstanding_wanted
    );

    // Property 2: the browse did real work, and the retry path was exercised.
    //
    // The floor is deliberately not "every path was requested". Stepping faster
    // than a decode takes means some queued jobs are coalesced away before a
    // worker picks them up — that is what `queued jobs dropped by rebuild`
    // counts, and it is the intended behaviour. What must be true is that the
    // final window (7 paths, all of which have to be delivered) plus a decent
    // slice of the flip region actually ran.
    let runs: usize = h.runs.values().sum();
    assert!(
        h.runs.len() >= 12,
        "flip-through barely ran: only {} distinct jobs, so it is not exercising \
         the queue",
        h.runs.len()
    );
    assert!(
        !h.abandoned.is_empty(),
        "nothing was ever abandoned, so the retry path was never exercised"
    );
    assert!(
        runs > h.runs.len(),
        "no job was ever retried despite abandonments, so the retry path did not work"
    );
    // The retry must actually have recovered something. Note this is *not* "every
    // abandonment was recovered": a picture that gave up and then went out of
    // scope must not be retried, so abandoned outnumbers delivered during a
    // fast browse. What matters is that at least one give-up was followed by a
    // successful retry, which is the property the stranding fix provides.
    let recovered = h
        .abandoned
        .iter()
        .filter(|k| h.delivered.contains(*k))
        .count();
    assert!(
        recovered > 0,
        "nothing that gave up was ever delivered, so no retry ever succeeded"
    );
    eprintln!(
        "flip-through: {} steps, {} distinct jobs, {} runs, {} abandoned, {} delivered, {:?}",
        steps.len(),
        h.runs.len(),
        runs,
        h.abandoned.len(),
        h.delivered.len(),
        started.elapsed()
    );
}
#[test]
fn the_plan_skips_cached_and_known_bad_paths() {
    let f = files(10);
    let cached: HashSet<ImgKey> = [&f[0], &f[1]].into_iter().map(|p| ImgKey::first(p)).collect();
    let failed: HashSet<(ImgKey, Kind)> = [(ImgKey::first(&f[5]), Kind::Preview)].into_iter().collect();
    let jobs = plan_jobs(&plan(&f, 0, &cached, &failed));

    assert!(
        !jobs.iter().any(|j| j.path() == f[0]),
        "already cached, must not be requested again"
    );
    assert!(
        !jobs.iter().any(|j| j.path() == f[5]),
        "known-bad preview must not be retried every frame"
    );
}

#[test]
fn the_plan_wraps_around_a_short_folder() {
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    let _ = &failed;
    // radius 3 on a 3-image folder: every image is a neighbour, and no index
    // may be requested twice.
    let jobs = plan_jobs(&Plan {
        files: &f,
        index: 0,
        current_page: 0,
        cached: &cached,
        failed: &failed,
        wants_exif: false,
        wants_full: false,
        current_stale: false,
        target_dim: 1024,
        radius: 3,
    });
    let mut seen = HashSet::new();
    for j in &jobs {
        assert!(seen.insert(j.path().to_path_buf()), "duplicate job for {:?}", j.path());
    }
    assert_eq!(jobs.len(), 3, "3 images, each once");
    assert_eq!(jobs[0].path(), f[0], "current image still first");
}

// ---------------------------------------------------------------------------
// Resize refinement
// ---------------------------------------------------------------------------
//
// The rule that replaced dropping the preview on resize. Two properties matter
// and both are easy to get wrong:
//
//  1. It must converge. A source smaller than the display can never be sharper,
//     so growing the window must cost at most one decode. Comparing the
//     *returned pixels* to the target instead of the target that was asked for
//     re-decodes on every resize, forever.
//  2. It must not fire for growth too small to see, nor for a source that has
//     nothing more to give.

/// preview_long, source_long, decoded_for, now_target -> refine?
fn refine(preview: u32, source: u32, decoded_for: u32, now: u32) -> bool {
    image_viewer::needs_refine(preview, source, decoded_for, now, image_viewer::RESIZE_PX_FLOOR)
}

#[test]
fn resize_refines_when_the_display_outgrew_the_texture() {
    // A 1600px texture decoded for a 1600px display; the window is now 2400px
    // and the source is big enough to fill it. This is the case that matters.
    assert!(refine(1600, 4000, 1600, 2400));
}

#[test]
fn resize_does_not_refine_when_the_texture_already_fills_the_display() {
    // Same window growth, but the texture is already 2400px: a re-decode would
    // produce an identical picture for a full decode's cost.
    assert!(!refine(2400, 4000, 1600, 2400));
}

#[test]
fn resize_does_not_refine_when_the_source_has_nothing_more_to_give() {
    // The convergence guard. A 1200px source on a 4000px display returns 1200px
    // whatever target it is asked for, so a pixels-vs-target rule re-decodes on
    // every resize, forever.
    assert!(!refine(1200, 1200, 1600, 4000));
    assert!(!refine(1200, 1200, 1600, 8000), "and it stays quiet as it grows");
}

#[test]
fn resize_ignores_growth_below_the_floor() {
    // A drag delivers a stream of 1-2px growths. One decode costs the same
    // whatever the size, so only growth past the floor is worth anything.
    let t = 1600;
    assert!(!refine(t, 4000, t, t + image_viewer::RESIZE_PX_FLOOR - 1));
    assert!(refine(t, 4000, t, t + image_viewer::RESIZE_PX_FLOOR + 1));
}

#[test]
fn resize_does_not_refine_when_the_display_shrank() {
    // Shrinking never makes the picture softer, and must not cost a decode.
    assert!(!refine(2400, 4000, 2400, 1600));
}

#[test]
fn resize_refine_stops_at_the_source_limit_and_never_restarts() {
    // A drag that keeps growing, against a source that eventually runs out.
    // Each settled size is a real growth and earns a decode, so the count is
    // not expected to be 1 — what must hold is that it *stops* at the source
    // limit and stays stopped, which is what stops a resize from turning into
    // an unbounded decode loop.
    let source = 5000u32;
    let mut preview = 1600u32;
    let mut decoded_for = 1600u32;
    let mut decodes = 0;
    let mut at_limit = None;

    for target in [2000u32, 2400, 3000, 4000, 5000, 6000, 7000, 8000] {
        if refine(preview, source, decoded_for, target) {
            decodes += 1;
            decoded_for = target;
            preview = target.min(source); // what the decode actually returns
            if preview == source && at_limit.is_none() {
                at_limit = Some(decodes);
            }
        }
    }
    assert!(at_limit.is_some(), "test setup: never reached the source limit");
    assert_eq!(
        decodes,
        at_limit.unwrap(),
        "no decode may happen once the texture is as large as the source allows"
    );
    assert_eq!(preview, source);
}

#[test]
fn a_source_smaller_than_the_display_is_never_re_decoded() {
    // The extreme of the same guard: a 1200px source, already sharp for the
    // display, on a window that grows to 6000px. Every step is free, because no
    // decode could ever return more than the 1200px already on screen.
    let source = 1200u32;
    let mut decoded_for = 1600u32;
    let mut decodes = 0;
    for target in [2000u32, 3000, 4000, 5000, 6000] {
        if refine(source, source, decoded_for, target) {
            decodes += 1;
            decoded_for = target;
        }
    }
    assert_eq!(decodes, 0, "nothing to gain, so nothing to pay for");
}

#[test]
fn stale_current_is_planned_even_though_it_is_cached() {
    // The point of the redesign: the preview stays on screen, so it is *in* the
    // cache, and the plan must still ask for a sharper one.
    let files: Vec<PathBuf> = ["a.jpg", "b.jpg", "c.jpg"].iter().map(PathBuf::from).collect();
    let cached: HashSet<ImgKey> = files.iter().map(|p| ImgKey::first(p)).collect();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();

    let plan = |stale: bool| Plan {
        files: &files,
        index: 0,
        current_page: 0,
        cached: &cached,
        failed: &failed,
        wants_exif: false,
        wants_full: false,
        current_stale: stale,
        target_dim: 2400,
        radius: 0,
    };

    assert!(
        plan_jobs(&plan(false)).is_empty(),
        "everything cached and nothing stale means no work"
    );
    let jobs = plan_jobs(&plan(true));
    assert_eq!(jobs.len(), 1, "one job: the current image");
    assert_eq!(jobs[0].path(), files[0]);
    assert_eq!(jobs[0].kind, Kind::Preview);
    assert_eq!(jobs[0].target_dim, 2400, "at the new, larger target");
    assert_eq!(
        jobs[0].prio,
        image_viewer::PRIO_CURRENT_PREVIEW,
        "still top priority, or a prefetch could delay it"
    );
}

#[test]
fn a_stale_current_is_not_planned_when_it_failed() {
    // A poisoned image must not be re-requested on every resize. `failed` is
    // permanent for the session, so honouring it is what stops a resize turning
    // into an endless retry loop on a file that will never decode.
    let files: Vec<PathBuf> = vec![PathBuf::from("bad.jpg")];
    let cached: HashSet<ImgKey> = files.iter().map(|p| ImgKey::first(p)).collect();
    let mut failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    failed.insert((ImgKey::first(&files[0]), Kind::Preview));
    let jobs = plan_jobs(&Plan {
        files: &files,
        index: 0,
        current_page: 0,
        cached: &cached,
        failed: &failed,
        wants_exif: false,
        wants_full: false,
        current_stale: true,
        target_dim: 2400,
        radius: 0,
    });
    assert!(jobs.is_empty(), "a failed image is not retried by a resize");
}
