//! XZ branch converters (BCJ) and the delta filter. Each turns relative branch targets into
//! absolute ones for the encoder and back for the decoder, which makes code compress better.
#![allow(clippy::indexing_slicing)] // every access below is bounded by the loop limits
use oc_core::{Error, ErrorCode, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86,
    PowerPc,
    Arm,
    ArmThumb,
    Sparc,
    Arm64,
}

impl Arch {
    pub fn from_filter_id(id: u64) -> Result<Self> {
        Ok(match id {
            0x04 => Self::X86,
            0x05 => Self::PowerPc,
            0x07 => Self::Arm,
            0x08 => Self::ArmThumb,
            0x09 => Self::Sparc,
            0x0A => Self::Arm64,
            0x06 | 0x0B => {
                return Err(Error::new(
                    ErrorCode::UnsupportedVariant,
                    "xz: the IA-64 and RISC-V branch filters are not implemented",
                ))
            }
            _ => return Err(Error::corrupt("xz: unknown filter")),
        })
    }

    pub fn filter_id(self) -> u64 {
        match self {
            Self::X86 => 0x04,
            Self::PowerPc => 0x05,
            Self::Arm => 0x07,
            Self::ArmThumb => 0x08,
            Self::Sparc => 0x09,
            Self::Arm64 => 0x0A,
        }
    }
}

fn msbyte(b: u8) -> bool {
    b == 0 || b == 0xFF
}

fn x86(buf: &mut [u8], enc: bool) {
    const ALLOWED: [bool; 8] = [true, true, true, false, true, false, false, false];
    const BITNUM: [u32; 8] = [0, 1, 2, 2, 3, 3, 3, 3];
    if buf.len() <= 4 {
        return;
    }
    let size = buf.len() - 4;
    let mut prev_pos: usize = usize::MAX;
    let mut prev_mask: u32 = 0;
    let mut i = 0;
    while i < size {
        if buf[i] & 0xFE != 0xE8 {
            i += 1;
            continue;
        }
        let gap = i.wrapping_sub(prev_pos);
        if gap > 3 {
            prev_mask = 0;
        } else {
            prev_mask = (prev_mask << (gap - 1)) & 7;
            if prev_mask != 0 {
                let b = buf[i + 4 - BITNUM[prev_mask as usize] as usize];
                if !ALLOWED[prev_mask as usize] || msbyte(b) {
                    prev_pos = i;
                    prev_mask = (prev_mask << 1) | 1;
                    i += 1;
                    continue;
                }
            }
        }
        prev_pos = i;
        if msbyte(buf[i + 4]) {
            let mut src = u32::from_le_bytes([buf[i + 1], buf[i + 2], buf[i + 3], buf[i + 4]]);
            let mut dest;
            loop {
                let pc = (i as u32).wrapping_add(5);
                dest = if enc { src.wrapping_add(pc) } else { src.wrapping_sub(pc) };
                if prev_mask == 0 {
                    break;
                }
                let j = BITNUM[prev_mask as usize] * 8;
                let b = (dest >> (24 - j)) as u8;
                if !msbyte(b) {
                    break;
                }
                src = dest ^ ((1u32 << (32 - j)) - 1);
            }
            dest &= 0x01FF_FFFF;
            dest |= 0u32.wrapping_sub(dest & 0x0100_0000);
            buf[i + 1..i + 5].copy_from_slice(&dest.to_le_bytes());
            i += 5;
        } else {
            prev_mask = (prev_mask << 1) | 1;
            i += 1;
        }
    }
}

fn arm(buf: &mut [u8], enc: bool) {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i + 3] == 0xEB {
            let src = (u32::from(buf[i + 2]) << 16 | u32::from(buf[i + 1]) << 8 | u32::from(buf[i])) << 2;
            let pc = (i as u32).wrapping_add(8);
            let dest = (if enc { src.wrapping_add(pc) } else { src.wrapping_sub(pc) }) >> 2;
            buf[i] = dest as u8;
            buf[i + 1] = (dest >> 8) as u8;
            buf[i + 2] = (dest >> 16) as u8;
        }
        i += 4;
    }
}

