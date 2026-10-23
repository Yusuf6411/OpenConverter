//! Debug helper for the writer: `pdfops images out.pdf a.jpg b.png`, `pdfops edit out.pdf in.pdf pages rotate [pw]`.
#![allow(clippy::expect_used, clippy::indexing_slicing)]
fn main() {
    let a: Vec<String> = std::env::args().collect();
    match a[1].as_str() {
        "images" => {
            let mut srcs = Vec::new();
            for f in &a[3..] {
                let d = std::fs::read(f).expect("read");
                if d.starts_with(&[0xFF, 0xD8]) {
                    srcs.push(oc_pdf::writer::Source::Jpeg(d, None));
                } else {
                    let (img, _) = oc_image::png::decode(&d, &oc_core::Limits::default(), &oc_core::NoProgress).expect("png");
                    srcs.push(oc_pdf::writer::Source::Raster(img));
                }
            }
            let out = oc_pdf::writer::images_to_pdf(srcs, &oc_pdf::writer::ImageOptions::default()).expect("write");
            std::fs::write(&a[2], out).expect("write");
        }
        "edit" => {
            let mut pdf = oc_pdf::Pdf::open(std::fs::read(&a[3]).expect("read")).expect("open");
            pdf.authenticate(b"");
            let n = pdf.pages().len();
            let pages = oc_pdf::edit::parse_pages(&a[4], n).expect("pages");
            let edit = oc_pdf::edit::Edit { rotate: a[5].parse().unwrap_or(0), compress: true, encrypt: a.get(6).map(|p| (p.as_bytes().to_vec(), p.as_bytes().to_vec())), ..Default::default() };
            let out = oc_pdf::edit::assemble(&[oc_pdf::edit::Part { pdf: &pdf, pages }], &edit).expect("assemble");
            for n in &out.notes {
                eprintln!("note: {n}");
            }
            std::fs::write(&a[2], out.data).expect("write");
        }
        _ => {}
    }
}
