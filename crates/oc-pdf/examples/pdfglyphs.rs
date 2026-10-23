//! Debug helper: lists the glyphs of each text run on page 1.
#![allow(clippy::expect_used, clippy::indexing_slicing)]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    let pdf = oc_pdf::Pdf::open(std::fs::read(args.get(1).expect("file")).expect("read")).expect("open");
    let pages = pdf.pages();
    let fonts = oc_font::db::FontDb::system();
    let set = oc_pdf::interp::Settings { fonts: Some(&fonts), annotations: false };
    let p = oc_pdf::interp::interpret_page(&pdf, &pages[0], &set);
    for op in &p.ops {
        if let oc_pdf::ops::Op::Text(r) = op {
            let s: Vec<String> = r.glyphs.iter().map(|g| format!("{:?}@{:.1},{:.1}+{:.1}", g.unicode, g.origin.x, g.origin.y, g.advance)).collect();
            println!("{} size={:.1} mode={} {}", r.font.name, r.size, r.mode, s.join(" "));
        }
    }
}
