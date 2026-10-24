#![no_main]
use libfuzzer_sys::fuzz_target;
use oc_raster::{Blend, Canvas, Cap, FillRule, Join, Paint, Path, Stroke, Transform};

fuzz_target!(|data: &[u8]| {
    let mut it = data.chunks_exact(4);
    let mut num = move || -> f64 {
        it.next()
            .map_or(0.0, |c| f64::from(i32::from_le_bytes([c[0], c[1], c[2], c[3]])) / 64.0)
    };
    let mut p = Path::new();
    for k in 0..24 {
        match k % 5 {
            0 => p.move_to(num(), num()),
            1 => p.line_to(num(), num()),
            2 => p.quad_to(num(), num(), num(), num()),
            3 => p.cubic_to(num(), num(), num(), num(), num(), num()),
            _ => p.close(),
        }
    }
    let ctm = Transform::new(num() / 100.0, num() / 100.0, num() / 100.0, num() / 100.0, num(), num());
    let Some(mut c) = Canvas::new(48, 48) else { return };
    let clip = c.clip();
    c.fill_path(&p, &ctm, FillRule::NonZero, &Paint::Solid([1.0, 0.0, 0.0, 1.0]), Blend::Normal, 1.0, &clip);
    let st = Stroke {
        width: num().abs() / 8.0,
        cap: Cap::Round,
        join: Join::Round,
        miter_limit: 4.0,
        dash: vec![num().abs(), num().abs()],
        dash_phase: num(),
        hairline: true,
    };
    c.stroke_path(&p, &ctm, &st, &Paint::Solid([0.0, 0.0, 1.0, 0.5]), Blend::Multiply, 0.5, &clip);
    c.blur(num().abs() / 16.0, 1.0);
});
