//! Compound File Binary (MS-CFB): the container of legacy Office files (DOC, XLS, PPT).
//! Read only: the directory tree, FAT and mini FAT chains, and whole streams by name.
#![allow(clippy::indexing_slicing, clippy::chunks_exact_to_as_chunks)]

use oc_core::{Error, ErrorCode, Limits, Result};

pub const MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];

pub struct Cfb<'a> {
    d: &'a [u8],
    ssz: usize,
    mini_ssz: usize,
    cutoff: usize,
    fat: Vec<u32>,
    mini_fat: Vec<u32>,
    mini_stream: Vec<u8>,
    pub entries: Vec<Entry>,
    max: u64,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub kind: u8,
    pub start: u32,
    pub size: u64,
}

fn corrupt(m: &str) -> Error {
    Error::new(ErrorCode::CorruptFile, m.to_string())
}

fn u16le(d: &[u8], p: usize) -> usize {
    d.get(p..p + 2)
        .map_or(0, |b| usize::from(u16::from_le_bytes([b[0], b[1]])))
}

fn u32le(d: &[u8], p: usize) -> u32 {
    d.get(p..p + 4)
        .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

const END: u32 = 0xFFFF_FFFE;
const FREE: u32 = 0xFFFF_FFFF;

impl<'a> Cfb<'a> {
    pub fn open(d: &'a [u8], limits: &Limits) -> Result<Cfb<'a>> {
        if d.len() < 512 || d[..8] != MAGIC {
            return Err(corrupt("this is not a compound file"));
        }
        let shift = u16le(d, 0x1E);
        let mini_shift = u16le(d, 0x20);
        if !(7..=16).contains(&shift) || mini_shift > shift || mini_shift < 2 {
            return Err(corrupt("the compound file has an invalid sector size"));
        }
        let ssz = 1usize << shift;
        let mut c = Cfb {
            d,
            ssz,
            mini_ssz: 1 << mini_shift,
            cutoff: u32le(d, 0x38) as usize,
            fat: Vec::new(),
            mini_fat: Vec::new(),
            mini_stream: Vec::new(),
            entries: Vec::new(),
            max: limits.max_alloc.min(1 << 31),
        };
        let nsect = d.len().saturating_sub(512).div_ceil(ssz);
        // DIFAT: 109 entries in the header, then chained sectors
        let mut fat_sectors: Vec<u32> = (0..109)
            .map(|i| u32le(d, 0x4C + 4 * i))
            .take_while(|s| *s != FREE)
            .collect();
        let mut dif = u32le(d, 0x44);
        let mut guard = 0;
        while dif != END && dif != FREE && guard < nsect + 1 {
            guard += 1;
            let Some(s) = c.sector(dif) else { break };
            let per = ssz / 4 - 1;
            for i in 0..per {
                let v = u32le(s, i * 4);
                if v != FREE && v != END {
                    fat_sectors.push(v);
                }
            }
            dif = u32le(s, per * 4);
        }
        for s in fat_sectors {
            if let Some(b) = c.sector(s) {
                for i in 0..ssz / 4 {
                    c.fat.push(u32le(b, i * 4));
                }
            }
        }
        // directory
        let dir_start = u32le(d, 0x30);
        let dir = c.chain(dir_start, None)?;
        for e in dir.chunks_exact(128) {
            let nlen = u16le(e, 64).min(64);
            let units: Vec<u16> = e[..nlen.saturating_sub(2)]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            let kind = e[66];
            let size = if ssz >= 4096 {
                u64::from(u32le(e, 120)) | (u64::from(u32le(e, 124)) << 32)
            } else {
                u64::from(u32le(e, 120))
            };
            c.entries.push(Entry {
                name: String::from_utf16_lossy(&units),
                kind,
                start: u32le(e, 116),
                size,
            });
        }
        let root = c.entries.iter().find(|e| e.kind == 5).cloned();
        if let Some(r) = root {
            c.mini_stream = c.chain(r.start, Some(r.size))?;
            let mf = u32le(d, 0x3C);
            let raw = c.chain(mf, None)?;
            c.mini_fat = raw
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect();
        }
        Ok(c)
    }

    fn sector(&self, n: u32) -> Option<&'a [u8]> {
        let off = (n as usize).checked_add(1)?.checked_mul(self.ssz)?;
        self.d.get(off..off + self.ssz).or_else(|| {
            // a short last sector
            let s = self.d.get(off..)?;
            (!s.is_empty()).then_some(s)
        })
    }

    fn chain(&self, start: u32, size: Option<u64>) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut n = start;
        let mut hops = 0usize;
        while n != END && n != FREE && (n as usize) < self.fat.len() {
            hops += 1;
            if hops > self.fat.len() + 1 {
                return Err(corrupt("a stream of the compound file loops"));
            }
            let Some(s) = self.sector(n) else { break };
            out.extend_from_slice(s);
            if out.len() as u64 > self.max {
                return Err(Error::limit(
                    "max_alloc",
                    "a stream of the compound file is too large",
                ));
            }
            if let Some(sz) = size {
                if out.len() as u64 >= sz {
                    break;
                }
            }
            n = self.fat[n as usize];
        }
        if let Some(sz) = size {
            out.truncate(usize::try_from(sz).unwrap_or(usize::MAX));
        }
        Ok(out)
    }

    fn mini_chain(&self, start: u32, size: u64) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut n = start;
        let mut hops = 0usize;
        while n != END && n != FREE && (n as usize) < self.mini_fat.len() {
            hops += 1;
            if hops > self.mini_fat.len() + 1 {
                return Err(corrupt("a small stream of the compound file loops"));
            }
            let off = n as usize * self.mini_ssz;
            let Some(s) = self.mini_stream.get(off..off + self.mini_ssz) else {
                break;
            };
            out.extend_from_slice(s);
            if out.len() as u64 >= size {
                break;
            }
            n = self.mini_fat[n as usize];
        }
        out.truncate(usize::try_from(size).unwrap_or(usize::MAX));
        Ok(out)
    }

    /// The bytes of the first stream with this name (any storage, case-insensitive).
    pub fn stream(&self, name: &str) -> Option<Vec<u8>> {
        let e = self
            .entries
            .iter()
            .find(|e| e.kind == 2 && e.name.eq_ignore_ascii_case(name))?;
        if e.size > self.max {
            return None;
        }
        if (e.size as usize) < self.cutoff {
            self.mini_chain(e.start, e.size).ok()
        } else {
            self.chain(e.start, Some(e.size)).ok()
        }
    }

    pub fn has_stream(&self, name: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.kind == 2 && e.name.eq_ignore_ascii_case(name))
    }
}
