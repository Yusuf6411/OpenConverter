//! The compact form in which a JPEG XL file stores an ICC profile (ISO/IEC 18181-1, annex on
//! ICC profile encoding): a predicted 128-byte header, a tag table described by short commands,
//! and the tag data, optionally byte-shuffled or delta-predicted.
use crate::bits::bad;
use oc_core::Result;

/// Largest profile we are willing to rebuild.
const MAX_SIZE: u64 = 1 << 28;

const COMMON_TAGS: [&[u8; 4]; 19] = [
    b"rTRC", b"rXYZ", b"cprt", b"wtpt", b"bkpt", b"rXYZ", b"gXYZ", b"bXYZ", b"kXYZ", b"rTRC",
    b"gTRC", b"bTRC", b"kTRC", b"chad", b"desc", b"chrm", b"dmnd", b"dmdd", b"lumi",
];
const COMMON_DATA: [&[u8; 4]; 8] = [
    b"XYZ ", b"desc", b"text", b"mluc", b"para", b"curv", b"sf32", b"gbd ",
];

/// A cursor over a byte slice that reads LEB128-style varints.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn byte(&mut self) -> Option<u8> {
        let b = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..63).step_by(7) {
            let b = self.byte().ok_or_else(|| bad("ICC stream is too short"))?;
            value |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                break;
            }
        }
        Ok(value)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.data.len())
            .ok_or_else(|| bad("ICC stream is too short"))?;
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }
}

/// The byte the header predictor expects at `idx`.
fn predict_header(idx: usize, size: u32, header: &[u8]) -> u8 {
    let h = |i: usize| header.get(i).copied().unwrap_or(0);
    match idx {
        0..=3 => size.to_be_bytes()[idx],
        8 => 4,
        12..=23 => b"mntrRGB XYZ "[idx - 12],
        36..=39 => b"acsp"[idx - 36],
        41..=43 if h(40) == b'A' => b"APPL"[idx - 40],
        41..=43 if h(40) == b'M' => b"MSFT"[idx - 40],
        42..=43 if h(40) == b'S' && h(41) == b'G' => b"SGI "[idx - 40],
        42..=43 if h(40) == b'S' && h(41) == b'U' => b"SUNW"[idx - 40],
        70 => 246,
        71 => 214,
        73 => 1,
        78 => 211,
        79 => 45,
        80..=83 => h(4 + idx - 80),
        _ => 0,
    }
}

/// Undo a 2-byte shuffle: the first half holds the high bytes, the second half the low bytes.
fn unshuffle2(b: &[u8]) -> Vec<u8> {
    let half = b.len() / 2;
    let odd = b.len() % 2;
    let mut out = Vec::with_capacity(b.len());
    for i in 0..half {
        out.push(b[i]);
        out.push(b[i + half + odd]);
    }
    if odd == 1 {
        out.push(b[half]);
    }
    out
}

/// Undo a 4-byte shuffle (four byte planes).
fn unshuffle4(b: &[u8]) -> Vec<u8> {
    let step = b.len() / 4;
    let wide = b.len() % 4;
    let mut out = Vec::with_capacity(b.len());
    for i in 0..step {
        let mut at = i;
        for lane in 0..4 {
            out.push(b[at]);
            at += step + usize::from(lane < wide);
        }
    }
    for i in 1..=wide {
        out.push(b[(step + 1) * i - 1]);
    }
    out
}

