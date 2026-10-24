//! PSD against Pillow and ImageMagick (tools/make_psd_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_core::{Limits, NoProgress};

#[test]
fn psd_matches_pillow_and_imagemagick() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/psd");
    let mut n = 0;
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if !matches!(p.extension().and_then(|x| x.to_str()), Some("psd" | "psb")) {
            continue;
        }
        let (img, _) =
            oc_image::psd::decode(&std::fs::read(&p).unwrap(), &Limits::default(), &NoProgress)
                .unwrap();
        let want = std::fs::read(p.with_extension("rgba")).unwrap();
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
        let rgba = oc_image::ops::convert_layout(&img, oc_image::Layout::Rgba, [0, 0, 0]).0;
        let bytes = if dims[2] == 16 { 2 } else { 1 };
        let got = if bytes == 2 {
            rgba.data.clone()
        } else {
            oc_image::ops::convert_depth(&rgba, oc_image::Depth::Eight)
                .0
                .data
        };
        assert_eq!(got.len(), want.len(), "{}", p.display());
        let mut bad = 0;
        for (k, (a, b)) in got
            .chunks(4 * bytes)
            .zip(want.chunks(4 * bytes))
            .enumerate()
        {
            let alpha_zero = a[4 * bytes - bytes..].iter().all(|&v| v == 0)
                && b[4 * bytes - bytes..].iter().all(|&v| v == 0);
            if a != b && !alpha_zero {
                bad += 1;
                if bad < 3 {
                    eprintln!("{} pixel {k}: {a:?} vs {b:?}", p.display());
                }
            }
        }
        assert_eq!(bad, 0, "{}", p.display());
        n += 1;
    }
    assert!(n >= 11, "found {n}");
}
