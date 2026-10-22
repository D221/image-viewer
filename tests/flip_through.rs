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
        partner: None,
        partner_needs_full: false,
        cached,
        failed,
        wants_exif: false,
        wants_full: false,
        current_stale: false,
        target_dim: 1024,
        radius: 3,
        // These tests pin the *window's* behaviour, not its budget: no file
        // lengths, unlimited spend, so every walked neighbour is planned
        // exactly as the plan always did.
        file_bytes: &[],
        preload_budget: u64::MAX,
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
    //
    // The first attempt is **latched mid-run until the second `replace` has
    // gone through** — the same mechanism the Harness spells `hold_ms`: a job
    // held in flight long enough to be caught mid-run. The precondition is a
    // *mid-flight* re-request, and the earlier version of this test only
    // hoped for one: it pulsed through a 2 ms poll, and whenever the whole
    // first attempt delivered inside that window (its cold cost here is a few
    // tenths of a millisecond, and slower still under the parallel suite)
    // the second `replace` arrived after delivery. By `replace`'s own
    // contract that is a *new* request for a fresh decode — correct loader
    // behaviour, wrong test — and the run count came back three against an
    // assert about re-queueing. Reproduced at 1-in-12 by looping the target;
    // the instrumented trace on the loss: replace#2 at t=2.214 ms, the job
    // already finished and delivered at t=0.087 ms.
    let f = files(4);
    let seen: Arc<Mutex<Vec<(u32, u32)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let go = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let go2 = go.clone();
    let (tx, rx) = channel::<(PathBuf, Kind, Done)>();
    let loader = Loader::spawn(
        1,
        tx,
        move |job: &Job| {
            seen2.lock().unwrap().push((job.prio, job.target_dim));
            let abandon = seen2.lock().unwrap().len() == 1;
            // Held only by the first attempt; the re-queued retry runs free.
            while abandon && !go2.load(std::sync::atomic::Ordering::Acquire) {
                std::hint::spin_loop();
            }
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
    // Same job wanted again while the latch holds it in flight; it abandons
    // on release, so it re-queues. Released **after** this `replace` — the
    // replace is the precondition, not the payload.
    loader.replace(vec![Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Preview,
        prio: 12,
        target_dim: 2048,
    }]);
    go.store(true, std::sync::atomic::Ordering::Release);

    let deadline = Instant::now() + Duration::from_secs(10);
    while seen.lock().unwrap().len() < 2 {
        assert!(Instant::now() < deadline, "re-queued job never ran");
        std::thread::sleep(Duration::from_millis(2));
    }
    // Settle the whole queue before reading, not just the seen count: the
    // re-queued attempt must have finished its own bookkeeping, or what the
    // strand test pins below would be read mid-flight.
    while loader.outstanding() > 0 {
        assert!(Instant::now() < deadline, "re-queued job never finished");
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
    drop(rx);
}

/// `wanted` stores the viewer's desired job while it decodes; a delivery
/// satisfies it, and must **clear it**. `wanted_but_idle` is the strand
/// signal — "the viewer wants a picture nobody is going to fetch" — and a
/// delivered job is not a strand: nobody needs to fetch it again. Keeping
/// the entry past its own delivery fired the signal permanently whenever the
/// loader actually idled, which the app never sees (every `schedule`
/// recomposes `wanted`, so the stale entry washes out between plans) and a
/// test that stops replacing saw for good: measured on the instrumented
/// flake above, outstanding 0 / queued 0 / busy 0 / `wanted_but_idle` 1.
#[test]
fn a_delivered_job_is_not_left_in_the_wanted_map() {
    let f = files(4);
    let mut h = Harness::new(1, delivered_always, 30u64);

    h.loader.replace(vec![Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Preview,
        prio: 0,
        target_dim: 1024,
    }]);
    h.wait_finished(1, "delivery");
    while h.loader.outstanding() > 0 {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        h.loader.wanted_but_idle(),
        0,
        "a delivered job must not read as a strand after the loader goes idle"
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

    let plan_at = |h: &mut Harness, cached: &mut HashSet<ImgKey>| {
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
        partner: None,
        partner_needs_full: false,
        cached: &cached,
        failed: &failed,
        wants_exif: false,
        wants_full: false,
        current_stale: false,
        target_dim: 1024,
        radius: 3,
        file_bytes: &[],
        preload_budget: u64::MAX,
    });
    let mut seen = HashSet::new();
    for j in &jobs {
        assert!(seen.insert(j.path().to_path_buf()), "duplicate job for {:?}", j.path());
    }
    assert_eq!(jobs.len(), 3, "3 images, each once");
    assert_eq!(jobs[0].path(), f[0], "current image still first");
}

