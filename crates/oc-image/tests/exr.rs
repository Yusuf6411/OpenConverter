//! OpenEXR against the OpenEXR library (tools/make_exr_fixtures.py).
#![allow(clippy::indexing_slicing, clippy::unwrap_used)]
use oc_core::{Limits, NoProgress};

#[test]
fn exr_matches_the_openexr_library() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/exr");
    let mut n = 0;
    let mut failures = Vec::new();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().and_then(|x| x.to_str()) != Some("exr") {
            continue;
        }
        let name = p.file_stem().unwrap().to_str().unwrap().to_string();
        let got =
            oc_image::exr::decode(&std::fs::read(&p).unwrap(), &Limits::default(), &NoProgress);
        let (img, _) = match got {
            Ok(g) => g,
            Err(e) => {
                failures.push(format!("{name}: {e}"));
                continue;
            }
        };
        let want = std::fs::read(p.with_extension("px")).unwrap();
        let meta = std::fs::read_to_string(p.with_extension("txt")).unwrap();
        let d: Vec<usize> = meta
            .split_whitespace()
            .map(|x| x.parse().unwrap())
            .collect();
        if (
            img.width as usize,
            img.height as usize,
            img.layout.channels(),
        ) != (d[0], d[1], d[2])
        {
            failures.push(format!(
                "{name}: shape {}x{}x{}",
                img.width,
                img.height,
                img.layout.channels()
            ));
            continue;
        }
        let worst = img
            .data
            .iter()
            .zip(&want)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap();
        if worst > 1 {
            failures.push(format!("{name}: off by {worst}"));
        }
        n += 1;
    }
    assert!(
        failures.is_empty(),
        "{} files differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(n >= 80, "found {n} fixtures");
}
