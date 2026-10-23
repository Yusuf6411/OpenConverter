#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
//! WOFF2 files written by fontTools (with the glyf, loca and hmtx transforms) read back to the
//! same glyphs, metrics and tables as the TrueType files they came from.
use oc_core::Limits;
use oc_font::woff2::woff2_to_sfnt;
use oc_font::{outline, Font};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn same(name: &str) {
    let ttf = data(&format!("{name}.ttf"));
    let w2 = data(&format!("{name}.woff2"));
    let out = woff2_to_sfnt(&w2, 0, &Limits::default()).unwrap();
    let a = Font::parse(&ttf).unwrap();
    let b = Font::parse(&out).unwrap();
    let n = u16::from_be_bytes([a.table(b"maxp").unwrap()[4], a.table(b"maxp").unwrap()[5]]);
    assert!(n > 5);
    for gid in 0..n {
        assert_eq!(
            format!("{:?}", outline(&a, gid)),
            format!("{:?}", outline(&b, gid)),
            "{name}: glyph {gid}"
        );
        assert_eq!(a.advance(gid), b.advance(gid), "{name}: advance {gid}");
        assert_eq!(
            a.left_side_bearing(gid),
            b.left_side_bearing(gid),
            "{name}: lsb {gid}"
        );
    }
    for t in [
        b"cmap", b"head", b"hhea", b"maxp", b"name", b"post", b"OS/2",
    ] {
        if let (Some(x), Some(y)) = (a.table(t), b.table(t)) {
            if t != b"head" {
                assert_eq!(x, y, "{name}: table {}", String::from_utf8_lossy(t));
            }
        }
    }
    assert_eq!(a.unicode_map(), b.unicode_map());
}

#[test]
fn hinted_truetype_with_composites() {
    same("dejavu");
}

#[test]
fn unhinted_truetype() {
    same("crimson");
}

#[test]
fn damaged_files_fail_cleanly() {
    let w2 = data("dejavu.woff2");
    assert!(woff2_to_sfnt(&w2[..w2.len() / 2], 0, &Limits::default()).is_err());
    let mut x = 3u32;
    for _ in 0..300 {
        let mut b = w2.clone();
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        let i = (x >> 8) as usize % b.len();
        b[i] ^= 1 << (x % 8);
        let _ = woff2_to_sfnt(&b, 0, &Limits::default());
    }
}

#[test]
fn own_woff2_reads_back() {
    let ttf = data("dejavu.ttf");
    let w2 = oc_font::build::sfnt_to_woff2(&ttf, 5).unwrap();
    let out = woff2_to_sfnt(&w2, 0, &Limits::default()).unwrap();
    let (a, b) = (Font::parse(&ttf).unwrap(), Font::parse(&out).unwrap());
    assert_eq!(a.unicode_map(), b.unicode_map());
}
