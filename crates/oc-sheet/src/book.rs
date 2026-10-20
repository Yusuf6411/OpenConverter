//! Workbooks (several sheets of text cells) and the cell helpers the XLSX and ODS code share:
//! A1 references, number and date recognition, date serial numbers.
// Parsers here walk byte and cell positions they have just bounds-checked.
#![allow(clippy::indexing_slicing)]

use crate::Table;
use std::collections::BTreeMap;

/// One sheet. `table` holds the displayed text; `formulas` and `merges` are side information
/// (positions are in `table` coordinates counting the header row as row 0) that only the
/// spreadsheet writers use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sheet {
    pub name: String,
    pub table: Table,
    pub hidden: bool,
    pub formulas: BTreeMap<(usize, usize), String>,
    /// Merged ranges as (first row, first column, last row, last column), zero based.
    pub merges: Vec<(usize, usize, usize, usize)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    /// True if cells were read from a workbook that used the 1904 date system.
    pub date1904: bool,
}

impl Workbook {
    pub fn single(table: Table, name: &str) -> Self {
        Self {
            sheets: vec![Sheet {
                name: name.to_string(),
                table,
                ..Sheet::default()
            }],
            date1904: false,
        }
    }
}

/// `0` becomes `A`, `26` becomes `AA`.
pub fn col_name(mut idx: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (idx % 26) as u8);
        if idx < 26 {
            break;
        }
        idx = idx / 26 - 1;
    }
    s.reverse();
    String::from_utf8_lossy(&s).into_owned()
}

/// `"B12"` becomes `(11, 1)` (row, column), zero based.
pub fn parse_ref(r: &str) -> Option<(usize, usize)> {
    let r = r.trim_start_matches('$');
    let split = r.find(|c: char| c.is_ascii_digit() || c == '$')?;
    let (letters, digits) = r.split_at(split);
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let mut col = 0usize;
    for c in letters.bytes() {
        if !c.is_ascii_alphabetic() {
            return None;
        }
        col = col * 26 + usize::from(c.to_ascii_uppercase() - b'A') + 1;
    }
    let row: usize = digits.trim_start_matches('$').parse().ok()?;
    if row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Days from 1970-01-01 to the day with serial number 0 in the 1900 system (1899-12-30, which
/// absorbs Lotus 1-2-3's non-existent 1900-02-29) and in the 1904 system.
const EPOCH_1900: i64 = -25_569;
const EPOCH_1904: i64 = -24_107;

/// Splits a serial number into (year, month, day, hour, minute, second). `None` if it is
/// negative or beyond year 9999.
pub fn serial_to_datetime(serial: f64, date1904: bool) -> Option<(i64, i64, i64, i64, i64, i64)> {
    if !serial.is_finite() || !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let mut days = serial.floor() as i64;
    let mut secs = ((serial - serial.floor()) * 86_400.0).round() as i64;
    if secs >= 86_400 {
        secs -= 86_400;
        days += 1;
    }
    // Serials below 61 of the 1900 system are one day later than the proleptic calendar says
    // (Lotus 1-2-3's 1900-02-29, which Excel keeps, never existed).
    let adjust = if !date1904 && days < 61 { 1 } else { 0 };
    let epoch = if date1904 { EPOCH_1904 } else { EPOCH_1900 };
    let (y, m, d) = civil_from_days(days + epoch + adjust);
    Some((y, m, d, secs / 3600, secs / 60 % 60, secs % 60))
}

pub fn datetime_to_serial(
    y: i64,
    m: i64,
    d: i64,
    hh: i64,
    mm: i64,
    ss: i64,
    date1904: bool,
) -> f64 {
    let epoch = if date1904 { EPOCH_1904 } else { EPOCH_1900 };
    let mut days = days_from_civil(y, m, d) - epoch;
    if !date1904 && days < 61 {
        days -= 1;
    }
    days as f64 + (hh * 3600 + mm * 60 + ss) as f64 / 86_400.0
}

/// How a cell's text is written to a spreadsheet.
#[derive(Debug, Clone, PartialEq)]
pub enum Kind<'a> {
    Empty,
    Number(&'a str),
    Bool(bool),
    /// Serial number and whether the text had a time of day.
    Date(f64, bool),
    Text,
}

fn is_plain_number(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = usize::from(b.first() == Some(&b'-'));
    let int_start = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    let int_len = i - int_start;
    if int_len == 0 || (int_len > 1 && b.get(int_start) == Some(&b'0')) {
        return false;
    }
    let mut digits = int_len;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let f = i;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
        if i == f {
            return false;
        }
        digits += i - f;
    }
    i == b.len() && digits <= 15 && s != "-0"
}

fn two(b: &[u8], at: usize) -> Option<i64> {
    let (x, y) = (*b.get(at)?, *b.get(at + 1)?);
    (x.is_ascii_digit() && y.is_ascii_digit())
        .then(|| i64::from(x - b'0') * 10 + i64::from(y - b'0'))
}

/// `YYYY-MM-DD` optionally followed by `T` or a space and `HH:MM[:SS]`.
pub fn parse_iso_datetime(s: &str) -> Option<(i64, i64, i64, i64, i64, i64, bool)> {
    let b = s.as_bytes();
    if b.len() < 10 || b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') {
        return None;
    }
    let y = two(b, 0)? * 100 + two(b, 2)?;
    let (m, d) = (two(b, 5)?, two(b, 8)?);
    if !(1900..=9999).contains(&y) || !(1..=12).contains(&m) {
        return None;
    }
    let dim = [
        31,
        if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) {
            29
        } else {
            28
        },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if d < 1 || d > *dim.get(m as usize - 1)? {
        return None;
    }
    if b.len() == 10 {
        return Some((y, m, d, 0, 0, 0, false));
    }
    if !matches!(b.get(10), Some(b'T' | b' ')) || b.get(13) != Some(&b':') {
        return None;
    }
    let (hh, mi) = (two(b, 11)?, two(b, 14)?);
    let ss = match b.len() {
        16 => 0,
        19 if b.get(16) == Some(&b':') => two(b, 17)?,
        _ => return None,
    };
    (hh < 24 && mi < 60 && ss < 60).then_some((y, m, d, hh, mi, ss, true))
}

