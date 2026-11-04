//! The turn's other half
//!
//! Manga mode shows a turn as one picture: either a single page or two side by
//! side. So the two halves are one unit of work to the reader, and the planner
//! gives the second half the current image's own priority rather than a
//! neighbour's — a partner that arrives on its own time is a spread that paints
//! one page and then the other, which is more visible than a slow first page
//! because the gap is *on screen* next to something that already arrived.
//!
//! Three files throughout, so the whole gallery sits inside the default preload
//! radius and the cursor page's partner is also the window's nearest "next" —
//! the only case where planning the partner and prefetching it can collide.

use image_viewer::{ImgKey, Kind, Plan, plan_jobs};
use std::collections::HashSet;
use std::path::PathBuf;

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
    let partner = ImgKey { path: f[1].clone(), page: 2 };
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

    let listed: Vec<(ImgKey, Kind, u32, u32)> =
        plan_jobs(&p).iter().map(|j| (j.key.clone(), j.kind, j.prio, j.target_dim)).collect();
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
    let partner = ImgKey { path: f[1].clone(), page: 2 };
    let mut p = plan(&f, 0, &cached, &failed);
    p.partner = Some(partner.clone());
    p.partner_needs_full = true;
    let jobs = plan_jobs(&p);

    let full = jobs
        .iter()
        .find(|j| j.kind == Kind::Full)
        .expect("the partner's half is on screen, so it must be sharpened too");
    assert_eq!(full.key, partner, "planned under the whole key it was given, page number included");
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
