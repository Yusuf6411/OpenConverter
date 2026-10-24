use crate::is_xml_char;
use oc_core::{Error, ErrorCode, Limits, Result};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// Qualified name as written, e.g. `xlink:href`.
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Decl {
        version: String,
        encoding: Option<String>,
        standalone: Option<bool>,
    },
    /// An empty element `<a/>` yields `Start` immediately followed by `End`.
    Start {
        name: String,
        ns: Option<String>,
        attrs: Vec<Attribute>,
    },
    End {
        name: String,
        ns: Option<String>,
    },
    Text(String),
    CData(String),
    Comment(String),
    Pi {
        target: String,
        data: String,
    },
    Eof,
}

/// Pull parser. Empty elements produce a `Start` immediately followed by an `End`.
type Bindings = Vec<(String, Option<String>)>;

pub struct Reader<'a> {
    src: &'a str,
    pos: usize,
    /// Stack of (qualified name, namespace bindings introduced by that element).
    stack: Vec<(String, Bindings)>,
    ns: HashMap<String, String>,
    pending_end: Option<(String, Option<String>)>,
    seen_root: bool,
    finished_root: bool,
    started: bool,
    max_depth: u32,
    max_attrs: usize,
}

fn err(msg: impl Into<String>, at: usize) -> Error {
    Error::new(ErrorCode::CorruptFile, msg).at(at as u64)
}

fn is_name_start(c: char) -> bool {
    c == '_' || c == ':' || c.is_alphabetic() || ('\u{C0}'..='\u{2FF}').contains(&c)
}
fn is_name_char(c: char) -> bool {
    is_name_start(c) || c == '-' || c == '.' || c.is_ascii_digit() || c == '\u{B7}'
}

impl<'a> Reader<'a> {
    pub fn new(src: &'a str, limits: &Limits) -> Self {
        let src_body = src.strip_prefix('\u{FEFF}').unwrap_or(src);
        let mut ns = HashMap::new();
        ns.insert(
            "xml".to_string(),
            "http://www.w3.org/XML/1998/namespace".to_string(),
        );
        Self {
            src: src_body,
            pos: 0,
            stack: Vec::new(),
            ns,
            pending_end: None,
            seen_root: false,
            finished_root: false,
            started: false,
            max_depth: limits.max_depth.min(256),
            max_attrs: 4096,
        }
    }

