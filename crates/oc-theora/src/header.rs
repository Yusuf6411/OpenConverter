//! The three Theora header packets: identification, comment and setup (spec chapter 6).
use crate::bits::Bits;
use oc_core::{Error, ErrorCode, Result};

/// What the identification header says about the video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Width and height of the coded frame in macro blocks.
    pub fmbw: u32,
    pub fmbh: u32,
    /// Picture region inside the frame; the offset is from the lower-left corner.
    pub pic_w: u32,
    pub pic_h: u32,
    pub pic_x: u32,
    pub pic_y: u32,
    pub fps_num: u32,
    pub fps_den: u32,
    pub par_num: u32,
    pub par_den: u32,
    /// 0 undefined, 1 Rec. 470M, 2 Rec. 470BG.
    pub color_space: u8,
    /// 0 is 4:2:0, 2 is 4:2:2, 3 is 4:4:4.
    pub pixel_format: u8,
    pub nominal_bitrate: u32,
    pub quality: u8,
    pub kf_shift: u8,
    pub version: (u8, u8, u8),
}

/// The comment header: vendor string and `NAME=value` user comments.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Comments {
    pub vendor: String,
    pub comments: Vec<String>,
}

/// A Huffman code as a binary tree: node values below zero are leaves holding `!token`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Tree {
    pub nodes: Vec<[i16; 2]>,
    pub root_leaf: Option<u8>,
}

impl Tree {
    pub fn decode(&self, b: &mut Bits) -> u8 {
        if let Some(t) = self.root_leaf {
            return t;
        }
        let mut n = 0usize;
        loop {
            let next = self.nodes[n][b.bit() as usize];
            if next < 0 {
                return (!next) as u8;
            }
            n = next as usize;
        }
    }
}

/// Everything the setup header carries.
#[derive(Debug, Clone)]
pub struct Setup {
    pub(crate) lflims: [u8; 64],
    /// `[qti][pli][qi]` quantisation matrices in natural order (spec 6.4.3).
    pub(crate) qmats: Vec<[u16; 64]>,
    pub(crate) trees: Vec<Tree>,
}

impl Setup {
    pub(crate) fn qmat(&self, qti: usize, pli: usize, qi: usize) -> &[u16; 64] {
        &self.qmats[(qti * 3 + pli) * 64 + qi]
    }
}

fn bad(msg: &str) -> Error {
    Error::corrupt(format!("Theora: {msg}"))
}

fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

fn common(b: &mut Bits, kind: u8) -> Result<()> {
    let t = b.read(8);
    if t != u32::from(kind) {
        return Err(bad("unexpected header type"));
    }
    for want in b"theora" {
        if b.read(8) != u32::from(*want) {
            return Err(bad("missing 'theora' signature"));
        }
    }
    Ok(())
}

/// Whether a packet is a Theora identification header.
pub fn is_identification(packet: &[u8]) -> bool {
    packet.len() >= 7 && packet[0] == 0x80 && &packet[1..7] == b"theora"
}