/// Decides how `s` is stored in a spreadsheet when type detection is on.
pub fn classify(s: &str, detect_dates: bool, date1904: bool) -> Kind<'_> {
    if s.is_empty() {
        return Kind::Empty;
    }
    if is_plain_number(s) {
        return Kind::Number(s);
    }
    match s {
        "TRUE" | "true" => return Kind::Bool(true),
        "FALSE" | "false" => return Kind::Bool(false),
        _ => {}
    }
    if detect_dates {
        if let Some((y, m, d, hh, mi, ss, time)) = parse_iso_datetime(s) {
            // 1900-01-01 and 1900-02-28 are fine; before March 1900 the serials are quirky.
            if y > 1900 || m >= 3 {
                return Kind::Date(datetime_to_serial(y, m, d, hh, mi, ss, date1904), time);
            }
        }
    }
    Kind::Text
}

/// A number the way it is shown: whole values without a fraction, others in their shortest
/// round-trip form.
pub fn format_number(v: f64) -> String {
    if v.is_finite() && v == v.trunc() && v.abs() < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{v}");
    if s.len() > 30 {
        format!("{v:e}")
    } else {
        s
    }
}

pub fn format_datetime(
    parts: (i64, i64, i64, i64, i64, i64),
    with_date: bool,
    with_time: bool,
) -> String {
    let (y, m, d, hh, mi, ss) = parts;
    match (with_date, with_time) {
        (true, true) => format!("{y:04}-{m:02}-{d:02} {hh:02}:{mi:02}:{ss:02}"),
        (true, false) => format!("{y:04}-{m:02}-{d:02}"),
        _ => format!("{hh:02}:{mi:02}:{ss:02}"),
    }
}

/// Excel sheet names: at most 31 characters, none of `[]:*?/\`, not empty; duplicates get a
/// numeric suffix.
pub fn unique_sheet_names(names: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (i, n) in names.iter().enumerate() {
        let mut base: String = n
            .chars()
            .map(|c| {
                if matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\') || c.is_control() {
                    '_'
                } else {
                    c
                }
            })
            .collect();
        base = base.trim_matches('\'').to_string();
        if base.trim().is_empty() {
            base = format!("Sheet{}", i + 1);
        }
        let mut cand: String = base.chars().take(31).collect();
        let mut k = 2;
        while out.iter().any(|o| o.eq_ignore_ascii_case(&cand)) {
            let suffix = format!(" ({k})");
            let keep = 31 - suffix.chars().count();
            cand = format!("{}{suffix}", base.chars().take(keep).collect::<String>());
            k += 1;
        }
        out.push(cand);
    }
    out
}

