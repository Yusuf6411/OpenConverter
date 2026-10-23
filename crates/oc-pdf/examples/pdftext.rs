//! Prints a PDF's text as txt, md or html (used by tools/pdf_interop.py).
#![allow(clippy::expect_used, clippy::indexing_slicing)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let inp = args.get(1).expect("file");
    let mode = args.get(2).map_or("txt", String::as_str);
    let pw = args.get(3).map(|s| s.as_bytes().to_vec());
    let mut pdf = oc_pdf::Pdf::open(std::fs::read(inp).expect("read")).expect("open");
    if pdf.needs_password() {
        pdf.authenticate(pw.as_deref().unwrap_or(b""));
    }
    let fonts = oc_font::db::FontDb::system();
    let set = oc_pdf::interp::Settings {
        fonts: Some(&fonts),
        annotations: false,
    };
    let pages: Vec<oc_pdf::text::PageText> = pdf
        .pages()
        .iter()
        .map(|p| oc_pdf::text::extract_page(&oc_pdf::interp::interpret_page(&pdf, p, &set)))
        .collect();
    let out = match mode {
        "md" => oc_pdf::text::to_markdown(&pages),
        "html" => oc_pdf::text::to_html(&pages, "document"),
        _ => oc_pdf::text::to_text(&pages),
    };
    print!("{out}");
}
