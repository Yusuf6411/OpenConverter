//! JPEG XL decoder (ISO/IEC 18181), written for OpenConverter.
//!
//! The format structure follows the specification; `jxl-oxide` (MIT/Apache-2.0) was used as a
//! reference for details the text leaves open. See `docs/formats/jxl.md` for what is covered.
#![forbid(unsafe_code)]

mod bits;
mod colour;
mod compose;
mod dct;
mod dequant;
mod entropy;
mod features;
mod filter;
mod frame;
mod header;
mod modular;
mod render;
mod vardct;

use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub use render::Picture;

const BARE: [u8; 2] = [0xff, 0x0a];
const BOX_SIGNATURE: [u8; 12] = [0, 0, 0, 0x0c, b'J', b'X', b'L', b' ', 0x0d, 0x0a, 0x87, 0x0a];

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(&BARE) || data.starts_with(&BOX_SIGNATURE)
}

/// Extract the codestream from a bare codestream or an ISO BMFF container.
fn codestream(data: &[u8]) -> Result<std::borrow::Cow<'_, [u8]>> {
    if data.starts_with(&BARE) {
        return Ok(data.into());
    }
    let mut pos = BOX_SIGNATURE.len();
    let mut out: Vec<u8> = Vec::new();
    let mut found = false;
    while pos + 8 <= data.len() {
        let size = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind = &data[pos + 4..pos + 8];
        let (header, len) = match size {
            0 => (8, data.len() - pos),
            1 => {
                if pos + 16 > data.len() {
                    break;
                }
                (16, u64::from_be_bytes(data[pos + 8..pos + 16].try_into().unwrap()) as usize)
            }
            n => (8, n),
        };
        if len < header || pos.checked_add(len).map_or(true, |e| e > data.len()) {
            return Err(Error::truncated("JPEG XL box extends past the end of the file"));
        }
        let body = &data[pos + header..pos + len];
        match kind {
            b"jxlc" => {
                out.extend_from_slice(body);
                found = true;
            }
            b"jxlp" => {
                if body.len() < 4 {
                    return Err(Error::corrupt("JPEG XL partial codestream box is too short"));
                }
                out.extend_from_slice(&body[4..]);
                found = true;
            }
            b"jbrd" => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "JPEG XL: JPEG reconstruction data is not supported",
                ));
            }
            _ => {}
        }
        pos += len;
    }
    if !found {
        return Err(Error::corrupt("JPEG XL container has no codestream"));
    }
    Ok(out.into())
}

/// Decode the first displayed frame of a JPEG XL file.
pub fn decode(data: &[u8], limits: &Limits) -> Result<(Picture, Notes)> {
    let cs = codestream(data)?;
    let mut br = bits::Br::new(&cs);
    let img = header::ImageHeader::parse(&mut br)?;
    let pixels = u64::from(img.width) * u64::from(img.height);
    if pixels > limits.max_pixels {
        return Err(Error::limit("max_pixels", "JPEG XL: image is too large"));
    }
    let mut notes = Notes::new();
    if img.colour.want_icc {
        header::skip_icc(&mut br)?;
        notes.push(Note::new(
            Severity::Warning,
            "icc_not_decoded",
            "the embedded ICC profile of the JPEG XL file is not carried over; colours are taken as sRGB",
        ));
    }
    br.align();
    let mut pos = br.bits_read() / 8;
    if img.preview.is_some() {
        let (_, len) = frame::frame_extent(&cs[pos..], &img)?;
        pos += len;
    }
    if img.animation.is_some() {
        notes.push(Note::new(Severity::Loss, "animation_flattened", "only the first frame of the animation is decoded"));
    }
    let mut slots: [Option<std::rc::Rc<render::Planes>>; 4] = Default::default();
    loop {
        if pos >= cs.len() {
            return Err(Error::truncated("JPEG XL codestream ends before the first frame"));
        }
        let mut dec = frame::decode_frame(&cs[pos..], &img, limits)?;
        pos += dec.consumed;
        let fh = dec.header.clone();
        let patches = dec.patches.take();
        let mut planes = render::frame_planes(dec, &img)?;
        if let Some(p) = patches {
            p.apply(&mut planes, &slots, &img)?;
        }
        let slot = fh.save_as_reference as usize;
        if fh.can_reference() && fh.save_before_ct {
            slots[slot] = Some(std::rc::Rc::new(compose::embed(&planes, &fh, &img)));
        }
        render::to_native(&mut planes, &img);
        if !fh.frame_type.is_normal() {
            // reference-only frames are not blended onto anything
            if fh.can_reference() && !fh.save_before_ct {
                slots[slot] = Some(std::rc::Rc::new(compose::embed(&planes, &fh, &img)));
            }
            continue;
        }
        let canvas = compose::compose(planes, &fh, &img, &slots)?;
        if fh.can_reference() && !fh.save_before_ct {
            slots[slot] = Some(std::rc::Rc::new(canvas.clone()));
        }
        if fh.is_keyframe() {
            let pic = render::finish(canvas, &img, &mut notes)?;
            return Ok((pic, notes));
        }
    }
}
