//! The preload window's byte-budget policy: what `plan_jobs` plans as the
//! budget is spent. No fixtures and no filesystem — the planner is a pure
//! function over paths and lengths, so a synthetic length list is the whole
//! input.
//!
//! The one property the budget exists to keep: it is spent **nearest-first
//! and only by admissions** — an oversized neighbour is skipped without
//! either breaking the walk or holding the budget, so a closer (later in the
//! order-wise walk) or smaller candidate behind it can still be planned; and
//! the current image, its partner, the full decodes and the EXIF read never
//! spend it, because those are what the reader asked for, not an
//! optimisation on top.
//!
//! Golden-alignment companion: `the_budget_changes_nothing_for_small_files`
//! must hold before *any* number can be trusted — the budget's only permission
//! over the shipped plan is its calendar of large files.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use image_viewer::{plan_jobs, ImgKey, Job, Kind, Plan};

/// Seven synthetic paths sized to a small wrap-around folder.
fn files() -> Vec<PathBuf> {
    (0..7).map(|i| PathBuf::from(format!("img_{i}.jpg"))).collect()
}

/// An unbounded-planner plan, every neighbour cache-free.
fn base_plan<'a>(
    f: &'a [PathBuf],
    lens: &'a [u64],
    budget: u64,
    cached: &'a HashSet<ImgKey>,
    failed: &'a HashSet<(ImgKey, Kind)>,
) -> Plan<'a> {
    Plan {
        files: f,
        index: 2,
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
        file_bytes: lens,
        preload_budget: budget,
    }
}

/// The empty plan-caches every budget test below wants.
fn empty_caches() -> (HashSet<ImgKey>, HashSet<(ImgKey, Kind)>) {
    (HashSet::new(), HashSet::new())
}

/// The window's preview jobs, in plan order (the current image's own precede
/// them, so filter to the walk priorities).
fn window_jobs(jobs: &[Job]) -> Vec<(PathBuf, u32)> {
    jobs.iter()
        .filter(|j| j.kind == Kind::Preview && j.prio >= 10)
        .map(|j| (j.path().to_path_buf(), j.prio))
        .collect()
}

/// Names of the walked window (paths from a fixture list), unwrapped.
fn names(jobs: &[(PathBuf, u32)]) -> Vec<String> {
    jobs.iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect()
}

/// The whole point of this field: small folders plan **identically** under
/// the budget and without it. The budget's only permission is its calendar
/// of large files, and if it bites a tiny library, it is wrong.
#[test]
fn the_budget_changes_nothing_for_small_files() {
    let f = files();
    let (cached, failed) = empty_caches();
    let lens = vec![1_000_000_u64; 7]; // 1 MB each, folder of 7 MB
    let free = window_jobs(&plan_jobs(&base_plan(&f, &lens, u64::MAX, &cached, &failed)));
    let bounded =
        window_jobs(&plan_jobs(&base_plan(&f, &lens, 256 * 1024 * 1024, &cached, &failed)));
    assert_eq!(
        free, bounded,
        "a 7 MB folder must plan the same window under a 256 MB budget"
    );
}

/// Nearest-first: with room for two of three decisions, the cursor's next and
/// previous are planned before a farther neighbour is. The walk order is
/// next-d1 (`i+1`), prev-d1 (`i-1`), next-d2, prev-d2, next-d3, prev-d3 —
/// budget 22 MB, next1 8, prev1 10, next2 9, prev2 3: the 9 skips a *spent*
/// window (18), but the prev-d2 (3 MB) is still planned — a skip is not a
/// hole and it does not hold the budget, and the admission test is
/// per-candidate.
#[test]
fn the_budget_spends_nearest_first_and_a_skip_is_not_a_hole() {
    let f = files();
    let m: u64 = 1024 * 1024;
    let lens = vec![
        3 * m, // idx0 = prev d2: 18 + 3 <= 22 — planned
        10 * m, // idx1 = prev d1: admitted
        0, // idx2 = the cursor, never walked
        8 * m, // idx3 = next d1: admitted
        9 * m, // idx4 = next d2: skipped, 18 + 9 > 22
        100 * m, 100 * m, // idx5/6 = d3, priced out of the argument
    ];
    let (cached, failed) = empty_caches();
    let jobs = window_jobs(&plan_jobs(&base_plan(&f, &lens, 22 * m, &cached, &failed)));
    let got = names(&jobs);
    assert_eq!(
        got,
        vec!(
            "img_3.jpg".to_string(), // next d1, prio 10
            "img_1.jpg".to_string(), // prev d1, prio 11
            "img_0.jpg".to_string(), // prev d2, prio 13: the skip opened no hole
        ),
        "nearest-first spend, and the far small neighbour survives the skip"
    );
    assert_eq!(jobs.len(), 3);
    assert_eq!(jobs[0].1, 12, "next d1's window priority (10 + 2d)");
    assert_eq!(jobs[2].1, 15, "prev d2's window priority (11 + 2d)");
}

