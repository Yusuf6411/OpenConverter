//! Huffman tables (ITU-T T.88 Annex B) and the MSB-first bit reader.
use oc_core::{Error, ErrorCode, Result};
use std::collections::HashMap;

pub fn corrupt(msg: &str) -> Error {
    Error::new(ErrorCode::CorruptFile, format!("JBIG2: {msg}"))
}

pub struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(d: &'a [u8]) -> Bits<'a> {
        Bits { d, pos: 0 }
    }

    pub fn bit(&mut self) -> Result<u32> {
        let b = *self
            .d
            .get(self.pos >> 3)
            .ok_or_else(|| Error::new(ErrorCode::TruncatedFile, "JBIG2: data ends early"))?;
        let v = (b >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(u32::from(v))
    }

    pub fn bits(&mut self, n: u32) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Ok(v)
    }

    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    pub fn byte_pos(&self) -> usize {
        self.pos.div_ceil(8)
    }

    pub fn skip_bytes(&mut self, n: usize) {
        self.align();
        self.pos += n * 8;
    }

    pub fn rest(&self) -> &'a [u8] {
        self.d.get(self.byte_pos()..).unwrap_or_default()
    }
}

#[derive(Clone, Copy)]
struct Line {
    pref: u32,
    range: u32,
    low: i32,
}

#[derive(Clone)]
pub struct Table {
    lines: Vec<Line>,
    /// Index of the lower range line, upper range line and out-of-band line, if present.
    lower: Option<usize>,
    upper: Option<usize>,
    oob: Option<usize>,
    codes: HashMap<(u32, u32), usize>,
    max_len: u32,
}

impl Table {
    /// Builds a table from lines in the order of Annex B: normal lines, lower range, upper
    /// range, then the out-of-band line.
    fn build(
        lines: Vec<Line>,
        lower: Option<usize>,
        upper: Option<usize>,
        oob: Option<usize>,
    ) -> Result<Table> {
        let max_len = lines.iter().map(|l| l.pref).max().unwrap_or(0);
        if max_len > 32 {
            return Err(corrupt("prefix code too long"));
        }
        let mut count = vec![0u32; max_len as usize + 2];
        for l in &lines {
            count[l.pref as usize] += 1;
        }
        count[0] = 0;
        let mut codes = HashMap::new();
        let mut first = 0u64;
        for len in 1..=max_len {
            first = (first + u64::from(count[len as usize - 1])) << 1;
            let mut cur = first;
            for (i, l) in lines.iter().enumerate() {
                if l.pref == len {
                    codes.insert((len, cur as u32), i);
                    cur += 1;
                }
            }
        }
        Ok(Table {
            lines,
            lower,
            upper,
            oob,
            codes,
            max_len,
        })
    }

    /// A table from `(prefix length, range length, range low)` triples where the last two
    /// (before an optional OOB line) are the lower and upper range lines.
    fn standard(rows: &[(u32, u32, i32)], has_oob: bool) -> Table {
        let lines: Vec<Line> = rows
            .iter()
            .map(|&(pref, range, low)| Line { pref, range, low })
            .collect();
        let n = lines.len();
        let oob = has_oob.then_some(n - 1);
        let tail = n - usize::from(has_oob);
        match Table::build(lines, Some(tail - 2), Some(tail - 1), oob) {
            Ok(t) => t,
            Err(_) => Table {
                lines: Vec::new(),
                lower: None,
                upper: None,
                oob: None,
                codes: HashMap::new(),
                max_len: 0,
            },
        }
    }

    /// A table whose lines are all plain prefix codes for the values `0..n` (symbol ids).
    pub fn symbols(lens: &[u32]) -> Result<Table> {
        let lines = lens
            .iter()
            .enumerate()
            .map(|(i, &pref)| Line {
                pref,
                range: 0,
                low: i as i32,
            })
            .collect();
        Table::build(lines, None, None, None)
    }

    /// Reads a code and its value; `None` is the out-of-band value.
    pub fn decode(&self, r: &mut Bits) -> Result<Option<i32>> {
        let mut code = 0u32;
        for len in 1..=self.max_len {
            code = (code << 1) | r.bit()?;
            if let Some(&i) = self.codes.get(&(len, code)) {
                let l = self.lines[i];
                if Some(i) == self.oob {
                    return Ok(None);
                }
                if Some(i) == self.lower {
                    let v = i64::from(l.low) - i64::from(r.bits(32)?);
                    return Ok(Some(v.max(i64::from(i32::MIN)) as i32));
                }
                let v = i64::from(l.low) + i64::from(r.bits(l.range)?);
                return Ok(Some(v.min(i64::from(i32::MAX)) as i32));
            }
        }
        Err(corrupt("invalid Huffman code"))
    }

