//! Damaged streams must produce errors or pictures, never a panic or a runaway allocation.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;
use std::path::Path;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

fn packets(name: &str) -> Vec<Vec<u8>> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let data = std::fs::read(dir.join(name)).unwrap();
    let (streams, _) = oc_audio::ogg::read(&data).unwrap();
    streams[0].packets.iter().map(|p| p.data.clone()).collect()
}

#[test]
fn damaged_data_packets() {
    let mut rng = Lcg(7);
    for name in ["420_testsrc.ogv", "444_testsrc.ogv", "420_life.ogv"] {
        let pk = packets(name);
        for round in 0..60 {
            let h = oc_theora::parse_headers(&pk[0], &pk[1], &pk[2]).unwrap();
            let mut dec = oc_theora::decoder(h, &Limits::default()).unwrap();
            for p in &pk[3..] {
                let mut p = p.clone();
                match round % 3 {
                    0 => {
                        for _ in 0..4 {
                            let i = rng.next() as usize % p.len();
                            p[i] = rng.next() as u8;
                        }
                    }
                    1 => p.truncate(rng.next() as usize % p.len()),
                    _ => {
                        let i = rng.next() as usize % p.len();
                        p[i] ^= 1 << (rng.next() % 8);
                    }
                }
                let _ = dec.decode(&p);
            }
        }
    }
}

#[test]
fn damaged_headers() {
    let mut rng = Lcg(11);
    let pk = packets("420_testsrc.ogv");
    for k in 0..3 {
        for _ in 0..300 {
            let mut hdr: Vec<Vec<u8>> = pk[..3].to_vec();
            let i = rng.next() as usize % hdr[k].len();
            hdr[k][i] = rng.next() as u8;
            if rng.next().is_multiple_of(4) {
                let n = rng.next() as usize % hdr[k].len();
                hdr[k].truncate(n);
            }
            if let Ok(h) = oc_theora::parse_headers(&hdr[0], &hdr[1], &hdr[2]) {
                let limits = Limits {
                    max_pixels: 1 << 22,
                    ..Limits::default()
                };
                if let Ok(mut dec) = oc_theora::decoder(h, &limits) {
                    let _ = dec.decode(&pk[3]);
                }
            }
        }
    }
}

#[test]
fn bad_first_packets() {
    let pk = packets("420_testsrc.ogv");
    let h = oc_theora::parse_headers(&pk[0], &pk[1], &pk[2]).unwrap();
    let mut dec = oc_theora::decoder(h, &Limits::default()).unwrap();
    // An empty packet before any frame, and an inter frame first.
    assert!(dec.decode(&[]).is_err());
    assert!(dec.decode(&[0x40, 0, 0, 0]).is_err());
    assert!(dec.decode(&pk[0]).is_err());
}

/// `THEORA_FILE=big.ogv cargo test --release -p oc-theora -- --ignored --nocapture speed`
#[test]
#[ignore]
fn speed() {
    let Ok(path) = std::env::var("THEORA_FILE") else {
        return;
    };
    let data = std::fs::read(path).unwrap();
    let (streams, _) = oc_audio::ogg::read(&data).unwrap();
    let pk = &streams[0].packets;
    let h = oc_theora::parse_headers(&pk[0].data, &pk[1].data, &pk[2].data).unwrap();
    let mut dec = oc_theora::decoder(h, &Limits::default()).unwrap();
    let t = std::time::Instant::now();
    let mut n = 0;
    let mut px = 0usize;
    for p in &pk[3..] {
        let f = dec.decode(&p.data).unwrap();
        px += f.width * f.height;
        n += 1;
    }
    let s = t.elapsed().as_secs_f64();
    eprintln!(
        "{n} frames in {s:.2}s: {:.1} fps, {:.1} Mpixel/s",
        f64::from(n) / s,
        px as f64 / s / 1e6
    );
}