// ---------------------------------------------------------------------------
// The turn's other half
// ---------------------------------------------------------------------------
//
// Manga mode shows a turn as one picture: either a single page or two side by
// side. So the two halves are one unit of work to the reader, and the planner
// gives the second half the current image's own priority rather than a
// neighbour's — a partner that arrives on its own time is a spread that paints
// one page and then the other, which is more visible than a slow first page
// because the gap is *on screen* next to something that already arrived.
//
// Three files throughout, so the whole gallery sits inside the default preload
// radius and the cursor page's partner is also the window's nearest "next" —
// the only case where planning the partner and prefetching it can collide.

#[test]
fn the_partner_of_a_turn_is_planned_at_the_current_priority() {
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(ImgKey::first(&f[1]));
    let jobs = plan_jobs(&p);

    let partner = jobs
        .iter()
        .find(|j| j.path() == f[1] && j.kind == Kind::Preview)
        .expect("the partner must be planned at all");
    assert_eq!(
        partner.prio,
        image_viewer::PRIO_CURRENT_PREVIEW,
        "a partner decoded at neighbour priority leaves a half-painted spread"
    );
    assert_eq!(
        jobs.iter().filter(|j| j.prio == image_viewer::PRIO_CURRENT_PREVIEW).count(),
        2,
        "both halves of the turn, and nothing else at the current priority"
    );
    assert_eq!(
        partner.target_dim, 1024,
        "one target for the whole turn: a second half decoded for a different size \
         would not sit beside the first"
    );
}

#[test]
fn a_partner_keeps_its_own_page_number() {
    // **The whole key, never `Job::path()`.** `path()` is the filesystem half of
    // the identity and drops the page, so every other test in this section reads
    // the same for a page-0 key and a page-2 key — which is how `plan_jobs`
    // pushing `ImgKey::first(&partner.path)` instead of the partner itself would
    // have gone unnoticed: it asks the worker for page 0 of the right document,
    // which paints without error and is a different picture.
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    // A same-document partner on page 2. The window never builds such a key — it
    // prefetches page 0 — so this case cannot be satisfied by any window entry.
    let partner = ImgKey {
        path: f[1].clone(),
        page: 2,
    };
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(partner.clone());
    let jobs = plan_jobs(&p);

    let planned = jobs
        .iter()
        .find(|j| j.kind == Kind::Preview && j.key.path == f[1])
        .expect("the partner's file must be planned");
    assert_eq!(
        planned.key, partner,
        "the partner is planned under the key it was given, page number included"
    );
    assert_eq!(
        planned.prio,
        image_viewer::PRIO_CURRENT_PREVIEW,
        "and still at the current image's priority"
    );
    assert_ne!(
        partner,
        ImgKey::first(&f[1]),
        "test setup: a page-2 key must not equal `ImgKey::first` of the same path, \
         or this assertion cannot fail"
    );
}

#[test]
fn a_partner_already_cached_is_not_planned_again() {
    let f = files(3);
    let cached: HashSet<ImgKey> = [ImgKey::first(&f[1])].into_iter().collect();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(ImgKey::first(&f[1]));
    let jobs = plan_jobs(&p);

    assert!(
        !jobs.iter().any(|j| j.path() == f[1]),
        "the partner is already decoded and on screen; asking again is the whole \
         cost this test exists to rule out"
    );
}

#[test]
fn a_failed_partner_is_not_retried() {
    // `failed` is permanent for the session, so a partner that could not decode
    // must not turn every single navigation into another attempt at a file that
    // never will. The cursor page gets the same treatment (`failed` in the plan
    // above), and a partner without it would be the one place a spread retries
    // forever.
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> =
        [(ImgKey::first(&f[1]), Kind::Preview)].into_iter().collect();
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(ImgKey::first(&f[1]));
    let jobs = plan_jobs(&p);

    assert!(
        !jobs.iter().any(|j| j.path() == f[1]),
        "a failed image is not retried, on either half of a turn"
    );
}

#[test]
fn a_turn_partner_is_planned_once_not_also_by_the_preload_window() {
    // The partner is the file after the cursor, which is *also* the preload
    // window's nearest "next". Two jobs with one `(key, kind)` are two decodes
    // of one picture: `replace` checks only `busy` and dedupes nothing, so the
    // pool runs them one after the other — the reader waits for the partner
    // twice, and the second result lands over the first for nothing.
    //
    // The second consequence is worse and is why the fix is not "let the queue
    // sort it out": `wanted` is keyed by `(key, kind)`, so of the pair it keeps
    // the **last** insert — the window's copy — and a give-up re-queue restores
    // the neighbour priority instead of the current one.
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(ImgKey::first(&f[1]));
    let jobs = plan_jobs(&p);

    let mut seen = HashSet::new();
    for j in &jobs {
        assert!(
            seen.insert((j.key.clone(), j.kind)),
            "planned twice, so decoded twice: {:?} {:?}",
            j.key,
            j.kind
        );
    }
}

