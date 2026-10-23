//! Renders an SVG to a binary PPM over white (used by tools/svg_interop.py).
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks
)]
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(inp), Some(out)) = (args.get(1), args.get(2)) else {
        return;
    };
    let text = std::fs::read_to_string(inp).unwrap_or_default();
    let fonts = oc_font::db::FontDb::system();
    let lim = oc_core::Limits::default();
    let dec = |b: &[u8]| {
        oc_image::png::decode(b, &lim, &oc_core::NoProgress)
            .ok()
            .map(|(i, _)| i)
    };
    let opts = oc_svg::Options {
        image_decoder: Some(&dec),
        width: args.get(3).and_then(|s| s.parse().ok()),
        background: Some([1.0, 1.0, 1.0, 1.0]),
        fonts: Some(&fonts),
        ..Default::default()
    };
    match oc_svg::render(&text, &opts, &oc_core::Limits::default()) {
        Ok(r) => {
            let rgba = r.canvas.to_rgba8(Some([1.0, 1.0, 1.0]));
            let mut f = std::fs::File::create(out).expect("create");
            let _ = write!(f, "P6\n{} {}\n255\n", r.canvas.width, r.canvas.height);
            let rgb: Vec<u8> = rgba
                .chunks_exact(4)
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            let _ = f.write_all(&rgb);
            for n in r.notes.iter() {
                eprintln!("note: {}", n.message);
            }
        }
        Err(e) => eprintln!("error: {e}"),
    }
}
