//! JBIG2 streams with MMR (T.6) coded data, decoded through the fax decoder of this crate.
//! The reference pages come from the `oc-jbig2` test data (MuPDF / pdfium agreed on them).
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
use std::path::Path;

#[test]
fn mmr_streams_match_references() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../oc-jbig2/tests/data");
    let mut seen = 0;
    for en in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = en.path();
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        if p.extension().is_none_or(|x| x != "jb2") || !name.contains("mmr") {
            continue;
        }
        let stream = std::fs::read(&p).unwrap();
        let glob = std::fs::read(p.with_extension("glob")).ok();
        let want = std::fs::read(p.with_extension("ref")).unwrap();
        let (w, h) = (
            u32::from_be_bytes(want[0..4].try_into().unwrap()) as usize,
            u32::from_be_bytes(want[4..8].try_into().unwrap()) as usize,
        );
        let (img, _) =
            oc_image::jbig2::decode_embedded(glob.as_deref(), &stream, &Limits::default())
                .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((img.width as usize, img.height as usize), (w, h), "{name}");
        let rb = w.div_ceil(8);
        for y in 0..h {
            for x in 0..w {
                let black = want[8 + y * rb + x / 8] >> (7 - x % 8) & 1 == 1;
                assert_eq!(img.data[y * w + x] == 0, black, "{name} at {x},{y}");
            }
        }
        seen += 1;
    }
    assert_eq!(seen, 3);
}