#[test]
fn no_partner_leaves_the_plan_exactly_as_it_was() {
    // The whole list, spelled out — which is stronger than comparing two plans
    // that both hold no partner, and stronger than the comparison it replaced.
    // That replacement was tried first: a plan with `partner: None` against the
    // same plan carrying a *cached* partner, on the reasoning that a partner
    // nothing is planned for must be inert. It is inert, and it catches nothing.
    // A cached partner's window entry is already suppressed by `need_preview`, so
    // suppressing it again by any other route cannot change the list — which is
    // why the variant that was run against it (seeding `seen` with the partner's
    // index) left this whole file green. A test that cannot fail is decoration, so
    // this pins the actual jobs.
    //
    // Six files, cursor on 2, radius 2, one neighbour already cached and one
    // known-bad, with the metadata read and the full decode both wanted — so the
    // list covers all four sources a plan draws from, in the order it draws them.
    let f = files(6);
    let cached: HashSet<ImgKey> = [ImgKey::first(&f[0])].into_iter().collect();
    let failed: HashSet<(ImgKey, Kind)> =
        [(ImgKey::first(&f[4]), Kind::Preview)].into_iter().collect();
    let mut p = plan(&f, 2, &cached, &failed);
    p.radius = 2;
    p.wants_exif = true;
    p.wants_full = true;
    p.partner = None;

    let listed: Vec<(ImgKey, Kind, u32, u32)> = plan_jobs(&p)
        .iter()
        .map(|j| (j.key.clone(), j.kind, j.prio, j.target_dim))
        .collect();
    assert_eq!(
        listed,
        vec![
            // The cursor page: priority 0, and the full decode rides the preview.
            (ImgKey::first(&f[2]), Kind::Preview, image_viewer::PRIO_CURRENT_PREVIEW, 1024),
            // EXIF describes the file, so the key is page 1 whatever is on screen.
            (ImgKey::first(&f[2]), Kind::Exif, image_viewer::PRIO_CURRENT_EXIF, 0),
            (ImgKey::first(&f[2]), Kind::Full, image_viewer::PRIO_FULL, 0),
            // The window, "next" slightly ahead of "previous" at each distance.
            (ImgKey::first(&f[3]), Kind::Preview, 12, 1024),
            (ImgKey::first(&f[1]), Kind::Preview, 13, 1024),
            // f[0] is cached and f[4] failed, so distance 2 contributes nothing.
        ],
        "no partner must add, drop, reorder or re-prioritise a single job"
    );
}

// ---------------------------------------------------------------------------
// The partner's full-resolution decode
// ---------------------------------------------------------------------------
//
// A turn is one picture to the reader, so both halves are shown at full resolution
// or the zoom is blurry on one of them. The partner's full buffer is usually
// **already in memory** - `sync_shown` adopts the cursor's by `Arc::clone`, and the
// same single-pass decode hands every cached preview one - so most of this costs
// nothing to ask for.
//
// **Not** most of the time, though, and that is the case this section exists for. A
// scan over `FAST_FULL_PIXELS` (12 MP) is decoded DCT-scaled, so its preview carries
// no full buffer at all, and manga scans are frequently large JPEGs. So the partner
// also needs a `Kind::Full` job planned for it, or that half of a zoomed spread stays
// at preview resolution for the whole session - which is exactly the defect the user
// reported, on the common case rather than the corner one.
//
// **A different priority from the partner's *preview*, on purpose.** The preview is
// what makes a half *appear*; both halves need it before anything is on screen, so it
// sits at the current priority with its counterpart. A full decode is the same
// refinement the cursor page's own is, and that one is [`PRIO_FULL`] - so a partner's
// full decode is a peer of it. At [`PRIO_CURRENT_PREVIEW`] it would outrank the
// cursor page's own preview (queued at 0 too, so insertion order would decide) and
// sit ahead of the whole preload window, which is the cost of a second 30 MB decode
// spent on a picture that has not been drawn yet.

/// A partner with no full buffer gets a full decode planned - at the *full* priority,
/// not the current one.
#[test]
fn a_partner_without_a_full_decode_gets_a_full_job() {
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    // A same-document partner on page 2, so the key carries its page: `Job::path()`
    // drops it, and the job would then decode page 0 of the right file - a different
    // picture, painted without error.
    let partner = ImgKey {
        path: f[1].clone(),
        page: 2,
    };
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(partner.clone());
    p.partner_needs_full = true;
    let jobs = plan_jobs(&p);

    let full = jobs
        .iter()
        .find(|j| j.kind == Kind::Full)
        .expect("the partner's half is on screen, so it must be sharpened too");
    assert_eq!(
        full.key, partner,
        "planned under the whole key it was given, page number included"
    );
    assert_eq!(
        full.prio,
        image_viewer::PRIO_FULL,
        "a full decode is a refinement of a picture already drawn, so it is a peer of the cursor \
         page's own full decode and not of its preview"
    );
    assert_ne!(
        full.prio,
        image_viewer::PRIO_CURRENT_PREVIEW,
        "the control: at the current priority a 30 MB decode would outrank the cursor page's own \
         preview and the whole preload window"
    );
    assert_eq!(full.target_dim, 0, "and it is not a preview job in disguise");
    // The partner's *preview* is still planned, still at the current priority: a full
    // decode does not put a picture on screen.
    assert!(
        jobs.iter().any(|j| j.key == partner
            && j.kind == Kind::Preview
            && j.prio == image_viewer::PRIO_CURRENT_PREVIEW),
        "both halves paint from their preview and sharpen from their full"
    );
}

