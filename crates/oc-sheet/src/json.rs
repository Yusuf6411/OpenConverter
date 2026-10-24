//! JSON tables: an array of flat objects (the header is the keys in first-seen order) or an
//! array of arrays (no header). Nested values are rejected.
//!
//! The syntax reader is strict RFC 8259: no comments, trailing commas, NaN or Infinity.
use crate::{Table, TableLimits};
use oc_core::{Error, ErrorCode, Note, Notes, Result, Severity};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    /// The number exactly as written.
    Num(String),
    Str(String),
    Arr(Vec<Value>),
    /// Members in first-seen order; a repeated key keeps its first position and last value.
    Obj(Vec<(String, Value)>),
}

struct Parser<'a> {
    text: &'a str,
    b: &'a [u8],
    at: usize,
    max_depth: u32,
}

impl<'a> Parser<'a> {
    fn err(&self, msg: &str) -> Error {
        let before = self.b.get(..self.at.min(self.b.len())).unwrap_or_default();
        let line = before.iter().filter(|&&c| c == b'\n').count() + 1;
        let col = before.iter().rev().take_while(|&&c| c != b'\n').count() + 1;
        Error::new(
            ErrorCode::CorruptFile,
            format!("invalid JSON: {msg} (line {line}, column {col})"),
        )
        .at(self.at as u64)
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.at).copied()
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.at += 1;
        }
    }

    fn eat(&mut self, lit: &str) -> bool {
        if self
            .b
            .get(self.at..)
            .is_some_and(|s| s.starts_with(lit.as_bytes()))
        {
            self.at += lit.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: u32) -> Result<Value> {
        if depth > self.max_depth {
            return Err(Error::limit("depth", "JSON is nested too deeply"));
        }
        self.ws();
        match self.peek() {
            None => Err(self.err("unexpected end of input")),
            Some(b'{') => {
                self.at += 1;
                let mut members: Vec<(String, Value)> = Vec::new();
                let mut index: HashMap<String, usize> = HashMap::new();
                self.ws();
                if self.peek() == Some(b'}') {
                    self.at += 1;
                    return Ok(Value::Obj(members));
                }
                loop {
                    self.ws();
                    if self.peek() != Some(b'"') {
                        return Err(self.err("expected a string key"));
                    }
                    let key = self.string()?;
                    self.ws();
                    if self.peek() != Some(b':') {
                        return Err(self.err("expected ':'"));
                    }
                    self.at += 1;
                    let v = self.value(depth + 1)?;
                    match index.get(&key) {
                        Some(&k) => {
                            if let Some(m) = members.get_mut(k) {
                                m.1 = v;
                            }
                        }
                        None => {
                            index.insert(key.clone(), members.len());
                            members.push((key, v));
                        }
                    }
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b'}') => {
                            self.at += 1;
                            return Ok(Value::Obj(members));
                        }
                        _ => return Err(self.err("expected ',' or '}'")),
                    }
                }
            }
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                self.ws();
                if self.peek() == Some(b']') {
                    self.at += 1;
                    return Ok(Value::Arr(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    self.ws();
                    match self.peek() {
                        Some(b',') => self.at += 1,
                        Some(b']') => {
                            self.at += 1;
                            return Ok(Value::Arr(items));
                        }
                        _ => return Err(self.err("expected ',' or ']'")),
                    }
                }
            }
            Some(b'"') => Ok(Value::Str(self.string()?)),
            Some(b't') if self.eat("true") => Ok(Value::Bool(true)),
            Some(b'f') if self.eat("false") => Ok(Value::Bool(false)),
            Some(b'n') if self.eat("null") => Ok(Value::Null),
            Some(b'-' | b'0'..=b'9') => self.number(),
            Some(_) => Err(self.err("unexpected character")),
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.at;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.at += 1;
        }
        self.at - start
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.at;
        if self.peek() == Some(b'-') {
            self.at += 1;
        }
        match self.peek() {
            Some(b'0') => self.at += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return Err(self.err("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.at += 1;
            if self.digits() == 0 {
                return Err(self.err("invalid number"));
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.at += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.at += 1;
            }
            if self.digits() == 0 {
                return Err(self.err("invalid number"));
            }
        }
        Ok(Value::Num(
            self.text
                .get(start..self.at)
                .unwrap_or_default()
                .to_string(),
        ))
    }

    fn hex4(&mut self) -> Result<u32> {
        let h = self
            .text
            .get(self.at..self.at + 4)
            .filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit()))
            .and_then(|h| u32::from_str_radix(h, 16).ok())
            .ok_or_else(|| self.err("invalid \\u escape"))?;
        self.at += 4;
        Ok(h)
    }

    fn string(&mut self) -> Result<String> {
        self.at += 1; // opening quote
        let mut out = String::new();
        loop {
            let run = self.at;
            while self
                .peek()
                .is_some_and(|c| c != b'"' && c != b'\\' && c >= 0x20)
            {
                self.at += 1;
            }
            out.push_str(self.text.get(run..self.at).unwrap_or_default());
            match self.peek() {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let Some(e) = self.peek() else {
                        return Err(self.err("unterminated string"));
                    };
                    self.at += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.hex4()?;
                            let cp = if (0xD800..0xDC00).contains(&hi) {
                                if !self.eat("\\u") {
                                    return Err(self.err("unpaired surrogate"));
                                }
                                let lo = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&lo) {
                                    return Err(self.err("unpaired surrogate"));
                                }
                                0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                            } else if (0xDC00..0xE000).contains(&hi) {
                                return Err(self.err("unpaired surrogate"));
                            } else {
                                hi
                            };
                            out.push(char::from_u32(cp).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err(self.err("invalid escape")),
                    }
                }
                Some(_) => return Err(self.err("control character in string")),
                None => return Err(self.err("unterminated string")),
            }
        }
    }
}

