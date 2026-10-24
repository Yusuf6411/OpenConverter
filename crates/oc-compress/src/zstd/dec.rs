//! Zstandard frame and block decoder (RFC 8878).
#![allow(clippy::indexing_slicing)] // sizes are validated before the slices are taken
use super::fse::{read_counts, BackReader, Table};
use super::huf::{read_tree, HufTable};
use oc_checksum::xxhash64;
use oc_core::{Error, ErrorCode, Limits, Progress, Result};

pub(crate) const LL_BASE: [u32; 36] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 18, 20, 22, 24, 28, 32, 40, 48, 64,
    128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];
pub(crate) const LL_BITS: [u8; 36] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 3, 3, 4, 6, 7, 8, 9, 10, 11,
    12, 13, 14, 15, 16,
];
pub(crate) const ML_BASE: [u32; 53] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
    28, 29, 30, 31, 32, 33, 34, 35, 37, 39, 41, 43, 47, 51, 59, 67, 83, 99, 131, 259, 515, 1027,
    2051, 4099, 8195, 16387, 32771, 65539,
];
pub(crate) const ML_BITS: [u8; 53] = {
    let mut t = [0u8; 53];
    let tail = [
        1u8, 1, 1, 1, 2, 2, 3, 3, 4, 4, 5, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16,
    ];
    let mut i = 0;
    while i < tail.len() {
        t[32 + i] = tail[i];
        i += 1;
    }
    t
};
pub(crate) const LL_DEFAULT: [i16; 36] = [
    4, 3, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 2, 1, 1, 1, 1, 1,
    -1, -1, -1, -1,
];
pub(crate) const ML_DEFAULT: [i16; 53] = {
    let mut t = [1i16; 53];
    let head = [1i16, 4, 3, 2, 2, 2, 2, 2, 2];
    let mut i = 0;
    while i < head.len() {
        t[i] = head[i];
        i += 1;
    }
    let mut j = 46;
    while j < 53 {
        t[j] = -1;
        j += 1;
    }
    t
};
pub(crate) const OF_DEFAULT: [i16; 29] = [
    1, 1, 1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, -1, -1, -1, -1, -1,
];

const MAX_BLOCK: usize = 128 * 1024;

pub(crate) fn default_tables() -> (Table, Table, Table) {
    (
        Table::from_counts(&LL_DEFAULT, 6).unwrap_or_else(|_| Table::rle(0)),
        Table::from_counts(&OF_DEFAULT, 5).unwrap_or_else(|_| Table::rle(0)),
        Table::from_counts(&ML_DEFAULT, 6).unwrap_or_else(|_| Table::rle(0)),
    )
}

struct FrameState {
    huf: Option<HufTable>,
    ll: Option<Table>,
    of: Option<Table>,
    ml: Option<Table>,
    reps: [u32; 3],
}

