//! DirectDraw Surface (DDS) textures.
//!
//! Read: the first surface (no mipmaps, cube faces or array slices beyond it) of uncompressed
//! RGB(A), luminance and alpha formats with any channel masks up to 32 bits, and BC1-BC5
//! (DXT1-5, ATI1/2) and BC7, both from the classic header and from the DX10 extension. BC4 and BC5
//! become gray and red-green images. Not read: BC6H, signed BC4/BC5, floating-point and
//! 16-bit formats, palettes and volume textures beyond their first slice.
//! Write: uncompressed 32-bit BGRA, or BC1 / BC3 with a principal-axis endpoint fit.
// Block and header offsets are fixed; every caller checks the lengths first (the header is
// at least 128 bytes, blocks are sliced to their exact size).
#![allow(clippy::indexing_slicing, clippy::needless_range_loop)]
use crate::image::{Depth, Image, Layout};
use crate::ops;
use oc_core::{Error, ErrorCode, Limits, Note, Notes, Result, Severity};

pub fn sniff(data: &[u8]) -> bool {
    data.starts_with(b"DDS ") && data.get(4..8) == Some(&[124, 0, 0, 0])
}

fn le32(d: &[u8], i: usize) -> u32 {
    d.get(i..i + 4)
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map_or(0, u32::from_le_bytes)
}

const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x2_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Bc1,
    Bc2,
    Bc3,
    Bc4,
    Bc5,
    Bc7,
}

impl Block {
    fn bytes(self) -> usize {
        match self {
            Block::Bc1 | Block::Bc4 => 8,
            _ => 16,
        }
    }
}

enum Format {
    Compressed(Block),
    /// Channel masks for red, green, blue and alpha, and the pixel size in bits.
    Masked {
        bits: u32,
        masks: [u32; 4],
        luminance: bool,
    },
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!("DDS format {what} is not supported"),
    )
}

fn dxgi_format(code: u32) -> Result<Format> {
    let masked = |bits, masks: [u32; 4]| Format::Masked {
        bits,
        masks,
        luminance: false,
    };
    Ok(match code {
        70..=72 => Format::Compressed(Block::Bc1),
        73..=75 => Format::Compressed(Block::Bc2),
        76..=78 => Format::Compressed(Block::Bc3),
        79 | 80 => Format::Compressed(Block::Bc4),
        82 | 83 => Format::Compressed(Block::Bc5),
        28 | 29 => masked(32, [0xFF, 0xFF00, 0xFF_0000, 0xFF00_0000]),
        87 | 91 => masked(32, [0xFF_0000, 0xFF00, 0xFF, 0xFF00_0000]),
        88 | 93 => masked(32, [0xFF_0000, 0xFF00, 0xFF, 0]),
        85 => masked(16, [0xF800, 0x07E0, 0x001F, 0]),
        86 => masked(16, [0x7C00, 0x03E0, 0x001F, 0x8000]),
        61 => Format::Masked {
            bits: 8,
            masks: [0xFF, 0xFF, 0xFF, 0],
            luminance: true,
        },
        65 => masked(8, [0, 0, 0, 0xFF]),
        94 | 95 => return Err(unsupported("BC6H")),
        98 | 99 => Format::Compressed(Block::Bc7),
        81 | 84 => return Err(unsupported("signed BC4/BC5")),
        other => return Err(unsupported(&format!("DXGI {other}"))),
    })
}

/// Expands a 5 or 6 bit value to 8 bits by replicating the top bits.
fn expand(v: u32, bits: u32) -> u8 {
    ((v << (8 - bits)) | (v >> (2 * bits - 8))) as u8
}

fn rgb565(c: u16) -> [u8; 3] {
    let c = u32::from(c);
    [
        expand(c >> 11, 5),
        expand((c >> 5) & 0x3F, 6),
        expand(c & 0x1F, 5),
    ]
}

/// The four colours of a BC1 colour block, with alpha. In the three-colour mode (only when
/// the block belongs to plain BC1) index 3 is transparent black.
fn bc1_palette(block: &[u8], allow_punch_through: bool) -> [[u8; 4]; 4] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let (a, b) = (rgb565(c0), rgb565(c1));
    let mix = |wa: u32, wb: u32, div: u32| -> [u8; 4] {
        let ch = |k: usize| ((wa * u32::from(a[k]) + wb * u32::from(b[k])) / div) as u8;
        [ch(0), ch(1), ch(2), 255]
    };
    if c0 > c1 || !allow_punch_through {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(2, 1, 3),
            mix(1, 2, 3),
        ]
    } else {
        [
            [a[0], a[1], a[2], 255],
            [b[0], b[1], b[2], 255],
            mix(1, 1, 2),
            [0, 0, 0, 0],
        ]
    }
}

