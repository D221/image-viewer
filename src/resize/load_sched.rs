//! Module `load_sched`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use load_sched::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

// ---------------------------------------------------------------------------
// Load scheduling: what to decode, and the worker pool that decodes it
// ---------------------------------------------------------------------------
//
// This is the machinery that caused "random images never load", so it lives
// here rather than in `main.rs` where it cannot be tested. `main.rs` keeps the
// egui shell (textures, panels) and hands this module a decode function; a test
// hands it a trivial one and can drive the queue through any interleaving.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

/// What a decode job is for.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Preview,
    Full,
    Exif,
    Thumb,
}

/// The identity of a decoded image: a file, and a page within it.
///
/// Every cache that holds *decoded pixels* is keyed on this rather than on a
/// `PathBuf`, because page 1 and page 3 of one document are different pictures.
/// A path-keyed cache hands back the wrong page — silently, and only for
/// multi-page files, which is the worst way for it to be wrong.
///
/// Caches that describe the *file* rather than its contents stay keyed on the
/// path: grid thumbnails, the folder watcher's change stamps, and the per-image
/// edit map. A thumbnail of page 1 is the file's thumbnail, and an edit applies to
/// the file you save.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ImgKey {
    pub path: PathBuf,
    /// 0-based. Non-zero only for pages of a multi-page TIFF.
    pub page: u32,
}

impl ImgKey {
    /// The ordinary case: the file's only page.
    pub fn first(path: &Path) -> Self {
        Self { path: path.to_path_buf(), page: 0 }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// A view remembered for one image: where the user left it.
///
/// Every field is what was on screen, not a policy. `fit` is remembered rather
/// than converted to a scale, because "fitted" is what the user saw — a
/// remembered fit re-fits to whoever looks at it next, with a different panel,
/// and that is correct rather than approximate (a numeral would have been).
/// `scale` is points per source pixel and `offset` is the image centre
/// relative to the panel centre, in points — the same units `View` in the
/// binary holds, so a save/restore round-trip is an identity rather than a
/// conversion.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SavedView {
    pub fit: bool,
    pub scale: f32,
    pub offset: (f32, f32),
}

impl SavedView {
    /// The no-memory answer: today's carry, unchanged.
    ///
    /// An image that was never visited goes back to the behaviour the setting
    /// exists beside — zoom carried over, pan owned by the picture it
    /// panned rather than dragged into the next one. Writing `0.0` here rather
    /// than calling it "default" is the point: this is the rule, stated.
    pub fn carried(fit: bool, scale: f32) -> Self {
        Self { fit, scale, offset: (0.0, 0.0) }
    }
}

/// Per-image view memory, one entry per `(file, page)` the user has visited.
///
/// Keyed on [`ImgKey`] and *not* on a path, for the reason the pixel caches
/// are: page 1 and page 3 of a document are different pictures, and returning
/// to page 3 with page 1's pan is a view that looks like a bug — offset by
/// someone else's drag.
///
/// The map is bounded by what the user has visited, not by the folder: one
/// visit adds one entry of sixteen bytes, so a whole session of browsing is
/// kilobytes. It is deliberately not persisted and deliberately not capped —
/// the shapes to prove otherwise would both have to beat the tiny alternative.
pub struct ViewMemory {
    map: HashMap<ImgKey, SavedView>,
}

impl Default for ViewMemory {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewMemory {
    pub fn new() -> Self {
        Self { map: HashMap::new() }
    }

    /// Record what the view of `key` was when the user left it.
    pub fn remember(&mut self, key: ImgKey, view: SavedView) {
        self.map.insert(key, view);
    }

    /// What `key` was remembered as, or `None` for never visited this session.
    pub fn restore(&self, key: &ImgKey) -> Option<SavedView> {
        self.map.get(key).copied()
    }

    /// Drop every page of one file: the caller does this when the file's
    /// pixels are gone (`invalidate_path`'s funnel), because a view remembered
    /// for an image that no longer exists is exactly the ladder-shaped value a
    /// latch produces — present, plausible, wrong.
    pub fn forget_file(&mut self, path: &Path) {
        self.map.retain(|k, _| k.path != path);
    }
}

/// The view an arriving image starts from, asked once in `goto` when the
/// setting is on: the remembered one when there is one, otherwise the carry.
///
/// A pure function rather than a branch in the caller, so the two rules are
/// testable side by side: **a remembered view is restored exactly** (round-trip
/// with [`ViewMemory::remember`]), and **no memory means today's behaviour,
/// byte for byte** — fit and scale carried, pan reset. Nothing here reads a
/// config; the caller decides whether to ask at all, which is why the setting
/// being off is not an arm of this function.
pub fn arriving_view(saved: Option<SavedView>, fit: bool, scale: f32) -> SavedView {
    saved.unwrap_or_else(|| SavedView::carried(fit, scale))
}

impl std::fmt::Display for ImgKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.page == 0 {
            write!(f, "{}", self.path.display())
        } else {
            write!(f, "{}#{}", self.path.display(), self.page + 1)
        }
    }
}

