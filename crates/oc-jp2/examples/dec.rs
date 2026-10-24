#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::print_stdout,
    clippy::print_stderr
)]
//! Decodes a JP2/J2K file to raw interleaved samples on stdout (8-bit bytes or 16-bit big endian).
use std::io::Write;

fn main() {
    let path = std::env::args().nth(1).expect("input");
    let data = std::fs::read(path).expect("read");
    if std::env::args().nth(2).as_deref() == Some("planes") {
        let mut out = std::io::stdout().lock();
        for (_, _, d) in oc_jp2::decode_components(&data, &oc_core::Limits::default()).unwrap() {
            out.write_all(&d.iter().map(|&v| v as u8).collect::<Vec<u8>>())
                .unwrap();
        }
        return;
    }
    let p = oc_jp2::decode(&data, &oc_core::Limits::default()).expect("decode");
    eprintln!(
        "{}x{} ch={} 16bit={}",
        p.width, p.height, p.channels, p.sixteen
    );
    let mut out = std::io::stdout().lock();
    let mut b = Vec::new();
    for &v in &p.samples {
        if p.sixteen {
            b.extend_from_slice(&v.to_be_bytes());
        } else {
            b.push(v as u8);
        }
    }
    out.write_all(&b).unwrap();
}