/// The eight values of a BC3/BC4 alpha block.
fn alpha_palette(a0: u8, a1: u8) -> [u8; 8] {
    let (x, y) = (u32::from(a0), u32::from(a1));
    let mut p = [a0, a1, 0, 0, 0, 0, 0, 255];
    if a0 > a1 {
        for i in 1..=6u32 {
            p[(i + 1) as usize] = (((7 - i) * x + i * y) / 7) as u8;
        }
    } else {
        for i in 1..=4u32 {
            p[(i + 1) as usize] = (((5 - i) * x + i * y) / 5) as u8;
        }
        p[6] = 0;
        p[7] = 255;
    }
    p
}

/// The 16 values of a BC3/BC4 single-channel block.
fn decode_channel(block: &[u8]) -> [u8; 16] {
    let pal = alpha_palette(block[0], block[1]);
    let mut bits = 0u64;
    for (i, &b) in block[2..8].iter().enumerate() {
        bits |= u64::from(b) << (8 * i);
    }
    let mut out = [0u8; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = pal[((bits >> (3 * i)) & 7) as usize];
    }
    out
}

fn decode_block(kind: Block, block: &[u8]) -> [[u8; 4]; 16] {
    let mut px = [[0u8; 4]; 16];
    match kind {
        Block::Bc1 | Block::Bc2 | Block::Bc3 => {
            let colour = if kind == Block::Bc1 {
                block
            } else {
                &block[8..16]
            };
            let pal = bc1_palette(colour, kind == Block::Bc1);
            let idx = u32::from_le_bytes([colour[4], colour[5], colour[6], colour[7]]);
            for (i, p) in px.iter_mut().enumerate() {
                *p = pal[((idx >> (2 * i)) & 3) as usize];
            }
            match kind {
                Block::Bc2 => {
                    for (i, p) in px.iter_mut().enumerate() {
                        let nib = (block[i / 2] >> (4 * (i % 2))) & 15;
                        p[3] = nib * 17;
                    }
                }
                Block::Bc3 => {
                    for (p, a) in px.iter_mut().zip(decode_channel(&block[..8])) {
                        p[3] = a;
                    }
                }
                _ => {}
            }
        }
        Block::Bc4 => {
            for (p, v) in px.iter_mut().zip(decode_channel(block)) {
                *p = [v, v, v, 255];
            }
        }
        Block::Bc7 => px = crate::bc7::decode_block(block),
        Block::Bc5 => {
            let (r, g) = (decode_channel(&block[..8]), decode_channel(&block[8..16]));
            for (i, p) in px.iter_mut().enumerate() {
                *p = [r[i], g[i], 0, 255];
            }
        }
    }
    px
}