/// One unit of decoding work.
#[derive(Clone, Debug)]
pub struct Job {
    pub key: ImgKey,
    pub kind: Kind,
    /// Lower runs first. The current image is 0 and every prefetch is >= 10, so
    /// prefetch can never starve it.
    pub prio: u32,
    /// Display target long side in physical pixels (preview jobs only).
    pub target_dim: u32,
}

impl Job {
    /// The file this job is for. The *filesystem* half of the identity; the page
    /// matters to the decoder and to the caches, not to anything that opens, moves
    /// or deletes the file.
    pub fn path(&self) -> &Path {
        self.key.path()
    }
}

/// Shared queue state. Public only so tests can assert on it.
#[derive(Default)]
pub struct QueueState {
    pub jobs: Vec<Job>,
    pub busy: HashSet<(ImgKey, Kind)>,
    /// The viewer's current desired set, as of the last `replace`, held *whole*
    /// so a re-queue can preserve priority and target size.
    ///
    /// This is the fix for the stranding bug, and getting it right took two
    /// attempts. The naive version remembered only those wanted jobs that
    /// happened to be *running* at the instant `replace` was called, which is
    /// not enough: a job that was merely queued then, and abandoned later, was
    /// still lost. The only correct question on a job finishing is "is this
    /// still wanted?", not "was this wanted and running a moment ago?".
    ///
    /// The earlier bug was `replace` filtering wanted-but-busy jobs out of the
    /// queue and forgetting them. That is harmless while every job runs to
    /// completion — the running one delivers. It becomes fatal as soon as a job
    /// can give up: the re-request is already discarded, and `schedule` only
    /// re-runs on a step, the current image's preview arriving, a dwell expiry
    /// or a resize, so a stranded *neighbour* is never asked for again.
    wanted: HashMap<(ImgKey, Kind), Job>,
}

/// Priority queue served by a small pool of worker threads.
/// How many worker threads the decode pool gets.
///
/// `preferred == 0` is auto: the shipped behaviour of available parallelism
/// clamped to 2..=4. Anything else is the user's explicit choice, clamped to
/// 1..=8 so a typo cannot spawn 64 threads or zero the pool.
pub fn resolve_worker_count(available: usize, preferred: usize) -> usize {
    if preferred == 0 {
        available.clamp(2, 4)
    } else {
        preferred.clamp(1, 8)
    }
}

pub struct Loader {
    shared: Arc<(Mutex<QueueState>, Condvar)>,
}

