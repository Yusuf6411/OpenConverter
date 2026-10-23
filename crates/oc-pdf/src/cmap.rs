//! CMaps: code to CID maps for composite fonts and ToUnicode maps.
use crate::object::{Lexer, Obj, Token};
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct CMap {
    /// (byte length, low, high) code space ranges.
    pub codespaces: Vec<(usize, u32, u32)>,
    /// (low, high, first cid) per byte length.
    pub cid_ranges: Vec<(usize, u32, u32, u32)>,
    pub bf_chars: HashMap<u32, String>,
    pub bf_ranges: Vec<(u32, u32, Vec<u16>)>,
    pub bf_array_ranges: Vec<(u32, u32, Vec<String>)>,
    pub vertical: bool,
    pub identity: bool,
}

fn code_of(b: &[u8]) -> u32 {
    b.iter().take(4).fold(0u32, |a, x| (a << 8) | u32::from(*x))
}

fn utf16(b: &[u8]) -> Vec<u16> {
    b.chunks(2)
        .map(|c| {
            if c.len() == 2 {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from(c[0])
            }
        })
        .collect()
}

impl CMap {
    pub fn identity(vertical: bool) -> CMap {
        CMap {
            codespaces: vec![(2, 0, 0xFFFF)],
            identity: true,
            vertical,
            ..CMap::default()
        }
    }

    pub fn parse(data: &[u8]) -> CMap {
        let mut cm = CMap::default();
        let mut lx = Lexer::new(data, 0);
        let mut stack: Vec<Obj> = Vec::new();
        loop {
            match lx.next() {
                Token::Eof => break,
                Token::Obj(o) => {
                    stack.push(o);
                    if stack.len() > 64 {
                        stack.remove(0);
                    }
                }
                Token::ArrayStart => {
                    let mut items = Vec::new();
                    loop {
                        match lx.next() {
                            Token::Obj(o) => items.push(o),
                            Token::ArrayEnd | Token::Eof => break,
                            _ => {}
                        }
                    }
                    stack.push(Obj::Array(items));
                }
                Token::Keyword(k) => {
                    match k.as_slice() {
                        b"begincodespacerange" => loop {
                            let a = lx.next();
                            let Token::Obj(Obj::Str(lo)) = a else { break };
                            let Token::Obj(Obj::Str(hi)) = lx.next() else {
                                break;
                            };
                            cm.codespaces.push((lo.len(), code_of(&lo), code_of(&hi)));
                        },
                        b"begincidrange" | b"beginnotdefrange" => {
                            let is_cid = k.as_slice() == b"begincidrange";
                            loop {
                                let Token::Obj(Obj::Str(lo)) = lx.next() else {
                                    break;
                                };
                                let Token::Obj(Obj::Str(hi)) = lx.next() else {
                                    break;
                                };
                                let Token::Obj(Obj::Int(cid)) = lx.next() else {
                                    break;
                                };
                                if is_cid {
                                    cm.cid_ranges.push((
                                        lo.len(),
                                        code_of(&lo),
                                        code_of(&hi),
                                        cid.max(0) as u32,
                                    ));
                                }
                            }
                        }
                        b"begincidchar" | b"beginnotdefchar" => {
                            let is_cid = k.as_slice() == b"begincidchar";
                            loop {
                                let Token::Obj(Obj::Str(c)) = lx.next() else {
                                    break;
                                };
                                let Token::Obj(Obj::Int(cid)) = lx.next() else {
                                    break;
                                };
                                if is_cid {
                                    let v = code_of(&c);
                                    cm.cid_ranges.push((c.len(), v, v, cid.max(0) as u32));
                                }
                            }
                        }
                        b"beginbfchar" => loop {
                            let Token::Obj(Obj::Str(c)) = lx.next() else {
                                break;
                            };
                            match lx.next() {
                                Token::Obj(Obj::Str(d)) => {
                                    cm.bf_chars
                                        .insert(code_of(&c), String::from_utf16_lossy(&utf16(&d)));
                                }
                                Token::Obj(Obj::Name(_)) => {}
                                _ => break,
                            }
                        },
                        b"beginbfrange" => loop {
                            let Token::Obj(Obj::Str(lo)) = lx.next() else {
                                break;
                            };
                            let Token::Obj(Obj::Str(hi)) = lx.next() else {
                                break;
                            };
                            match lx.next() {
                                Token::Obj(Obj::Str(d)) => {
                                    cm.bf_ranges.push((code_of(&lo), code_of(&hi), utf16(&d)))
                                }
                                Token::ArrayStart => {
                                    let mut v = Vec::new();
                                    loop {
                                        match lx.next() {
                                            Token::Obj(Obj::Str(d)) => {
                                                v.push(String::from_utf16_lossy(&utf16(&d)))
                                            }
                                            Token::ArrayEnd | Token::Eof => break,
                                            _ => {}
                                        }
                                    }
                                    cm.bf_array_ranges.push((code_of(&lo), code_of(&hi), v));
                                }
                                _ => break,
                            }
                        },
                        b"def" => {
                            if stack.len() >= 2 {
                                if let (Some(Obj::Name(n)), Some(Obj::Int(v))) =
                                    (stack.get(stack.len() - 2), stack.last())
                                {
                                    if n == "WMode" {
                                        cm.vertical = *v == 1;
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                    stack.clear();
                }
                _ => {}
            }
        }
        cm
    }

    /// Split `bytes` into character codes: (code, byte length).
    pub fn codes(&self, bytes: &[u8]) -> Vec<(u32, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        let max_len = self
            .codespaces
            .iter()
            .map(|c| c.0)
            .max()
            .unwrap_or(1)
            .clamp(1, 4);
        let min_len = self
            .codespaces
            .iter()
            .map(|c| c.0)
            .min()
            .unwrap_or(1)
            .clamp(1, 4);
        while i < bytes.len() {
            let mut matched = None;
            for len in 1..=max_len.min(bytes.len() - i) {
                let code = code_of(&bytes[i..i + len]);
                if self
                    .codespaces
                    .iter()
                    .any(|(l, lo, hi)| *l == len && code >= *lo && code <= *hi)
                {
                    matched = Some((code, len));
                    break;
                }
            }
            let (code, len) = matched.unwrap_or_else(|| {
                let len = min_len.min(bytes.len() - i).max(1);
                (code_of(&bytes[i..i + len]), len)
            });
            out.push((code, len));
            i += len;
        }
        out
    }

    pub fn cid(&self, code: u32, len: usize) -> u32 {
        if self.identity {
            return code;
        }
        for (l, lo, hi, cid) in &self.cid_ranges {
            if *l == len && code >= *lo && code <= *hi {
                return cid + (code - lo);
            }
        }
        0
    }

    pub fn unicode(&self, code: u32) -> Option<String> {
        if let Some(s) = self.bf_chars.get(&code) {
            return Some(s.clone());
        }
        for (lo, hi, dst) in &self.bf_ranges {
            if code >= *lo && code <= *hi {
                let mut d = dst.clone();
                if let Some(last) = d.last_mut() {
                    *last = last.wrapping_add((code - lo) as u16);
                }
                return Some(String::from_utf16_lossy(&d));
            }
        }
        for (lo, hi, list) in &self.bf_array_ranges {
            if code >= *lo && code <= *hi {
                return list.get((code - lo) as usize).cloned();
            }
        }
        None
    }

    pub fn has_unicode(&self) -> bool {
        !(self.bf_chars.is_empty() && self.bf_ranges.is_empty() && self.bf_array_ranges.is_empty())
    }
}
