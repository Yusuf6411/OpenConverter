//! JPEG XL read support, on top of the `oc-jxl` decoder.
use crate::image::{Depth, Image, Layout};
use oc_core::{Limits, Notes, Result};

pub use oc_jxl::sniff;

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let (pic, notes) = oc_jxl::decode(data, limits)?;
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
    Ok((Image::new(pic.width, pic.height, layout, depth, bytes)?, notes))
}