impl Loader {
    /// Start the pool.
    ///
    /// `run` does the work for one job and returns `(message, delivered)`.
    /// `delivered` is false only when the job gave up *without producing a
    /// result* — today that never happens, and it is the hook that makes
    /// abandoning work safe: a job that gave up is re-queued if it was still
    /// wanted, and a job that delivered is not re-run.
    ///
    /// `panic_msg` turns a caught panic back into a message so a corrupt file
    /// cannot kill a worker, and keeps the payload — a bare "decoder panicked"
    /// once hid OOM, the most likely cause, behind an unreproducible symptom.
    /// `wake` is called on the thread that received a result, to ask for a
    /// repaint.
    pub fn spawn<M, R, P, W>(workers: usize, tx: Sender<M>, run: R, panic_msg: P, wake: W) -> Self
    where
        M: Send + 'static,
        R: Fn(&Job) -> (M, bool) + Send + Sync + 'static,
        P: Fn(&Job, String) -> (M, bool) + Send + Sync + 'static,
        W: Fn() + Send + Sync + 'static,
    {
        let shared: Arc<(Mutex<QueueState>, Condvar)> =
            Arc::new((Mutex::new(QueueState::default()), Condvar::new()));
        let run = Arc::new(run);
        let panic_msg = Arc::new(panic_msg);
        let wake = Arc::new(wake);
        for _ in 0..workers.max(1) {
            let shared = shared.clone();
            let tx = tx.clone();
            let run = run.clone();
            let panic_msg = panic_msg.clone();
            let wake = wake.clone();
            std::thread::spawn(move || {
                loop {
                    let job = {
                        let (lock, cv) = &*shared;
                        let mut q = lock.lock().unwrap();
                        loop {
                            let best = q
                                .jobs
                                .iter()
                                .enumerate()
                                .min_by_key(|(_, j)| j.prio)
                                .map(|(i, _)| i);
                            if let Some(i) = best {
                                let job = q.jobs.swap_remove(i);
                                q.busy.insert((job.key.clone(), job.kind));
                                break job;
                            }
                            q = cv.wait(q).unwrap();
                        }
                    };

                    let (msg, delivered) = match std::panic::catch_unwind(
                        std::panic::AssertUnwindSafe(|| run(&job)),
                    ) {
                        Ok(r) => r,
                        Err(payload) => {
                            perf::count(perf::C_DECODE_PANICS, 1);
                            let detail = payload
                                .downcast_ref::<&str>()
                                .map(|s| (*s).to_string())
                                .or_else(|| payload.downcast_ref::<String>().cloned())
                                .unwrap_or_else(|| "unknown".to_string());
                            // A panicking decode is still a *delivered* failure:
                            // it is reported and remembered, not retried here.
                            panic_msg(&job, format!("decoder panicked: {detail}"))
                        }
                    };

                    let alive = tx.send(msg).is_ok();
                    // Free the slot, then honour anything still waiting on it.
                    let (lock, cv) = &*shared;
                    let mut q = lock.lock().unwrap();
                    let key = (job.key.clone(), job.kind);
                    q.busy.remove(&key);
                    if delivered {
                        // Delivered: the wanted entry was *for* this job and is
                        // satisfied; keeping it would leave `wanted_but_idle` > 0
                        // with the loader idle — the strand signal that map's doc
                        // defines, fired permanently for a job that is actually
                        // done (invisible while a schedule keeps recomposing
                        // `wanted`, and exactly what a test that stops replacing
                        // sees). A fresh decode asks for itself: the next plan
                        // that wants the key re-inserts it.
                        q.wanted.remove(&key);
                    } else if let Some(wanted) = q.wanted.get(&key).cloned() {
                        // Gave up without producing anything. If it is still wanted,
                        // ask again — this is the whole safety property, and it is
                        // why the wanted set is kept rather than inferred.
                        if !q.jobs.iter().any(|j| (j.key.clone(), j.kind) == key) {
                            q.jobs.push(wanted);
                            cv.notify_all();
                        }
                    }
                    drop(q);
                    if !alive {
                        return; // UI is gone
                    }
                    wake();
                }
            });
        }
        Self { shared }
    }

    /// Queue a job without disturbing existing queued work (the grid's
    /// on-demand thumbnail requests). Already queued or already running is
    /// left alone.
    pub fn push(&self, job: Job) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        if q.busy.contains(&(job.key.clone(), job.kind)) {
            return;
        }
        if q.jobs.iter().any(|j| j.key == job.key && j.kind == job.kind) {
            return;
        }
        q.jobs.push(job);
        cv.notify_all();
    }

    /// Make the queued work match `jobs` exactly. In-flight work is not
    /// disturbed; a wanted job that is already running is remembered in
    /// `pending` and re-queued when it finishes, so nothing wanted is lost.
    pub fn replace(&self, jobs: Vec<Job>) {
        let (lock, cv) = &*self.shared;
        let mut q = lock.lock().unwrap();
        let before = q.jobs.len();
        q.wanted.clear();
        let mut keep = Vec::with_capacity(jobs.len());
        for j in jobs {
            let key = (j.key.clone(), j.kind);
            // `wanted` is recorded either way, and is what makes losing the job
            // safe: a job that is already running is not queued twice, and a job
            // that is queued can still be re-queued if it gives up later.
            let running = q.busy.contains(&key);
            q.wanted.insert(key, j.clone());
            if !running {
                keep.push(j);
            }
        }
        q.jobs = keep;
        // Coalescing: how much queued work a navigation threw away. High values
        // mean the user outran the decoder, which is the thing to tune.
        if before > q.jobs.len() {
            perf::count(perf::C_JOBS_DROPPED, (before - q.jobs.len()) as u64);
        }
        cv.notify_all();
    }

    /// Queued + running, for tests and diagnostics.
    pub fn outstanding(&self) -> usize {
        let q = self.shared.0.lock().unwrap();
        q.jobs.len() + q.busy.len()
    }

    /// Jobs waiting to be picked up.
    pub fn queued(&self) -> usize {
        self.shared.0.lock().unwrap().jobs.len()
    }

    /// Jobs a worker is currently running.
    pub fn running(&self) -> usize {
        self.shared.0.lock().unwrap().busy.len()
    }

    /// Wanted jobs that are neither queued nor running. Once the loader is idle
    /// this must be zero: non-zero means the viewer wants a picture that nobody
    /// is going to fetch, which is the "random images never load" failure.
    pub fn wanted_but_idle(&self) -> usize {
        let q = self.shared.0.lock().unwrap();
        q.wanted
            .keys()
            .filter(|k| {
                !q.busy.contains(*k) && !q.jobs.iter().any(|j| (j.key.clone(), j.kind) == **k)
            })
            .count()
    }
}

