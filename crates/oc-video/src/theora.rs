//! Theora video: the stream's three header packets (Xiph-laced, as Matroska keeps them) and
//! data packets become 4:2:0 pictures.
use crate::pic::Picture;
use crate::{corrupt, Res};
use oc_core::Limits;

/// Lace packets the way Matroska's `CodecPrivate` does for Xiph codecs.
pub fn xiph_lace(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = vec![(parts.len() - 1) as u8];
    for p in &parts[..parts.len() - 1] {
        let mut n = p.len();
        while n >= 255 {
            out.push(255);
            n -= 255;
        }
        out.push(n as u8);
    }
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// The packets of a Xiph-laced block.
pub fn xiph_unlace(p: &[u8]) -> Res<Vec<&[u8]>> {
    let count = usize::from(*p.first().ok_or_else(|| corrupt("empty Xiph codec data"))?) + 1;
    let mut pos = 1usize;
    let mut sizes = Vec::with_capacity(count);
    for _ in 0..count - 1 {
        let mut n = 0usize;
        loop {
            let b = *p.get(pos).ok_or_else(|| corrupt("bad Xiph lacing"))?;
            pos += 1;
            n += usize::from(b);
            if b < 255 {
                break;
            }
        }
        sizes.push(n);
    }
    let mut out = Vec::with_capacity(count);
    for n in sizes {
        out.push(
            p.get(pos..pos + n)
                .ok_or_else(|| corrupt("bad Xiph lacing"))?,
        );
        pos += n;
    }
    out.push(p.get(pos..).ok_or_else(|| corrupt("bad Xiph lacing"))?);
    Ok(out)
}

pub struct TheoraDecoder {
    inner: oc_theora::Decoder,
}

impl TheoraDecoder {
    pub fn new(private: &[u8], limits: &Limits) -> Res<Self> {
        let parts = xiph_unlace(private)?;
        let [id, comment, setup] = parts[..] else {
            return Err(corrupt("Theora needs three header packets"));
        };
        let headers = oc_theora::parse_headers(id, comment, setup)?;
        Ok(Self {
            inner: oc_theora::decoder(headers, limits)?,
        })
    }

    pub fn decode(&mut self, data: &[u8]) -> Res<Option<Picture>> {
        let f = self.inner.decode(data)?;
        if f.width == 0 || f.height == 0 {
            return Err(corrupt("Theora picture is empty"));
        }
        Ok(Some(to_picture(&f)))
    }
}

/// A 4:2:0 picture from a frame of any Theora pixel format; 4:2:2 and 4:4:4 chroma is averaged
/// down.
fn to_picture(f: &oc_theora::Frame) -> Picture {
    let mut p = Picture::black(f.width, f.height);
    p.y.copy_from_slice(&f.y);
    let (cw, ch) = (p.cw(), p.ch());
    let (sx, sy) = match f.pixel_format {
        0 => (1usize, 1usize),
        2 => (1, 2),
        _ => (2, 2),
    };
    let avg = |src: &[u8], out: &mut [u8]| {
        for cy in 0..ch {
            for cx in 0..cw {
                let (mut sum, mut n) = (0u32, 0u32);
                for dy in 0..sy {
                    for dx in 0..sx {
                        let (x, y) = (cx * sx + dx, cy * sy + dy);
                        if x < f.chroma_width && y < f.chroma_height {
                            sum += u32::from(src[y * f.chroma_width + x]);
                            n += 1;
                        }
                    }
                }
                out[cy * cw + cx] = (sum + n / 2).checked_div(n).map_or(128, |v| v as u8);
            }
        }
    };
    avg(&f.cb, &mut p.u);
    avg(&f.cr, &mut p.v);
    p
}
