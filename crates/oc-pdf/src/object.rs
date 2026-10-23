//! PDF objects and the tokenizer/parser that reads them from bytes.
use std::collections::BTreeMap;
use std::sync::Arc;

pub type Dict = BTreeMap<String, Obj>;

#[derive(Debug, Clone, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(String),
    Array(Vec<Obj>),
    Dict(Dict),
    Stream(Arc<Stream>),
    Ref(u32, u16),
}

/// A stream: its dictionary and where the raw (still encoded, possibly encrypted) bytes are.
#[derive(Debug, Clone, PartialEq)]
pub struct Stream {
    pub dict: Dict,
    /// Raw bytes (copied out of the file so streams from object streams and rebuilt files work alike).
    pub raw: Vec<u8>,
    /// Object number and generation, used to derive the decryption key.
    pub id: (u32, u16),
}

impl Obj {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Obj::Int(i) => Some(*i),
            Obj::Real(r) if r.is_finite() => Some(*r as i64),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Obj::Int(i) => Some(*i as f64),
            Obj::Real(r) => Some(*r),
            _ => None,
        }
    }
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            Obj::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Obj]> {
        match self {
            Obj::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) => Some(d),
            Obj::Stream(s) => Some(&s.dict),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Obj::Bool(b) => Some(*b),
            _ => None,
        }
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Obj::Null)
    }
}

pub fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

pub fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// A token or keyword that is not an object (`obj`, `endobj`, `R`, content operators ...).
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Obj(Obj),
    Keyword(Vec<u8>),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
    Eof,
}

pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn read_name(&mut self) -> String {
        let mut out = Vec::new();
        while let Some(b) = self.peek() {
            if is_ws(b) || is_delim(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let h = self.data.get(self.pos).copied().and_then(hex_val);
                let l = self.data.get(self.pos + 1).copied().and_then(hex_val);
                if let (Some(h), Some(l)) = (h, l) {
                    out.push(h * 16 + l);
                    self.pos += 2;
                    continue;
                }
            }
            out.push(b);
        }
        String::from_utf8(out.clone()).unwrap_or_else(|_| out.iter().map(|b| char::from(*b)).collect())
    }

    fn read_string(&mut self) -> Vec<u8> {
        // after '('
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(c) = self.peek() else { break };
                    self.pos += 1;
                    match c {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(c - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                b'\r' => {
                    // a bare CR or CRLF in a string is a single LF
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    out.push(b'\n');
                }
                _ => out.push(b),
            }
        }
        out
    }

    fn read_hex_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(v) = hex_val(b) {
                match hi.take() {
                    Some(h) => out.push(h * 16 + v),
                    None => hi = Some(v),
                }
            }
        }
        if let Some(h) = hi {
            out.push(h * 16);
        }
        out
    }

    fn number(&mut self) -> Option<Obj> {
        let start = self.pos;
        let mut p = start;
        let mut seen_dot = false;
        let mut digits = 0;
        if matches!(self.data.get(p), Some(b'+' | b'-')) {
            p += 1;
            // some writers emit "--5"
            while matches!(self.data.get(p), Some(b'-')) {
                p += 1;
            }
        }
        while let Some(&b) = self.data.get(p) {
            match b {
                b'0'..=b'9' => digits += 1,
                b'.' if !seen_dot => seen_dot = true,
                _ => break,
            }
            p += 1;
        }
        if digits == 0 {
            return None;
        }
        if let Some(&b) = self.data.get(p) {
            if !(is_ws(b) || is_delim(b)) {
                return None;
            }
        }
        let text = std::str::from_utf8(self.data.get(start..p)?).ok()?;
        let neg = text.starts_with('-');
        let cleaned: String = text.trim_start_matches(['+', '-']).to_string();
        self.pos = p;
        if seen_dot {
            let v: f64 = cleaned.parse().unwrap_or(0.0);
            Some(Obj::Real(if neg { -v } else { v }))
        } else {
            match cleaned.parse::<i64>() {
                Ok(v) => Some(Obj::Int(if neg { -v } else { v })),
                Err(_) => Some(Obj::Real(cleaned.parse::<f64>().map_or(0.0, |v| if neg { -v } else { v }))),
            }
        }
    }

    pub fn next(&mut self) -> Token {
        self.skip_ws();
        let Some(b) = self.peek() else { return Token::Eof };
        match b {
            b'/' => {
                self.pos += 1;
                Token::Obj(Obj::Name(self.read_name()))
            }
            b'(' => {
                self.pos += 1;
                Token::Obj(Obj::Str(self.read_string()))
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Token::DictStart
                } else {
                    self.pos += 1;
                    Token::Obj(Obj::Str(self.read_hex_string()))
                }
            }
            b'>' => {
                if self.data.get(self.pos + 1) == Some(&b'>') {
                    self.pos += 2;
                    Token::DictEnd
                } else {
                    self.pos += 1;
                    self.next()
                }
            }
            b'[' => {
                self.pos += 1;
                Token::ArrayStart
            }
            b']' => {
                self.pos += 1;
                Token::ArrayEnd
            }
            b'{' | b'}' | b')' => {
                self.pos += 1;
                Token::Keyword(vec![b])
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                if let Some(n) = self.number() {
                    return Token::Obj(n);
                }
                self.keyword()
            }
            _ => self.keyword(),
        }
    }

    fn keyword(&mut self) -> Token {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if is_ws(b) || is_delim(b) {
                break;
            }
            self.pos += 1;
        }
        if self.pos == start {
            self.pos += 1;
        }
        let kw = self.data.get(start..self.pos).unwrap_or_default().to_vec();
        match kw.as_slice() {
            b"true" => Token::Obj(Obj::Bool(true)),
            b"false" => Token::Obj(Obj::Bool(false)),
            b"null" => Token::Obj(Obj::Null),
            _ => Token::Keyword(kw),
        }
    }

    /// Parse one object (resolving `n g R`), given its first token.
    pub fn object_from(&mut self, tok: Token, depth: u32) -> Option<Obj> {
        if depth > 64 {
            return None;
        }
        match tok {
            Token::Obj(Obj::Int(n)) if n >= 0 => {
                // look ahead for "g R"
                let save = self.pos;
                if let Token::Obj(Obj::Int(g)) = self.next() {
                    if g >= 0 {
                        if let Token::Keyword(k) = self.next() {
                            if k == b"R" {
                                return Some(Obj::Ref(n as u32, g.min(65535) as u16));
                            }
                        }
                    }
                }
                self.pos = save;
                Some(Obj::Int(n))
            }
            Token::Obj(o) => Some(o),
            Token::ArrayStart => {
                let mut v = Vec::new();
                loop {
                    let t = self.next();
                    match t {
                        Token::ArrayEnd | Token::Eof => break,
                        Token::DictEnd => continue,
                        Token::Keyword(ref k) if k == b"endobj" || k == b"endstream" => break,
                        t => {
                            if let Some(o) = self.object_from(t, depth + 1) {
                                v.push(o);
                            }
                        }
                    }
                    if v.len() > 4_000_000 {
                        break;
                    }
                }
                Some(Obj::Array(v))
            }
            Token::DictStart => {
                let mut d = Dict::new();
                loop {
                    let t = self.next();
                    match t {
                        Token::DictEnd | Token::Eof => break,
                        Token::Keyword(ref k) if k == b"endobj" || k == b"stream" => {
                            self.pos -= k.len();
                            break;
                        }
                        Token::Obj(Obj::Name(key)) => {
                            let vt = self.next();
                            if matches!(vt, Token::DictEnd) {
                                break;
                            }
                            if let Some(v) = self.object_from(vt, depth + 1) {
                                d.insert(key, v);
                            }
                        }
                        _ => {}
                    }
                }
                Some(Obj::Dict(d))
            }
            _ => None,
        }
    }

    pub fn object(&mut self) -> Option<Obj> {
        let t = self.next();
        self.object_from(t, 0)
    }
}