pub fn parse_identification(packet: &[u8]) -> Result<Info> {
    let mut b = Bits::new(packet);
    common(&mut b, 0x80)?;
    let version = (b.read(8) as u8, b.read(8) as u8, b.read(8) as u8);
    if version.0 != 3 || version.1 != 2 {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            format!(
                "Theora version {}.{} is not supported",
                version.0, version.1
            ),
        ));
    }
    let fmbw = b.read(16);
    let fmbh = b.read(16);
    let pic_w = b.read(24);
    let pic_h = b.read(24);
    let pic_x = b.read(8);
    let pic_y = b.read(8);
    let fps_num = b.read(32);
    let fps_den = b.read(32);
    let par_num = b.read(24);
    let par_den = b.read(24);
    let color_space = b.read(8) as u8;
    let nominal_bitrate = b.read(24);
    let quality = b.read(6) as u8;
    let kf_shift = b.read(5) as u8;
    let pixel_format = b.read(2) as u8;
    let reserved = b.read(3);
    if b.overrun() {
        return Err(Error::truncated("Theora identification header ended early"));
    }
    if fmbw == 0 || fmbh == 0 {
        return Err(bad("frame has no macro blocks"));
    }
    if pic_w > fmbw * 16
        || pic_h > fmbh * 16
        || pic_x + pic_w > fmbw * 16
        || pic_y + pic_h > fmbh * 16
    {
        return Err(bad("picture region lies outside the frame"));
    }
    if pixel_format == 1 {
        return Err(bad("reserved pixel format"));
    }
    if reserved != 0 {
        return Err(bad("reserved bits are set"));
    }
    Ok(Info {
        fmbw,
        fmbh,
        pic_w,
        pic_h,
        pic_x,
        pic_y,
        fps_num,
        fps_den,
        par_num,
        par_den,
        color_space,
        pixel_format,
        nominal_bitrate,
        quality,
        kf_shift,
        version,
    })
}

pub fn parse_comments(packet: &[u8]) -> Result<Comments> {
    let mut b = Bits::new(packet);
    common(&mut b, 0x81)?;
    let string = |b: &mut Bits| -> Result<String> {
        let len = b.read(8) | (b.read(8) << 8) | (b.read(8) << 16) | (b.read(8) << 24);
        if len as usize > packet.len() {
            return Err(Error::truncated("Theora comment is longer than its header"));
        }
        let mut v = Vec::with_capacity(len as usize);
        for _ in 0..len {
            v.push(b.read(8) as u8);
        }
        if b.overrun() {
            return Err(Error::truncated("Theora comment header ended early"));
        }
        Ok(String::from_utf8_lossy(&v).into_owned())
    };
    let vendor = string(&mut b)?;
    let n = {
        let n = b.read(8) | (b.read(8) << 8) | (b.read(8) << 16) | (b.read(8) << 24);
        n as usize
    };
    let mut comments = Vec::new();
    for _ in 0..n.min(packet.len()) {
        comments.push(string(&mut b)?);
    }
    Ok(Comments { vendor, comments })
}

fn read_tree(b: &mut Bits, tree: &mut Tree, entries: &mut u32, depth: u32) -> Result<i16> {
    if depth > 32 {
        return Err(bad("Huffman code is longer than 32 bits"));
    }
    if b.bit() == 1 {
        if *entries >= 32 {
            return Err(bad("Huffman table has more than 32 entries"));
        }
        *entries += 1;
        let token = b.read(5) as i16;
        return Ok(!token);
    }
    let at = tree.nodes.len();
    tree.nodes.push([0, 0]);
    for side in 0..2 {
        if b.overrun() {
            return Err(Error::truncated("Theora setup header ended early"));
        }
        let child = read_tree(b, tree, entries, depth + 1)?;
        tree.nodes[at][side] = child;
    }
    Ok(at as i16)
}

