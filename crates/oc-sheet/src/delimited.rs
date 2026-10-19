//! RFC 4180 delimited text (CSV, TSV and the semicolon and pipe variants).
//!
//! The reader is lenient in the way spreadsheet programs are: text between a closing quote
//! and the next delimiter is kept, blank lines are skipped, and rows may have different
//! lengths. The writer quotes only fields that need it.
use crate::{Table, TableLimits};
use oc_core::{Error, ErrorCode, Progress, Result};

pub const DELIMITERS: [(&str, u8); 4] = [
    ("comma", b','),
    ("semicolon", b';'),
    ("tab", b'\t'),
    ("pipe", b'|'),
];

pub fn delimiter_name(d: u8) -> &'static str {
    DELIMITERS
        .iter()
        .find(|(_, c)| *c == d)
        .map_or("custom", |(n, _)| n)
}

pub fn delimiter_from_name(name: &str) -> Option<u8> {
    DELIMITERS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

/// Picks the delimiter whose per-line count (outside quotes) is most consistent over the
/// first 50 lines. Ties go to the earlier of `, ; tab |`.
pub fn detect_delimiter(text: &str, default: u8) -> u8 {
    let bytes = text.as_bytes();
    let bytes = bytes.get(..bytes.len().min(65536)).unwrap_or(bytes);
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut current = Vec::new();
    let mut in_quotes = false;
    for &b in bytes {
        if b == b'"' {
            in_quotes = !in_quotes;
        } else if b == b'\n' && !in_quotes {
            lines.push(std::mem::take(&mut current));
            if lines.len() >= 50 {
                break;
            }
        } else if !in_quotes {
            current.push(b);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.retain(|l| !l.iter().all(u8::is_ascii_whitespace));
    if lines.is_empty() {
        return default;
    }
    let mut best: Option<((f64, f64), u8)> = None;
    for &d in b",;\t|" {
        let counts: Vec<usize> = lines
            .iter()
            .map(|l| l.iter().filter(|&&c| c == d).count())
            .collect();
        if counts.iter().all(|&c| c == 0) {
            continue;
        }
        let first = counts.first().copied().unwrap_or(0);
        let consistent = counts.iter().filter(|&&c| c == first && c > 0).count();
        let n = counts.len() as f64;
        let score = (
            consistent as f64 / n,
            counts.iter().sum::<usize>() as f64 / n,
        );
        if best.is_none_or(|(b, _)| score > b) {
            best = Some((score, d));
        }
    }
    best.map_or(default, |(_, d)| d)
}

fn line_end_or_delim(bytes: &[u8], from: usize, delim: u8) -> usize {
    bytes
        .get(from..)
        .and_then(|s| {
            s.iter()
                .position(|&b| b == delim || b == b'\r' || b == b'\n')
        })
        .map_or(bytes.len(), |p| from + p)
}

fn limit(msg: &str) -> Error {
    Error::limit("table", msg)
}

/// Parses `text` into rows of fields. A leading byte-order mark is ignored.
pub fn parse(
    text: &str,
    delim: u8,
    limits: &TableLimits,
    progress: &dyn Progress,
) -> Result<Vec<Vec<String>>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let bytes = text.as_bytes();
    let n = bytes.len();
    let slice = |a: usize, b: usize| text.get(a..b).unwrap_or_default();
    let skip_newlines = |mut i: usize| {
        while bytes.get(i).is_some_and(|&b| b == b'\r' || b == b'\n') {
            i += 1;
        }
        i
    };
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cells = 0u64;
    let mut i = skip_newlines(0);
    let mut next_check = 1usize << 16;
    while i < n {
        let value;
        if bytes.get(i) == Some(&b'"') {
            let mut j = i + 1;
            let mut v = String::new();
            loop {
                let Some(k) = bytes
                    .get(j..)
                    .and_then(|s| s.iter().position(|&b| b == b'"'))
                    .map(|p| j + p)
                else {
                    return Err(Error::new(
                        ErrorCode::CorruptFile,
                        format!("unterminated quoted field in row {}", rows.len() + 1),
                    ));
                };
                v.push_str(slice(j, k));
                if bytes.get(k + 1) == Some(&b'"') {
                    v.push('"');
                    j = k + 2;
                    continue;
                }
                i = k + 1;
                break;
            }
            // Text after the closing quote is kept as written.
            let stop = line_end_or_delim(bytes, i, delim);
            v.push_str(slice(i, stop));
            i = stop;
            value = v;
        } else {
            let stop = line_end_or_delim(bytes, i, delim);
            value = slice(i, stop).to_string();
            i = stop;
        }
        if value.len() as u64 > limits.max_field_bytes {
            return Err(limit("a field is longer than the allowed maximum"));
        }
        row.push(value);
        cells += 1;
        if cells > limits.max_cells {
            return Err(limit("the table has more cells than the allowed maximum"));
        }
        let Some(&ch) = bytes.get(i) else { break };
        if ch == delim {
            i += 1;
            if i >= n {
                row.push(String::new()); // a delimiter ends the file: one more, empty, field
            }
            continue;
        }
        i += if ch == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
            2
        } else {
            1
        };
        rows.push(std::mem::take(&mut row));
        i = skip_newlines(i);
        if i >= next_check {
            next_check = i + (1 << 16);
            progress.check_cancelled()?;
            progress.report(i as u64, n as u64, "parsing");
        }
    }
    if !row.is_empty() {
        rows.push(row);
    }
    Ok(rows)
}

/// Splits parsed rows into header and body.
pub fn to_table(mut rows: Vec<Vec<String>>, has_header: bool) -> Table {
    if has_header && !rows.is_empty() {
        let header = rows.remove(0);
        Table::new(Some(header), rows)
    } else {
        Table::new(None, rows)
    }
}

fn push_field(out: &mut String, v: &str, delim: char) {
    // A leading U+FEFF at the start of the file would be read back as a byte-order mark.
    if v.contains([delim, '\r', '\n', '"']) || (out.is_empty() && v.starts_with('\u{feff}')) {
        out.push('"');
        out.push_str(&v.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(v);
    }
}

fn push_row(out: &mut String, row: &[String], delim: char, nl: &str) {
    // One empty field must not turn into a blank line, which readers skip.
    if row.len() == 1 && row.first().is_some_and(String::is_empty) {
        out.push_str("\"\"");
    } else {
        for (k, v) in row.iter().enumerate() {
            if k > 0 {
                out.push(delim);
            }
            push_field(out, v, delim);
        }
    }
    out.push_str(nl);
}

/// Writes `table` with short rows padded to the table width.
pub fn write(table: &Table, delim: u8, crlf: bool, progress: &dyn Progress) -> Result<String> {
    let nl = if crlf { "\r\n" } else { "\n" };
    let delim = char::from(delim);
    let mut out = String::new();
    if let Some(h) = &table.header {
        push_row(&mut out, &table.padded(h), delim, nl);
    }
    for (idx, r) in table.rows.iter().enumerate() {
        push_row(&mut out, &table.padded(r), delim, nl);
        if idx & 4095 == 0 {
            progress.check_cancelled()?;
            progress.report(idx as u64, table.rows.len().max(1) as u64, "writing");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use oc_core::NoProgress;

    fn p(text: &str, d: u8) -> Vec<Vec<String>> {
        parse(text, d, &TableLimits::default(), &NoProgress).unwrap()
    }
    fn v(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.iter().map(|s| (*s).to_string()).collect())
            .collect()
    }

    #[test]
    fn rfc4180_edge_cases() {
        assert_eq!(
            p("a,b,c\r\n1,\"x, y\",\"say \"\"hi\"\"\"\r\n", b','),
            v(&[&["a", "b", "c"], &["1", "x, y", "say \"hi\""]])
        );
        assert_eq!(
            p("a,\"line1\nline2\",c\n", b','),
            v(&[&["a", "line1\nline2", "c"]])
        );
        assert_eq!(p("a,,c\n,,\n", b','), v(&[&["a", "", "c"], &["", "", ""]]));
        assert_eq!(
            p("a,b,\n1,2,", b','),
            v(&[&["a", "b", ""], &["1", "2", ""]])
        );
        assert_eq!(
            p("a,b\n1\n1,2,3", b','),
            v(&[&["a", "b"], &["1"], &["1", "2", "3"]])
        );
        assert_eq!(p("\"\"\n\"\",\"\"", b','), v(&[&[""], &["", ""]]));
        assert_eq!(p("a,b", b','), v(&[&["a", "b"]]));
        assert_eq!(p("\u{feff}a,b\n", b','), v(&[&["a", "b"]]));
        assert_eq!(p("a\n\n\nb\r\n\r\n", b','), v(&[&["a"], &["b"]]));
        assert_eq!(p("\n\na\n", b','), v(&[&["a"]]));
        assert_eq!(p("a\rb\r", b','), v(&[&["a"], &["b"]]));
        assert_eq!(p("", b','), Vec::<Vec<String>>::new());
        assert_eq!(p("\"a\"b,c", b','), v(&[&["ab", "c"]]));
    }

    #[test]
    fn other_delimiters() {
        assert_eq!(p("a\tb\t\"c\td\"\n", b'\t'), v(&[&["a", "b", "c\td"]]));
        assert_eq!(p("a;b|c\n", b';'), v(&[&["a", "b|c"]]));
    }

    #[test]
    fn unterminated_quote_is_corrupt() {
        let e = parse("a,\"open\n", b',', &TableLimits::default(), &NoProgress).unwrap_err();
        assert_eq!(e.code, ErrorCode::CorruptFile);
    }

    #[test]
    fn limits_apply() {
        let small = TableLimits {
            max_cells: 20,
            max_field_bytes: 50,
            max_depth: 8,
        };
        let big = "a,b,c\n".repeat(10);
        assert_eq!(
            parse(&big, b',', &small, &NoProgress).unwrap_err().code,
            ErrorCode::LimitExceeded
        );
        assert_eq!(
            parse(&"x".repeat(100), b',', &small, &NoProgress)
                .unwrap_err()
                .code,
            ErrorCode::LimitExceeded
        );
    }

    #[test]
    fn delimiter_detection() {
        assert_eq!(detect_delimiter("a;b;c\n1;2;3\n", b','), b';');
        assert_eq!(detect_delimiter("a\tb\n1\t2\n", b','), b'\t');
        assert_eq!(detect_delimiter("a,b\n\"x;y;z\",2\n", b','), b',');
        assert_eq!(detect_delimiter("a|b|c\n1|2|3", b','), b'|');
        assert_eq!(detect_delimiter("single", b','), b',');
        assert_eq!(detect_delimiter("", b';'), b';');
    }

    #[test]
    fn a_leading_bom_character_in_data_survives() {
        let t = Table::new(None, v(&[&["\u{feff}x", "\u{feff}y"]]));
        let out = write(&t, b',', false, &NoProgress).unwrap();
        assert_eq!(out, "\"\u{feff}x\",\u{feff}y\n");
        assert_eq!(p(&out, b','), t.rows);
    }

    #[test]
    fn write_quotes_only_what_needs_it() {
        let t = Table::new(
            Some(vec!["h1".into(), "h,2".into()]),
            v(&[&["plain", "q\"uote"], &["multi\nline", ""], &["only"]]),
        );
        let out = write(&t, b',', true, &NoProgress).unwrap();
        assert_eq!(
            out,
            "h1,\"h,2\"\r\nplain,\"q\"\"uote\"\r\n\"multi\nline\",\r\nonly,\r\n"
        );
        assert_eq!(
            p(&out, b','),
            v(&[
                &["h1", "h,2"],
                &["plain", "q\"uote"],
                &["multi\nline", ""],
                &["only", ""]
            ])
        );
        let single = Table::new(None, v(&[&[""]]));
        assert_eq!(write(&single, b',', false, &NoProgress).unwrap(), "\"\"\n");
    }
}
