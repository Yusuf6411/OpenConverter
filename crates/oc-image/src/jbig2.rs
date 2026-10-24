//! JBIG2 read support (standalone files and PDF-style embedded streams), on top of `oc-jbig2`.
use crate::image::{Depth, Image, Layout};
use oc_core::{Limits, Notes, Result};

pub use oc_jbig2::sniff;

fn env(limits: &Limits) -> oc_jbig2::Env {
    oc_jbig2::Env::new(Some(crate::ccitt::decode_mmr), limits.clone())
}

fn to_image(page: oc_jbig2::Page) -> Result<Image> {
    let rb = page.width.div_ceil(8);
    let mut px = Vec::with_capacity(page.width * page.height);
    for y in 0..page.height {
        for x in 0..page.width {
            let byte = page.bits.get(y * rb + x / 8).copied().unwrap_or(0);
            let bit = byte >> (7 - x % 8) & 1;
            // JBIG2 black is 1; the image is grey with black at 0
            px.push(if bit == 1 { 0 } else { 255 });
        }
    }
    Image::new(
        page.width as u32,
        page.height as u32,
        Layout::Gray,
        Depth::Eight,
        px,
    )
}

/// Decodes the first page of a standalone JBIG2 file.
pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let page = oc_jbig2::decode_file(data, &env(limits))?;
    Ok((to_image(page)?, Notes::new()))
}

/// Decodes an embedded stream (PDF `JBIG2Decode`) with its optional globals.
pub fn decode_embedded(
    globals: Option<&[u8]>,
    data: &[u8],
    limits: &Limits,
) -> Result<(Image, Notes)> {
    let page = oc_jbig2::decode_embedded(globals, data, &env(limits))?;
    Ok((to_image(page)?, Notes::new()))
}
