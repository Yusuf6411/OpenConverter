//! SVG pictures: read-only. Reading one means drawing it, at its own size or at the
//! `width`/`height` asked for, and the result goes on to any raster format.
use oc_core::limits::Limits;
use oc_core::options::Options;
use oc_core::planner::{Edge, EdgeKind, Registry};
use oc_core::{
    Capabilities, Category, Error, ErrorCode, FormatDescriptor, FormatId, Note, Notes, Ownership,
    Result, Severity, Sniffer,
};

pub const SVG: FormatId = FormatId("svg");
pub const FORMATS: [FormatId; 1] = [SVG];

pub fn is_svg(id: FormatId) -> bool {
    id == SVG
}

pub fn register(registry: &mut Registry) {
    registry.add_format(FormatDescriptor {
        id: SVG,
        name: "SVG",
        category: Category::Image,
        extensions: &["svg", "svgz"],
        mime: &["image/svg+xml"],
        sniffers: vec![Sniffer {
            offset: 0,
            magic: b"<svg",
            confidence: 90,
        }],
        capabilities: Capabilities {
            read: true,
            write: false,
            ..Capabilities::default()
        },
        writer_options: Default::default(),
        reader_options: Default::default(),
        ownership: Ownership::Own,
        spec: "W3C SVG 1.1 / SVG 2 static subset (drawn by our own rasteriser)",
    });
    for img in crate::image::FORMATS {
        if matches!(
            img,
            crate::image::PNG
                | crate::image::JPEG
                | crate::image::WEBP
                | crate::image::BMP
                | crate::image::TIFF
                | crate::image::GIF
                | crate::image::QOI
                | crate::image::ICO
        ) {
            registry.add_edge(Edge {
                from: SVG,
                to: img,
                kind: EdgeKind::Transcode,
                loss: 1,
                ownership: Ownership::Own,
                time: 3,
                description: "Draw the vector picture",
            });
        }
    }
}

/// Whether `data` looks like SVG (plain or gzipped).
pub fn detect(data: &[u8]) -> bool {
    if data.starts_with(&[0x1F, 0x8B]) {
        return true;
    }
    let head = data.get(..data.len().min(4096)).unwrap_or_default();
    let text = String::from_utf8_lossy(head);
    text.contains("<svg")
}

fn unpack(data: &[u8], limits: &Limits) -> Result<Vec<u8>> {
    if data.starts_with(&[0x1F, 0x8B]) {
        return oc_compress::gzip::decompress(data, limits, &oc_core::NoProgress);
    }
    Ok(data.to_vec())
}

pub fn render(data: &[u8], opts: &Options, limits: &Limits) -> Result<(oc_image::Image, Notes)> {
    let bytes = unpack(data, limits)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut notes = Notes::new();
    // Installed fonts are only scanned when the picture has text.
    let fonts = text.contains("<text").then(oc_font::db::FontDb::system);
    let lim = limits.clone();
    let decoder = move |b: &[u8]| -> Option<oc_image::Image> {
        let fmt = sniff_raster(b)?;
        crate::image::decode(fmt, b, &lim, &oc_core::NoProgress)
            .ok()
            .map(|(i, _)| i)
    };
    let positive = |k: &str| opts.int(k).filter(|v| *v > 0).map(|v| v.min(65535) as u32);
    let ropts = oc_svg::Options {
        width: positive("width"),
        height: positive("height"),
        scale: 1.0,
        background: None,
        fonts: fonts.as_ref(),
        image_decoder: Some(&decoder),
    };
    let r = oc_svg::render(&text, &ropts, limits)?;
    for n in r.notes.iter() {
        notes.push(n.clone());
    }
    if r.intrinsic.0 > 8192.0 || r.intrinsic.1 > 8192.0 {
        notes.push(Note::new(
            Severity::Info,
            "svg.large",
            "the picture is very large; set a width or height to choose the size",
        ));
    }
    let rgba = r.canvas.to_rgba8(None);
    let img = oc_image::Image::new(
        r.canvas.width as u32,
        r.canvas.height as u32,
        oc_image::Layout::Rgba,
        oc_image::Depth::Eight,
        rgba,
    )
    .map_err(|e| Error::new(ErrorCode::InternalError, e.to_string()))?;
    Ok((img, notes))
}

fn sniff_raster(b: &[u8]) -> Option<FormatId> {
    if b.starts_with(b"\x89PNG") {
        Some(crate::image::PNG)
    } else if b.starts_with(&[0xFF, 0xD8]) {
        Some(crate::image::JPEG)
    } else if b.starts_with(b"GIF8") {
        Some(crate::image::GIF)
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        Some(crate::image::WEBP)
    } else if b.starts_with(b"BM") {
        Some(crate::image::BMP)
    } else {
        None
    }
}