/// Inputs to [`plan_jobs`]. Everything the planner needs, and nothing it does
/// not, so the plan is a pure function of observable state.
pub struct Plan<'a> {
    pub files: &'a [PathBuf],
    pub index: usize,
    /// Page of `files[index]` to decode. 0 for everything but a multi-page TIFF.
    /// The preload window ignores this and prefetches page 0.
    pub current_page: u32,
    /// The other page of the current turn, in a spread. Planned at the current
    /// image's own priority so both halves arrive as a pair; a neighbour-priority
    /// decode would leave the spread half-painted.
    ///
    /// **A key, not a position**, because the plan is expressed in keys and the
    /// walk that decides whether a partner exists at all belongs to the app
    /// (`MyApp::turn_partner`) — so `Plan` is still a pure function of the state
    /// it is handed, which is the property the stranding bug was untestable
    /// without.
    pub partner: Option<ImgKey>,
    /// The partner has a preview but **no full-resolution buffer**, so the turn's
    /// other half is painted from a scaled decode until it gets one.
    ///
    /// **A flag, not a key.** The key is [`Plan::partner`] itself, so a separate
    /// `Option<ImgKey>` here could name a page the plan is not showing a partner of
    /// — the stranding failure this planner's purity exists to rule out, arrived at
    /// through a field nobody cross-checks. `false` with `partner: None` is simply
    /// not a state the app can produce: both come from one `turn_partner()` call.
    ///
    /// **A full decode, not another preview,** and the common case rather than the
    /// corner one: a source over [`FAST_FULL_MAX_PIXELS`] is decoded DCT-scaled and
    /// carries no full buffer at all, and manga scans are frequently large JPEGs.
    /// When the single-pass decode *did* hand the partner a full buffer there is
    /// nothing to ask for and the app says so — [`MyApp::needs_full`] is the answer
    /// for both halves.
    ///
    /// **Planned at [`PRIO_FULL`], unlike the partner's *preview*.** The preview is
    /// what makes a half *appear*, so both halves need it before anything is on
    /// screen and it sits at [`PRIO_CURRENT_PREVIEW`] with its counterpart. A full
    /// decode is the same refinement the cursor page's own is, so it is a peer of
    /// that one: at the current priority a 30 MB decode would outrank the cursor
    /// page's own preview — queued at 0 too, so insertion order would decide — and
    /// sit ahead of the entire preload window, to sharpen a page not yet drawn.
    pub partner_needs_full: bool,
    /// Images that already have a cached preview.
    pub cached: &'a HashSet<ImgKey>,
    /// `(key, kind)` pairs known to be undecodable this session.
    pub failed: &'a HashSet<(ImgKey, Kind)>,
    /// Exif for the current image has not been fetched yet.
    pub wants_exif: bool,
    /// The current image still needs a full-resolution decode.
    pub wants_full: bool,
    /// The current image *has* a cached preview, but it is softer than the
    /// display can show, so it must be re-decoded anyway. See [`needs_refine`].
    pub current_stale: bool,
    /// Preview texture long side, physical pixels.
    pub target_dim: u32,
    pub radius: usize,
    /// File lengths **parallel to [`Plan::files`]** — what the watcher's stamp
    /// registry already holds (`Stamp.len`), so the planner never stats. The
    /// budget is a *file-size* budget because that is what is observable
    /// before a decode exists: the decoded footprint of a neighbour that has
    /// never been decoded is exactly the thing a dims slice cannot know.
    /// `0` means unknown — a file the scanner has not stamped yet — and an
    /// unknown is admitted, the fail-open reading: the budget is calibrated
    /// to the scan the session has seen, not a guess.
    pub file_bytes: &'a [u64],
    /// The preload window's whole budget in file bytes. The window spends it
    /// outward from the cursor, nearest-first, so a closer neighbour is
    /// admitted before a farther one; a file larger than the *whole* budget
    /// is therefore not preloaded at all — the rule that keeps a 300 MP scan
    /// (1.2 GB decoded) from ever being prefetched — and skipping an
    /// oversized file opens room for its smaller contrary, because the walk
    /// continues past a skip rather than breaking. Only the window spends:
    /// the current image, its partner, the full decodes and the EXIF read are
    /// how the reader sees what they asked for, and the budget never touches
    /// those.
    pub preload_budget: u64,
}

