//! `dec <stream> [globals]` prints the decoded page as text (`#` black, `.` white).
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let stream = std::fs::read(&a[1]).unwrap();
    let glob = a.get(2).map(|p| std::fs::read(p).unwrap());
    let env = oc_jbig2::Env::new(None, Limits::default());
    match oc_jbig2::decode_embedded(glob.as_deref(), &stream, &env) {
        Ok(p) => {
            let rb = p.width.div_ceil(8);
            for y in 0..p.height {
                let row: String = (0..p.width)
                    .map(|x| {
                        if p.bits[y * rb + x / 8] >> (7 - x % 8) & 1 == 1 {
                            '#'
                        } else {
                            '.'
                        }
                    })
                    .collect();
                println!("{row}");
            }
        }
        Err(e) => eprintln!("error: {e}"),
    }
}