/// Parses one JSON document.
pub fn parse(text: &str, limits: &TableLimits) -> Result<Value> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut p = Parser {
        text,
        b: text.as_bytes(),
        at: 0,
        max_depth: limits.max_depth,
    };
    let v = p.value(0)?;
    p.ws();
    if p.at < p.b.len() {
        return Err(p.err("extra data after the document"));
    }
    Ok(v)
}

fn nested(v: &Value, where_: &str) -> Error {
    let kind = if matches!(v, Value::Obj(_)) {
        "object"
    } else {
        "array"
    };
    Error::new(
        ErrorCode::UnsupportedVariant,
        format!(
            "the JSON value at {where_} is a nested {kind}; only flat tables (arrays of flat objects, or arrays of arrays) can be converted"
        ),
    )
}

/// Returns the cell text for a scalar, and whether it was not a string.
fn scalar(v: &Value, where_: &dyn Fn() -> String) -> Result<(String, bool)> {
    Ok(match v {
        Value::Str(s) => (s.clone(), false),
        Value::Null => (String::new(), true),
        Value::Bool(b) => (b.to_string(), true),
        Value::Num(n) => (n.clone(), true),
        Value::Arr(_) | Value::Obj(_) => return Err(nested(v, &where_())),
    })
}

fn too_many() -> Error {
    Error::limit("table", "the table has more cells than the allowed maximum")
}

/// Maps a parsed document to a table.
pub fn to_table(doc: &Value, limits: &TableLimits) -> Result<Table> {
    let Value::Arr(items) = doc else {
        return Err(Error::new(
            ErrorCode::UnsupportedVariant,
            "JSON must be an array of objects or an array of arrays to become a table",
        ));
    };
    if items.is_empty() {
        return Ok(Table::default());
    }
    let mut coerced = false;
    if items.iter().all(|i| matches!(i, Value::Obj(_))) {
        let mut header: Vec<&str> = Vec::new();
        let mut col: HashMap<&str, usize> = HashMap::new();
        for item in items {
            if let Value::Obj(m) = item {
                for (k, _) in m {
                    if !col.contains_key(k.as_str()) {
                        col.insert(k, header.len());
                        header.push(k);
                    }
                }
            }
        }
        if (header.len() as u64).saturating_mul(items.len() as u64) > limits.max_cells {
            return Err(too_many());
        }
        let mut rows = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let mut row = vec![String::new(); header.len()];
            if let Value::Obj(m) = item {
                for (k, v) in m {
                    let (text, c) = scalar(v, &|| format!("row {}, key '{k}'", i + 1))?;
                    coerced |= c;
                    if let Some(cell) = col.get(k.as_str()).and_then(|&c| row.get_mut(c)) {
                        *cell = text;
                    }
                }
            }
            rows.push(row);
        }
        let mut t = Table::new(Some(header.into_iter().map(str::to_string).collect()), rows);
        t.coerced_scalars = coerced;
        return Ok(t);
    }
    if items.iter().all(|i| matches!(i, Value::Arr(_))) {
        let mut cells = 0u64;
        let mut rows = Vec::with_capacity(items.len());
        for (i, item) in items.iter().enumerate() {
            let Value::Arr(vals) = item else { continue };
            cells += vals.len() as u64;
            if cells > limits.max_cells {
                return Err(too_many());
            }
            let mut row = Vec::with_capacity(vals.len());
            for (j, v) in vals.iter().enumerate() {
                let (text, c) = scalar(v, &|| format!("row {}, column {}", i + 1, j + 1))?;
                coerced |= c;
                row.push(text);
            }
            rows.push(row);
        }
        let mut t = Table::new(None, rows);
        t.coerced_scalars = coerced;
        return Ok(t);
    }
    Err(Error::new(
        ErrorCode::UnsupportedVariant,
        "the JSON array mixes objects with other values; it cannot become a table",
    ))
}