/// Priority of the current image's preview. Every prefetch is >= 10, so
/// prefetch can never starve it.
pub const PRIO_CURRENT_PREVIEW: u32 = 0;
/// Priority of the current image's full-resolution decode.
pub const PRIO_FULL: u32 = 15;
/// Priority of the current image's exif read.
pub const PRIO_CURRENT_EXIF: u32 = 5;
/// Grid thumbnail priorities start here.
pub const PRIO_THUMB_BASE: u32 = 45;

/// The work the viewer wants for the current position, as a job list.
///
/// Pure on purpose: the stranding bug lived in the interaction between this
/// function and a queue that discarded wanted-but-busy jobs, and neither half
/// was testable. Now the plan can be asserted directly.
///
/// "Next" is queued slightly ahead of "previous" at each distance, so a step
/// forward is favoured — that is the fixed asymmetric window, not
/// direction-aware, which is a later change and must be measured separately.
///
/// And the window is **bounded by bytes** before it is bounded by distance:
/// [`Plan::preload_budget`] is spent outward nearest-first over the file
/// lengths the caller already holds, so a folder of large scans preloads
/// fewer of them while a manga folder's identical radius plans identically.
/// The budget spends on **admissions only** — a skipped oversized neighbour
/// neither consumes nor blocks a window position, and the current image, its
/// partner and the full decodes never spend it (they are why the reader is
/// here, not an optimisation on top of that).
pub fn plan_jobs(p: &Plan) -> Vec<Job> {
    let mut jobs = Vec::new();
    let n = p.files.len();
    if n == 0 {
        return jobs;
    }
    let i = p.index.min(n - 1);
    let cur = &p.files[i];
    // The current image's page. Only the current image has a non-zero one: the
    // preload window deliberately prefetches page 0 of its neighbours, because
    // prefetching a 200-page document's other 199 pages would be absurd, and
    // because you arrive at a neighbour's later pages by pressing right, at which
    // point it is the current image and gets planned then.
    let cur_key = ImgKey { path: cur.clone(), page: p.current_page };

    let need_preview = |key: &ImgKey| {
        // **An archive is never a decode job.** `goto` enters the one the cursor
        // is on, but the preload window walks its *neighbours* without consulting
        // it: a folder of pages with a `.cbz` between two of them would queue a
        // decode of the container, and the worker would sniff, fail, and write a
        // `failed` entry that is permanent for the session — so re-entering the
        // archive later would find the file poisoned by having had a neighbour.
        // The cost of getting this wrong is paid by the reader as a page that
        // refuses to open, with no error to act on.
        !is_archive_path(&key.path)
            && !p.cached.contains(key)
            && !p.failed.contains(&(key.clone(), Kind::Preview))
    };

    // A stale preview is cached *and* still wanted: the texture stays on screen
    // (so the display never blanks) while a sharper one is decoded behind it.
    // Deliberately current-image-only — refining the whole preload window costs
    // one decode per neighbour, which is the difference between 1 and 7 decodes
    // after a drag.
    if need_preview(&cur_key)
        || (p.current_stale && !p.failed.contains(&(cur_key.clone(), Kind::Preview)))
    {
        jobs.push(Job {
            key: cur_key.clone(),
            kind: Kind::Preview,
            prio: PRIO_CURRENT_PREVIEW,
            target_dim: p.target_dim,
        });
    }
    // The turn's other half, at the current image's own priority. A turn is one
    // picture to the reader, so the cursor page arriving without its partner is a
    // half-painted spread — and unlike a slow neighbour it is visibly *wrong*,
    // because the empty half is on screen beside the half that did arrive.
    //
    // Deliberately **not** gated on `current_stale`. Staleness is a claim about
    // the *current* image's texture being softer than the display, and refining
    // the partner as well would double every decode a window drag issues for
    // sharpness nobody asked for.
    if let Some(partner) = &p.partner {
        if need_preview(partner) {
            jobs.push(Job {
                key: partner.clone(),
                kind: Kind::Preview,
                prio: PRIO_CURRENT_PREVIEW,
                target_dim: p.target_dim,
            });
        }
        // ...and its full-resolution decode, so a **zoomed** turn is sharp on both
        // halves. A partner's `pv.full` is usually already set - the single-pass
        // decode hands every cached preview one - in which case `partner_needs_full`
        // is false and there is nothing here, which is why this arm is not a cost in
        // the common case. It is a cost for a source over `FAST_FULL_MAX_PIXELS`,
        // whose preview is DCT-scaled and carries no full buffer, and those are
        // ordinary manga scans.
        //
        // `target_dim: 0` like every other full job: a full decode is not asked for a
        // preview size, and a nonzero one would be a lie about what it was asked for.
        //
        // Keyed on the whole `partner` key for the reason the preview above is - a
        // partner on page 2 is a different picture from its file's page 0.
        if p.partner_needs_full && !p.failed.contains(&(partner.clone(), Kind::Full)) {
            jobs.push(Job {
                key: partner.clone(),
                kind: Kind::Full,
                prio: PRIO_FULL,
                target_dim: 0,
            });
        }
    }
    if p.wants_exif {
        jobs.push(Job {
            // Exif describes the *file*, so it is read once and not per page.
            key: ImgKey::first(cur),
            kind: Kind::Exif,
            prio: PRIO_CURRENT_EXIF,
            target_dim: 0,
        });
    }
    if p.wants_full {
        jobs.push(Job { key: cur_key.clone(), kind: Kind::Full, prio: PRIO_FULL, target_dim: 0 });
    }

    let mut seen: HashSet<usize> = HashSet::from([i]);
    // The window's byte budget, spent nearest-first. Note what a skip does
    // **not** do: it does not break the walk and it does not hold the budget.
    // An oversized next-neighbour followed by a tiny previous-then neighbour
    // plans the tiny one — each candidate is checked on its own against what
    // the window has *actually admitted* so far, which is the reading that
    // keeps "one huge file" from punching a hole through the rest of the
    // window as well.
    let mut spent: u64 = 0;
    for d in 1..=p.radius {
        for (idx, prio) in
            [((i + d) % n, 10 + 2 * d as u32), ((i + n - d % n) % n, 11 + 2 * d as u32)]
        {
            let key = ImgKey::first(&p.files[idx]);
            // The partner is skipped by **key**, not by seeding `seen` with its
            // index. `seen` is the window's dedup of *indices* and the partner is a
            // *key*, and `seen.insert` runs either way — so the window still walks
            // the same indices it always did and this only decides whether it pushes
            // one of them.
            //
            // Skipping has to happen at all, for two reasons that are both about the
            // queue rather than about cost. The partner is normally the cursor's own
            // next neighbour, so without this the plan holds two jobs with one
            // `(key, kind)` and `replace` dedupes nothing: the pool decodes the
            // partner **twice** on every navigation, the second result landing over
            // the first. And `wanted` is a map keyed by `(key, kind)`, so the pair
            // would collapse to one entry holding the *last* insert — the window's
            // priority — and a give-up re-queue would then restore the window's copy
            // rather than the partner's. One is wasted work; the other is a plan that
            // heals itself back to the wrong priority after a failure.
            //
            // Compared as whole keys, and that is what makes it right for a
            // document: **the window deliberately prefetches page 0** (see
            // `cur_key`) while the partner carries its *real* page, because
            // [`turn_partner`] hands over the key of the position it stepped to. A
            // partner on page 2 is a different image from that document's page-0
            // neighbour, so neither job duplicates the other and both are wanted.
            if seen.insert(idx) && need_preview(&key) && p.partner.as_ref() != Some(&key) {
                // **The budget is spent nearest-first and only by admissions.**
                // `spent + len > budget` skips the candidate without spending
                // anything: a skip is a decision about this window's shape,
                // never a standing charge. The alone case falls out without a
                // branch — an unbudgeted first candidate (`spent` 0) with a
                // length past the whole budget is over before it can spend.
                let len = p.file_bytes.get(idx).copied().unwrap_or(0);
                if spent.saturating_add(len) > p.preload_budget {
                    continue;
                }
                spent += len;
                jobs.push(Job { key, kind: Kind::Preview, prio, target_dim: p.target_dim });
            }
        }
    }
    jobs
}
