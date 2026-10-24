//! JBIG2 and JPEG 2000 images inside PDF files, drawn at one pixel per image pixel.
#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use oc_core::NoProgress;
use oc_engine::{ConvertRequest, Engine};
use std::path::PathBuf;

fn data(krate: &str, name: &str) -> Vec<u8> {
    std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../")
            .join(krate)
            .join("tests/data")
            .join(name),
    )
    .unwrap()
}

fn pdf(w: usize, h: usize, dict: &str, stream: &[u8], extra: Option<&[u8]>) -> Vec<u8> {
    let content = format!("q {w} 0 0 {h} 0 0 cm /Im0 Do Q");
    let mut objs: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents 5 0 R /Resources << /XObject << /Im0 4 0 R >> >> >>").into_bytes(),
    ];
    let mut img = format!(
        "<< /Type /XObject /Subtype /Image /Width {w} /Height {h} {dict} /Length {} >>\nstream\n",
        stream.len()
    )
    .into_bytes();
    img.extend(stream);
    img.extend(b"\nendstream");
    objs.push(img);
    objs.push(
        format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        )
        .into_bytes(),
    );
    if let Some(g) = extra {
        let mut o = format!("<< /Length {} >>\nstream\n", g.len()).into_bytes();
        o.extend(g);
        o.extend(b"\nendstream");
        objs.push(o);
    }
    let mut out = b"%PDF-1.5\n".to_vec();
    let mut offs = Vec::new();
    for (n, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n", n + 1).as_bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Converts to a binary PPM and returns `(width, height, rgb)`.
fn render(tag: &str, pdf: &[u8]) -> (usize, usize, Vec<u8>) {
    let d = std::env::temp_dir().join(format!("oc-pdfimg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let (i, o) = (d.join("in.pdf"), d.join("out.ppm"));
    std::fs::write(&i, pdf).unwrap();
    let e = Engine::new();
    let raw = vec![("dpi".to_string(), "72".to_string())];
    let options = e.parse_options_for(&i, &o, &raw).unwrap();
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
    let b = std::fs::read(&o).unwrap();
    let _ = std::fs::remove_dir_all(d);
    let text: Vec<&[u8]> = b.splitn(4, |c| c.is_ascii_whitespace()).collect();
    assert_eq!(text[0], b"P6");
    let w: usize = String::from_utf8_lossy(text[1]).parse().unwrap();
    let h: usize = String::from_utf8_lossy(text[2]).parse().unwrap();
    let rest = text[3];
    let start = rest.iter().position(|c| c.is_ascii_whitespace()).unwrap() + 1;
    (w, h, rest[start..].to_vec())
}

#[test]
fn jbig2_with_globals_is_drawn() {
    let want = data("oc-jbig2", "text_globals.ref");
    let (w, h) = (
        u32::from_be_bytes(want[0..4].try_into().unwrap()) as usize,
        u32::from_be_bytes(want[4..8].try_into().unwrap()) as usize,
    );
    let doc = pdf(
        w,
        h,
        "/ColorSpace /DeviceGray /BitsPerComponent 1 /Filter /JBIG2Decode /DecodeParms << /JBIG2Globals 6 0 R >>",
        &data("oc-jbig2", "text_globals.jb2"),
        Some(&data("oc-jbig2", "text_globals.glob")),
    );
    let (pw, ph, px) = render("jb2", &doc);
    assert_eq!((pw, ph), (w, h));
    let rb = w.div_ceil(8);
    let mut wrong = 0;
    for y in 0..h {
        for x in 0..w {
            let black = want[8 + y * rb + x / 8] >> (7 - x % 8) & 1 == 1;
            let drawn_black = px[(y * w + x) * 3] < 128;
            wrong += usize::from(black != drawn_black);
        }
    }
    assert!(wrong * 100 < w * h, "{wrong} of {} pixels differ", w * h);
}

#[test]
fn jpeg2000_is_drawn() {
    let jp2 = data("oc-jp2", "rgb_lossless.jp2");
    let want = data("oc-jp2", "rgb_lossless.ref");
    let at = jp2.windows(4).position(|w| w == b"ihdr").unwrap() + 4;
    let h = u32::from_be_bytes(jp2[at..at + 4].try_into().unwrap()) as usize;
    let w = u32::from_be_bytes(jp2[at + 4..at + 8].try_into().unwrap()) as usize;
    assert_eq!(want.len(), w * h * 3);
    let doc = pdf(
        w,
        h,
        "/ColorSpace /DeviceRGB /Filter /JPXDecode",
        &jp2,
        None,
    );
    let (pw, ph, px) = render("jpx", &doc);
    assert_eq!((pw, ph), (w, h));
    let total: usize = px
        .iter()
        .zip(&want)
        .map(|(a, b)| usize::from(a.abs_diff(*b)))
        .sum();
    assert!(total < w * h * 3 * 2, "mean error {}", total / (w * h * 3));
}