/// Removes characters XML 1.0 cannot carry; reports whether any were dropped.
pub fn xml_clean(s: &str) -> (std::borrow::Cow<'_, str>, bool) {
    if s.chars().all(oc_xml::is_xml_char) {
        return (std::borrow::Cow::Borrowed(s), false);
    }
    (
        s.chars()
            .filter(|c| oc_xml::is_xml_char(*c))
            .collect::<String>()
            .into(),
        true,
    )
}

/// A formula as ODF stores it (`of:=SUM([.A1:.B2];[$Sheet2.C3])`) written the Excel way
/// (`SUM(A1:B2,Sheet2!C3)`). Function names are not translated.
pub fn odf_formula_to_a1(f: &str) -> String {
    let f = f.trim();
    let f = f
        .strip_prefix("of:")
        .or_else(|| f.strip_prefix("oooc:"))
        .unwrap_or(f);
    let f = f.strip_prefix('=').unwrap_or(f);
    let mut out = String::new();
    let mut chars = f.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push(c);
                for d in chars.by_ref() {
                    out.push(d);
                    if d == '"' {
                        break;
                    }
                }
            }
            '[' => {
                let mut inner = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    inner.push(d);
                }
                let parts: Vec<String> = inner
                    .split(':')
                    .map(|p| match p.strip_prefix('.') {
                        Some(r) => r.to_string(),
                        None => match p.split_once('.') {
                            Some((sheet, r)) => {
                                let sheet = sheet.strip_prefix('$').unwrap_or(sheet);
                                if sheet.contains(' ') && !sheet.starts_with('\'') {
                                    format!("'{sheet}'!{r}")
                                } else {
                                    format!("{sheet}!{r}")
                                }
                            }
                            None => p.to_string(),
                        },
                    })
                    .collect();
                out.push_str(&parts.join(":"));
            }
            ';' => out.push(','),
            _ => out.push(c),
        }
    }
    out
}

fn cell_ref_len(b: &[u8], mut i: usize) -> Option<usize> {
    let start = i;
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let l = i;
    while b.get(i).is_some_and(u8::is_ascii_alphabetic) {
        i += 1;
    }
    if !(1..=3).contains(&(i - l)) {
        return None;
    }
    if b.get(i) == Some(&b'$') {
        i += 1;
    }
    let d = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    (i > d).then_some(i - start)
}

