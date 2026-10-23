#![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::vec_init_then_push)]
use oc_compress::brotli::compress;

fn samples() -> Vec<(&'static str, Vec<u8>)> {
    let mut v: Vec<(&'static str, Vec<u8>)> = Vec::new();
    v.push(("empty", vec![]));
    v.push(("one", vec![7]));
    v.push(("hello", b"hello hello hello world".to_vec()));
    v.push(("zeros", vec![0; 100_000]));
    v.push((
        "text",
        b"The quick brown fox jumps over the lazy dog. ".repeat(500),
    ));
    let mut x = 12345u32;
    let rnd: Vec<u8> = (0..70_000)
        .map(|_| {
            x = x.wrapping_mul(1664525).wrapping_add(1013904223);
            (x >> 24) as u8
        })
        .collect();
    v.push(("random", rnd.clone()));
    let mut mixed = rnd[..5000].to_vec();
    mixed.extend(b"abcabcabc".repeat(3000));
    mixed.extend(&rnd[..5000]);
    v.push(("mixed", mixed));
    let src = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/src/inflate.rs")).unwrap();
    v.push(("source", src));
    let big: Vec<u8> = (0..6_000_000u32)
        .map(|i| (i.wrapping_mul(i) >> 9) as u8 ^ (i >> 13) as u8)
        .collect();
    v.push(("big", big));
    v
}

/// Python's `brotli` module is the reference decoder; the test is skipped where it is absent.
#[test]
fn reference_decoder_accepts_every_stream() {
    let probe = std::process::Command::new("python3")
        .args(["-c", "import brotli"])
        .output();
    if !probe.is_ok_and(|o| o.status.success()) {
        eprintln!("skipped: no python3 brotli module");
        return;
    }
    let dir = std::env::temp_dir().join(format!("oc-brotli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, data) in samples() {
        for level in [1, 5, 11] {
            let raw = dir.join(format!("{name}.raw"));
            let br = dir.join(format!("{name}.br"));
            std::fs::write(&raw, &data).unwrap();
            std::fs::write(&br, compress(&data, level)).unwrap();
            let out = std::process::Command::new("python3")
                .args([
                    "-c",
                    "import brotli,sys; sys.exit(0 if brotli.decompress(open(sys.argv[1],'rb').read())==open(sys.argv[2],'rb').read() else 3)",
                ])
                .arg(&br)
                .arg(&raw)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name} level {level}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}
