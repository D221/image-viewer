use criterion::{black_box, criterion_group, criterion_main, Criterion};
use image_viewer::{load_image, Gallery};
use std::path::Path;

#[path = "common/bench_fixtures.rs"]
mod bench_fixtures;

fn bench_load_large_image(c: &mut Criterion) {
    let Some(path) = bench_fixtures::bench_image() else {
        return;
    };
    c.bench_function("load_large_jpg", |b| {
        b.iter(|| {
            let result = load_image(&path, 1600).unwrap();
            black_box(result.rgba.len());
        })
    });
}

fn bench_gallery_navigation(c: &mut Criterion) {
    if !bench_fixtures::have_folder() {
        eprintln!("\n  skipping gallery_next_and_prev: test-images/ is empty.\n");
        return;
    }
    let gallery = Gallery::new(Path::new(r"test-images"));
    c.bench_function("gallery_next_and_prev", |b| {
        b.iter(|| {
            let mut g = gallery.clone();
            for _ in 0..gallery.len() {
                g.next();
                black_box(&g);
            }
        })
    });
}

criterion_group!(benches, bench_load_large_image, bench_gallery_navigation);
criterion_main!(benches);
