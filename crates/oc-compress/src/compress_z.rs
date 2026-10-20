//! Unix `compress` (.Z) files: adaptive LZW with 9 to 16 bit codes. Read only.
#![allow(clippy::indexing_slicing)]
use oc_core::{Error, ErrorCode, Limits, Progress, Result};

pub fn decompress(input: &[u8], limits: &Limits, progress: &dyn Progress) -> Result<Vec<u8>> {
    if input.len() < 3 || input[0] != 0x1F || input[1] != 0x9D {
        return Err(Error::corrupt("not a compress (.Z) file"));
    }
    let flags = input[2];
    let max_bits = u32::from(flags & 0x1F);
    let block_mode = flags & 0x80 != 0;
    if flags & 0x60 != 0 {
        return Err(Error::corrupt("compress: reserved flag bits set"));
    }
    if !(9..=16).contains(&max_bits) {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "compress: code size outside 9 to 16 bits",
        ));
    }
    let data = &input[3..];
    let total_bits = data.len() * 8;
    let max_max = 1usize << max_bits;
    let mut prefix = vec![0u16; max_max];
    let mut suffix = vec![0u8; max_max];
    let mut n_bits = 9u32;
    let mut max_code = (1usize << n_bits) - 1;
    let first_free = if block_mode { 257 } else { 256 };
    let mut free = first_free;
    let mut pos = 0usize; // in bits
    let mut out: Vec<u8> = Vec::new();
    let mut old: Option<usize> = None;
    let mut fin = 0u8;
    let mut stack: Vec<u8> = Vec::new();
    let mut count = 0usize;
    // Codes come in groups of eight; a width change or a clear pads to the end of the group,
    // counted from where the current width began.
    let mut region = 0usize;
    let align = |pos: usize, n_bits: u32, region: usize| -> usize {
        let group = n_bits as usize * 8;
        region + (pos - region).div_ceil(group) * group
    };
    loop {
        if free > max_code && n_bits < max_bits {
            pos = align(pos, n_bits, region);
            region = pos;
            n_bits += 1;
            max_code = if n_bits == max_bits {
                max_max
            } else {
                (1usize << n_bits) - 1
            };
        }
        if pos + n_bits as usize > total_bits {
            break;
        }
        let mut code = 0usize;
        for k in 0..n_bits as usize {
            let p = pos + k;
            code |= usize::from((data[p / 8] >> (p % 8)) & 1) << k;
        }
        pos += n_bits as usize;
        count += 1;
        if count & 0xFFFF == 0 {
            progress.check_cancelled()?;
            limits.check_expansion(input.len() as u64, out.len() as u64)?;
        }
        if block_mode && code == 256 {
            pos = align(pos, n_bits, region);
            region = pos;
            n_bits = 9;
            max_code = (1usize << n_bits) - 1;
            free = first_free;
            old = None;
            continue;
        }
        let Some(prev) = old else {
            if code > 255 {
                return Err(Error::corrupt("compress: first code is not a literal"));
            }
            out.push(code as u8);
            fin = code as u8;
            old = Some(code);
            continue;
        };
        let incode = code;
        stack.clear();
        let mut c = code;
        if c >= free {
            if c > free {
                return Err(Error::corrupt("compress: code beyond the table"));
            }
            stack.push(fin);
            c = prev;
        }
        while c >= 256 {
            if c >= free {
                return Err(Error::corrupt("compress: bad table link"));
            }
            stack.push(suffix[c]);
            c = usize::from(prefix[c]);
        }
        fin = c as u8;
        stack.push(fin);
        out.extend(stack.iter().rev());
        if free < max_max {
            prefix[free] = prev as u16;
            suffix[free] = fin;
            free += 1;
        }
        old = Some(incode);
        limits.check_alloc(out.len() as u64)?;
    }
    Ok(out)
}