fn literals(data: &[u8], st: &mut FrameState) -> Result<(Vec<u8>, usize)> {
    let b0 = *data
        .first()
        .ok_or_else(|| Error::truncated("zstd: literals header"))?;
    let kind = b0 & 3;
    let fmt = (b0 >> 2) & 3;
    let need = |n: usize| -> Result<&[u8]> {
        data.get(..n)
            .ok_or_else(|| Error::truncated("zstd: literals header"))
    };
    if kind < 2 {
        let (regen, hdr) = match fmt {
            0 | 2 => (usize::from(b0 >> 3), 1),
            1 => {
                let h = need(2)?;
                ((usize::from(h[0]) >> 4) | (usize::from(h[1]) << 4), 2)
            }
            _ => {
                let h = need(3)?;
                (
                    (usize::from(h[0]) >> 4) | (usize::from(h[1]) << 4) | (usize::from(h[2]) << 12),
                    3,
                )
            }
        };
        if regen > MAX_BLOCK {
            return Err(Error::corrupt("zstd: literals larger than a block"));
        }
        if kind == 0 {
            let body = data
                .get(hdr..hdr + regen)
                .ok_or_else(|| Error::truncated("zstd: raw literals"))?;
            return Ok((body.to_vec(), hdr + regen));
        }
        let b = *data
            .get(hdr)
            .ok_or_else(|| Error::truncated("zstd: RLE literal"))?;
        return Ok((vec![b; regen], hdr + 1));
    }
    let (hdr, regen, comp, streams4) = match fmt {
        0 | 1 => {
            let h = need(3)?;
            let v = u32::from(h[0]) | u32::from(h[1]) << 8 | u32::from(h[2]) << 16;
            (
                3,
                ((v >> 4) & 0x3FF) as usize,
                ((v >> 14) & 0x3FF) as usize,
                fmt == 1,
            )
        }
        2 => {
            let h = need(4)?;
            let v = u32::from_le_bytes([h[0], h[1], h[2], h[3]]);
            (
                4,
                ((v >> 4) & 0x3FFF) as usize,
                ((v >> 18) & 0x3FFF) as usize,
                true,
            )
        }
        _ => {
            let h = need(5)?;
            let v = u64::from(h[0])
                | u64::from(h[1]) << 8
                | u64::from(h[2]) << 16
                | u64::from(h[3]) << 24
                | u64::from(h[4]) << 32;
            (
                5,
                ((v >> 4) & 0x3FFFF) as usize,
                ((v >> 22) & 0x3FFFF) as usize,
                true,
            )
        }
    };
    if regen > MAX_BLOCK {
        return Err(Error::corrupt("zstd: literals larger than a block"));
    }
    let body = data
        .get(hdr..hdr + comp)
        .ok_or_else(|| Error::truncated("zstd: compressed literals"))?;
    let (table, tree_len) = if kind == 2 {
        let (t, n) = read_tree(body)?;
        st.huf = Some(t.clone());
        (t, n)
    } else {
        (
            st.huf
                .clone()
                .ok_or_else(|| Error::corrupt("zstd: literals reuse a missing Huffman table"))?,
            0,
        )
    };
    let streams = body.get(tree_len..).unwrap_or(&[]);
    let mut out = Vec::with_capacity(regen);
    if !streams4 {
        table.decode_stream(streams, regen, &mut out)?;
    } else {
        let j = streams
            .get(..6)
            .ok_or_else(|| Error::truncated("zstd: literal stream sizes"))?;
        let s1 = usize::from(u16::from_le_bytes([j[0], j[1]]));
        let s2 = usize::from(u16::from_le_bytes([j[2], j[3]]));
        let s3 = usize::from(u16::from_le_bytes([j[4], j[5]]));
        let rest = &streams[6..];
        if s1 + s2 + s3 > rest.len() {
            return Err(Error::corrupt("zstd: literal streams overrun"));
        }
        let per = regen.div_ceil(4);
        if per * 3 > regen {
            return Err(Error::corrupt("zstd: literal split"));
        }
        let (a, rest) = rest.split_at(s1);
        let (b, rest) = rest.split_at(s2);
        let (c, d) = rest.split_at(s3);
        table.decode_stream(a, per, &mut out)?;
        table.decode_stream(b, per, &mut out)?;
        table.decode_stream(c, per, &mut out)?;
        table.decode_stream(d, regen - 3 * per, &mut out)?;
    }
    Ok((out, hdr + comp))
}

fn sequence_table(
    mode: u8,
    data: &[u8],
    pos: &mut usize,
    default: Table,
    prev: &Option<Table>,
    max_symbol: usize,
    max_al: u32,
) -> Result<Table> {
    match mode {
        0 => Ok(default),
        1 => {
            let s = *data
                .get(*pos)
                .ok_or_else(|| Error::truncated("zstd: RLE sequence symbol"))?;
            *pos += 1;
            if usize::from(s) > max_symbol {
                return Err(Error::corrupt("zstd: sequence symbol out of range"));
            }
            Ok(Table::rle(s))
        }
        2 => {
            let (counts, al, n) = read_counts(data.get(*pos..).unwrap_or(&[]), max_symbol, max_al)?;
            *pos += n;
            Table::from_counts(&counts, al)
        }
        _ => prev
            .clone()
            .ok_or_else(|| Error::corrupt("zstd: repeated sequence table is missing")),
    }
}