    fn rest(&self) -> &'a str {
        self.src.get(self.pos..).unwrap_or("")
    }

    fn eat(&mut self, s: &str) -> bool {
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        let r = self.rest();
        self.pos += r.len() - r.trim_start_matches([' ', '\t', '\r', '\n']).len();
    }

    fn take_until(&mut self, end: &str, what: &str) -> Result<&'a str> {
        let r = self.rest();
        let i = r
            .find(end)
            .ok_or_else(|| err(format!("unterminated {what}"), self.pos))?;
        self.pos += i + end.len();
        r.get(..i)
            .ok_or_else(|| err("internal slice error", self.pos))
    }

    fn name(&mut self) -> Result<String> {
        let r = self.rest();
        let mut it = r.char_indices();
        match it.next() {
            Some((_, c)) if is_name_start(c) => {}
            _ => return Err(err("expected a name", self.pos)),
        }
        let end = r
            .char_indices()
            .find(|&(_, c)| !is_name_char(c))
            .map_or(r.len(), |(i, _)| i);
        self.pos += end;
        Ok(r.get(..end).unwrap_or("").to_string())
    }

    /// Resolve predefined entities and character references; reject everything else.
    fn unescape(&self, raw: &str, at: usize, attr: bool) -> Result<String> {
        let mut out = String::with_capacity(raw.len());
        let mut rest = raw;
        while let Some(i) = rest.find('&') {
            out.push_str(rest.get(..i).unwrap_or(""));
            let after = rest.get(i + 1..).unwrap_or("");
            let semi = after
                .find(';')
                .ok_or_else(|| err("unterminated entity reference", at))?;
            let ent = after.get(..semi).unwrap_or("");
            match ent {
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "amp" => out.push('&'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ => {
                    let num = ent.strip_prefix('#').ok_or_else(|| {
                        Error::new(
                            ErrorCode::UnsupportedVariant,
                            "custom entities are not supported",
                        )
                        .with_detail(ent.to_string())
                        .at(at as u64)
                    })?;
                    let v = match num.strip_prefix('x') {
                        Some(h) => u32::from_str_radix(h, 16),
                        None => num.parse::<u32>(),
                    }
                    .map_err(|_| err("bad character reference", at))?;
                    let c = char::from_u32(v)
                        .filter(|&c| is_xml_char(c))
                        .ok_or_else(|| err("character reference is not an XML character", at))?;
                    out.push(c);
                }
            }
            rest = after.get(semi + 1..).unwrap_or("");
        }
        out.push_str(rest);
        if attr {
            // Attribute-value normalisation: literal whitespace becomes a space.
            out = out.replace(['\t', '\n', '\r'], " ");
        }
        Ok(out)
    }

    fn lookup(&self, qname: &str, is_attr: bool) -> Option<String> {
        match qname.split_once(':') {
            Some((p, _)) => self.ns.get(p).cloned(),
            None if is_attr => None,
            None => self.ns.get("").cloned(),
        }
    }

    pub fn read_event(&mut self) -> Result<Event> {
        if let Some((name, ns)) = self.pending_end.take() {
            return Ok(Event::End { name, ns });
        }
        if !self.started {
            self.started = true;
            if self.rest().starts_with("<?xml")
                && self
                    .rest()
                    .get(5..6)
                    .is_some_and(|c| c.starts_with([' ', '\t', '\r', '\n']))
            {
                return self.decl();
            }
        }
        loop {
            if self.rest().is_empty() {
                if let Some((n, _)) = self.stack.last() {
                    return Err(err(format!("unclosed element <{n}>"), self.pos));
                }
                if !self.seen_root {
                    return Err(err("document has no root element", self.pos));
                }
                return Ok(Event::Eof);
            }
            if !self.rest().starts_with('<') {
                let r = self.rest();
                let end = r.find('<').unwrap_or(r.len());
                let raw = r.get(..end).unwrap_or("");
                let at = self.pos;
                self.pos += end;
                if self.stack.is_empty() {
                    if raw.trim_matches([' ', '\t', '\r', '\n']).is_empty() {
                        continue;
                    }
                    return Err(err("text outside the root element", at));
                }
                if raw.contains("]]>") {
                    return Err(err("']]>' in text", at));
                }
                let text = self.unescape(raw, at, false)?;
                return Ok(Event::Text(text));
            }
            if self.eat("<!--") {
                let at = self.pos;
                let body = self.take_until("-->", "comment")?;
                if body.contains("--") || body.ends_with('-') {
                    return Err(err("'--' inside comment", at));
                }
                return Ok(Event::Comment(body.to_string()));
            }
            if self.eat("<![CDATA[") {
                if self.stack.is_empty() {
                    return Err(err("CDATA outside the root element", self.pos));
                }
                let body = self.take_until("]]>", "CDATA section")?;
                return Ok(Event::CData(body.to_string()));
            }
            if self.eat("<?") {
                let target = self.name()?;
                if target.eq_ignore_ascii_case("xml") {
                    return Err(err(
                        "XML declaration must be first in the document",
                        self.pos,
                    ));
                }
                let body = self.take_until("?>", "processing instruction")?;
                return Ok(Event::Pi {
                    target,
                    data: body.trim_start().to_string(),
                });
            }
            if self.eat("<!DOCTYPE") {
                self.doctype()?;
                continue;
            }
            if self.eat("</") {
                return self.end_tag();
            }
            self.pos += 1;
            return self.start_tag();
        }
    }

    fn decl(&mut self) -> Result<Event> {
        let at = self.pos;
        self.pos += 5;
        let body = self.take_until("?>", "XML declaration")?;
        let get = |key: &str| -> Option<String> {
            let i = body.find(key)?;
            let after = body
                .get(i + key.len()..)?
                .trim_start()
                .strip_prefix('=')?
                .trim_start();
            let q = after.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            let inner = after.get(1..)?;
            inner.get(..inner.find(q)?).map(str::to_string)
        };
        let version = get("version").ok_or_else(|| err("XML declaration lacks a version", at))?;
        Ok(Event::Decl {
            version,
            encoding: get("encoding"),
            standalone: get("standalone").map(|s| s == "yes"),
        })
    }

    fn doctype(&mut self) -> Result<()> {
        let at = self.pos;
        // Scan to the matching '>' while refusing an internal subset with declarations.
        let r = self.rest();
        let gt = r.find('>').ok_or_else(|| err("unterminated DOCTYPE", at))?;
        let head = r.get(..gt).unwrap_or("");
        if head.contains('[') {
            return Err(Error::new(
                ErrorCode::UnsupportedVariant,
                "DOCTYPE internal subsets (entity declarations) are not supported",
            )
            .at(at as u64));
        }
        self.pos += gt + 1;
        Ok(())
    }

    fn end_tag(&mut self) -> Result<Event> {
        let at = self.pos;
        let name = self.name()?;
        self.skip_ws();
        if !self.eat(">") {
            return Err(err("malformed end tag", at));
        }
        let (open, binds) = self
            .stack
            .pop()
            .ok_or_else(|| err("unexpected end tag", at))?;
        if open != name {
            return Err(err(
                format!("end tag </{name}> does not match <{open}>"),
                at,
            ));
        }
        let ns = self.lookup(&name, false);
        self.pop_bindings(binds);
        if self.stack.is_empty() {
            self.finished_root = true;
        }
        Ok(Event::End { name, ns })
    }

    fn pop_bindings(&mut self, binds: Bindings) {
        for (prefix, old) in binds.into_iter().rev() {
            match old {
                Some(u) => {
                    self.ns.insert(prefix, u);
                }
                None => {
                    self.ns.remove(&prefix);
                }
            }
        }
    }

    fn start_tag(&mut self) -> Result<Event> {
        let at = self.pos;
        if self.finished_root {
            return Err(err("content after the root element", at));
        }
        if self.stack.len() as u32 >= self.max_depth {
            return Err(Error::limit("max_depth", "XML nesting too deep").at(at as u64));
        }
        let name = self.name()?;
        let mut attrs: Vec<Attribute> = Vec::new();
        let empty;
        loop {
            let had_ws = self.rest().starts_with([' ', '\t', '\r', '\n']);
            self.skip_ws();
            if self.eat("/>") {
                empty = true;
                break;
            }
            if self.eat(">") {
                empty = false;
                break;
            }
            if !had_ws {
                return Err(err("expected whitespace between attributes", self.pos));
            }
            if attrs.len() >= self.max_attrs {
                return Err(Error::limit("max_entries", "too many attributes").at(self.pos as u64));
            }
            let an = self.name()?;
            self.skip_ws();
            if !self.eat("=") {
                return Err(err("attribute without a value", self.pos));
            }
            self.skip_ws();
            let q = self
                .rest()
                .chars()
                .next()
                .filter(|c| *c == '"' || *c == '\'');
            let q = q.ok_or_else(|| err("attribute value must be quoted", self.pos))?;
            self.pos += 1;
            let vat = self.pos;
            let raw = self.take_until(&q.to_string(), "attribute value")?;
            if raw.contains('<') {
                return Err(err("'<' in attribute value", vat));
            }
            if attrs.iter().any(|a| a.name == an) {
                return Err(err(format!("duplicate attribute '{an}'"), vat));
            }
            attrs.push(Attribute {
                name: an,
                value: self.unescape(raw, vat, true)?,
            });
        }
        // Namespace declarations are scoped to this element.
        let mut binds = Vec::new();
        for a in &attrs {
            let prefix = if a.name == "xmlns" {
                Some(String::new())
            } else {
                a.name.strip_prefix("xmlns:").map(str::to_string)
            };
            if let Some(p) = prefix {
                binds.push((p.clone(), self.ns.insert(p, a.value.clone())));
            }
        }
        let ns = self.lookup(&name, false);
        if name.contains(':') && ns.is_none() {
            return Err(err(format!("undeclared namespace prefix in <{name}>"), at));
        }
        self.seen_root = true;
        if empty {
            self.pop_bindings(binds);
            if self.stack.is_empty() {
                self.finished_root = true;
            }
            self.pending_end = Some((name.clone(), ns.clone()));
        } else {
            self.stack.push((name.clone(), binds));
        }
        Ok(Event::Start { name, ns, attrs })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)]
    use super::*;

    fn events(src: &str) -> Result<Vec<Event>> {
        let mut r = Reader::new(src, &Limits::default());
        let mut v = Vec::new();
        loop {
            match r.read_event()? {
                Event::Eof => return Ok(v),
                e => v.push(e),
            }
        }
    }

    #[test]
    fn basic_document() {
        let ev = events("<?xml version=\"1.0\" encoding=\"UTF-8\"?><!--c--><a x='1' y=\"&lt;2&gt;\">t&amp;<b/><![CDATA[<raw>]]></a>")
            .unwrap_or_default();
        assert!(
            matches!(&ev[0], Event::Decl { version, encoding: Some(e), .. } if version == "1.0" && e == "UTF-8")
        );
        assert_eq!(ev[1], Event::Comment("c".into()));
        match &ev[2] {
            Event::Start { name, attrs, .. } => {
                assert_eq!(name, "a");
                assert_eq!(attrs[1].value, "<2>");
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(ev[3], Event::Text("t&".into()));
        assert!(matches!(&ev[4], Event::Start { name, .. } if name == "b"));
        assert!(matches!(&ev[5], Event::End { name, .. } if name == "b"));
        assert_eq!(ev[6], Event::CData("<raw>".into()));
        assert!(matches!(&ev[7], Event::End { name, .. } if name == "a"));
    }

    #[test]
    fn namespaces_resolve_and_scope() {
        let ev =
            events("<r xmlns='urn:d' xmlns:p='urn:p'><p:a><b xmlns='urn:inner'/></p:a><c/></r>")
                .unwrap_or_default();
        let ns_of = |name: &str| {
            ev.iter().find_map(|e| match e {
                Event::Start { name: n, ns, .. } if n == name => Some(ns.clone()),
                _ => None,
            })
        };
        assert_eq!(ns_of("p:a"), Some(Some("urn:p".into())));
        assert_eq!(ns_of("b"), Some(Some("urn:inner".into())));
        assert_eq!(ns_of("c"), Some(Some("urn:d".into())));
        assert!(events("<p:a/>").is_err());
    }

    #[test]
    fn char_references() {
        let ev = events("<a>&#65;&#x1F600;</a>").unwrap_or_default();
        assert_eq!(ev[1], Event::Text("A😀".into()));
        assert!(events("<a>&#0;</a>").is_err());
        assert!(events("<a>&#xD800;</a>").is_err());
    }

    #[test]
    fn rejects_malformed_documents() {
        for bad in [
            "",
            "text",
            "<a>",
            "<a></b>",
            "<a/><b/>",
            "<a x=1/>",
            "<a x='1' x='2'/>",
            "<a x='<'/>",
            "<a><!-- -- --></a>",
            "<a>]]></a>",
            "</a>",
            "<a b/>",
            "<a>&nope;</a>",
            "<?xml version='1.0'?><?xml version='1.0'?><a/>",
        ] {
            assert!(events(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn entity_declarations_are_refused() {
        let bomb = "<!DOCTYPE lolz [<!ENTITY a \"aaaa\"><!ENTITY b \"&a;&a;\">]><lolz>&b;</lolz>";
        let e = events(bomb).err();
        assert_eq!(e.map(|e| e.code), Some(ErrorCode::UnsupportedVariant));
        let xxe = "<!DOCTYPE d [<!ENTITY x SYSTEM \"file:///etc/passwd\">]><d>&x;</d>";
        assert!(events(xxe).is_err());
        // A plain DOCTYPE (as in XHTML) is skipped.
        assert!(events("<!DOCTYPE html><html/>").is_ok());
    }

    #[test]
    fn depth_is_bounded() {
        let limits = Limits {
            max_depth: 10,
            ..Limits::default()
        };
        let deep = format!("{}{}", "<a>".repeat(50), "</a>".repeat(50));
        let mut r = Reader::new(&deep, &limits);
        let mut result = Ok(Event::Eof);
        for _ in 0..60 {
            result = r.read_event();
            if result.is_err() {
                break;
            }
        }
        assert_eq!(result.err().map(|e| e.code), Some(ErrorCode::LimitExceeded));
    }

    #[test]
    fn garbage_never_panics() {
        let mut s = 99u32;
        let alphabet: Vec<char> = "<>/?!-[]&;=\"' abxml:#0".chars().collect();
        for len in 0..300usize {
            let junk: String = (0..len)
                .map(|_| {
                    s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    alphabet[(s >> 24) as usize % alphabet.len()]
                })
                .collect();
            let mut r = Reader::new(&junk, &Limits::default());
            for _ in 0..(len + 5) {
                match r.read_event() {
                    Ok(Event::Eof) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
    }
}
