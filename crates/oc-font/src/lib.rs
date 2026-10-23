//! Font parsing: TrueType and OpenType (glyf and CFF) outlines, character maps, metrics,
//! WOFF, and conversions between the containers.
#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks,
    clippy::needless_range_loop,
    clippy::type_complexity
)]

pub mod build;
pub mod cff;
pub mod convert;
pub mod db;
pub mod glyf;
pub mod post;
pub mod reader;
pub mod render;
pub mod sfnt;
pub mod woff2;

pub use sfnt::Font;

use oc_raster::Path;

/// Outline of a glyph in font units, y up.
pub fn outline(font: &Font, gid: u16) -> Option<Path> {
    if font.is_cff {
        font.cff_table()?.glyph(gid as usize).map(|g| g.path)
    } else {
        glyf::outline(font, gid)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::build::{build_sfnt, sfnt_to_woff, sfnt_to_woff2, woff_to_sfnt};
    use crate::reader::{put_u16, put_u32};
    use oc_core::Limits;

    /// A two-glyph TrueType font: `.notdef` is empty, `A` is a 600x700 box with a notch,
    /// drawn with one off-curve point so quadratic handling is covered.
    fn tiny_font() -> Vec<u8> {
        let mut glyf = Vec::new();
        // glyph 1: contour of 5 points: (0,0) (600,0) (600,700) off(300,900) (0,700)
        let pts: [(i16, i16, u8); 5] = [
            (0, 0, 1),
            (600, 0, 1),
            (600, 700, 1),
            (300, 900, 0),
            (0, 700, 1),
        ];
        put_u16(&mut glyf, 1);
        for v in [0i16, 0, 600, 900] {
            put_u16(&mut glyf, v as u16);
        }
        put_u16(&mut glyf, 4); // end point of contour 0
        put_u16(&mut glyf, 0); // instruction length
        for p in &pts {
            glyf.push(p.2);
        }
        let mut prev = 0i16;
        for p in &pts {
            put_u16(&mut glyf, (p.0 - prev) as u16);
            prev = p.0;
        }
        prev = 0;
        for p in &pts {
            put_u16(&mut glyf, (p.1 - prev) as u16);
            prev = p.1;
        }
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
        let mut loca = Vec::new();
        for v in [0u32, 0, glyf.len() as u32] {
            put_u32(&mut loca, v);
        }
        let mut head = vec![0u8; 54];
        head[..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
        head[18..20].copy_from_slice(&1000u16.to_be_bytes());
        head[50..52].copy_from_slice(&1u16.to_be_bytes());
        let mut maxp = Vec::new();
        put_u32(&mut maxp, 0x0000_5000);
        put_u16(&mut maxp, 2);
        let mut hhea = vec![0u8; 36];
        hhea[..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
        hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
        hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
        hhea[34..36].copy_from_slice(&2u16.to_be_bytes());
        let mut hmtx = Vec::new();
        for (adv, lsb) in [(500u16, 0u16), (700, 0)] {
            put_u16(&mut hmtx, adv);
            put_u16(&mut hmtx, lsb);
        }
        // cmap: format 4 mapping 'A' (65) to glyph 1
        let mut sub = Vec::new();
        for v in [4u16, 32, 0, 4, 4, 1, 0, 65, 0xFFFF, 0, 65, 0xFFFF] {
            put_u16(&mut sub, v);
        }
        // idDelta: 'A' -> gid 1 (delta = 1 - 65), 0xFFFF -> 1; then two zero range offsets
        put_u16(&mut sub, 1u16.wrapping_sub(65));
        put_u16(&mut sub, 1);
        put_u16(&mut sub, 0);
        put_u16(&mut sub, 0);
        let sub_len = sub.len() as u16;
        sub[2..4].copy_from_slice(&sub_len.to_be_bytes());
        let mut cmap = Vec::new();
        for v in [0u16, 1, 3, 1] {
            put_u16(&mut cmap, v);
        }
        put_u32(&mut cmap, 12);
        cmap.extend_from_slice(&sub);
        let mut tables = vec![
            (*b"glyf", glyf),
            (*b"loca", loca),
            (*b"head", head),
            (*b"maxp", maxp),
            (*b"hhea", hhea),
            (*b"hmtx", hmtx),
            (*b"cmap", cmap),
        ];
        build_sfnt([0, 1, 0, 0], &mut tables)
    }

    fn area(p: &Path) -> f64 {
        let mut t = 0.0;
        for poly in p.flatten(0.01) {
            let n = poly.pts.len();
            for i in 0..n {
                let (a, b) = (poly.pts[i], poly.pts[(i + 1) % n]);
                t += a.x * b.y - b.x * a.y;
            }
        }
        t / 2.0
    }

    #[test]
    fn parses_maps_and_outlines() {
        let f = Font::parse(&tiny_font()).unwrap();
        assert_eq!((f.num_glyphs, f.units_per_em), (2, 1000));
        assert_eq!(f.glyph_for_char('A'), Some(1));
        assert_eq!(f.glyph_for_char('B'), None);
        assert_eq!(f.advance(1), 700);
        let a = area(&outline(&f, 1).unwrap());
        // box 600x700 plus a quadratic cap: 2/3 * 600 * 100 (the curve peaks halfway to the control point)
        assert!((a - 460_000.0).abs() < 50.0, "{a}");
    }

    #[test]
    fn woff_round_trip_is_exact() {
        let sf = tiny_font();
        let w = sfnt_to_woff(&sf, 9).unwrap();
        assert_eq!(&w[..4], b"wOFF");
        let back = woff_to_sfnt(&w, &Limits::default()).unwrap();
        assert_eq!(back, sf);
    }

    #[test]
    fn woff2_has_a_valid_header_and_python_reads_it() {
        let sf = tiny_font();
        let w = sfnt_to_woff2(&sf, 5).unwrap();
        assert_eq!(&w[..4], b"wOF2");
        assert_eq!(
            u32::from_be_bytes([w[8], w[9], w[10], w[11]]) as usize,
            w.len()
        );
        assert_eq!(w.len() % 4, 0);
        // fontTools (with the brotli module) is the reference reader; skipped where absent.
        let probe = std::process::Command::new("python3")
            .args(["-c", "import fontTools.ttLib, brotli"])
            .output();
        if !probe.is_ok_and(|o| o.status.success()) {
            return;
        }
        let dir = std::env::temp_dir().join(format!("oc-woff2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.ttf"), &sf).unwrap();
        std::fs::write(dir.join("a.woff2"), &w).unwrap();
        let out = std::process::Command::new("python3")
            .args([
                "-c",
                "import sys; from fontTools.ttLib import TTFont; a=TTFont(sys.argv[1]); b=TTFont(sys.argv[2]); \
                 assert b.flavor=='woff2'; \
                 assert all(a.getTableData(t)==b.getTableData(t) for t in a.keys() if t not in ('GlyphOrder','head'))",
            ])
            .arg(dir.join("a.ttf"))
            .arg(dir.join("a.woff2"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn truetype_to_cff_and_back_keeps_the_shape() {
        let f = Font::parse(&tiny_font()).unwrap();
        let cff = convert::truetype_to_cff(&f).unwrap();
        let g = Font::parse(&cff).unwrap();
        assert!(g.is_cff);
        let a = area(&outline(&g, 1).unwrap());
        assert!((a - 460_000.0).abs() < 300.0, "{a}");
        assert_eq!(g.advance(1), 700);
        let ttf = convert::cff_to_truetype(&g, 0.5).unwrap();
        let h = Font::parse(&ttf).unwrap();
        assert!(!h.is_cff);
        let a2 = area(&outline(&h, 1).unwrap());
        assert!((a2 - 460_000.0).abs() < 400.0, "{a2}");
    }

    #[test]
    fn damaged_fonts_error_without_panicking() {
        let sf = tiny_font();
        for cut in [0usize, 3, 11, 40, 100, sf.len() - 1] {
            let _ = Font::parse(&sf[..cut]).map(|f| {
                let _ = outline(&f, 1);
                f.unicode_map()
            });
        }
        let mut bad = sf.clone();
        for b in bad.iter_mut().skip(60) {
            *b = b.wrapping_mul(31).wrapping_add(7);
        }
        if let Ok(f) = Font::parse(&bad) {
            let _ = outline(&f, 1);
            let _ = f.glyph_for_char('A');
        }
    }

    #[test]
    fn specimen_draws_something() {
        let f = Font::parse(&tiny_font()).unwrap();
        let c = render::specimen(&f, 400).unwrap();
        assert!(c.data.chunks(4).any(|p| p[0] < 0.5));
    }
}
