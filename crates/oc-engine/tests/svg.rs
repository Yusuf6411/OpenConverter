//! SVG as a source: drawn at its own size or at a requested size, then written as a raster.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::NoProgress;
use oc_engine::{ConvertRequest, Engine};
use std::path::PathBuf;

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"><rect width="10" height="10" fill="#f00"/></svg>"##;

fn run(tag: &str, svg: &[u8], name: &str, to: &str, opts: &[(&str, &str)]) -> Vec<u8> {
    let d: PathBuf = std::env::temp_dir().join(format!("oc-svg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let i = d.join(name);
    let o = d.join(format!("out.{to}"));
    std::fs::write(&i, svg).unwrap();
    let e = Engine::new();
    let raw: Vec<(String, String)> = opts
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect();
    let options = e.parse_options(&o, &raw).unwrap();
    e.convert(
        &ConvertRequest {
            input: i,
            output: o.clone(),
            overwrite: true,
            display_name: None,
            options,
        },
        &NoProgress,
    )
    .unwrap();
    let out = std::fs::read(o).unwrap();
    let _ = std::fs::remove_dir_all(d);
    out
}

fn png_size(b: &[u8]) -> (u32, u32) {
    (
        u32::from_be_bytes(b[16..20].try_into().unwrap()),
        u32::from_be_bytes(b[20..24].try_into().unwrap()),
    )
}

#[test]
fn svg_to_png_at_intrinsic_and_requested_size() {
    let p = run("a", SVG.as_bytes(), "in.svg", "png", &[]);
    assert_eq!(png_size(&p), (20, 10));
    let p = run("b", SVG.as_bytes(), "in.svg", "png", &[("width", "100")]);
    assert_eq!(png_size(&p), (100, 50));
}

#[test]
fn gzipped_svg_is_read() {
    let z = oc_compress::gzip::compress(SVG.as_bytes(), 6);
    let p = run("c", &z, "in.svgz", "png", &[]);
    assert_eq!(png_size(&p), (20, 10));
}
