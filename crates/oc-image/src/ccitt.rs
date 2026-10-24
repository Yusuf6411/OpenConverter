//! CCITT fax decoding for TIFF: Modified Huffman (compression 2), T.4 / Group 3 in one or
//! two dimensions (3) and T.6 / Group 4 (4). Output is one bit per pixel, black = 1, rows
//! padded to whole bytes.
use crate::ccitt_tables::{BLACK, WHITE};
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// Compression 2: every row is a byte-aligned 1D row without EOL.
    ModifiedHuffman,
    /// Compression 3; `two_d` when T4Options bit 0 is set.
    Group3 { two_d: bool },
    /// Compression 4.
    Group4,
}

const MAX_CODE: usize = 13;
const NONE: u16 = u16::MAX;

/// `[len << 13 | code]` to run length, for white and black.
fn tables() -> &'static (Vec<u16>, Vec<u16>) {
    static T: OnceLock<(Vec<u16>, Vec<u16>)> = OnceLock::new();
    T.get_or_init(|| {
        let build = |src: &[(u16, u8, u16)]| {
            let mut t = vec![NONE; (MAX_CODE + 1) << 13];
            for &(code, len, run) in src {
                if let Some(slot) = t.get_mut(usize::from(len) << 13 | usize::from(code)) {
                    *slot = run;
                }
            }
            t
        };
        (build(&WHITE), build(&BLACK))
    })
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn left(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }
    /// The next `n` (<= 16) bits as a number, zeros beyond the end.
    fn peek(&self, n: usize) -> u32 {
        let mut v = 0u32;
        for k in 0..n {
            let p = self.pos + k;
            let bit = self
                .data
                .get(p / 8)
                .map_or(0, |&b| u32::from(b >> (7 - p % 8)) & 1);
            v = v << 1 | bit;
        }
        v
    }
    fn skip(&mut self, n: usize) {
        self.pos += n;
    }
    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
    /// Consumes zero fill bits and an EOL (at least eleven zeros and a one), if there is one.
    fn eol(&mut self) -> bool {
        let start = self.pos;
        while self.left() > 0 && self.peek(1) == 0 {
            self.pos += 1;
        }
        if self.pos - start >= 11 && self.left() > 0 {
            self.pos += 1;
            true
        } else {
            self.pos = start;
            false
        }
    }
}

/// One run of the given colour: any make-up codes and the terminating code.
fn run(r: &mut Bits, black: bool) -> Option<usize> {
    let (white_t, black_t) = tables();
    let t = if black { black_t } else { white_t };
    let mut total = 0usize;
    loop {
        let window = r.peek(MAX_CODE);
        let mut found = None;
        for len in 2..=MAX_CODE {
            let code = (window >> (MAX_CODE - len)) as usize;
            let v = *t.get(len << 13 | code)?;
            if v != NONE {
                found = Some((len, usize::from(v)));
                break;
            }
        }
        let (len, v) = found?;
        if r.left() < len {
            return None;
        }
        r.skip(len);
        total += v;
        if v < 64 {
            return Some(total);
        }
        if total > 1 << 20 {
            return None;
        }
    }
}

/// Changing elements of a 1D row; `None` on a bad code or a row that overruns.
fn row_1d(r: &mut Bits, width: usize) -> Option<Vec<usize>> {
    let mut x = 0;
    let mut black = false;
    let mut changes = Vec::new();
    while x < width {
        x += run(r, black)?;
        if x > width {
            return None;
        }
        changes.push(x);
        black = !black;
    }
    Some(changes)
}

#[derive(Debug)]
enum Code {
    Pass,
    Horizontal,
    Vertical(i64),
    Eol,
    Other,
}

fn mode_code(r: &mut Bits) -> Code {
    let w = r.peek(7);
    let (code, len) = if w >> 6 == 1 {
        (Code::Vertical(0), 1)
    } else if w >> 4 == 0b011 {
        (Code::Vertical(1), 3)
    } else if w >> 4 == 0b010 {
        (Code::Vertical(-1), 3)
    } else if w >> 4 == 0b001 {
        (Code::Horizontal, 3)
    } else if w >> 3 == 0b0001 {
        (Code::Pass, 4)
    } else if w >> 1 == 0b000011 {
        (Code::Vertical(2), 6)
    } else if w >> 1 == 0b000010 {
        (Code::Vertical(-2), 6)
    } else if w == 0b0000011 {
        (Code::Vertical(3), 7)
    } else if w == 0b0000010 {
        (Code::Vertical(-3), 7)
    } else if w == 0 {
        (Code::Eol, 0)
    } else {
        (Code::Other, 0)
    };
    r.skip(len);
    code
}