fn decode_blocks(kind: Block, data: &[u8], w: usize, h: usize) -> Result<Vec<u8>> {
    let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
    let need = bw * bh * kind.bytes();
    let data = data
        .get(..need)
        .ok_or_else(|| Error::truncated("DDS pixel data"))?;
    let mut out = vec![0u8; w * h * 4];
    for by in 0..bh {
        for bx in 0..bw {
            let at = (by * bw + bx) * kind.bytes();
            let block = data.get(at..at + kind.bytes()).unwrap_or(&[]);
            if block.len() < kind.bytes() {
                continue;
            }
            let px = decode_block(kind, block);
            for (i, p) in px.iter().enumerate() {
                let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                if x < w && y < h {
                    if let Some(d) = out.get_mut((y * w + x) * 4..(y * w + x) * 4 + 4) {
                        d.copy_from_slice(p);
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Value of the bits selected by `mask`, scaled to 0..=255.
fn channel(v: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    ((u64::from((v & mask) >> shift) * 255 + max / 2) / max) as u8
}

fn decode_masked(
    data: &[u8],
    w: usize,
    h: usize,
    bits: u32,
    masks: [u32; 4],
    luminance: bool,
    alpha_flag: bool,
) -> Result<Image> {
    // Some writers put the masks of narrow formats at the top of the 32-bit field.
    let masks = masks.map(|m| {
        if bits < 32 && m >> bits != 0 {
            m >> (32 - bits)
        } else {
            m
        }
    });
    let bytes = (bits / 8) as usize;
    let need = w * h * bytes;
    let data = data
        .get(..need)
        .ok_or_else(|| Error::truncated("DDS pixel data"))?;
    let has_alpha = masks[3] != 0 && (alpha_flag || luminance || masks[0] == 0);
    let alpha_only = masks[0] == 0 && masks[1] == 0 && masks[2] == 0 && masks[3] != 0;
    let (layout, mut out) = if luminance {
        (
            if has_alpha {
                Layout::GrayAlpha
            } else {
                Layout::Gray
            },
            Vec::new(),
        )
    } else if alpha_only {
        (Layout::GrayAlpha, Vec::new())
    } else if has_alpha {
        (Layout::Rgba, Vec::new())
    } else {
        (Layout::Rgb, Vec::new())
    };
    out.reserve(w * h * layout.channels());
    for px in data.chunks_exact(bytes) {
        let mut v = 0u32;
        for (i, &b) in px.iter().enumerate() {
            v |= u32::from(b) << (8 * i);
        }
        let [r, g, b, a] = [
            channel(v, masks[0]),
            channel(v, masks[1]),
            channel(v, masks[2]),
            channel(v, masks[3]),
        ];
        if alpha_only {
            out.extend_from_slice(&[255, a]);
        } else if luminance {
            out.push(r);
            if has_alpha {
                out.push(a);
            }
        } else {
            out.extend_from_slice(&[r, g, b]);
            if has_alpha {
                out.push(a);
            }
        }
    }
    Image::new(w as u32, h as u32, layout, Depth::Eight, out)
}

pub fn decode(data: &[u8], limits: &Limits) -> Result<(Image, Notes)> {
    let mut notes = Notes::new();
    if !sniff(data) {
        return Err(Error::new(ErrorCode::UnsupportedFormat, "not a DDS file"));
    }
    if data.len() < 128 {
        return Err(Error::truncated("DDS header"));
    }
    let (height, width) = (le32(data, 12), le32(data, 16));
    let mip_count = le32(data, 28);
    if width == 0 || height == 0 {
        return Err(Error::corrupt("DDS image has no size"));
    }
    limits.check_pixels(width, height)?;
    limits.check_alloc(u64::from(width) * u64::from(height) * 4)?;
    if le32(data, 76) != 32 {
        return Err(Error::corrupt("DDS pixel format header has the wrong size"));
    }
    let (pf_flags, four_cc) = (le32(data, 80), &data[84..88]);
    let bits = le32(data, 88);
    let masks = [
        le32(data, 92),
        le32(data, 96),
        le32(data, 100),
        le32(data, 104),
    ];
    let caps2 = le32(data, 112);
    let mut offset = 128;
    let format = if pf_flags & DDPF_FOURCC != 0 {
        match four_cc {
            b"DXT1" => Format::Compressed(Block::Bc1),
            b"DXT2" | b"DXT3" => Format::Compressed(Block::Bc2),
            b"DXT4" | b"DXT5" => Format::Compressed(Block::Bc3),
            b"ATI1" | b"BC4U" => Format::Compressed(Block::Bc4),
            b"ATI2" | b"BC5U" => Format::Compressed(Block::Bc5),
            b"DX10" => {
                if data.len() < 148 {
                    return Err(Error::truncated("DDS DX10 header"));
                }
                offset = 148;
                if le32(data, 132) != 3 && le32(data, 132) != 0 {
                    return Err(unsupported("non-2D resource"));
                }
                dxgi_format(le32(data, 128))?
            }
            other => {
                return Err(unsupported(&String::from_utf8_lossy(other)));
            }
        }
    } else if pf_flags & (DDPF_RGB | DDPF_LUMINANCE | DDPF_ALPHA) != 0 {
        if ![8, 16, 24, 32].contains(&bits) {
            return Err(unsupported(&format!("{bits}-bit pixels")));
        }
        Format::Masked {
            bits,
            masks: if pf_flags & DDPF_ALPHA != 0 && pf_flags & DDPF_RGB == 0 {
                [0, 0, 0, masks[3]]
            } else {
                masks
            },
            luminance: pf_flags & DDPF_LUMINANCE != 0,
        }
    } else {
        return Err(unsupported("pixel format"));
    };
    let body = data.get(offset..).unwrap_or(&[]);
    let (w, h) = (width as usize, height as usize);
    let img = match format {
        Format::Compressed(kind) => {
            let rgba = decode_blocks(kind, body, w, h)?;
            let img = Image::new(width, height, Layout::Rgba, Depth::Eight, rgba)?;
            let target = match kind {
                Block::Bc4 => Layout::Gray,
                Block::Bc5 => Layout::Rgb,
                _ if img.has_translucency() => Layout::Rgba,
                _ => Layout::Rgb,
            };
            ops::convert_layout(&img, target, [0, 0, 0]).0
        }
        Format::Masked {
            bits,
            masks,
            luminance,
        } => decode_masked(
            body,
            w,
            h,
            bits,
            masks,
            luminance,
            pf_flags & DDPF_ALPHAPIXELS != 0,
        )?,
    };
    if mip_count > 1 || caps2 & 0x200 != 0 || caps2 & 0x20_0000 != 0 {
        notes.push(Note::new(
            Severity::Info,
            "dds.first_surface",
            "only the top mipmap level of the first surface was read",
        ));
    }
    Ok((img, notes))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    /// Uncompressed 32-bit BGRA.
    None,
    Bc1,
    Bc3,
}

#[derive(Debug, Clone, Copy)]
pub struct EncodeOptions {
    pub compression: Compression,
}

impl Default for EncodeOptions {
    fn default() -> Self {
        Self {
            compression: Compression::Bc3,
        }
    }
}

fn to_565(c: [f32; 3]) -> u16 {
    let q = |v: f32, max: f32| (v.clamp(0.0, 255.0) * max / 255.0 + 0.5) as u16;
    (q(c[0], 31.0) << 11) | (q(c[1], 63.0) << 5) | q(c[2], 31.0)
}

/// Encodes one 4x4 block of RGBA pixels as BC1 colour data. Transparent pixels (alpha < 128)
/// are only honoured when `punch` is set, which selects the three-colour mode.
fn encode_bc1_block(px: &[[u8; 4]; 16], punch: bool) -> [u8; 8] {
    let opaque: Vec<[f32; 3]> = px
        .iter()
        .filter(|p| !punch || p[3] >= 128)
        .map(|p| [f32::from(p[0]), f32::from(p[1]), f32::from(p[2])])
        .collect();
    let has_clear = punch && opaque.len() < 16;
    let mut out = [0u8; 8];
    if opaque.is_empty() {
        // All transparent: any colours, every index 3.
        out[4..8].copy_from_slice(&[0xFF; 4]);
        return out;
    }
    let n = opaque.len() as f32;
    let mean: [f32; 3] = std::array::from_fn(|k| opaque.iter().map(|c| c[k]).sum::<f32>() / n);
    // Principal axis by power iteration on the covariance matrix.
    let mut cov = [[0f32; 3]; 3];
    for c in &opaque {
        let d: [f32; 3] = std::array::from_fn(|k| c[k] - mean[k]);
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j];
            }
        }
    }
    let mut axis = [1f32, 1.0, 1.0];
    for _ in 0..8 {
        let next: [f32; 3] =
            std::array::from_fn(|i| (0..3).map(|j| cov[i][j] * axis[j]).sum::<f32>());
        let len = next.iter().map(|v| v * v).sum::<f32>().sqrt();
        if len < 1e-6 {
            break;
        }
        axis = next.map(|v| v / len);
    }
    let proj = |c: &[f32; 3]| (0..3).map(|k| (c[k] - mean[k]) * axis[k]).sum::<f32>();
    let (lo, hi) = opaque
        .iter()
        .map(proj)
        .fold((f32::MAX, f32::MIN), |(l, h), t| (l.min(t), h.max(t)));
    let end = |t: f32| -> [f32; 3] { std::array::from_fn(|k| mean[k] + axis[k] * t) };
    let (mut c0, mut c1) = (to_565(end(hi)), to_565(end(lo)));
    // Four-colour mode needs c0 > c1; three-colour mode needs c0 <= c1.
    if has_clear {
        if c0 > c1 {
            std::mem::swap(&mut c0, &mut c1);
        }
    } else if c0 < c1 {
        std::mem::swap(&mut c0, &mut c1);
    } else if c0 == c1 && c0 != 0 {
        c1 = c0 - 1;
    } else if c0 == c1 {
        c0 = 1;
    }
    let mut block = [0u8; 8];
    block[0..2].copy_from_slice(&c0.to_le_bytes());
    block[2..4].copy_from_slice(&c1.to_le_bytes());
    let pal = bc1_palette(&block, true);
    let mut idx = 0u32;
    for (i, p) in px.iter().enumerate() {
        let k = if punch && p[3] < 128 {
            3
        } else {
            let limit = if has_clear { 3 } else { 4 };
            (0..limit)
                .min_by_key(|&k| {
                    (0..3)
                        .map(|c| {
                            let d = i32::from(pal[k][c]) - i32::from(p[c]);
                            d * d
                        })
                        .sum::<i32>()
                })
                .unwrap_or(0) as u32
        };
        idx |= k << (2 * i);
    }
    block[4..8].copy_from_slice(&idx.to_le_bytes());
    block
}

/// Encodes 16 alpha values as a BC3 alpha block (eight-value mode).
fn encode_alpha_block(a: &[u8; 16]) -> [u8; 8] {
    let (lo, hi) = a
        .iter()
        .fold((255u8, 0u8), |(l, h), &v| (l.min(v), h.max(v)));
    // a0 > a1 selects the eight-value mode; equal endpoints use the six-value one harmlessly.
    let (a0, a1) = (hi, lo);
    let pal = alpha_palette(a0, a1);
    let mut bits = 0u64;
    for (i, &v) in a.iter().enumerate() {
        let k = (0..8)
            .min_by_key(|&k| (i32::from(pal[k]) - i32::from(v)).abs())
            .unwrap_or(0) as u64;
        bits |= k << (3 * i);
    }
    let mut out = [0u8; 8];
    out[0] = a0;
    out[1] = a1;
    out[2..8].copy_from_slice(&bits.to_le_bytes()[..6]);
    out
}

fn header(w: u32, h: u32, pf: [u32; 8], linear_size: u32, flags: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(128);
    out.extend_from_slice(b"DDS ");
    let mut words = vec![124, flags, h, w, linear_size, 0, 1];
    words.extend([0u32; 11]);
    words.extend([32, pf[0], pf[1], pf[2], pf[3], pf[4], pf[5], pf[6]]);
    words.extend([0x1000, 0, 0, 0, 0]);
    for v in words {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

pub fn encode(img: &Image, opts: &EncodeOptions) -> Result<(Vec<u8>, Notes)> {
    let mut notes = Notes::new();
    let (e8, n) = ops::convert_depth(img, Depth::Eight);
    notes.extend(n);
    let (rgba, _) = ops::convert_layout(&e8, Layout::Rgba, [255, 255, 255]);
    let (w, h) = (rgba.width as usize, rgba.height as usize);
    // DDSD_CAPS | HEIGHT | WIDTH | PIXELFORMAT
    let base = 0x1 | 0x2 | 0x4 | 0x1000;
    let (mut out, body) = match opts.compression {
        Compression::None => {
            let mut body = Vec::with_capacity(w * h * 4);
            for p in rgba.data.as_chunks::<4>().0 {
                body.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
            }
            let pf = [
                DDPF_RGB | DDPF_ALPHAPIXELS,
                0,
                32,
                0xFF_0000,
                0xFF00,
                0xFF,
                0xFF00_0000,
                0,
            ];
            (
                header(rgba.width, rgba.height, pf, (w * 4) as u32, base | 0x8),
                body,
            )
        }
        kind => {
            let bc3 = kind == Compression::Bc3;
            let (bw, bh) = (w.div_ceil(4), h.div_ceil(4));
            let mut body = Vec::with_capacity(bw * bh * if bc3 { 16 } else { 8 });
            let mut lossy_alpha = false;
            for by in 0..bh {
                for bx in 0..bw {
                    let mut px = [[0u8; 4]; 16];
                    for (i, p) in px.iter_mut().enumerate() {
                        // Edge blocks repeat the last row and column.
                        let x = (bx * 4 + i % 4).min(w - 1);
                        let y = (by * 4 + i / 4).min(h - 1);
                        if let Some(s) = rgba.data.get((y * w + x) * 4..(y * w + x) * 4 + 4) {
                            p.copy_from_slice(s);
                        }
                    }
                    if bc3 {
                        let alpha: [u8; 16] = std::array::from_fn(|i| px[i][3]);
                        body.extend_from_slice(&encode_alpha_block(&alpha));
                    } else {
                        lossy_alpha |= px.iter().any(|p| p[3] != 0 && p[3] != 255);
                    }
                    body.extend_from_slice(&encode_bc1_block(&px, !bc3));
                }
            }
            if lossy_alpha {
                notes.push(Note::new(
                    Severity::Loss,
                    "dds.alpha_binary",
                    "DXT1 transparency is on or off; partly transparent pixels were rounded",
                ));
            }
            notes.push(Note::new(
                Severity::Info,
                "image.lossy_compression",
                if bc3 {
                    "block-compressed DDS (BC3 / DXT5)"
                } else {
                    "block-compressed DDS (BC1 / DXT1)"
                },
            ));
            let cc = u32::from_le_bytes(if bc3 { *b"DXT5" } else { *b"DXT1" });
            let pf = [DDPF_FOURCC, cc, 0, 0, 0, 0, 0, 0];
            (
                header(
                    rgba.width,
                    rgba.height,
                    pf,
                    body.len() as u32,
                    base | 0x8_0000,
                ),
                body,
            )
        }
    };
    out.extend_from_slice(&body);
    Ok((out, notes))
}

#[cfg(test)]
#[allow(clippy::indexing_slicing, clippy::unwrap_used)]
mod tests {
    use super::*;

    fn picture(w: u32, h: u32) -> Image {
        let mut d = Vec::new();
        for y in 0..h {
            for x in 0..w {
                d.extend_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x + y) * 9) as u8,
                    if x < 3 { 0 } else { 255 },
                ]);
            }
        }
        Image::new(w, h, Layout::Rgba, Depth::Eight, d).unwrap()
    }

    #[test]
    fn uncompressed_roundtrips_exactly() {
        let img = picture(13, 9);
        let (bytes, _) = encode(
            &img,
            &EncodeOptions {
                compression: Compression::None,
            },
        )
        .unwrap();
        assert!(sniff(&bytes));
        let (back, _) = decode(&bytes, &Limits::default()).unwrap();
        assert_eq!(back.data, img.data);
    }

    #[test]
    fn block_compression_stays_close_and_keeps_alpha() {
        let img = picture(21, 14);
        for (c, tol) in [(Compression::Bc3, 24.0), (Compression::Bc1, 24.0)] {
            let (bytes, _) = encode(&img, &EncodeOptions { compression: c }).unwrap();
            let (back, _) = decode(&bytes, &Limits::default()).unwrap();
            assert_eq!((back.width, back.height), (21, 14));
            let back = ops::convert_layout(&back, Layout::Rgba, [0, 0, 0]).0;
            let mut err = 0f64;
            for (a, b) in back
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .zip(img.data.as_chunks::<4>().0)
            {
                assert_eq!(a[3] == 0, b[3] == 0);
                if b[3] != 0 {
                    err += (0..3).map(|k| f64::from(a[k].abs_diff(b[k]))).sum::<f64>() / 3.0;
                }
            }
            let mean = err / (21.0 * 14.0);
            assert!(mean < tol, "{c:?}: mean error {mean}");
        }
    }

    #[test]
    fn alpha_and_colour_blocks_decode() {
        // A BC3 block: alpha 255..0 ramp, colour block of one flat colour.
        let mut b = vec![255, 0];
        b.extend_from_slice(&[1 << 3, 0, 0, 0, 0, 0]); // pixel 1 uses index 1 (alpha 0)
        let c = 0xF800u16; // pure red
        b.extend_from_slice(&c.to_le_bytes());
        b.extend_from_slice(&c.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        let px = decode_block(Block::Bc3, &b);
        assert_eq!(px[0], [255, 0, 0, 255]);
        assert_eq!(px[1], [255, 0, 0, 0]);
        let pal = alpha_palette(255, 0);
        assert_eq!(pal[2], 218);
        assert_eq!(alpha_palette(0, 255)[6..], [0, 255]);
    }

    #[test]
    fn damaged_files_are_errors() {
        let (good, _) = encode(&picture(16, 16), &EncodeOptions::default()).unwrap();
        for cut in [0, 3, 20, 127, 128, good.len() - 1] {
            assert!(
                decode(&good[..cut], &Limits::default()).is_err(),
                "cut {cut}"
            );
        }
        let mut bad = good.clone();
        bad[84..88].copy_from_slice(b"BC7?");
        assert_eq!(
            decode(&bad, &Limits::default()).unwrap_err().code,
            ErrorCode::UnsupportedVariant
        );
    }
}