fn block(data: &[u8], out: &mut Vec<u8>, frame_start: usize, st: &mut FrameState) -> Result<()> {
    let (lits, mut pos) = literals(data, st)?;
    let b0 = *data
        .get(pos)
        .ok_or_else(|| Error::truncated("zstd: sequences header"))?;
    pos += 1;
    let nseq = if b0 == 0 {
        0usize
    } else if b0 < 128 {
        usize::from(b0)
    } else if b0 < 255 {
        let b1 = *data
            .get(pos)
            .ok_or_else(|| Error::truncated("zstd: sequence count"))?;
        pos += 1;
        ((usize::from(b0) - 128) << 8) + usize::from(b1)
    } else {
        let h = data
            .get(pos..pos + 2)
            .ok_or_else(|| Error::truncated("zstd: sequence count"))?;
        pos += 2;
        usize::from(h[0]) + (usize::from(h[1]) << 8) + 0x7F00
    };
    let mut lit_pos = 0usize;
    if nseq > 0 {
        let modes = *data
            .get(pos)
            .ok_or_else(|| Error::truncated("zstd: sequence modes"))?;
        pos += 1;
        if modes & 3 != 0 {
            return Err(Error::corrupt("zstd: reserved sequence mode bits set"));
        }
        let (dll, dof, dml) = default_tables();
        let ll = sequence_table(modes >> 6, data, &mut pos, dll, &st.ll, 35, 9)?;
        let of = sequence_table((modes >> 4) & 3, data, &mut pos, dof, &st.of, 31, 8)?;
        let ml = sequence_table((modes >> 2) & 3, data, &mut pos, dml, &st.ml, 52, 9)?;
        st.ll = Some(ll.clone());
        st.of = Some(of.clone());
        st.ml = Some(ml.clone());
        let mut r = BackReader::new(data.get(pos..).unwrap_or(&[]))?;
        let mut sl = r.read(ll.al) as usize;
        let mut so = r.read(of.al) as usize;
        let mut sm = r.read(ml.al) as usize;
        let history = out.len() - frame_start;
        let mut block_out = 0usize;
        for i in 0..nseq {
            let (cl, co, cm) = (ll.cells[sl], of.cells[so], ml.cells[sm]);
            let (lc, oc, mc) = (
                usize::from(cl.symbol),
                usize::from(co.symbol),
                usize::from(cm.symbol),
            );
            if lc > 35 || mc > 52 || oc > 31 {
                return Err(Error::corrupt("zstd: sequence code out of range"));
            }
            let ofv = (1u32 << oc) + r.read(oc as u32);
            let ml_v = ML_BASE[mc] + r.read(u32::from(ML_BITS[mc]));
            let ll_v = LL_BASE[lc] + r.read(u32::from(LL_BITS[lc]));
            if i + 1 < nseq {
                sl = usize::from(cl.base) + r.read(u32::from(cl.nb)) as usize;
                sm = usize::from(cm.base) + r.read(u32::from(cm.nb)) as usize;
                so = usize::from(co.base) + r.read(u32::from(co.nb)) as usize;
            }
            if r.overflow {
                return Err(Error::corrupt("zstd: sequence stream ended early"));
            }
            // Resolve the offset (repeat codes shift when there are no literals).
            let offset = if ofv > 3 {
                let o = ofv - 3;
                st.reps = [o, st.reps[0], st.reps[1]];
                o
            } else {
                let idx = if ll_v == 0 { ofv } else { ofv - 1 }; // 0..=3
                let o = if idx == 3 {
                    st.reps[0].saturating_sub(1)
                } else {
                    st.reps[idx as usize]
                };
                if o == 0 {
                    return Err(Error::corrupt("zstd: zero offset"));
                }
                match idx {
                    0 => {}
                    1 => st.reps = [o, st.reps[0], st.reps[2]],
                    _ => st.reps = [o, st.reps[0], st.reps[1]],
                }
                o
            };
            let ll_n = ll_v as usize;
            let ml_n = ml_v as usize;
            let chunk = lits
                .get(lit_pos..lit_pos + ll_n)
                .ok_or_else(|| Error::corrupt("zstd: sequences use more literals than exist"))?;
            out.extend_from_slice(chunk);
            lit_pos += ll_n;
            block_out += ll_n + ml_n;
            if block_out > MAX_BLOCK + 3 * 1024 * 1024 {
                return Err(Error::corrupt("zstd: block expands too much"));
            }
            let produced = out.len() - frame_start;
            let off = offset as usize;
            if off > produced {
                return Err(Error::corrupt("zstd: offset beyond the start of the frame")
                    .at(out.len() as u64));
            }
            let start = out.len() - off;
            if off >= ml_n {
                out.extend_from_within(start..start + ml_n);
            } else {
                for k in 0..ml_n {
                    let b = out[start + k];
                    out.push(b);
                }
            }
        }
        if r.remaining() != 0 {
            return Err(Error::corrupt("zstd: sequence stream not used exactly"));
        }
        let _ = history;
    }
    out.extend_from_slice(lits.get(lit_pos..).unwrap_or(&[]));
    Ok(())
}

