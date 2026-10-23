//! Numbers, lengths, transforms and path data.
use oc_raster::{Path, Pt, Transform};

/// A cursor over an attribute value.
pub struct Scan<'a> {
    pub s: &'a [u8],
    pub i: usize,
}

impl<'a> Scan<'a> {
    pub fn new(s: &'a str) -> Self {
        Self {
            s: s.as_bytes(),
            i: 0,
        }
    }
    pub fn done(&self) -> bool {
        self.i >= self.s.len()
    }
    pub fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    pub fn skip_ws(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }
    pub fn skip_sep(&mut self) {
        self.skip_ws();
        if self.peek() == Some(b',') {
            self.i += 1;
            self.skip_ws();
        }
    }
    pub fn number(&mut self) -> Option<f64> {
        self.skip_ws();
        let st = self.i;
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.i += 1;
        }
        let mut digits = 0;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
            digits += 1;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            self.i = st;
            return None;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            let save = self.i;
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let mut ed = 0;
            while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                self.i += 1;
                ed += 1;
            }
            if ed == 0 {
                self.i = save;
            }
        }
        std::str::from_utf8(self.s.get(st..self.i)?)
            .ok()?
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
    }
    /// A flag of an arc command: a single `0` or `1` that may touch the next number.
    pub fn flag(&mut self) -> Option<bool> {
        self.skip_ws();
        let c = self.peek()?;
        if c == b'0' || c == b'1' {
            self.i += 1;
            Some(c == b'1')
        } else {
            None
        }
    }
}