/// A partner that already has its full buffer is not decoded again.
///
/// `pv.full` is set by the single-pass decode, so for most files this is the state -
/// planning a second decode of a picture whose full resolution is already sitting in
/// memory would be the waste the single-pass design exists to avoid.
#[test]
fn a_partner_that_already_has_its_full_decode_is_not_planned_again() {
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(ImgKey::first(&f[1]));
    // What the app passes when `needs_full` says the partner is covered: the buffer is
    // already there, so there is nothing to ask for.
    p.partner_needs_full = false;

    let jobs = plan_jobs(&p);
    assert!(
        !jobs.iter().any(|j| j.path() == f[1] && j.kind == Kind::Full),
        "the buffer is already in memory; a second full decode of the same page is pure cost"
    );
    // The premise, so the assertion above is not satisfied by a plan that forgot the
    // partner entirely: it is still planned for its preview.
    assert!(
        jobs.iter().any(|j| j.path() == f[1] && j.kind == Kind::Preview),
        "the partner is still planned - what it does not need is a *full* decode"
    );
    // ...and the control: the same plan with the field set does add exactly one job,
    // so the two assertions above are about `partner_needs_full` and not about a plan that
    // never mentions a partner's full decode at all.
    p.partner_needs_full = true;
    assert_eq!(
        plan_jobs(&p).len(),
        jobs.len() + 1,
        "asking for it is exactly one more job, so the two assertions above are about the field"
    );
}

/// A partner whose full decode failed is not retried, because `failed` is permanent
/// for the session and a spread would otherwise retry a hopeless file on every
/// navigation.
#[test]
fn a_partner_whose_full_decode_failed_is_not_retried() {
    let f = files(3);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> =
        [(ImgKey::first(&f[1]), Kind::Full)].into_iter().collect();
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(ImgKey::first(&f[1]));
    p.partner_needs_full = true;
    let jobs = plan_jobs(&p);

    assert!(
        !jobs.iter().any(|j| j.path() == f[1] && j.kind == Kind::Full),
        "a failure is remembered for the session; retrying it every navigation is the \
         'a transient error poisons the image' failure in its other direction"
    );
    assert!(
        jobs.iter().any(|j| j.path() == f[1] && j.kind == Kind::Preview),
        "the control: this fixture failed only the *full* decode, so the preview is still \
         wanted and planned. If this ever asserts, the guard above is filtering on more than \
         `Kind::Full` and a failed full decode would take the page off screen"
    );
}

/// Manga mode off: no partner, no partner full decode, and the plan is the one this
/// file has always pinned.
///
/// The whole list again, deliberately. Comparing two plans that both hold no partner
/// proves nothing about either of them - that is the variant `no_partner_leaves_the_
/// plan_exactly_as_it_was` rejected and the reason is written down there - so this
/// spells the jobs out and adds the control that makes the spelling mean something.
#[test]
fn no_partner_full_job_when_manga_mode_is_off() {
    let f = files(6);
    let cached: HashSet<ImgKey> = [ImgKey::first(&f[0])].into_iter().collect();
    let failed: HashSet<(ImgKey, Kind)> =
        [(ImgKey::first(&f[4]), Kind::Preview)].into_iter().collect();
    let mut p = plan(&f, 2, &cached, &failed);
    p.radius = 2;
    p.wants_exif = true;
    p.wants_full = true;
    // Manga mode off is `turn_partner() == None`, and the full decode is planned from
    // the same call - so both fields are `None` together.
    p.partner = None;
    p.partner_needs_full = false;

    let listed = |jobs: &[Job]| {
        jobs.iter()
            .map(|j| (j.key.clone(), j.kind, j.prio, j.target_dim))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        listed(&plan_jobs(&p)),
        vec![
            (ImgKey::first(&f[2]), Kind::Preview, image_viewer::PRIO_CURRENT_PREVIEW, 1024),
            (ImgKey::first(&f[2]), Kind::Exif, image_viewer::PRIO_CURRENT_EXIF, 0),
            (ImgKey::first(&f[2]), Kind::Full, image_viewer::PRIO_FULL, 0),
            (ImgKey::first(&f[3]), Kind::Preview, 12, 1024),
            (ImgKey::first(&f[1]), Kind::Preview, 13, 1024),
        ],
        "the only `Kind::Full` job is the cursor page's own, at the cursor's own key"
    );

    // The control. With the mode off nothing sets the flag, so this half cannot happen -
    // which is why the assertion above cannot be satisfied by a plan that adds a
    // partner's full decode unconditionally. Compared against the *same* plan with a
    // partner and no full buffer, so the only difference is the flag.
    let with_partner_no_full = plan_jobs(&{
        let mut q = plan(&f, 2, &cached, &failed);
        q.radius = 2;
        q.wants_exif = true;
        q.wants_full = true;
        q.partner = Some(ImgKey::first(&f[3]));
        q.partner_needs_full = false;
        q
    });
    p.partner = Some(ImgKey::first(&f[3]));
    p.partner_needs_full = true;
    assert_eq!(
        listed(&plan_jobs(&p)).len(),
        listed(&with_partner_no_full).len() + 1,
        "so with a partner and no full buffer the plan really is one job longer"
    );
    assert_eq!(
        plan_jobs(&p)
            .iter()
            .filter(|j| j.kind == Kind::Full)
            .count(),
        2,
        "the cursor page's own and the partner's, and nothing else anywhere in the plan"
    );
}

