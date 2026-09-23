use criterion::{black_box, criterion_group, criterion_main, Criterion};
use image_viewer::{load_image, Gallery};
use std::path::Path;

fn bench_load_large_image(c: &mut Criterion) {
    let path = Path::new(r"test-images\booklet_front.jpg");
    c.bench_function("load_large_jpg_26MB", |b| {
        b.iter(|| {
            let result = load_image(path, 1600).unwrap();
            black_box(result.rgba.len());
        })
    });
}

fn bench_gallery_navigation(c: &mut Criterion) {
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