/// The reverse of [`odf_formula_to_a1`]: wraps cell and range references in `[.A1]` brackets and
/// turns argument commas into semicolons.
pub fn a1_formula_to_odf(f: &str) -> String {
    let f = f.trim();
    let f = f.strip_prefix('=').unwrap_or(f);
    let b = f.as_bytes();
    let mut out = String::from("of:=");
    let mut i = 0usize;
    let ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'.';
    while i < b.len() {
        let c = b[i];
        if c == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                j += 1;
            }
            let end = (j + 1).min(b.len());
            out.push_str(&f[i..end]);
            i = end;
            continue;
        }
        if c == b',' {
            out.push(';');
            i += 1;
            continue;
        }
        let prev_ok = i == 0 || !ident(b[i - 1]) && b[i - 1] != b'$';
        if prev_ok {
            // Optional sheet prefix.
            let mut j = i;
            let mut sheet: Option<&str> = None;
            if c == b'\'' {
                if let Some(q) = f[i + 1..].find('\'') {
                    if b.get(i + q + 2) == Some(&b'!') {
                        sheet = Some(&f[i..i + q + 2]);
                        j = i + q + 3;
                    }
                }
            } else if c.is_ascii_alphabetic() || c == b'_' {
                let mut k = i;
                while k < b.len() && (b[k].is_ascii_alphanumeric() || b[k] == b'_') {
                    k += 1;
                }
                if b.get(k) == Some(&b'!') {
                    sheet = Some(&f[i..k]);
                    j = k + 1;
                }
            }
            if let Some(n1) = cell_ref_len(b, j) {
                let mut end = j + n1;
                let mut second = None;
                if b.get(end) == Some(&b':') {
                    if let Some(n2) = cell_ref_len(b, end + 1) {
                        second = Some(&f[end + 1..end + 1 + n2]);
                        end += 1 + n2;
                    }
                }
                if !b
                    .get(end)
                    .is_some_and(|&n| ident(n) || n == b'(' || n == b'$')
                {
                    out.push('[');
                    match sheet {
                        Some(s) => {
                            out.push('$');
                            out.push_str(s);
                            out.push('.');
                        }
                        None => out.push('.'),
                    }
                    out.push_str(&f[j..j + n1]);
                    if let Some(s2) = second {
                        out.push_str(":.");
                        out.push_str(s2);
                    }
                    out.push(']');
                    i = end;
                    continue;
                }
            }
        }
        let ch = f[i..].chars().next().unwrap_or(' ');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn references() {
        assert_eq!(col_name(0), "A");
        assert_eq!(col_name(25), "Z");
        assert_eq!(col_name(26), "AA");
        assert_eq!(col_name(701), "ZZ");
        assert_eq!(col_name(702), "AAA");
        assert_eq!(parse_ref("A1"), Some((0, 0)));
        assert_eq!(parse_ref("$B$12"), Some((11, 1)));
        assert_eq!(parse_ref("AA3"), Some((2, 26)));
        assert_eq!(parse_ref("A0"), None);
        assert_eq!(parse_ref("12"), None);
        for i in [0, 1, 25, 26, 27, 700, 701, 702, 16383] {
            assert_eq!(parse_ref(&format!("{}1", col_name(i))), Some((0, i)));
        }
    }

    #[test]
    #[allow(clippy::unwrap_used)]
    fn serial_numbers() {
        // Values Excel shows: 1 = 1900-01-01, 59 = 1900-02-28, 61 = 1900-03-01, 45000 = 2023-03-15.
        for (serial, ymd) in [
            (1.0, (1900, 1, 1)),
            (59.0, (1900, 2, 28)),
            (61.0, (1900, 3, 1)),
            (44927.0, (2023, 1, 1)),
            (45000.0, (2023, 3, 15)),
        ] {
            let (y, m, d, ..) = serial_to_datetime(serial, false).unwrap();
            assert_eq!((y, m, d), ymd, "serial {serial}");
            assert_eq!(
                datetime_to_serial(ymd.0, ymd.1, ymd.2, 0, 0, 0, false),
                serial
            );
        }
        assert_eq!(
            serial_to_datetime(0.5, true).map(|t| (t.0, t.3)),
            Some((1904, 12))
        );
        let t = serial_to_datetime(45000.75, false).unwrap();
        assert_eq!((t.3, t.4, t.5), (18, 0, 0));
        assert!(serial_to_datetime(-1.0, false).is_none());
    }

    #[test]
    fn cell_kinds() {
        assert_eq!(classify("", true, false), Kind::Empty);
        assert_eq!(classify("12", true, false), Kind::Number("12"));
        assert_eq!(classify("-3.25", true, false), Kind::Number("-3.25"));
        assert_eq!(classify("007", true, false), Kind::Text);
        assert_eq!(classify("1e5", true, false), Kind::Text);
        assert_eq!(classify("12345678901234567", true, false), Kind::Text);
        assert_eq!(classify("+5", true, false), Kind::Text);
        assert_eq!(classify("TRUE", true, false), Kind::Bool(true));
        assert!(matches!(
            classify("2024-02-29", true, false),
            Kind::Date(_, false)
        ));
        assert!(matches!(
            classify("2024-02-29 10:30", true, false),
            Kind::Date(_, true)
        ));
        assert_eq!(classify("2023-02-29", true, false), Kind::Text);
        assert_eq!(classify("2024-02-29", false, false), Kind::Text);
    }

    #[test]
    fn sheet_names() {
        let n = unique_sheet_names(&["a/b".into(), "A_B".into(), String::new(), "x".repeat(40)]);
        assert_eq!(n[0], "a_b");
        assert_eq!(n[1], "A_B (2)");
        assert_eq!(n[2], "Sheet3");
        assert_eq!(n[3].chars().count(), 31);
    }

    #[test]
    fn formula_translation() {
        assert_eq!(
            odf_formula_to_a1("of:=SUM([.A1:.B2];[$Sheet2.C3])"),
            "SUM(A1:B2,Sheet2!C3)"
        );
        assert_eq!(odf_formula_to_a1("of:=[.$A$1]*2+\"x;y\""), "$A$1*2+\"x;y\"");
        assert_eq!(
            a1_formula_to_odf("=SUM(A1:B2,Sheet2!C3)"),
            "of:=SUM([.A1:.B2];[$Sheet2.C3])"
        );
        assert_eq!(
            a1_formula_to_odf("LOG10(A1)+\"A1,\""),
            "of:=LOG10([.A1])+\"A1,\""
        );
        assert_eq!(
            a1_formula_to_odf("'My Sheet'!B2*2"),
            "of:=[$'My Sheet'.B2]*2"
        );
        for f in ["SUM(A1:B2,Sheet2!C3)", "IF(A1>2,\"yes\",B$3)"] {
            assert_eq!(odf_formula_to_a1(&a1_formula_to_odf(f)), f);
        }
    }
}