/// Decodes every frame in `input` (skippable frames are skipped).
pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0usize;
    let mut frames = 0;
    while pos < input.len() {
        progress.check_cancelled()?;
        let magic = input
            .get(pos..pos + 4)
            .map(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]]))
            .ok_or_else(|| Error::truncated("zstd: frame header"))?;
        if magic & 0xFFFF_FFF0 == 0x184D_2A50 {
            let n = input
                .get(pos + 4..pos + 8)
                .map(|m| u32::from_le_bytes([m[0], m[1], m[2], m[3]]) as usize)
                .ok_or_else(|| Error::truncated("zstd: skippable frame"))?;
            pos += 8 + n;
            if pos > input.len() {
                return Err(Error::truncated("zstd: skippable frame"));
            }
            continue;
        }
        if magic != 0xFD2F_B528 {
            if frames == 0 {
                return Err(Error::corrupt("not a Zstandard stream"));
            }
            return Err(Error::corrupt("zstd: unexpected data after the last frame").at(pos as u64));
        }
        pos += 4;
        let fd = *input
            .get(pos)
            .ok_or_else(|| Error::truncated("zstd: frame descriptor"))?;
        pos += 1;
        let fcs_flag = fd >> 6;
        let single = fd & 0x20 != 0;
        let checksum = fd & 4 != 0;
        let did = fd & 3;
        if fd & 8 != 0 {
            return Err(Error::corrupt("zstd: reserved descriptor bit set"));
        }
        let mut window = 0u64;
        if !single {
            let w = *input
                .get(pos)
                .ok_or_else(|| Error::truncated("zstd: window"))?;
            pos += 1;
            let exp = u32::from(w >> 3);
            let base = 1u64 << (10 + exp);
            window = base + (base / 8) * u64::from(w & 7);
        }
        let did_len = [0usize, 1, 2, 4][usize::from(did)];
        let dict_id = input
            .get(pos..pos + did_len)
            .ok_or_else(|| Error::truncated("zstd: dictionary id"))?
            .iter()
            .rev()
            .fold(0u32, |a, &b| (a << 8) | u32::from(b));
        pos += did_len;
        if dict_id != 0 {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "zstd: frames that need a dictionary are not supported",
            ));
        }
        let fcs_len = match fcs_flag {
            0 => usize::from(single),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        let fcs_bytes = input
            .get(pos..pos + fcs_len)
            .ok_or_else(|| Error::truncated("zstd: content size"))?;
        pos += fcs_len;
        let mut fcs = fcs_bytes
            .iter()
            .rev()
            .fold(0u64, |a, &b| (a << 8) | u64::from(b));
        if fcs_flag == 1 {
            fcs += 256;
        }
        let known = fcs_len > 0;
        if single {
            window = fcs;
        }
        if window > 1 << 31 {
            return Err(Error::limit("max_alloc", "zstd: window larger than 2 GiB"));
        }
        if known {
            limits.check_alloc(fcs)?;
        }
        let frame_start = out.len();
        let mut st = FrameState {
            huf: None,
            ll: None,
            of: None,
            ml: None,
            reps: [1, 4, 8],
        };
        loop {
            let h = input
                .get(pos..pos + 3)
                .ok_or_else(|| Error::truncated("zstd: block header").at(pos as u64))?;
            let bh = u32::from(h[0]) | u32::from(h[1]) << 8 | u32::from(h[2]) << 16;
            pos += 3;
            let last = bh & 1 == 1;
            let btype = (bh >> 1) & 3;
            let bsize = (bh >> 3) as usize;
            match btype {
                0 => {
                    let b = input
                        .get(pos..pos + bsize)
                        .ok_or_else(|| Error::truncated("zstd: raw block"))?;
                    out.extend_from_slice(b);
                    pos += bsize;
                }
                1 => {
                    let b = *input
                        .get(pos)
                        .ok_or_else(|| Error::truncated("zstd: RLE block"))?;
                    limits.check_alloc((out.len() + bsize) as u64)?;
                    out.resize(out.len() + bsize, b);
                    pos += 1;
                }
                2 => {
                    if bsize > MAX_BLOCK {
                        return Err(Error::corrupt("zstd: compressed block larger than 128 KiB"));
                    }
                    let b = input
                        .get(pos..pos + bsize)
                        .ok_or_else(|| Error::truncated("zstd: compressed block"))?;
                    block(b, &mut out, frame_start, &mut st).map_err(|e| e.at(pos as u64))?;
                    pos += bsize;
                }
                _ => return Err(Error::corrupt("zstd: reserved block type")),
            }
            limits.check_expansion(input.len() as u64, out.len() as u64)?;
            if last {
                break;
            }
        }
        if known && (out.len() - frame_start) as u64 != fcs {
            return Err(Error::corrupt("zstd: content size mismatch"));
        }
        if checksum {
            let c = input
                .get(pos..pos + 4)
                .ok_or_else(|| Error::truncated("zstd: checksum"))?;
            let want = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            let got = xxhash64(&out[frame_start..], 0) as u32;
            if want != got {
                return Err(Error::corrupt("zstd: content checksum mismatch"));
            }
            pos += 4;
        }
        frames += 1;
        progress.report(pos as u64, input.len() as u64, "zstd");
    }
    Ok(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn default_distributions_sum_to_their_table_size() {
        let sum = |t: &[i16]| {
            t.iter()
                .map(|c| i32::from(c.abs().max(if *c == -1 { 1 } else { 0 })))
                .sum::<i32>()
        };
        assert_eq!(sum(&LL_DEFAULT), 64);
        assert_eq!(sum(&ML_DEFAULT), 64);
        assert_eq!(sum(&OF_DEFAULT), 32);
    }
}
