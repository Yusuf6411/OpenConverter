//! Radiance HDR against an independent reader of the format (tools/make_hdr_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_core::Limits;

#[test]
fn hdr_matches_the_reference_reader() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/hdr");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("hdr") {
            continue;
        }
        let (img, _) = oc_image::hdr::decode(&std::fs::read(&p).unwrap(), &Limits::default())
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let want = std::fs::read(p.with_extension("rgb")).unwrap();
        let meta = std::fs::read_to_string(p.with_extension("txt")).unwrap();
        let dims: Vec<u32> = meta
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        assert_eq!(
            (img.width, img.height),
            (dims[0], dims[1]),
            "{}",
            p.display()
        );
        assert_eq!(img.data.len(), want.len(), "{}", p.display());
        // The reference works in float64, we use a table for the sRGB curve: one code value apart is fine.
        let worst = img
            .data
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        assert!(worst <= 1, "{}: off by {worst}", p.display());
        n += 1;
    }
    assert!(n >= 20, "found {n} fixtures");
}