// ---------------------------------------------------------------------------
// The metadata read
// ---------------------------------------------------------------------------
//
// Fullscreen hides the metadata panel, and the panel is the only thing that
// displays EXIF — but the read is a whole-file `fs::read` (for the ICC profile)
// on a worker that a preview decode is waiting for, at a priority ahead of every
// prefetch. So a hidden panel should stop asking for it, and that has two halves
// which have to agree:
//
//   * `schedule` leaves the job out of the plan while the panel is hidden.
//   * the panel asks for it again the frame it reappears, or leaving fullscreen
//     leaves the panel reading "Loading EXIF…" with nothing behind it.

#[test]
fn exif_is_wanted_only_while_the_panel_that_shows_it_is_drawn() {
    assert!(
        image_viewer::wants_exif(true, true),
        "a visible panel with nothing loaded must get its data"
    );
    assert!(
        !image_viewer::wants_exif(false, true),
        "a hidden panel must not pay for a read nobody can see"
    );
    assert!(
        !image_viewer::wants_exif(true, false),
        "already loaded: a second read is pure waste"
    );
    assert!(!image_viewer::wants_exif(false, false));
}

#[test]
fn a_hidden_panel_drops_the_metadata_job_and_nothing_else() {
    let f = files(10);
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    // Page 3 of a multi-page document: the metadata describes the *file*, so the
    // job's key is page 1 even though that is not what is on screen. A gate that
    // quietly switched the key to the current page would poison a different
    // (key, kind) pair every navigation.
    let mut visible = plan(&f, 0, &cached, &failed);
    visible.current_page = 2;
    visible.wants_exif = image_viewer::wants_exif(true, true);
    let mut hidden = plan(&f, 0, &cached, &failed);
    hidden.current_page = 2;
    hidden.wants_exif = image_viewer::wants_exif(false, true);
    let (visible, hidden) = (plan_jobs(&visible), plan_jobs(&hidden));

    let exif_of = |jobs: &[Job]| -> Vec<Job> {
        jobs.iter().filter(|j| j.kind == Kind::Exif).cloned().collect()
    };
    let shown = exif_of(&visible);
    assert_eq!(shown.len(), 1, "a visible panel gets exactly one read");
    assert_eq!(
        shown[0].key,
        ImgKey::first(&f[0]),
        "EXIF is read once per file, not per page"
    );
    assert_eq!(shown[0].prio, image_viewer::PRIO_CURRENT_EXIF);
    assert!(
        exif_of(&hidden).is_empty(),
        "a hidden panel must not queue a metadata read"
    );

    // The gate must remove that job and *only* that job. Comparing job for job
    // is the point: "we made the plan smaller" is not evidence that the pictures
    // still got planned.
    let rest = |jobs: Vec<Job>| -> Vec<(ImgKey, Kind, u32)> {
        jobs.into_iter()
            .filter(|j| j.kind != Kind::Exif)
            .map(|j| (j.key, j.kind, j.prio))
            .collect()
    };
    assert_eq!(
        rest(visible),
        rest(hidden),
        "hiding the panel changed something other than the metadata job"
    );
}

