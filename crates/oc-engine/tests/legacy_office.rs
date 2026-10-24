//! Legacy binary Office files (compound files): content checks and damaged-input behaviour.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::{Limits, Notes};

const DOC: &[u8] = include_bytes!("fixtures/sample.doc");
const PPT: &[u8] = include_bytes!("fixtures/sample.ppt");
const XLS: &[u8] = include_bytes!("fixtures/sample.xls");

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
        v[i] ^= (s >> 20) as u8 | 1;
    }
    if seed.is_multiple_of(3) {
        v.truncate((s >> 17) as usize % v.len().max(1));
    }
    v
}

fn flatten(doc: &oc_document::Document) -> String {
    format!("{:?}", doc.blocks)
}

#[test]
fn doc_text_is_read() {
    let mut n = Notes::new();
    let d = oc_document::doc_in::read(DOC, &Limits::default(), &mut n).unwrap();
    let t = flatten(&d);
    assert!(t.contains("Heading One") && t.contains("nested b") && t.contains("example.com"));
}

#[test]
fn ppt_slides_are_read() {
    let mut n = Notes::new();
    let d = oc_document::ppt_in::read(PPT, &Limits::default(), &mut n).unwrap();
    let t = flatten(&d);
    assert!(t.contains("Hello") && t.contains("two"));
}

#[test]
fn xls_cells_are_read() {
    let mut n = Notes::new();
    let b = oc_sheet::xls::read(XLS, &Limits::default(), true, &mut n);
    assert!(b.is_ok());
}

#[test]
fn damaged_compound_files_never_panic() {
    let limits = Limits::default();
    for seed in 0..300u64 {
        let mut n = Notes::new();
        let _ = oc_document::doc_in::read(&mutate(DOC, seed), &limits, &mut n);
        let _ = oc_document::ppt_in::read(&mutate(PPT, seed), &limits, &mut n);
        let _ = oc_sheet::xls::read(&mutate(XLS, seed), &limits, true, &mut n);
    }
}
