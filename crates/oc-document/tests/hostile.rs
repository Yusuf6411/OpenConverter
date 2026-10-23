//! Truncated and corrupted packages must come back as errors or documents, never panics.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{Limits, Notes};
use oc_document::markdown::{self, ReadOptions};

const MD: &str = "---\ntitle: T\n---\n# H\n\ntext *em* **st** [l](http://x.y) ![i](data:image/png;base64,iVBORw0KGgo=)\n\n- a\n  - b\n\n1. x\n\n> q\n\n```\ncode\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n# H2\n\nmore\n";

fn mutate(data: &[u8], seed: u64) -> Vec<u8> {
    let mut s = seed
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let mut v = data.to_vec();
    for _ in 0..(1 + (seed % 8)) {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let i = (s >> 33) as usize % v.len().max(1);
        if !v.is_empty() {
            v[i] ^= (s >> 20) as u8 | 1;
        }
    }
    if seed.is_multiple_of(3) {
        let cut = (s >> 17) as usize % v.len().max(1);
        v.truncate(cut);
    }
    v
}

#[test]
fn readers_do_not_panic_on_damaged_input() {
    let doc = markdown::read(MD, &ReadOptions { github: true });
    let limits = Limits::default();
    let mut n = Notes::new();
    let files: Vec<(&str, Vec<u8>)> = vec![
        ("docx", oc_document::docx_out::write(&doc, &limits).unwrap()),
        ("odt", oc_document::odt_out::write(&doc, &limits).unwrap()),
        ("epub", oc_document::epub_out::write(&doc, &limits).unwrap()),
        ("rtf", oc_document::rtf_out::write(&doc, &mut n)),
    ];
    for (kind, bytes) in &files {
        for seed in 0..150u64 {
            let m = mutate(bytes, seed);
            let mut notes = Notes::new();
            let _ = match *kind {
                "docx" => oc_document::docx_in::read(&m, &limits, &mut notes),
                "odt" => oc_document::odt_in::read(&m, &limits, &mut notes),
                "epub" => oc_document::epub_in::read(&m, &limits, &mut notes),
                _ => oc_document::rtf_in::read(&m, &limits, &mut notes),
            };
        }
    }
}

#[test]
fn rtf_deep_groups_and_huge_numbers() {
    let mut s = b"{\\rtf1".to_vec();
    s.extend(std::iter::repeat_n(b'{', 100_000));
    s.extend_from_slice(b"\\u999999999999999 \\li99999999999 x");
    let mut notes = Notes::new();
    let _ = oc_document::rtf_in::read(&s, &Limits::default(), &mut notes);
}
