#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::print_stdout
)]
//! Decode an IVF file and print the FNV-1a hash of every shown frame (a debugging aid).
use oc_vp9::Decoder;

fn main() {
    let path = std::env::args().nth(1).expect("usage: decode_ivf FILE.ivf");
    let data = std::fs::read(path).expect("cannot read file");
    let mut at = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut dec = Decoder::default();
    let started = std::time::Instant::now();
    let mut frames = 0;
    while at + 12 <= data.len() {
        let n = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]) as usize;
        let pkt = &data[at + 12..at + 12 + n];
        at += 12 + n;
        match dec.decode(pkt) {
            Ok(pics) => {
                for p in pics {
                    let mut h = 0xcbf2_9ce4_8422_2325u64;
                    for plane in &p.planes {
                        for &s in plane {
                            let bytes = s.to_le_bytes();
                            for b in if p.bit_depth == 8 {
                                &bytes[..1]
                            } else {
                                &bytes[..]
                            } {
                                h = (h ^ u64::from(*b)).wrapping_mul(0x0000_0100_0000_01b3);
                            }
                        }
                    }
                    println!("{h:016x}");
                    frames += 1;
                }
            }
            Err(e) => eprintln!("error: {e}"),
        }
    }
    eprintln!("{frames} frames in {:?}", started.elapsed());
}
