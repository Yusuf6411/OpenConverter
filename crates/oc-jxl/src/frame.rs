//! Frame decoding: section layout, the global and group streams of Modular frames.
use crate::bits::{bad, Br};
use crate::header::{FrameHeader, ImageHeader, Toc, FLAG_NOISE, FLAG_PATCHES, FLAG_SPLINES};
use crate::modular::{Chan, MaTree, Stream};
use oc_core::{Error, ErrorCode, Limits, Result};

pub fn unsupported(msg: &str) -> Error {
    Error::new(ErrorCode::UnsupportedVariant, format!("JPEG XL: {msg}"))
}

/// Access to the frame's sections. A frame with a single TOC entry stores everything in one
/// bitstream, each part starting on a byte boundary.
pub struct Sections<'a> {
    toc: Vec<(usize, usize)>,
    data: &'a [u8],
    single: Option<Br<'a>>,
}

impl<'a> Sections<'a> {
    pub fn new(toc: &Toc, data: &'a [u8]) -> Result<Self> {
        let mut s = Self { toc: toc.sections.clone(), data: &data[toc.data_start.min(data.len())..], single: None };
        for &(off, size) in &s.toc {
            if off.checked_add(size).map_or(true, |end| end > s.data.len()) {
                return Err(crate::bits::eof());
            }
        }
        if s.toc.len() == 1 {
            let (off, size) = s.toc[0];
            s.single = Some(Br::new(&s.data[off..off + size]));
        }
        Ok(s)
    }

    pub fn is_single(&self) -> bool {
        self.single.is_some()
    }

    pub fn open(&mut self, idx: usize) -> Result<Br<'a>> {
        if let Some(b) = &self.single {
            return Ok(b.clone());
        }
        let &(off, size) = self.toc.get(idx).ok_or_else(|| bad("missing section"))?;
        Ok(Br::new(&self.data[off..off + size]))
    }

    pub fn close(&mut self, mut br: Br<'a>) {
        br.align();
        if self.single.is_some() {
            self.single = Some(br);
        }
    }

    /// Total bytes of section data.
    pub fn total(&self) -> usize {
        self.toc.iter().map(|s| s.1).sum()
    }
}

/// The frame as decoded, before colour processing.
pub struct Decoded {
    pub header: FrameHeader,
    /// Modular channels: the colour channels (modular frames) followed by the extra channels.
    pub chans: Vec<Chan>,
    pub lf_dequant: [f32; 3],
    /// Bytes of the codestream this frame occupied.
    pub consumed: usize,
}

/// Which stream-relative regions of a channel belong to one group.
struct Tile {
    ci: usize,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
}

fn tiles(chans: &[Chan], first: usize, fh: &FrameHeader, lf: bool, group: usize, pass: Option<u32>) -> Vec<Tile> {
    let gd = fh.group_dim() as usize;
    let shifts = fh.pass_shifts();
    let (ow, oh) = (fh.color_w() as usize, fh.color_h() as usize);
    let mut out = Vec::new();
    for (ci, c) in chans.iter().enumerate().skip(first) {
        if c.hs < 0 || c.vs < 0 {
            continue;
        }
        let small = c.hs < 3 || c.vs < 3;
        if lf == small {
            continue;
        }
        if small {
            let s = c.hs.min(c.vs);
            let p = shifts.iter().find(|(_, (lo, hi))| (*lo..*hi).contains(&s)).map(|x| x.0);
            if p != pass {
                continue;
            }
        }
        let (tw, th, nx) = if small {
            (gd >> c.hs, gd >> c.vs, ow.div_ceil(gd))
        } else {
            (gd >> (c.hs - 3), gd >> (c.vs - 3), ow.div_ceil(gd << 3))
        };
        if tw == 0 || th == 0 {
            continue;
        }
        let _ = oh;
        let (gx, gy) = (group % nx, group / nx);
        let (x0, y0) = (gx * tw, gy * th);
        if x0 >= c.w || y0 >= c.h {
            continue;
        }
        out.push(Tile { ci, x0, y0, w: tw.min(c.w - x0), h: th.min(c.h - y0) });
    }
    out
}

fn decode_group(
    br: &mut Br,
    chans: &mut [Chan],
    list: &[Tile],
    tree: Option<&MaTree>,
    stream_idx: u32,
    bit_depth: u32,
    max_samples: u64,
) -> Result<()> {
    if list.is_empty() {
        return Ok(());
    }
    let input = list.iter().map(|t| Chan::info(t.w, t.h, chans[t.ci].hs, chans[t.ci].vs)).collect();
    let mut s = Stream::parse(br, tree, input, max_samples)?;
    let n = s.chans.len();
    s.decode(br, tree, n, stream_idx)?;
    let out = s.finish(bit_depth)?;
    if out.len() != list.len() {
        return Err(bad("group transform changed the channel count"));
    }
    for (t, o) in list.iter().zip(&out) {
        if o.w != t.w || o.h != t.h {
            return Err(bad("group channel has the wrong size"));
        }
        let dst = &mut chans[t.ci];
        for y in 0..t.h {
            let d = (t.y0 + y) * dst.w + t.x0;
            dst.d[d..d + t.w].copy_from_slice(&o.d[y * o.w..(y + 1) * o.w]);
        }
    }
    Ok(())
}