/// Changing elements of a 2D row against the changes of the line above.
fn row_2d(r: &mut Bits, refs: &[usize], width: usize) -> Option<Vec<usize>> {
    let mut a0: i64 = -1;
    let mut black = false;
    let mut changes: Vec<usize> = Vec::new();
    let mut bi = 0usize;
    let w = width as i64;
    while a0 < w {
        while bi > 0 && refs.get(bi - 1).is_some_and(|&p| p as i64 > a0) {
            bi -= 1;
        }
        while refs.get(bi).is_some_and(|&p| p as i64 <= a0) {
            bi += 1;
        }
        // b1 changes to the colour opposite to the current one: even indices go white to black.
        if (bi % 2 == 1) != black {
            bi += 1;
        }
        let b1 = refs.get(bi).map_or(w, |&p| p as i64);
        let b2 = refs.get(bi + 1).map_or(w, |&p| p as i64);
        match mode_code(r) {
            Code::Pass => a0 = b2,
            Code::Horizontal => {
                let first = run(r, black)? as i64;
                let second = run(r, !black)? as i64;
                let start = a0.max(0);
                let a1 = start + first;
                let a2 = a1 + second;
                if a2 > w {
                    return None;
                }
                changes.push(a1 as usize);
                changes.push(a2 as usize);
                a0 = a2;
            }
            Code::Vertical(d) => {
                let a1 = b1 + d;
                if a1 < a0.max(0) || a1 > w {
                    return None;
                }
                changes.push(a1 as usize);
                black = !black;
                a0 = a1;
            }
            Code::Eol | Code::Other => return None,
        }
        // A pass or a horizontal run that ends exactly at the edge finishes the row.
        if a0 >= w {
            break;
        }
    }
    Some(changes)
}

fn paint(out: &mut [u8], changes: &[usize], width: usize) {
    let mut x = 0;
    let mut black = false;
    let mut fill = |from: usize, to: usize| {
        for p in from..to.min(width) {
            if let Some(b) = out.get_mut(p / 8) {
                *b |= 0x80 >> (p % 8);
            }
        }
    };
    for &c in changes {
        let c = c.min(width);
        if black {
            fill(x, c);
        }
        x = x.max(c);
        black = !black;
    }
    if black {
        fill(x, width);
    }
}

/// Decodes up to `rows` rows of `width` pixels. Returns the packed rows and how many were
/// decoded in full; the rest are white.
pub fn decode(data: &[u8], width: usize, rows: usize, mode: Mode) -> (Vec<u8>, usize) {
    let (out, done, _) = decode_inner(data, width, rows, mode);
    (out, done)
}

/// Group 4 decoding for JBIG2 MMR data: returns the packed rows (1 = black) and the number of
/// bytes used, including an end-of-block marker when one follows the rows.
pub fn decode_mmr(data: &[u8], width: usize, rows: usize) -> (Vec<u8>, usize) {
    let (out, _, bits) = decode_inner(data, width, rows, Mode::Group4);
    let mut r = Bits { data, pos: bits };
    if r.peek(12) == 1 {
        r.skip(12);
        if r.peek(12) == 1 {
            r.skip(12);
        }
    }
    (out, r.pos.div_ceil(8).min(data.len()))
}

fn decode_inner(data: &[u8], width: usize, rows: usize, mode: Mode) -> (Vec<u8>, usize, usize) {
    let row_bytes = width.div_ceil(8);
    let mut out = vec![0u8; row_bytes * rows];
    let mut r = Bits { data, pos: 0 };
    // Line above the first row: all white.
    let mut refs: Vec<usize> = Vec::new();
    let mut done = 0;
    for y in 0..rows {
        if r.left() == 0 {
            break;
        }
        let one_d = match mode {
            Mode::ModifiedHuffman => true,
            Mode::Group3 { two_d } => {
                let mut tag_1d = true;
                if r.eol() {
                    if two_d {
                        // The tag bit after the EOL says whether this row is 1D.
                        tag_1d = r.peek(1) == 1;
                        r.skip(1);
                    }
                    if r.peek(11) == 0 {
                        break; // another EOL right away: return to control
                    }
                } else if two_d {
                    break;
                }
                tag_1d
            }
            Mode::Group4 => {
                if r.peek(12) == 1 {
                    break; // EOFB
                }
                false
            }
        };
        let changes = if one_d {
            row_1d(&mut r, width)
        } else {
            row_2d(&mut r, &refs, width)
        };
        let Some(changes) = changes else {
            break;
        };
        if let Some(row) = out.get_mut(y * row_bytes..(y + 1) * row_bytes) {
            paint(row, &changes, width);
        }
        refs = changes;
        done += 1;
        if mode == Mode::ModifiedHuffman {
            r.align();
        }
    }
    (out, done, r.pos)
}