pub fn parse_setup(packet: &[u8]) -> Result<Setup> {
    let mut b = Bits::new(packet);
    common(&mut b, 0x82)?;

    let nbits = b.read(3);
    let mut lflims = [0u8; 64];
    for l in &mut lflims {
        *l = b.read(nbits) as u8;
    }

    let nbits = b.read(4) + 1;
    let mut acscale = [0u16; 64];
    for s in &mut acscale {
        *s = b.read(nbits) as u16;
    }
    let nbits = b.read(4) + 1;
    let mut dcscale = [0u16; 64];
    for s in &mut dcscale {
        *s = b.read(nbits) as u16;
    }

    let nbms = b.read(9) as usize + 1;
    if nbms > 384 {
        return Err(bad("more than 384 base matrices"));
    }
    let mut bms = vec![[0u8; 64]; nbms];
    for m in &mut bms {
        for c in m.iter_mut() {
            *c = b.read(8) as u8;
        }
    }
    if b.overrun() {
        return Err(Error::truncated("Theora setup header ended early"));
    }

    // Quant ranges per (qti, pli): sizes and base matrix indices.
    let mut sizes = [
        [Vec::<u32>::new(), Vec::new(), Vec::new()],
        [Vec::new(), Vec::new(), Vec::new()],
    ];
    let mut bmis = [
        [Vec::<usize>::new(), Vec::new(), Vec::new()],
        [Vec::new(), Vec::new(), Vec::new()],
    ];
    for qti in 0..2usize {
        for pli in 0..3usize {
            let newqr = if qti > 0 || pli > 0 { b.read(1) } else { 1 };
            if newqr == 0 {
                let rpqr = if qti > 0 { b.read(1) } else { 0 };
                let (qtj, plj) = if rpqr == 1 {
                    (qti - 1, pli)
                } else {
                    ((3 * qti + pli - 1) / 3, (pli + 2) % 3)
                };
                sizes[qti][pli] = sizes[qtj][plj].clone();
                bmis[qti][pli] = bmis[qtj][plj].clone();
            } else {
                let mut qi = 0u32;
                let mut s = Vec::new();
                let mut m = Vec::new();
                let bm_bits = ilog(nbms as u32 - 1);
                let first = b.read(bm_bits) as usize;
                if first >= nbms {
                    return Err(bad("base matrix index is out of range"));
                }
                m.push(first);
                loop {
                    let size = b.read(ilog(62u32.saturating_sub(qi))) + 1;
                    s.push(size);
                    qi += size;
                    let bi = b.read(bm_bits) as usize;
                    if bi >= nbms {
                        return Err(bad("base matrix index is out of range"));
                    }
                    m.push(bi);
                    if qi >= 63 {
                        break;
                    }
                    if b.overrun() {
                        return Err(Error::truncated("Theora setup header ended early"));
                    }
                }
                if qi > 63 {
                    return Err(bad("quant ranges run past qi 63"));
                }
                sizes[qti][pli] = s;
                bmis[qti][pli] = m;
            }
        }
    }

    let mut trees = Vec::with_capacity(80);
    for _ in 0..80 {
        let mut t = Tree::default();
        let mut entries = 0;
        let root = read_tree(&mut b, &mut t, &mut entries, 0)?;
        if root < 0 {
            t.root_leaf = Some((!root) as u8);
        }
        trees.push(t);
    }
    if b.overrun() {
        return Err(Error::truncated("Theora setup header ended early"));
    }

    // Quantisation matrices (spec 6.4.3).
    let mut qmats = vec![[0u16; 64]; 2 * 3 * 64];
    for qti in 0..2usize {
        for pli in 0..3usize {
            let (s, m) = (&sizes[qti][pli], &bmis[qti][pli]);
            for qi in 0..64u32 {
                let mut start = 0u32;
                let mut qri = 0usize;
                while qri + 1 < s.len() && qi > start + s[qri] {
                    start += s[qri];
                    qri += 1;
                }
                let end = start + s[qri];
                let (bmi, bmj) = (m[qri], m[qri + 1]);
                let out = &mut qmats[(qti * 3 + pli) * 64 + qi as usize];
                for ci in 0..64usize {
                    let bm = (2 * (end - qi) * u32::from(bms[bmi][ci])
                        + 2 * (qi - start) * u32::from(bms[bmj][ci])
                        + s[qri])
                        / (2 * s[qri]);
                    let qmin = match (qti, ci) {
                        (0, 0) => 16,
                        (0, _) => 8,
                        (_, 0) => 32,
                        _ => 16,
                    };
                    let scale = u32::from(if ci == 0 {
                        dcscale[qi as usize]
                    } else {
                        acscale[qi as usize]
                    });
                    out[ci] = ((scale * bm / 100) * 4).min(4096).max(qmin) as u16;
                }
            }
        }
    }
    Ok(Setup {
        lflims,
        qmats,
        trees,
    })
}