fn arm_thumb(buf: &mut [u8], enc: bool) {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i + 1] & 0xF8 == 0xF0 && buf[i + 3] & 0xF8 == 0xF8 {
            let src = ((u32::from(buf[i + 1]) & 7) << 19
                | u32::from(buf[i]) << 11
                | (u32::from(buf[i + 3]) & 7) << 8
                | u32::from(buf[i + 2]))
                << 1;
            let pc = (i as u32).wrapping_add(4);
            let dest = (if enc { src.wrapping_add(pc) } else { src.wrapping_sub(pc) }) >> 1;
            buf[i + 1] = 0xF0 | ((dest >> 19) & 7) as u8;
            buf[i] = (dest >> 11) as u8;
            buf[i + 3] = 0xF8 | ((dest >> 8) & 7) as u8;
            buf[i + 2] = dest as u8;
            i += 2;
        }
        i += 2;
    }
}

fn ppc(buf: &mut [u8], enc: bool) {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i] >> 2 == 0x12 && buf[i + 3] & 3 == 1 {
            let src = (u32::from(buf[i]) & 3) << 24
                | u32::from(buf[i + 1]) << 16
                | u32::from(buf[i + 2]) << 8
                | (u32::from(buf[i + 3]) & !3);
            let pc = i as u32;
            let dest = if enc { src.wrapping_add(pc) } else { src.wrapping_sub(pc) };
            buf[i] = 0x48 | ((dest >> 24) & 3) as u8;
            buf[i + 1] = (dest >> 16) as u8;
            buf[i + 2] = (dest >> 8) as u8;
            buf[i + 3] = (buf[i + 3] & 3) | (dest as u8 & !3);
        }
        i += 4;
    }
}

fn sparc(buf: &mut [u8], enc: bool) {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if (buf[i] == 0x40 && buf[i + 1] & 0xC0 == 0) || (buf[i] == 0x7F && buf[i + 1] & 0xC0 == 0xC0) {
            let src = u32::from_be_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]) << 2;
            let pc = i as u32;
            let mut dest = (if enc { src.wrapping_add(pc) } else { src.wrapping_sub(pc) }) >> 2;
            dest = ((0u32.wrapping_sub((dest >> 22) & 1) << 22) & 0x3FFF_FFFF) | (dest & 0x3F_FFFF) | 0x4000_0000;
            buf[i..i + 4].copy_from_slice(&dest.to_be_bytes());
        }
        i += 4;
    }
}

fn arm64(buf: &mut [u8], enc: bool) {
    let mut i = 0;
    while i + 4 <= buf.len() {
        let mut instr = u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
        if instr >> 26 == 0x25 {
            let pc = (i as u32) >> 2;
            let dest = if enc { instr.wrapping_add(pc) } else { instr.wrapping_sub(pc) };
            instr = 0x9400_0000 | (dest & 0x03FF_FFFF);
            buf[i..i + 4].copy_from_slice(&instr.to_le_bytes());
        } else if instr & 0x9F00_0000 == 0x9000_0000 {
            let src = ((instr >> 29) & 3) | ((instr >> 3) & 0x001F_FFFC);
            if src.wrapping_add(0x0002_0000) & 0x001C_0000 == 0 {
                let pc = (i as u32) >> 12;
                let dest = if enc { src.wrapping_add(pc) } else { src.wrapping_sub(pc) };
                instr &= 0x9000_001F;
                instr |= (dest & 3) << 29;
                instr |= (dest & 0x0003_FFFC) << 3;
                instr |= 0u32.wrapping_sub(dest & 0x0002_0000) & 0x00E0_0000;
                buf[i..i + 4].copy_from_slice(&instr.to_le_bytes());
            }
        }
        i += 4;
    }
}

