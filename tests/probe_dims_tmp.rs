#[test]
fn probe_dims_tmp() {
    for f in [
        "test-images/0_booklet_inside.jpg",
        "test-images/1_booklet_front.jpg",
    ] {
        let img = image::open(f).unwrap();
        println!("{f}: {}x{}", img.width(), img.height());
    }
}
