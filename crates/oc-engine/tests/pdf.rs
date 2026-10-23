//! PDF routes: pages to pictures, text, page ZIPs, page edits, and pictures to PDF.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::NoProgress;
use oc_engine::{ConvertRequest, Engine};
use std::path::PathBuf;

fn run(
    tag: &str,
    input: &[u8],
    name: &str,
    to: &str,
    opts: &[(&str, &str)],
) -> Result<Vec<u8>, String> {
    let d: PathBuf = std::env::temp_dir().join(format!("oc-pdf-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let i = d.join(name);
    let o = d.join(format!("out.{to}"));
    std::fs::write(&i, input).unwrap();
    let e = Engine::new();
    let raw: Vec<(String, String)> = opts
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    let options = e
        .parse_options_for(&i, &o, &raw)
        .map_err(|e| e.to_string())?;
    let r = e.convert(
        &ConvertRequest {
            input: i,
            output: o.clone(),
            overwrite: true,
            display_name: None,
            options,
        },
        &NoProgress,
    );
    let out = r
        .map(|_| std::fs::read(&o).unwrap())
        .map_err(|e| e.to_string());
    let _ = std::fs::remove_dir_all(d);
    out
}

/// A one-page PDF with the text "Hello PDF" (hand-written, so the test needs no fixture).
fn tiny() -> Vec<u8> {
    let stream = "BT /F1 24 Tf 20 50 Td (Hello PDF) Tj ET";
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{}\nendstream", stream.len(), stream),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offs = Vec::new();
    for (n, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{}\nendobj\n", n + 1, o).as_bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

#[test]
fn pdf_to_png_uses_page_size_and_dpi() {
    let p = run("a", &tiny(), "in.pdf", "png", &[("dpi", "144")]).unwrap();
    assert_eq!(&p[1..4], b"PNG");
    let w = u32::from_be_bytes(p[16..20].try_into().unwrap());
    let h = u32::from_be_bytes(p[20..24].try_into().unwrap());
    assert_eq!((w, h), (400, 200));
}

#[test]
fn pdf_to_text_finds_the_words() {
    let t = run("b", &tiny(), "in.pdf", "txt", &[]).unwrap();
    assert!(String::from_utf8(t).unwrap().contains("Hello PDF"));
}

#[test]
fn pdf_to_pdf_encrypts_and_needs_the_password() {
    let enc = run("c", &tiny(), "in.pdf", "pdf", &[("password", "pw")]).unwrap();
    let err = run("d", &enc, "in.pdf", "txt", &[]).unwrap_err();
    assert!(err.contains("password"), "{err}");
    let t = run("e", &enc, "in.pdf", "txt", &[("source_password", "pw")]).unwrap();
    assert!(String::from_utf8(t).unwrap().contains("Hello PDF"));
}

#[test]
fn picture_round_trips_through_pdf_and_zip() {
    let png = run("f", &tiny(), "in.pdf", "png", &[]).unwrap();
    let pdf = run("g", &png, "in.png", "pdf", &[]).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    let zip = run(
        "h",
        &tiny(),
        "in.pdf",
        "pages.zip",
        &[("page_format", "txt")],
    )
    .unwrap();
    assert_eq!(&zip[..2], b"PK");
}

#[test]
fn bad_page_selection_is_refused() {
    assert!(run("i", &tiny(), "in.pdf", "png", &[("pages", "9")]).is_err());
}

#[test]
fn garbage_is_not_a_pdf() {
    assert!(run("j", b"%PDF-1.4 nonsense", "in.pdf", "txt", &[]).is_err());
}