/// Reads JSON text as a table.
pub fn read(text: &str, limits: &TableLimits) -> Result<Table> {
    to_table(&parse(text, limits)?, limits)
}

/// Column names made unique: empty names become `column_N` and repeats `name_2`, `name_3`.
pub fn unique_header(header: &[String], width: usize, notes: &mut Notes) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(width);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut changed = false;
    for i in 0..width {
        let name = header.get(i).map_or("", String::as_str);
        let base = if name.is_empty() {
            format!("column_{}", i + 1)
        } else {
            name.to_string()
        };
        let mut candidate = base.clone();
        let mut k = 2;
        while seen.contains(&candidate) {
            candidate = format!("{base}_{k}");
            k += 1;
        }
        changed |= candidate != name;
        seen.insert(candidate.clone());
        out.push(candidate);
    }
    if changed {
        notes.push(Note::new(
            Severity::Warning,
            "columns_renamed",
            "empty or duplicate column names were renamed (column_N, name_2, ...)",
        ));
    }
    out
}

fn push_string(out: &mut String, s: &str, ascii: bool) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (ascii && !c.is_ascii()) => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Writes `table` as JSON text ending in a newline. `indent` 0 gives compact output.
pub fn write(table: &Table, indent: usize, ensure_ascii: bool, notes: &mut Notes) -> String {
    let width = table.width();
    let header = table
        .header
        .as_ref()
        .map(|h| unique_header(h, width, notes));
    let (nl, pad, colon) = if indent > 0 {
        ("\n", " ", ": ")
    } else {
        ("", "", ":")
    };
    let ind = |level: usize| pad.repeat(indent * level);
    let mut out = String::from("[");
    let n = table.rows.len();
    for (r, row) in table.rows.iter().enumerate() {
        out.push_str(nl);
        out.push_str(&ind(1));
        let cells = Table::padded_to(width, row);
        let (open, close) = if header.is_some() {
            ('{', '}')
        } else {
            ('[', ']')
        };
        out.push(open);
        if cells.is_empty() {
            out.push(close);
        } else {
            for (c, cell) in cells.iter().enumerate() {
                out.push_str(nl);
                out.push_str(&ind(2));
                if let Some(h) = &header {
                    push_string(&mut out, h.get(c).map_or("", String::as_str), ensure_ascii);
                    out.push_str(colon);
                }
                push_string(&mut out, cell, ensure_ascii);
                if c + 1 < cells.len() {
                    out.push(',');
                }
            }
            out.push_str(nl);
            out.push_str(&ind(1));
            out.push(close);
        }
        if r + 1 < n {
            out.push(',');
        }
    }
    if n > 0 {
        out.push_str(nl);
    }
    out.push_str("]\n");
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::indexing_slicing)]
    use super::*;

    fn rd(s: &str) -> Result<Table> {
        read(s, &TableLimits::default())
    }
    fn v(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|s| (*s).to_string()).collect())
            .collect()
    }

    #[test]
    fn array_of_objects() {
        let t = rd(r#"[{"a": 1, "b": true}, {"c": null, "a": "x"}, {"b": 1.5}]"#).unwrap();
        assert_eq!(t.header.as_deref().unwrap(), ["a", "b", "c"]);
        assert_eq!(
            t.rows,
            v(&[&["1", "true", ""], &["x", "", ""], &["", "1.5", ""]])
        );
        assert!(t.coerced_scalars);
    }

    #[test]
    fn arrays_and_empty() {
        let t = rd(r#"[["a", "b"], [1, false]]"#).unwrap();
        assert!(t.header.is_none());
        assert_eq!(t.rows, v(&[&["a", "b"], &["1", "false"]]));
        assert!(rd("[]").unwrap().rows.is_empty());
        assert!(!rd(r#"[{"n": "1"}]"#).unwrap().coerced_scalars);
    }

    #[test]
    fn numbers_keep_their_text() {
        let t = rd(r#"[[1e5, -0.50, 12345678901234567890]]"#).unwrap();
        assert_eq!(t.rows, v(&[&["1e5", "-0.50", "12345678901234567890"]]));
    }

    #[test]
    fn rejections() {
        for (bad, code) in [
            (r#"{"a": 1}"#, ErrorCode::UnsupportedVariant),
            (r#"[{"a": {"b": 1}}]"#, ErrorCode::UnsupportedVariant),
            ("[[1, [2]]]", ErrorCode::UnsupportedVariant),
            ("[1, 2]", ErrorCode::UnsupportedVariant),
            (r#"[{"a": 1}, [1]]"#, ErrorCode::UnsupportedVariant),
            (r#"[{"a": 1"#, ErrorCode::CorruptFile),
            ("[1,]", ErrorCode::CorruptFile),
            ("[01]", ErrorCode::CorruptFile),
            ("[NaN]", ErrorCode::CorruptFile),
            ("[\"a\nb\"]", ErrorCode::CorruptFile),
            (r#"["\ud800"]"#, ErrorCode::CorruptFile),
            ("[] []", ErrorCode::CorruptFile),
            ("", ErrorCode::CorruptFile),
        ] {
            assert_eq!(rd(bad).unwrap_err().code, code, "{bad}");
        }
        let e = rd(r#"[{"a": 1}, {"a": [1]}]"#).unwrap_err();
        assert!(e.message.contains("row 2"), "{}", e.message);
    }

    #[test]
    fn deep_nesting_hits_the_limit() {
        let deep = "[".repeat(10_000);
        assert_eq!(rd(&deep).unwrap_err().code, ErrorCode::LimitExceeded);
    }

    #[test]
    fn escapes_and_duplicate_keys() {
        let t = rd(r#"[{"a": "é😀\n\/", "a": "last", "b": "1"}]"#).unwrap();
        assert_eq!(t.rows, v(&[&["last", "1"]]));
        let t = rd(r#"[["é😀\n\/\b"]]"#).unwrap();
        assert_eq!(t.rows, v(&[&["é😀\n/\u{8}"]]));
    }

    #[test]
    fn writer_dedups_header() {
        let mut notes = Notes::new();
        let t = Table::new(
            Some(vec!["a".into(), "a".into(), String::new()]),
            v(&[&["1", "2", "3"]]),
        );
        assert_eq!(
            write(&t, 0, false, &mut notes),
            "[{\"a\":\"1\",\"a_2\":\"2\",\"column_3\":\"3\"}]\n"
        );
        assert!(!notes.is_empty());
    }

    #[test]
    fn indent_and_ascii() {
        let mut n = Notes::new();
        let t = Table::new(Some(vec!["k".into()]), v(&[&["é😀"]]));
        assert_eq!(write(&t, 0, false, &mut n), "[{\"k\":\"é😀\"}]\n");
        assert_eq!(
            write(&t, 0, true, &mut n),
            "[{\"k\":\"\\u00e9\\ud83d\\ude00\"}]\n"
        );
        assert_eq!(
            write(&t, 2, false, &mut n),
            "[\n  {\n    \"k\": \"é😀\"\n  }\n]\n"
        );
        assert_eq!(write(&Table::default(), 2, false, &mut n), "[]\n");
        let arrays = Table::new(None, v(&[&["a", "b"], &[]]));
        assert_eq!(
            write(&arrays, 2, false, &mut n),
            "[\n  [\n    \"a\",\n    \"b\"\n  ],\n  [\n    \"\",\n    \"\"\n  ]\n]\n"
        );
    }

    #[test]
    fn control_characters_are_escaped() {
        let mut n = Notes::new();
        let t = Table::new(None, v(&[&["a\u{1}b\"c\\"]]));
        assert_eq!(write(&t, 0, false, &mut n), "[[\"a\\u0001b\\\"c\\\\\"]]\n");
    }
}
