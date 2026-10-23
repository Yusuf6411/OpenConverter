//! Renders one PDF page to a binary PPM over white (used by tools/pdf_interop.py).
#![allow(clippy::expect_used, clippy::indexing_slicing, clippy::chunks_exact_to_as_chunks)]
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(inp), Some(out)) = (args.get(1), args.get(2)) else { return };
    let page_no: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
    let dpi: f64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(72.0);
    let pw = args.get(5).map(|s| s.as_bytes().to_vec());
    let data = std::fs::read(inp).expect("read");
    let mut pdf = match oc_pdf::Pdf::open(data) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return;
        }
    };
    if pdf.needs_password() && !pdf.authenticate(pw.as_deref().unwrap_or(b"")) {
        eprintln!("error: password");
        return;
    }
    let pages = pdf.pages();
    let Some(page) = pages.get(page_no.saturating_sub(1)) else {
        eprintln!("error: no such page");
        return;
    };
    let fonts = oc_font::db::FontDb::system();
    let set = oc_pdf::interp::Settings { fonts: Some(&fonts), annotations: true };
    let p = oc_pdf::interp::interpret_page(&pdf, page, &set);
    let opts = oc_pdf::render::RenderOptions { scale: dpi / 72.0, background: Some([1.0, 1.0, 1.0]) };
    match oc_pdf::render::render(&p, &opts, &oc_core::Limits::default()) {
        Ok(cv) => {
            let rgba = cv.to_rgba8(Some([1.0, 1.0, 1.0]));
            let mut f = std::fs::File::create(out).expect("create");
            let _ = write!(f, "P6\n{} {}\n255\n", cv.width, cv.height);
            let rgb: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
            let _ = f.write_all(&rgb);
            for n in &p.notes {
                eprintln!("note: {n}");
            }
        }
        Err(e) => eprintln!("error: {e}"),
    }
}
