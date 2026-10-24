//! Prints per-glyph metrics and outline areas as JSON lines (used by tools/font_interop.py).
use oc_font::Font;
use oc_raster::Seg;

fn area(path: &oc_raster::Path) -> f64 {
    let mut total = 0.0;
    for poly in path.flatten(0.01) {
        let n = poly.pts.len();
        for i in 0..n {
            let (Some(p), Some(q)) = (poly.pts.get(i), poly.pts.get((i + 1) % n)) else {
                continue;
            };
            total += p.x * q.y - q.x * p.y;
        }
    }
    total / 2.0
}

fn main() {
    let Some(file) = std::env::args().nth(1) else {
        return;
    };
    if file == "--stdenc" {
        let v: Vec<String> = (0..=255u8)
            .map(|c| oc_font::cff::standard_encoding_sid(c).to_string())
            .collect();
        println!("[{}]", v.join(","));
        return;
    }
    let Ok(data) = std::fs::read(&file) else {
        return;
    };
    let Ok(font) = Font::parse(&data) else {
        println!("{{\"error\":\"parse\"}}");
        return;
    };
    let step: usize = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    println!(
        "{{\"upem\":{},\"glyphs\":{},\"cff\":{}}}",
        font.units_per_em, font.num_glyphs, font.is_cff
    );
    for gid in (0..font.num_glyphs).step_by(step) {
        let a = oc_font::outline(&font, gid).map_or(0.0, |p| area(&p));
        let segs = oc_font::outline(&font, gid).map_or(0, |p| {
            p.segs.iter().filter(|s| !matches!(s, Seg::Close)).count()
        });
        println!(
            "{{\"g\":{gid},\"adv\":{},\"area\":{a:.3},\"segs\":{segs}}}",
            font.advance(gid)
        );
    }
    let mut map = font.unicode_map();
    map.sort();
    for (c, g) in map.iter().step_by(step.max(1)) {
        println!("{{\"c\":{c},\"gid\":{g}}}");
    }
}
