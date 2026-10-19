//! ICNS against Pillow (tools/make_icns_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_core::{Limits, NoProgress};

#[test]
fn icns_matches_pillow() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/icns");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("icns") {
            continue;
        }
        let (img, _) =
            oc_image::icns::decode(&std::fs::read(&p).unwrap(), &Limits::default(), &NoProgress)
                .unwrap();
        let want = std::fs::read(p.with_extension("rgba")).unwrap();
        let dims: Vec<u32> = std::fs::read_to_string(p.with_extension("txt"))
            .unwrap()
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!(
            (img.width, img.height),
            (dims[0], dims[1]),
            "{}",
            p.display()
        );
        let rgba = oc_image::ops::convert_layout(&img, oc_image::Layout::Rgba, [0, 0, 0]).0;
        for (k, (a, b)) in rgba.data.chunks(4).zip(want.chunks(4)).enumerate() {
            assert!(
                a == b || (a[3] == 0 && b[3] == 0),
                "{} pixel {k}: {a:?} vs {b:?}",
                p.display()
            );
        }
        n += 1;
    }
    assert!(n >= 7, "found {n} fixtures");
}
