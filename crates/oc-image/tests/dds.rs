//! DDS against Pillow's decoder (tools/make_dds_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_core::Limits;

#[test]
fn dds_matches_pillow() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/dds");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("dds") {
            continue;
        }
        let (img, _) =
            oc_image::dds::decode(&std::fs::read(&p).unwrap(), &Limits::default()).unwrap();
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
        let mut bad = 0;
        for (k, (a, b)) in rgba.data.chunks(4).zip(want.chunks(4)).enumerate() {
            if !(a == b || (a[3] == 0 && b[3] == 0)) {
                bad += 1;
                if bad < 3 {
                    eprintln!("{} pixel {k}: {a:?} vs {b:?}", p.display());
                }
            }
        }
        assert_eq!(bad, 0, "{}", p.display());
        n += 1;
    }
    assert!(n >= 24, "found {n} fixtures");
}
