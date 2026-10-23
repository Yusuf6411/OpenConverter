#![allow(clippy::expect_used, clippy::indexing_slicing)]
fn main() {
    let path = std::env::args().nth(1).expect("file");
    let data = std::fs::read(path).expect("read");
    match oc_pdf::Pdf::open(data) {
        Ok(p) => {
            let pages = p.pages();
            println!("pages={} recovered={} encrypted={} version={}", pages.len(), p.recovered, p.is_encrypted(), p.version);
            for (i, pg) in pages.iter().take(3).enumerate() {
                println!("page {i}: obj {} mediabox={:?}", pg.num, p.dget_numbers(&pg.dict, "MediaBox"));
            }
        }
        Err(e) => println!("error: {e}"),
    }
}