    /// A custom table from a "Tables" segment (B.2).
    pub fn custom(d: &[u8]) -> Result<Table> {
        let flags = *d.first().ok_or_else(|| corrupt("short table segment"))?;
        let oob = flags & 1 != 0;
        let ps = u32::from((flags >> 1) & 7) + 1;
        let rs = u32::from((flags >> 4) & 7) + 1;
        let rd = |o: usize| -> Result<i32> {
            let b = d
                .get(o..o + 4)
                .ok_or_else(|| corrupt("short table segment"))?;
            Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        };
        let (low, high) = (rd(1)?, rd(5)?);
        let mut r = Bits::new(d.get(9..).unwrap_or_default());
        let mut lines = Vec::new();
        let mut cur = i64::from(low);
        while cur < i64::from(high) {
            let pref = r.bits(ps)?;
            let range = r.bits(rs)?;
            if range > 32 || lines.len() > 1 << 16 {
                return Err(corrupt("bad table line"));
            }
            lines.push(Line {
                pref,
                range,
                low: cur as i32,
            });
            cur += 1i64 << range;
        }
        let pref = r.bits(ps)?;
        lines.push(Line {
            pref,
            range: 32,
            low: low.wrapping_sub(1),
        });
        let lower = lines.len() - 1;
        let pref = r.bits(ps)?;
        lines.push(Line {
            pref,
            range: 32,
            low: high,
        });
        let upper = lines.len() - 1;
        let oob_i = if oob {
            let pref = r.bits(ps)?;
            lines.push(Line {
                pref,
                range: 0,
                low: 0,
            });
            Some(lines.len() - 1)
        } else {
            None
        };
        Table::build(lines, Some(lower), Some(upper), oob_i)
    }
}

