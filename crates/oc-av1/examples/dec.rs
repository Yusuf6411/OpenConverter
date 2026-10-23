#![allow(clippy::expect_used, clippy::unwrap_used, clippy::print_stdout, clippy::print_stderr)]
//! Decodes an AV1 OBU stream to raw planar YUV on stdout (8-bit as bytes, deeper as 16-bit LE).
use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).expect("input.obu");
    let data = std::fs::read(path).expect("read");
    let mut dec = oc_av1::Decoder::new();
    let pics = dec.decode(&data).expect("decode");
    let mut out = std::io::stdout().lock();
    for p in pics {
        for pl in &p.planes {
            if p.bit_depth == 8 {
                let b: Vec<u8> = pl.iter().map(|&v| v as u8).collect();
                out.write_all(&b).unwrap();
            } else {
                for &v in pl {
                    out.write_all(&v.to_le_bytes()).unwrap();
                }
            }
        }
    }
}