/// Converts `buf` in place; `encode` selects relative-to-absolute.
pub fn convert(arch: Arch, buf: &mut [u8], encode: bool) {
    match arch {
        Arch::X86 => x86(buf, encode),
        Arch::PowerPc => ppc(buf, encode),
        Arch::Arm => arm(buf, encode),
        Arch::ArmThumb => arm_thumb(buf, encode),
        Arch::Sparc => sparc(buf, encode),
        Arch::Arm64 => arm64(buf, encode),
    }
}

/// Delta filter: each byte is stored as the difference to the byte `dist` positions earlier.
pub fn delta(buf: &mut [u8], dist: usize, encode: bool) {
    if encode {
        for i in (dist..buf.len()).rev() {
            buf[i] = buf[i].wrapping_sub(buf[i - dist]);
        }
    } else {
        for i in dist..buf.len() {
            buf[i] = buf[i].wrapping_add(buf[i - dist]);
        }
    }
}

/// The 7z BCJ2 decoder: x86 CALL/JMP/Jcc targets are split into a main stream, big-endian
/// absolute call and jump targets, and a range-coded flag per candidate instruction.
pub fn bcj2_decode(main: &[u8], call: &[u8], jump: &[u8], rc: &[u8], out_len: usize) -> Result<Vec<u8>> {
    fn is_j(b0: u8, b1: u8) -> bool {
        (b1 & 0xFE) == 0xE8 || (b0 == 0x0F && (b1 & 0xF0) == 0x80)
    }
    let bad = || Error::corrupt("BCJ2: inconsistent streams");
    let mut out: Vec<u8> = Vec::with_capacity(out_len);
    let mut probs = [1u16 << 10; 258];
    let (mut inp, mut call_pos, mut jump_pos) = (0usize, 0usize, 0usize);
    let mut rcp = 0usize;
    let mut code = 0u32;
    let mut range = 0xFFFF_FFFFu32;
    for _ in 0..5 {
        code = (code << 8) | u32::from(*rc.get(rcp).ok_or_else(bad)?);
        rcp += 1;
    }
    let mut prev = 0u8;
    while out.len() < out_len {
        // Copy plain bytes up to and including the next branch opcode.
        let mut hit = false;
        while inp < main.len() && out.len() < out_len {
            let b = main[inp];
            out.push(b);
            if is_j(prev, b) {
                hit = true;
                break;
            }
            inp += 1;
            prev = b;
        }
        if !hit || out.len() == out_len {
            break;
        }
        let b = main[inp];
        inp += 1;
        let idx = match b {
            0xE8 => usize::from(prev),
            0xE9 => 256,
            _ => 257,
        };
        let p = u32::from(probs[idx]);
        let bound = (range >> 11) * p;
        let bit = if code < bound {
            range = bound;
            probs[idx] = (p + ((2048 - p) >> 5)) as u16;
            false
        } else {
            range -= bound;
            code -= bound;
            probs[idx] = (p - (p >> 5)) as u16;
            true
        };
        if range < (1 << 24) {
            range <<= 8;
            code = (code << 8) | u32::from(*rc.get(rcp).ok_or_else(bad)?);
            rcp += 1;
        }
        if !bit {
            prev = b;
            continue;
        }
        let src = if b == 0xE8 { (call, &mut call_pos) } else { (jump, &mut jump_pos) };
        let v = src.0.get(*src.1..*src.1 + 4).ok_or_else(bad)?;
        *src.1 += 4;
        let dest = u32::from_be_bytes([v[0], v[1], v[2], v[3]]).wrapping_sub(out.len() as u32 + 4);
        for (k, byte) in dest.to_le_bytes().into_iter().enumerate() {
            if out.len() == out_len {
                break;
            }
            out.push(byte);
            if k == 3 {
                prev = byte;
            }
        }
    }
    if out.len() == out_len { Ok(out) } else { Err(bad()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_roundtrip() {
        let mut x = 7u32;
        let data: Vec<u8> = (0..5000)
            .map(|i| {
                x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                if i % 7 == 0 { 0xE8 } else if i % 11 == 0 { 0xEB } else { (x >> 16) as u8 }
            })
            .collect();
        for arch in [Arch::X86, Arch::PowerPc, Arch::Arm, Arch::ArmThumb, Arch::Sparc, Arch::Arm64] {
            let mut b = data.clone();
            convert(arch, &mut b, true);
            convert(arch, &mut b, false);
            assert_eq!(b, data, "{arch:?}");
        }
        let mut b = data.clone();
        delta(&mut b, 3, true);
        delta(&mut b, 3, false);
        assert_eq!(b, data);
    }

    /// BCJ2 encoder in the shape of the reference one, to exercise the decoder both ways.
    fn bcj2_encode(data: &[u8], convert_all: bool) -> [Vec<u8>; 4] {
        let (mut main, mut call, mut jump, mut rc) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut probs = [1u16 << 10; 258];
        let (mut low, mut range, mut cache, mut cache_size) = (0u64, 0xFFFF_FFFFu32, 0u8, 1u64);
        let mut shift_low = |low: &mut u64, cache: &mut u8, cache_size: &mut u64, rc: &mut Vec<u8>| {
            if (*low as u32) < 0xFF00_0000 || (*low >> 32) != 0 {
                let mut t = *cache;
                loop {
                    rc.push(t.wrapping_add((*low >> 32) as u8));
                    t = 0xFF;
                    *cache_size -= 1;
                    if *cache_size == 0 {
                        break;
                    }
                }
                *cache = (*low >> 24) as u8;
            }
            *cache_size += 1;
            *low = (*low & 0x00FF_FFFF) << 8;
        };
        let mut prev = 0u8;
        let mut i = 0usize;
        let mut x = 99u32;
        while i < data.len() {
            let b = data[i];
            main.push(b);
            i += 1;
            let j = (b & 0xFE) == 0xE8 || (prev == 0x0F && (b & 0xF0) == 0x80);
            if !j {
                prev = b;
                continue;
            }
            if i == data.len() {
                break;
            }
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
            let take = i + 4 <= data.len() && (convert_all || (x >> 16) & 3 != 0);
            let idx = match b { 0xE8 => usize::from(prev), 0xE9 => 256, _ => 257 };
            let p = u32::from(probs[idx]);
            let bound = (range >> 11) * p;
            if take {
                low += u64::from(bound);
                range -= bound;
                probs[idx] = (p - (p >> 5)) as u16;
            } else {
                range = bound;
                probs[idx] = (p + ((2048 - p) >> 5)) as u16;
            }
            while range < (1 << 24) {
                range <<= 8;
                shift_low(&mut low, &mut cache, &mut cache_size, &mut rc);
            }
            if !take {
                prev = b;
                continue;
            }
            let src = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
            let abs = src.wrapping_add(i as u32 + 4);
            if b == 0xE8 { call.extend_from_slice(&abs.to_be_bytes()) } else { jump.extend_from_slice(&abs.to_be_bytes()) }
            prev = data[i + 3];
            i += 4;
        }
        for _ in 0..5 {
            shift_low(&mut low, &mut cache, &mut cache_size, &mut rc);
        }
        [main, call, jump, rc]
    }

    #[test]
    fn bcj2_roundtrips() {
        let mut x = 5u32;
        for len in [0usize, 1, 5, 6, 64, 3000, 40000] {
            for convert_all in [true, false] {
                let data: Vec<u8> = (0..len)
                    .map(|i| {
                        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        match i % 9 { 0 => 0xE8, 3 => 0xE9, 5 => 0x0F, 6 => 0x85, _ => (x >> 16) as u8 }
                    })
                    .collect();
                let [m, c, j, r] = bcj2_encode(&data, convert_all);
                assert_eq!(bcj2_decode(&m, &c, &j, &r, data.len()).unwrap(), data, "len {len}");
            }
        }
        assert!(bcj2_decode(&[0xE8, 1, 2], &[], &[], &[0; 5], 9).is_err());
    }
}