pub fn numbers(s: &str) -> Vec<f64> {
    let mut sc = Scan::new(s);
    let mut v = Vec::new();
    loop {
        sc.skip_sep();
        match sc.number() {
            Some(n) => v.push(n),
            None => break,
        }
    }
    v
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    None,
    Px,
    Pt,
    Pc,
    Mm,
    Cm,
    In,
    Em,
    Ex,
    Percent,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Length {
    pub v: f64,
    pub unit: Unit,
}

impl Length {
    pub fn px(v: f64) -> Self {
        Self {
            v,
            unit: Unit::None,
        }
    }
    pub fn parse(s: &str) -> Option<Length> {
        let mut sc = Scan::new(s.trim());
        let v = sc.number()?;
        let rest = std::str::from_utf8(sc.s.get(sc.i..)?).ok()?.trim();
        let unit = match rest.to_ascii_lowercase().as_str() {
            "" => Unit::None,
            "px" => Unit::Px,
            "pt" => Unit::Pt,
            "pc" => Unit::Pc,
            "mm" => Unit::Mm,
            "cm" => Unit::Cm,
            "in" => Unit::In,
            "em" => Unit::Em,
            "ex" => Unit::Ex,
            "%" => Unit::Percent,
            _ => return None,
        };
        Some(Length { v, unit })
    }
    /// Resolve to user units. `reference` is the 100% length, `font_size` the em.
    pub fn resolve(&self, reference: f64, font_size: f64) -> f64 {
        match self.unit {
            Unit::None | Unit::Px => self.v,
            Unit::Pt => self.v * 96.0 / 72.0,
            Unit::Pc => self.v * 16.0,
            Unit::Mm => self.v * 96.0 / 25.4,
            Unit::Cm => self.v * 96.0 / 2.54,
            Unit::In => self.v * 96.0,
            Unit::Em => self.v * font_size,
            Unit::Ex => self.v * font_size * 0.5,
            Unit::Percent => self.v / 100.0 * reference,
        }
    }
}

pub fn length_list(s: &str) -> Vec<Length> {
    s.split(|c: char| c == ',' || c.is_whitespace())
        .filter(|p| !p.is_empty())
        .filter_map(Length::parse)
        .collect()
}

/// An SVG transform list.
pub fn transform(s: &str) -> Option<Transform> {
    let mut t = Transform::IDENTITY;
    let mut rest = s.trim();
    while !rest.is_empty() {
        let open = rest.find('(')?;
        let name = rest.get(..open)?.trim().to_ascii_lowercase();
        let close = rest.find(')')?;
        let args = numbers(rest.get(open + 1..close)?);
        rest = rest
            .get(close + 1..)?
            .trim_start_matches([',', ' ', '\t', '\n', '\r']);
        let m = match (name.as_str(), args.as_slice()) {
            ("matrix", [a, b, c, d, e, f]) => Transform::new(*a, *b, *c, *d, *e, *f),
            ("translate", [x]) => Transform::translate(*x, 0.0),
            ("translate", [x, y]) => Transform::translate(*x, *y),
            ("scale", [x]) => Transform::scale(*x, *x),
            ("scale", [x, y]) => Transform::scale(*x, *y),
            ("rotate", [a]) => Transform::rotate(a.to_radians()),
            ("rotate", [a, cx, cy]) => Transform::translate(-cx, -cy)
                .then(&Transform::rotate(a.to_radians()))
                .then(&Transform::translate(*cx, *cy)),
            ("skewx", [a]) => Transform::skew_x(a.to_radians()),
            ("skewy", [a]) => Transform::skew_y(a.to_radians()),
            _ => return None,
        };
        // The list reads left to right as nested transforms: the rightmost applies first.
        t = m.then(&t);
    }
    Some(t)
}

/// Path data to a path. Returns what was parsed before any error, as the SVG rules ask.
pub fn path_data(d: &str) -> Path {
    let mut p = Path::new();
    let mut sc = Scan::new(d);
    let (mut cx, mut cy) = (0.0f64, 0.0f64);
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    let mut last_ctrl: Option<Pt> = None; // for S/T reflection
    let mut last_cmd = b' ';
    let mut cmd = b' ';
    let mut first = true;
    loop {
        sc.skip_ws();
        let Some(c) = sc.peek() else { break };
        if c.is_ascii_alphabetic() {
            cmd = c;
            sc.i += 1;
            if first && cmd != b'M' && cmd != b'm' {
                break;
            }
            if cmd == b'z' || cmd == b'Z' {
                p.close();
                cx = sx;
                cy = sy;
                last_ctrl = None;
                last_cmd = b'z';
                continue;
            }
        } else if cmd == b' ' || cmd == b'z' || cmd == b'Z' {
            break;
        }
        first = false;
        let rel = cmd.is_ascii_lowercase();
        let up = cmd.to_ascii_uppercase();
        macro_rules! num {
            () => {{
                sc.skip_sep();
                match sc.number() {
                    Some(v) => v,
                    None => return p,
                }
            }};
        }
        match up {
            b'M' => {
                let (mut x, mut y) = (num!(), num!());
                if rel {
                    x += cx;
                    y += cy;
                }
                p.move_to(x, y);
                cx = x;
                cy = y;
                sx = x;
                sy = y;
                // following pairs are implicit lineto
                cmd = if rel { b'l' } else { b'L' };
                last_ctrl = None;
            }
            b'L' => {
                let (mut x, mut y) = (num!(), num!());
                if rel {
                    x += cx;
                    y += cy;
                }
                p.line_to(x, y);
                cx = x;
                cy = y;
                last_ctrl = None;
            }
            b'H' => {
                let mut x = num!();
                if rel {
                    x += cx;
                }
                p.line_to(x, cy);
                cx = x;
                last_ctrl = None;
            }
            b'V' => {
                let mut y = num!();
                if rel {
                    y += cy;
                }
                p.line_to(cx, y);
                cy = y;
                last_ctrl = None;
            }
            b'C' => {
                let (mut x1, mut y1, mut x2, mut y2, mut x, mut y) =
                    (num!(), num!(), num!(), num!(), num!(), num!());
                if rel {
                    x1 += cx;
                    y1 += cy;
                    x2 += cx;
                    y2 += cy;
                    x += cx;
                    y += cy;
                }
                p.cubic_to(x1, y1, x2, y2, x, y);
                last_ctrl = Some(Pt::new(x2, y2));
                cx = x;
                cy = y;
            }
            b'S' => {
                let (mut x2, mut y2, mut x, mut y) = (num!(), num!(), num!(), num!());
                if rel {
                    x2 += cx;
                    y2 += cy;
                    x += cx;
                    y += cy;
                }
                let (x1, y1) = match (last_ctrl, last_cmd) {
                    (Some(c), b'C' | b'S') => (2.0 * cx - c.x, 2.0 * cy - c.y),
                    _ => (cx, cy),
                };
                p.cubic_to(x1, y1, x2, y2, x, y);
                last_ctrl = Some(Pt::new(x2, y2));
                cx = x;
                cy = y;
            }
            b'Q' => {
                let (mut x1, mut y1, mut x, mut y) = (num!(), num!(), num!(), num!());
                if rel {
                    x1 += cx;
                    y1 += cy;
                    x += cx;
                    y += cy;
                }
                p.quad_to(x1, y1, x, y);
                last_ctrl = Some(Pt::new(x1, y1));
                cx = x;
                cy = y;
            }
            b'T' => {
                let (mut x, mut y) = (num!(), num!());
                if rel {
                    x += cx;
                    y += cy;
                }
                let (x1, y1) = match (last_ctrl, last_cmd) {
                    (Some(c), b'Q' | b'T') => (2.0 * cx - c.x, 2.0 * cy - c.y),
                    _ => (cx, cy),
                };
                p.quad_to(x1, y1, x, y);
                last_ctrl = Some(Pt::new(x1, y1));
                cx = x;
                cy = y;
            }
            b'A' => {
                let rx = num!();
                let ry = num!();
                let rot = num!();
                sc.skip_sep();
                let Some(large) = sc.flag() else { return p };
                sc.skip_sep();
                let Some(sweep) = sc.flag() else { return p };
                let (mut x, mut y) = (num!(), num!());
                if rel {
                    x += cx;
                    y += cy;
                }
                p.arc_to(Pt::new(cx, cy), rx, ry, rot, large, sweep, Pt::new(x, y));
                cx = x;
                cy = y;
                last_ctrl = None;
            }
            _ => break,
        }
        last_cmd = up;
        sc.skip_sep();
    }
    p
}

pub fn points(s: &str) -> Vec<Pt> {
    numbers(s)
        .chunks_exact(2)
        .map(|c| Pt::new(c[0], c[1]))
        .collect()
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let h = |c: u8| (c as char).to_digit(16);
            if let (Some(a), Some(c)) = (h(b[i + 1]), h(b[i + 2])) {
                out.push((a * 16 + c) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}