/// Rebuild the profile from its compact form.
pub fn decode(stream: &[u8]) -> Result<Vec<u8>> {
    let mut r = Reader {
        data: stream,
        pos: 0,
    };
    let output_size = r.varint()?;
    let commands_size = r.varint()?;
    if output_size > MAX_SIZE || commands_size > stream.len() as u64 {
        return Err(bad("invalid ICC stream sizes"));
    }
    let commands = r.take(commands_size as usize)?;
    let mut data = Reader {
        data: &stream[r.pos..],
        pos: 0,
    };
    let mut cmd = Reader {
        data: commands,
        pos: 0,
    };

    let header_len = output_size.min(128) as usize;
    let header_data = data.take(header_len)?;
    let mut out = Vec::with_capacity(output_size as usize);
    for (i, &e) in header_data.iter().enumerate() {
        out.push(predict_header(i, output_size as u32, header_data).wrapping_add(e));
    }
    if output_size <= 128 {
        return Ok(out);
    }

    // tag table
    if let Some(num_tags) = cmd.varint()?.checked_sub(1) {
        if (output_size - 128) / 12 < num_tags {
            return Err(bad("too many ICC tags"));
        }
        let num_tags = num_tags as u32;
        out.extend_from_slice(&num_tags.to_be_bytes());
        let mut prev_start = num_tags * 12 + 128;
        let mut prev_size = 0u32;
        while let Some(command) = cmd.byte() {
            let code = command & 63;
            let tag: [u8; 4] = match code {
                0 => break,
                1 => {
                    let t = data.take(4)?;
                    [t[0], t[1], t[2], t[3]]
                }
                2..=20 => *COMMON_TAGS[usize::from(code) - 2],
                _ => return Err(bad("invalid ICC tag code")),
            };
            let start = if command & 64 == 0 {
                prev_start.wrapping_add(prev_size)
            } else {
                cmd.varint()? as u32
            };
            let size = if command & 128 != 0 {
                cmd.varint()? as u32
            } else if matches!(
                &tag,
                b"rXYZ" | b"gXYZ" | b"bXYZ" | b"kXYZ" | b"wtpt" | b"bkpt" | b"lumi"
            ) {
                20
            } else {
                prev_size
            };
            if u64::from(start) + u64::from(size) > output_size {
                return Err(bad("ICC tag lies outside the profile"));
            }
            prev_start = start;
            prev_size = size;
            let mut entry = |t: &[u8; 4], s: u32| {
                out.extend_from_slice(t);
                out.extend_from_slice(&s.to_be_bytes());
                out.extend_from_slice(&size.to_be_bytes());
            };
            entry(&tag, start);
            if code == 2 {
                entry(b"gTRC", start);
                entry(b"bTRC", start);
            } else if code == 3 {
                entry(b"gXYZ", start.wrapping_add(size));
                entry(b"bXYZ", start.wrapping_add(size.wrapping_mul(2)));
            }
        }
    }

    // tag data
    while let Some(command) = cmd.byte() {
        match command {
            1 => {
                let n = cmd.varint()? as usize;
                out.extend_from_slice(data.take(n)?);
            }
            2 | 3 => {
                let n = cmd.varint()? as usize;
                let bytes = data.take(n)?;
                out.extend_from_slice(&if command == 2 {
                    unshuffle2(bytes)
                } else {
                    unshuffle4(bytes)
                });
            }
            4 => {
                let flags = cmd.byte().ok_or_else(|| bad("ICC stream is too short"))?;
                let width = usize::from(flags & 3) + 1;
                let order = usize::from((flags >> 2) & 3);
                if width == 3 || order == 3 {
                    return Err(bad("invalid ICC predictor"));
                }
                let stride = if flags & 16 == 0 {
                    width
                } else {
                    let s = cmd.varint()? as usize;
                    if s < width {
                        return Err(bad("invalid ICC stride"));
                    }
                    s
                };
                if stride.saturating_mul(4) >= out.len() {
                    return Err(bad("ICC stride is too large"));
                }
                let n = cmd.varint()? as usize;
                let raw = data.take(n)?;
                let bytes = match width {
                    2 => unshuffle2(raw),
                    4 => unshuffle4(raw),
                    _ => raw.to_vec(),
                };
                for i in (0..n).step_by(width) {
                    let mut prev = [0u32; 3];
                    for (j, p) in prev.iter_mut().enumerate().take(order + 1) {
                        let at = out.len() - stride * (j + 1);
                        let mut b = [0u8; 4];
                        b[4 - width..].copy_from_slice(&out[at..at + width]);
                        *p = u32::from_be_bytes(b);
                    }
                    let predicted = match order {
                        0 => prev[0],
                        1 => prev[0].wrapping_mul(2).wrapping_sub(prev[1]),
                        _ => prev[0]
                            .wrapping_sub(prev[1])
                            .wrapping_mul(3)
                            .wrapping_add(prev[2]),
                    };
                    for j in 0..width.min(n - i) {
                        out.push(
                            u32::from(bytes[i + j]).wrapping_add(predicted >> (8 * (width - 1 - j)))
                                as u8,
                        );
                    }
                }
            }
            10 => {
                out.extend_from_slice(b"XYZ \0\0\0\0");
                out.extend_from_slice(data.take(12)?);
            }
            16..=23 => {
                out.extend_from_slice(COMMON_DATA[usize::from(command) - 16]);
                out.extend_from_slice(&[0; 4]);
            }
            _ => return Err(bad("invalid ICC command")),
        }
    }
    if out.len() as u64 != output_size {
        return Err(bad("rebuilt ICC profile has the wrong size"));
    }
    Ok(out)
}
