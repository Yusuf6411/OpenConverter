//! A small XML 1.0 pull parser and writer, safe by default.
//!
//! Safety: entity declarations and external entities are rejected, so there is no
//! billion-laughs or XXE surface; nesting depth and attribute counts are bounded.
//! Input must already be decoded to UTF-8 (`oc-text` handles encodings).
#![forbid(unsafe_code)]

mod reader;
mod writer;

pub use reader::{Attribute, Event, Reader};
pub use writer::Writer;

/// Escape text content (`&`, `<`, `>`).
pub fn escape_text(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            _ => o.push(c),
        }
    }
    o
}

/// Escape an attribute value for use inside double quotes.
pub fn escape_attr(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '"' => o.push_str("&quot;"),
            '\t' => o.push_str("&#9;"),
            '\n' => o.push_str("&#10;"),
            '\r' => o.push_str("&#13;"),
            _ => o.push(c),
        }
    }
    o
}

/// True for characters allowed in XML 1.0 documents (production `Char`).
pub fn is_xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}')
}