/// The standard tables B.1 to B.15, numbered from 1.
pub fn standard(n: usize) -> Table {
    match n {
        1 => Table::standard(
            &[
                (1, 4, 0),
                (2, 8, 16),
                (3, 16, 272),
                (0, 32, -1),
                (3, 32, 65808),
            ],
            false,
        ),
        2 => Table::standard(
            &[
                (1, 0, 0),
                (2, 0, 1),
                (3, 0, 2),
                (4, 3, 3),
                (5, 6, 11),
                (0, 32, -1),
                (6, 32, 75),
                (6, 0, 0),
            ],
            true,
        ),
        3 => Table::standard(
            &[
                (8, 8, -256),
                (1, 0, 0),
                (2, 0, 1),
                (3, 0, 2),
                (4, 3, 3),
                (5, 6, 11),
                (8, 32, -257),
                (7, 32, 75),
                (6, 0, 0),
            ],
            true,
        ),
        4 => Table::standard(
            &[
                (1, 0, 1),
                (2, 0, 2),
                (3, 0, 3),
                (4, 3, 4),
                (5, 6, 12),
                (0, 32, -1),
                (5, 32, 76),
            ],
            false,
        ),
        5 => Table::standard(
            &[
                (7, 8, -255),
                (1, 0, 1),
                (2, 0, 2),
                (3, 0, 3),
                (4, 3, 4),
                (5, 6, 12),
                (7, 32, -256),
                (6, 32, 76),
            ],
            false,
        ),
        6 => Table::standard(
            &[
                (5, 10, -2048),
                (4, 9, -1024),
                (4, 8, -512),
                (4, 7, -256),
                (5, 6, -128),
                (5, 5, -64),
                (4, 5, -32),
                (2, 7, 0),
                (3, 7, 128),
                (3, 8, 256),
                (4, 9, 512),
                (4, 10, 1024),
                (6, 32, -2049),
                (6, 32, 2048),
            ],
            false,
        ),
        7 => Table::standard(
            &[
                (4, 9, -1024),
                (3, 8, -512),
                (4, 7, -256),
                (5, 6, -128),
                (5, 5, -64),
                (4, 5, -32),
                (4, 5, 0),
                (5, 5, 32),
                (5, 6, 64),
                (4, 7, 128),
                (3, 8, 256),
                (3, 9, 512),
                (3, 10, 1024),
                (5, 32, -1025),
                (5, 32, 2048),
            ],
            false,
        ),
        8 => Table::standard(
            &[
                (8, 3, -15),
                (9, 1, -7),
                (8, 1, -5),
                (9, 0, -3),
                (7, 0, -2),
                (4, 0, -1),
                (2, 1, 0),
                (5, 0, 2),
                (6, 0, 3),
                (3, 4, 4),
                (6, 1, 20),
                (4, 4, 22),
                (4, 5, 38),
                (5, 6, 70),
                (5, 7, 134),
                (6, 7, 262),
                (7, 8, 390),
                (6, 10, 646),
                (9, 32, -16),
                (9, 32, 1670),
                (2, 0, 0),
            ],
            true,
        ),
        9 => Table::standard(
            &[
                (8, 4, -31),
                (9, 2, -15),
                (8, 2, -11),
                (9, 1, -7),
                (7, 1, -5),
                (4, 1, -3),
                (3, 1, -1),
                (3, 1, 1),
                (5, 1, 3),
                (6, 1, 5),
                (3, 5, 7),
                (6, 2, 39),
                (4, 5, 43),
                (4, 6, 75),
                (5, 7, 139),
                (5, 8, 267),
                (6, 8, 523),
                (7, 9, 779),
                (6, 11, 1291),
                (9, 32, -32),
                (9, 32, 3339),
                (2, 0, 0),
            ],
            true,
        ),
        10 => Table::standard(
            &[
                (7, 4, -21),
                (8, 0, -5),
                (7, 0, -4),
                (5, 0, -3),
                (2, 2, -2),
                (5, 0, 2),
                (6, 0, 3),
                (7, 0, 4),
                (8, 0, 5),
                (2, 6, 6),
                (5, 5, 70),
                (6, 5, 102),
                (6, 6, 134),
                (6, 7, 198),
                (6, 8, 326),
                (6, 9, 582),
                (6, 10, 1094),
                (7, 11, 2118),
                (8, 32, -22),
                (8, 32, 4166),
                (2, 0, 0),
            ],
            true,
        ),
        11 => Table::standard(
            &[
                (1, 0, 1),
                (2, 1, 2),
                (4, 0, 4),
                (4, 1, 5),
                (5, 1, 7),
                (5, 2, 9),
                (6, 2, 13),
                (7, 2, 17),
                (7, 3, 21),
                (7, 4, 29),
                (7, 5, 45),
                (7, 6, 77),
                (0, 32, -1),
                (7, 32, 141),
            ],
            false,
        ),
        12 => Table::standard(
            &[
                (1, 0, 1),
                (2, 0, 2),
                (3, 1, 3),
                (5, 0, 5),
                (5, 1, 6),
                (6, 1, 8),
                (7, 0, 10),
                (7, 1, 11),
                (7, 2, 13),
                (7, 3, 17),
                (7, 4, 25),
                (8, 5, 41),
                (8, 32, 73),
                (0, 32, -1),
                (0, 32, 0),
            ],
            false,
        ),
        13 => Table::standard(
            &[
                (1, 0, 1),
                (3, 0, 2),
                (4, 0, 3),
                (5, 0, 4),
                (4, 1, 5),
                (3, 3, 7),
                (6, 1, 15),
                (6, 2, 17),
                (6, 3, 21),
                (6, 4, 29),
                (6, 5, 45),
                (7, 6, 77),
                (0, 32, -1),
                (7, 32, 141),
            ],
            false,
        ),
        14 => Table::standard(
            &[
                (3, 0, -2),
                (3, 0, -1),
                (1, 0, 0),
                (3, 0, 1),
                (3, 0, 2),
                (0, 32, -1),
                (0, 32, 3),
            ],
            false,
        ),
        _ => Table::standard(
            &[
                (7, 4, -24),
                (6, 2, -8),
                (5, 1, -4),
                (4, 0, -2),
                (3, 0, -1),
                (1, 0, 0),
                (3, 0, 1),
                (4, 0, 2),
                (5, 1, 3),
                (6, 2, 5),
                (7, 4, 9),
                (7, 32, -25),
                (7, 32, 25),
            ],
            false,
        ),
    }
}
