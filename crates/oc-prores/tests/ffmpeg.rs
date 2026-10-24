//! Frames encoded by ffmpeg's `prores_ks` and decoded by ffmpeg's `prores`, as planar 10-bit
//! little-endian (`.yuv`); ours must agree to within one level (the IDCTs differ in rounding).
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::Limits;

fn frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 8 <= data.len() {
        let n = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        out.push(&data[at..at + n]);
        at += n;
    }
    out
}

#[allow(clippy::chunks_exact_to_as_chunks)]
fn le16(b: &[u8]) -> Vec<u16> {
    b.chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn check(name: &str, alpha: bool) {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/");
    let coded = std::fs::read(format!("{dir}{name}.prores")).unwrap();
    let reference = le16(&std::fs::read(format!("{dir}{name}.yuv")).unwrap());
    let fs = frames(&coded);
    assert!(!fs.is_empty());
    let mut at = 0;
    let (mut worst, mut total) = (0i32, 0usize);
    for f in &fs {
        let d = oc_prores::decode(f, &Limits::default()).unwrap();
        let (cw, ch) = (d.chroma_width, d.height);
        let mut planes = vec![&d.y, &d.cb, &d.cr];
        let a;
        if alpha {
            a = d.alpha.clone().unwrap();
            planes.push(&a);
        }
        for (i, p) in planes.iter().enumerate() {
            let n = if i == 0 || i == 3 {
                d.width * d.height
            } else {
                cw * ch
            };
            for (k, &v) in p.iter().enumerate() {
                let want = i32::from(reference[at + k]);
                // ffmpeg hands out 16-bit alpha shifted down to the 10-bit pixel format
                let v = if i == 3 { v >> (d.alpha_bits - 10) } else { v };
                let diff = (i32::from(v) - want).abs();
                if i == 3 {
                    assert_eq!(diff, 0, "alpha differs at {k}: {v} vs {want}");
                }
                worst = worst.max(diff);
                total += usize::from(diff != 0);
            }
            at += n;
        }
    }
    assert_eq!(at, reference.len(), "{name}: sample count");
    eprintln!("{name}: worst {worst}, {total} samples differ");
    assert!(worst <= 1, "{name}: worst difference {worst}");
}

#[test]
fn profiles() {
    for n in ["prof0", "prof1", "prof2", "prof3"] {
        check(n, false);
    }
}

#[test]
fn four_four_four() {
    check("yuv444", false);
}

#[test]
fn alpha() {
    check("alpha", true);
}

#[test]
fn interlaced() {
    check("il_tff", false);
    check("il_bff", false);
}
