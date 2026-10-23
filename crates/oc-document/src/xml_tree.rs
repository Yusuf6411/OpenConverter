//! A small element tree on top of the pull parser, with names reduced to their local part
//! (the office formats are read by element name; prefixes differ between producers).
use oc_core::{Limits, Result};
use oc_xml::{Event, Reader};

#[derive(Debug, Clone, Default)]
pub struct El {
    /// Local name (no prefix).
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<Kid>,
}

#[derive(Debug, Clone)]
pub enum Kid {
    El(El),
    Text(String),
}

fn local(n: &str) -> &str {
    n.rsplit(':').next().unwrap_or(n)
}

impl El {
    /// Attribute by local name (`w:val` is `val`).
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    pub fn children(&self) -> impl Iterator<Item = &El> {
        self.kids.iter().filter_map(|k| match k {
            Kid::El(e) => Some(e),
            Kid::Text(_) => None,
        })
    }

    pub fn child(&self, name: &str) -> Option<&El> {
        self.children().find(|e| e.name == name)
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for k in &self.kids {
            match k {
                Kid::Text(t) => s.push_str(t),
                Kid::El(e) => s.push_str(&e.text()),
            }
        }
        s
    }
}

pub fn parse(xml: &str, limits: &Limits) -> Result<El> {
    let mut r = Reader::new(xml, limits);
    let mut stack: Vec<El> = Vec::new();
    let mut root: Option<El> = None;
    loop {
        match r.read_event()? {
            Event::Start { name, attrs, .. } => {
                stack.push(El {
                    name: local(&name).to_string(),
                    attrs: attrs
                        .into_iter()
                        .filter(|a| !a.name.starts_with("xmlns"))
                        .map(|a| (local(&a.name).to_string(), a.value))
                        .collect(),
                    kids: Vec::new(),
                });
            }
            Event::End { .. } => {
                if let Some(e) = stack.pop() {
                    match stack.last_mut() {
                        Some(p) => p.kids.push(Kid::El(e)),
                        None => root = Some(e),
                    }
                }
            }
            Event::Text(t) | Event::CData(t) => {
                if let Some(p) = stack.last_mut() {
                    p.kids.push(Kid::Text(t));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root.ok_or_else(|| oc_core::Error::new(oc_core::ErrorCode::CorruptFile, "empty XML document"))
}
