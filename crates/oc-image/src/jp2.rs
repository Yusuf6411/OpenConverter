//! JPEG 2000 read support (JP2 files and raw codestreams), on top of the `oc-jp2` decoder.
use crate::image::{Depth, Image, Layout};
use oc_core::{Limits, Notes, Result};

pub use oc_jp2::sniff;

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let pic = oc_jp2::decode(data, limits)?;
    let layout = match pic.channels {
        1 => Layout::Gray,
        2 => Layout::GrayAlpha,
        3 => Layout::Rgb,
        _ => Layout::Rgba,
    };
    let (depth, mut bytes) = if pic.sixteen {
        (Depth::Sixteen, Vec::with_capacity(pic.samples.len() * 2))
    } else {
        (Depth::Eight, Vec::with_capacity(pic.samples.len()))
    };
    for &v in &pic.samples {
        if pic.sixteen {
            bytes.extend_from_slice(&v.to_be_bytes());
        } else {
            bytes.push(v as u8);
        }
    }
    let mut image = Image::new(pic.width, pic.height, layout, depth, bytes)?;
    image.metadata.icc_profile = pic.icc;
    Ok((image, Notes::new()))
}
