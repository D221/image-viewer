use criterion::{Criterion, black_box, criterion_group, criterion_main};
use image_viewer::{Gallery, load_image};
use std::path::Path;

#[path = "common/bench_fixtures.rs"]
mod bench_fixtures;

// Benchmark loading the same large image with different strategies
fn bench_load_variants(c: &mut Criterion) {
    let Some(path) = bench_fixtures::bench_image() else {
        return;
    };
    let mut group = c.benchmark_group("load_variants");

    group.bench_function("downscale_1600", |b| {
        b.iter(|| {
            let d = load_image(&path, 1600).unwrap();
            black_box(d.width);
            black_box(d.height);
        })
    });

    group.bench_function("downscale_800", |b| {
        b.iter(|| {
            let d = load_image(&path, 800).unwrap();
            black_box(d.width);
            black_box(d.height);
        })
    });

    group.bench_function("full_no_downscale", |b| {
        b.iter(|| {
            let d = load_image(&path, 99999).unwrap(); // max_dim very high = no downscale
            black_box(d.width);
            black_box(d.height);
        })
    });

    group.finish();
}

fn bench_gallery_switch(c: &mut Criterion) {
    if !bench_fixtures::have_folder() {
        eprintln!("\n  skipping gallery_switch: test-images/ is empty.\n");
        return;
    }
    let mut group = c.benchmark_group("gallery_switch");
    let folder = Path::new(r"test-images");

    group.bench_function("next_prev_cycle", |b| {
        b.iter(|| {
            let mut g = Gallery::new(folder);
            for _ in 0..g.len() {
                g.next();
                black_box(&g);
            }
        })
    });

    group.finish();
}

criterion_group!(benches, bench_load_variants, bench_gallery_switch);
criterion_main!(benches);