#[test]
fn a_metadata_read_dropped_by_a_rebuild_can_be_asked_for_again() {
    // The panel re-asks on every frame it is drawn with nothing loaded, and
    // `push` does not put the job in the queue's `wanted` set — so a navigation
    // that rebuilds the plan can throw the read away. That is fine only because
    // the next frame asks again; this is the test for that, and for `push` not
    // growing a dedup that would refuse the second ask.
    let f = files(4);
    // One worker, and it is about to be held busy by something else, so the read
    // cannot be picked up before the rebuild gets to it. Without that this is a
    // race: sometimes the read runs first and there is nothing left to drop.
    let mut h = Harness::new(1, delivered_always, 200);
    let exif = Job {
        key: ImgKey::first(&f[0]),
        kind: Kind::Exif,
        prio: image_viewer::PRIO_CURRENT_EXIF,
        target_dim: 0,
    };
    h.loader.push(Job {
        key: ImgKey::first(&f[1]),
        kind: Kind::Preview,
        prio: image_viewer::PRIO_CURRENT_PREVIEW,
        target_dim: 1024,
    });
    // Wait for the pool to actually be busy, so "queued" below means queued and
    // not "about to be taken".
    let busy_by = Instant::now() + Duration::from_secs(10);
    while h.loader.running() == 0 {
        assert!(
            Instant::now() < busy_by,
            "the worker never picked anything up"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    h.loader.push(exif.clone());
    assert_eq!(
        h.loader.queued(),
        1,
        "the read should be waiting behind the busy worker"
    );
    h.loader.replace(vec![]); // a navigation: exif is not in this plan
    h.wait_until_idle("after the rebuild dropped the read");
    assert_eq!(
        h.runs_of(&f[0], Kind::Exif),
        0,
        "the read was queued, so a rebuild dropping it means it never ran"
    );

    h.loader.push(exif);
    h.wait_until_idle("after the re-asked read");
    assert_eq!(
        h.runs_of(&f[0], Kind::Exif),
        1,
        "asking again after a rebuild must actually run the read"
    );
    assert!(
        h.delivered.contains(&(f[0].clone(), Kind::Exif)),
        "the re-asked read produced nothing"
    );
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
    // Shrinking inside the band the pipeline filters at must cost nothing: this is
    // the ordinary window drag and the `M` toggle, where the picture is at worst
    // `PREFILTER_FACTOR`x the panel target and the CPU pass has filtered it.
    //
    // **This used to carry the stronger claim "shrinking never makes the picture
    // softer, and must not cost a decode", and that was wrong** - it is the
    // reported regression. Past the band it is not true at all, which is
    // `a_picture_decoded_far_above_the_display_is_refined_even_though_it_is_sharp`
    // and the whole reason `needs_refine` reads `decoded_for` in both directions.
    assert!(!refine(2400, 4000, 2400, 1600));
}

/// A preview decoded for a target far above the one it is now painted at is
/// **not** a sharp picture, and refining it is the reported regression's fix.
///
/// **Every number here is measured on the running app**, and the reason the arm
/// exists is that the growth-only rule could not see any of them. A 2223x3109
/// manga page in the preload window, decoded for `PREVIEW_TEXTURE_DIM` because the
/// first plan of a session ran before any panel size existed (so
/// `downscale_image` was a **no-op** - `load_page_preview_full(path, 4096, 0)`
/// is byte-identical to the source decode), painted into the rectangle a manga
/// half-panel actually draws it in:
///
/// | state | texture | painted | ratio |
/// |---|---|---|---|
/// | decoded for 4096 | 2223x3109 | 478.5 x 669.3 | **4.645** |
/// | decoded for the panel | 653x914 | 478.5 x 669.3 | 1.365 |
/// | decoded for 4096, normal mode | 2223x3109 | 654.4 x 915.3 | **3.397** |
/// | decoded for the panel, normal | 684x957 | 654.4 x 915.3 | 1.045 |
///
/// 4.645x of minification by a bilinear sampler with no mipmaps, over content
/// nothing in this crate ever low-passed. It is the lattice the resample work
/// removed, and the old rule called none of it a refine: `now_target (914) <=
/// decoded_for (4096) + 8` short-circuited on the first line, and
/// `source_long > preview_long` was false besides (3109 > 3109), so **no resize,
/// for the rest of the session, could ever have corrected it**.
///
/// **Judged on the request, not on the pixels**, which is the whole reason the
/// two numbers are different arguments. A pixel rule would fire forever on a
/// source smaller than the display - see
/// `an_oversized_refine_converges_after_one_decode` - and `decoded_for` converges
/// because the next decode is requested for the current target.
#[test]
fn a_picture_decoded_far_above_the_display_is_refined_even_though_it_is_sharp() {
    const PAGE: u32 = 3109; // 32.jpg and every other 2223x3109 page
    let target = 914; // manga half-panel on a 957x914 physical panel

    // The reported state, verbatim from the running app.
    assert!(
        refine(PAGE, PAGE, image_viewer::PREVIEW_TEXTURE_DIM, target),
        "a full-resolution texture painted into a half panel at 4.6x is not a sharp \
         picture, and the old rule answered false for it on both of its tests"
    );

    // The texture is sharp by every pixel measure - it is the *source*, at 1:1 -
    // which is why the growth-only rule could never see this.
    assert!(PAGE > target);
    assert_eq!(PAGE, PAGE);

    // Normal mode is the same defect at a smaller factor: 3109 of 915.3 is 3.397.
    assert!(refine(PAGE, PAGE, image_viewer::PREVIEW_TEXTURE_DIM, 957));

    // **And it stops once the picture is inside the band the pipeline filters
    // at**, which is the control that says the arm is not "re-decode on any
    // resize": a texture decoded for the panel is quiet at that same panel.
    let panel_tex = 941;
    assert!(!refine(panel_tex, PAGE, panel_tex, target));
    assert!(
        !refine(panel_tex, panel_tex, panel_tex, panel_tex),
        "a texture decoded for exactly the target is never refined, in either direction"
    );
}

/// The over-sized arm is judged on the **request**, so a source that can never be
/// bigger than the display still converges instead of re-decoding forever.
///
/// This is the loop guard for the new arm, and it is the reason `decoded_for` is
/// an argument at all: a 600px source on a 4000px panel returns 600px whatever it
/// is asked for, so a rule written on `preview_long > now_target * 2` would be
/// true on **every frame forever**. `decoded_for` is a number the decode actually
/// accepted, so the second call is quiet.
#[test]
fn an_oversized_refine_converges_after_one_decode() {
    let source = 600u32;
    let mut decoded_for = 4000u32; // the fallback target a session starts at
    let mut preview = source; // and what it returned: the whole source
    let mut decodes = 0;
    let target = 900u32;

    // Eight frames at the same display size - what a settle looks like.
    for _ in 0..8 {
        if refine(preview, source, decoded_for, target) {
            decodes += 1;
            decoded_for = target;
            preview = target.min(source);
        }
    }
    assert_eq!(decodes, 1, "one decode and then quiet, not one per frame");
    assert_eq!(decoded_for, target);

    // **And a pixel rule would not have.** The number it would keep seeing is
    // pinned here rather than asserted on `needs_refine`, because it is the
    // reason the arm reads `decoded_for`: a 600px source on a 900px display is
    // permanently "oversized" by pixels and permanently sharp by pixels.
    assert!(preview <= target);
}

/// The over-sized arm is quiet **inside** the band, so a shrink — the window
/// restore, the panel toggle, `M` on a narrow panel — costs no decode.
///
/// The numbers are the ones measured on the running app on this box: the panel's
/// long side moves between 914 (a manga half-panel's height) and 957 (the whole
/// panel's width), and previews decoded at either have to survive the other
/// without a decode.
///
/// **A growth inside the band is not free, and the test used to claim it was.**
/// `941 -> 957` is 16 px, past [`image_viewer::RESIZE_PX_FLOOR`], so the
/// *pre-existing* growth arm charges a decode — and that is the growth arm
/// working, not the new one. It is also the mechanism behind the burst of
/// `resize-driven re-decodes` measured when `M` is pressed after this fix (0
/// before it, because before it nothing was ever decoded at a real target and the
/// arm was unreachable). Asserted here so the boundary is stated rather than
/// assumed: the over-sized arm owns shrinks, the growth arm owns growths.
#[test]
fn an_oversize_inside_the_filter_band_costs_no_decode() {
    const PAGE: u32 = 3109;
    for (decoded_for, target) in [(941u32, 914u32), (957, 914), (914, 900), (957, 500)] {
        if decoded_for > target * image_viewer::PREFILTER_FACTOR + image_viewer::RESIZE_PX_FLOOR {
            continue;
        }
        assert!(
            !refine(decoded_for, PAGE, decoded_for, target),
            "{decoded_for} -> {target} is inside the {}-band the pipeline filters at, so \
             the over-sized arm must be quiet about it",
            image_viewer::PREFILTER_FACTOR
        );
    }
    // And the growth side of the boundary, so the two arms do not overlap silently.
    assert!(
        refine(941, PAGE, 941, 941 + image_viewer::RESIZE_PX_FLOOR + 1),
        "a growth past the floor is the growth arm's business and it is unchanged"
    );
    assert!(!refine(941, PAGE, 941, 941 + image_viewer::RESIZE_PX_FLOOR - 1));
    // The band is `PREFILTER_FACTOR`, and it is `prefilter_dim`'s, not a number
    // chosen here: a texture more than that above the target is one whose
    // pre-reduction could not fire.
    assert_eq!(
        image_viewer::PREFILTER_FACTOR, 2,
        "if the prefilter's own factor moves, `prefilter_dim` and the over-sized arm \
         move with it and these numbers have to be re-measured"
    );
}

/// There is **no decode target** before the display area has been measured, and
/// that is a value the caller has to answer for rather than a default.
///
/// **The default was the reported regression's root cause.** `schedule` used to
/// fall back to `PREVIEW_TEXTURE_DIM` for the gap between the first `schedule()`
/// — which runs from `goto` in `frame_prelude` — and the first `central` that
/// measures the panel. `PREVIEW_TEXTURE_DIM` (4096) is the *animation* ceiling and
/// not a decode request, so on every cold open the current page, the turn's other
/// half and all six preload neighbours were decoded at 4096, which for any source
/// under 4096 px means the full-resolution page and no resampling at all. The scan
/// `the_plan_is_empty_until_the_display_area_is_measured` is the other half: it
/// cannot see this function, only that `MyApp::schedule` asks it.
#[test]
fn there_is_no_decode_target_before_the_display_area_is_measured() {
    assert_eq!(
        image_viewer::decode_target(None),
        None,
        "an unmeasured display area must not borrow the animation ceiling as a decode \
         request: for every source under 4096 px that is a full-resolution texture, \
         minified by the GPU with no mipmaps and no low-pass anywhere in the pipeline"
    );
    // A measured one is the panel's long side, unchanged from what it always was.
    assert_eq!(image_viewer::decode_target(Some((957, 914))), Some(957));
    assert_eq!(image_viewer::decode_target(Some((478, 914))), Some(914));
    assert_eq!(
        image_viewer::decode_target(Some((957, 914))),
        Some(image_viewer::preview_target((957, 914))),
        "and it is `preview_target` - one number, so the plan and the paint cannot \
         disagree about what a display area means"
    );
}

/// The property, stated once: **a page is painted from a texture decoded for the
/// target it is painted at**, and the planner cannot ask for anything else.
///
/// Every other test here is one instance of it. This one walks the whole chain —
/// the panel, the decode target, the texture the decode returns and the rectangle
/// the layout draws — and asserts that the texture is never more than the factor
/// the pipeline low-passed at above the drawn rectangle's long side. The
/// offenders are named rather than generated, because a generated oversize would
/// be the *hypothetical* one and the reported one is these four.
#[test]
fn a_painted_page_comes_from_a_texture_inside_the_filtered_band() {
    // A real 2223x3109 manga page, and the panel measured on this box with the
    // viewer at its default 1280x800 window (`panel_px` = 957x914 physical).
    const PANEL_NORMAL: (u32, u32) = (957, 914);
    let page = (2223u32, 3109u32);
    let panel_manga = image_viewer::decode_panel_px(PANEL_NORMAL, true, false);
    let panel_lone_spread = image_viewer::decode_panel_px(PANEL_NORMAL, true, true);
    assert_eq!(panel_manga, (478, 914), "a portrait page keeps its half");

    for (what, panel) in [
        ("normal mode", PANEL_NORMAL),
        ("manga, two pages or a lone page", panel_manga),
        ("manga, a lone spread fills the turn", panel_lone_spread),
    ] {
        let target = image_viewer::decode_target(Some(panel)).expect("a measured panel");
        // What the decode at that target actually returns for this page: a
        // proportional reduction, because the source is larger than the target on
        // both axes. 2223/3109 * 914 = 653.7 -> 653.
        let drawn_long =
            (page.0 as f64 * panel.0 as f64 / page.1 as f64).max(panel.1 as f64);
        let tex_long = page.1.min(target);
        let ratio = tex_long as f64 / drawn_long;
        assert!(
            ratio <= image_viewer::PREFILTER_FACTOR as f64 + 0.35,
            "{what}: decoded for {target} the texture is {tex_long} long and the page is \
             drawn {drawn_long:.0} wide, a ratio of {ratio:.3} - outside the {}-band, so \
             the GPU is minifying by more than anything here low-passed for",
            image_viewer::PREFILTER_FACTOR
        );
        // And the refine machine must be quiet about it: the picture is what it
        // asked for.
        assert!(
            !image_viewer::needs_refine(
                tex_long,
                page.1,
                target,
                target,
                image_viewer::RESIZE_PX_FLOOR
            ),
            "{what}: a texture decoded for the current target must never be refined, or \
             every frame of a still picture would cost a decode"
        );
    }

    // **The offender, for the record**, so the number is in the suite rather than
    // only in the report: 4.645x at the same panel.
    let fallback_texture = page.1; // a decode at PREVIEW_TEXTURE_DIM is a no-op
    let drawn = 478.5f64;
    assert!(
        fallback_texture as f64 / drawn > 4.0,
        "the state the fix removes: a {}px texture drawn into {}px is {}x, and nothing \
         in the pipeline filtered the difference",
        fallback_texture,
        drawn,
        fallback_texture as f64 / drawn
    );
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
        partner: None,
        partner_needs_full: false,
        cached: &cached,
        failed: &failed,
        wants_exif: false,
        wants_full: false,
        current_stale: stale,
        target_dim: 2400,
        radius: 0,
        file_bytes: &[],
        preload_budget: u64::MAX,
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
        partner: None,
        partner_needs_full: false,
        cached: &cached,
        failed: &failed,
        wants_exif: false,
        wants_full: false,
        current_stale: true,
        target_dim: 2400,
        radius: 0,
        file_bytes: &[],
        preload_budget: u64::MAX,
    });
    assert!(jobs.is_empty(), "a failed image is not retried by a resize");
}