/// A scan larger than the whole budget is **not preloaded at all** — and its
/// skip does not take the rest of the window with it.
#[test]
fn a_file_larger_than_the_budget_is_not_preloaded_at_all() {
    let f = files();
    let m: u64 = 1024 * 1024;
    let lens = vec![
        0, 0, 0, 500 * m, // next d1: half a gig, past a 64 MB budget
        10 * m, // prev d1: admitted
        0, 0,
    ];
    let (cached, failed) = empty_caches();
    let jobs = window_jobs(&plan_jobs(&base_plan(&f, &lens, 64 * m, &cached, &failed)));
    let got = names(&jobs);
    assert!(
        !got.iter().any(|n| n == "img_3.jpg"),
        "the oversized neighbour is never preloaded: {got:?}"
    );
    assert!(
        got.iter().any(|n| n == "img_1.jpg"),
        "and the small previous neighbour is not a casualty"
    );
}

/// The budget is the preload window's rule only. With a budget of zero no
/// file is ever affordable, so the *window* empties — and the reader still
/// gets everything they asked for: the current preview, the current full,
/// the EXIF read, and the turn's partner at its own priority, none of which
/// spend a byte.
#[test]
fn a_zero_budget_never_touches_current_partner_full_or_exif() {
    let f = files();
    let lens: Vec<u64> = vec![1; 7]; // every file costs; a zero budget affords nothing
    let partner = Some(ImgKey {
        path: f[3].clone(),
        page: 0,
    });
    let cached: HashSet<ImgKey> = HashSet::new();
    let failed: HashSet<(ImgKey, Kind)> = HashSet::new();
    let jobs = plan_jobs(&Plan {
        files: &f,
        index: 2,
        current_page: 0,
        partner,
        partner_needs_full: true,
        cached: &cached,
        failed: &failed,
        wants_exif: true,
        wants_full: true,
        current_stale: false,
        target_dim: 1024,
        radius: 3,
        file_bytes: &lens,
        preload_budget: 0,
    });
    let previews_current: Vec<_> = jobs
        .iter()
        .filter(|j| {
            j.kind == Kind::Preview
                && j.path() == Path::new("img_2.jpg")
                || j.kind == Kind::Preview && j.key.page == 0 && j.path() == Path::new("img_3.jpg")
        })
        .collect();
    assert!(
        previews_current.len() >= 2,
        "current and partner previews survive a zero budget"
    );
    assert!(
        jobs.iter().any(|j| j.kind == Kind::Full && j.path() == Path::new("img_2.jpg")),
        "the current full survives the budget"
    );
    assert!(
        jobs.iter()
            .any(|j| j.kind == Kind::Full && j.path() == Path::new("img_3.jpg")),
        "the partner's full survives the budget"
    );
    assert!(
        jobs.iter().any(|j| j.kind == Kind::Exif),
        "the EXIF read survives the budget"
    );
    assert!(
        window_jobs(&jobs).is_empty(),
        "only the window empties at a zero budget"
    );
}

/// A file the scanner has not stamped yet (`file_bytes` shorter than the
/// files is the shape of the registry that has not caught up) is admitted —
/// the fail-open reading. Two shapes: a **known zero** is admitted even at a
/// fully-spent window, and a registry that answers nothing for the far
/// neighbours plans the **same walk an unlimited budget does**, because the
/// budget cannot see what it has never been told.
#[test]
fn an_unknown_file_is_admitted_and_spends_nothing() {
    let f = files();
    let m: u64 = 1024 * 1024;
    let (cached, failed) = empty_caches();
    // Budget spent exactly by the two d1 neighbours; next-d2 priced out.
    let known = vec![
        0, // idx0 = prev d2: a known zero at a fully-spent window
        10 * m, // idx1 = prev d1
        0, // idx2 = the cursor
        10 * m, // idx3 = next d1
        50 * m, // idx4 = next d2: skipped
    ];
    let jobs = window_jobs(&plan_jobs(&base_plan(&f, &known, 20 * m, &cached, &failed)));
    let got = names(&jobs);
    assert!(
        got.iter().any(|n| n == "img_0.jpg"),
        "the known-zero prev d2 is admitted at a fully-spent window: {got:?}"
    );
    let unlimited = names(&window_jobs(&plan_jobs(&base_plan(
        &f, &known, u64::MAX, &cached, &failed,
    ))));
    assert_ne!(
        got, unlimited,
        "the priced-out next d2 is a budget decision, not a coincidence"
    );

    // The registry has only caught up with the first two entries — the shape
    // of `schedule` handing a stamps map that has not walked the folder yet.
    // Every neighbour the window then walks answers unknown, admission is
    // free for all of them, and the walk plans exactly what an unlimited
    // budget plans.
    let stale = vec![10 * m, 10 * m];
    let a = window_jobs(&plan_jobs(&base_plan(&f, &stale, 20 * m, &cached, &failed)));
    let b = window_jobs(&plan_jobs(&base_plan(&f, &stale, u64::MAX, &cached, &failed)));
    assert_eq!(
        a, b,
        "an un-stamped neighbour is invisible to the budget, so the walk is the unbounded one"
    );
}