/// Decode one frame starting at `data[0]`. Frames that need the VarDCT path are rejected.
pub fn decode_frame(data: &[u8], img: &ImageHeader, limits: &Limits) -> Result<Decoded> {
    let mut br = Br::new(data);
    let fh = FrameHeader::parse(&mut br, img)?;
    let toc = Toc::parse(&mut br, &fh)?;
    let header_len = toc.data_start;
    let mut secs = Sections::new(&toc, data)?;
    let consumed = header_len + secs.total();

    let pixels = u64::from(fh.width) * u64::from(fh.height);
    if pixels > limits.max_pixels {
        return Err(Error::limit("max_pixels", "JPEG XL: frame is too large"));
    }
    let max_samples = (pixels * (fh.encoded_color_channels + img.ec.len()) as u64 + (1 << 16)).min(limits.max_alloc / 4);

    if fh.flags & FLAG_PATCHES != 0 {
        return Err(unsupported("patches are not supported"));
    }
    if fh.flags & FLAG_SPLINES != 0 {
        return Err(unsupported("splines are not supported"));
    }
    if fh.flags & FLAG_NOISE != 0 {
        return Err(unsupported("noise synthesis is not supported"));
    }
    if fh.upsampling != 1 || fh.ec_upsampling.iter().any(|&u| u != 1) {
        return Err(unsupported("upsampled frames are not supported"));
    }
    if fh.do_ycbcr {
        return Err(unsupported("YCbCr (JPEG reconstruction) frames are not supported"));
    }
    if !fh.modular {
        return Err(unsupported("VarDCT frames are not supported yet"));
    }

    // LfGlobal
    let mut br = secs.open(0)?;
    let mut lf_dequant = [1.0 / 32.0, 1.0 / 4.0, 1.0 / 2.0];
    if !br.bool()? {
        for v in &mut lf_dequant {
            *v = br.f16()?;
        }
    }
    for v in &mut lf_dequant {
        *v /= 128.0;
        if *v < 1e-8 {
            return Err(bad("LF dequantisation weight is too small"));
        }
    }
    let nch = (fh.encoded_color_channels + img.ec.len()) as u64;
    let node_limit = (1024 + pixels * nch / 16).min(1 << 22) as usize;
    let tree = if br.bool()? { Some(MaTree::parse(&mut br, node_limit)?) } else { None };

    let (cw, ch) = (fh.color_w() as usize, fh.color_h() as usize);
    let mut infos = Vec::new();
    for _ in 0..fh.encoded_color_channels {
        infos.push(Chan::info(cw, ch, 0, 0));
    }
    for e in &img.ec {
        let shift = e.dim_shift as i32;
        infos.push(Chan::info(cw.div_ceil(1 << shift), ch.div_ceil(1 << shift), shift, shift));
    }
    let gd = fh.group_dim() as usize;
    let bit_depth = img.bit_depth.bits;
    let mut stream = Stream::parse(&mut br, tree.as_ref(), infos, max_samples)?;
    let gcount = stream.global_count(gd);
    stream.decode(&mut br, tree.as_ref(), gcount, 0)?;
    secs.close(br);

    // LF groups (only modular channels shifted by 3 or more land here)
    let num_lf = fh.num_lf_groups() as usize;
    for g in 0..num_lf {
        let mut br = secs.open(1 + g)?;
        let list = tiles(&stream.chans, gcount, &fh, true, g, None);
        decode_group(&mut br, &mut stream.chans, &list, tree.as_ref(), (1 + num_lf + g) as u32, bit_depth, max_samples)?;
        secs.close(br);
    }

    // HfGlobal is empty for modular frames.
    let num_groups = fh.num_groups() as usize;
    for (pass, _) in fh.pass_shifts() {
        for g in 0..num_groups {
            let idx = 1 + num_lf + 1 + pass as usize * num_groups + g;
            let list = tiles(&stream.chans, gcount, &fh, false, g, Some(pass));
            if list.is_empty() {
                continue;
            }
            let mut br = secs.open(idx)?;
            let sid = (1 + 3 * num_lf + 17 + pass as usize * num_groups + g) as u32;
            decode_group(&mut br, &mut stream.chans, &list, tree.as_ref(), sid, bit_depth, max_samples)?;
            secs.close(br);
        }
    }

    let chans = stream.finish(bit_depth)?;
    Ok(Decoded { header: fh, chans, lf_dequant, consumed })
}

/// Parse a frame's header and table of contents and report how many bytes it occupies.
pub fn frame_extent(data: &[u8], img: &ImageHeader) -> Result<(FrameHeader, usize)> {
    let mut br = Br::new(data);
    let fh = FrameHeader::parse(&mut br, img)?;
    let toc = Toc::parse(&mut br, &fh)?;
    let total: usize = toc.sections.iter().map(|s| s.1).sum();
    let end = toc.data_start.checked_add(total).ok_or_else(|| bad("frame size overflow"))?;
    if end > data.len() {
        return Err(crate::bits::eof());
    }
    Ok((fh, end))
}
