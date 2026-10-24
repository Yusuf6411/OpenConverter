//! XML to an arena DOM, with the CSS cascade applied to each element.
use oc_core::{Error, Limits, Result};
use oc_xml::{Event, Reader};
use std::collections::HashMap;

pub const SVG_NS: &str = "http://www.w3.org/2000/svg";

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub tag: String,
    pub attrs: HashMap<String, String>,
    /// Presentation attributes, then style sheet rules, then the `style` attribute.
    pub style: HashMap<String, String>,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    /// Character data of a `#text` node.
    pub text: String,
}

#[derive(Debug, Default)]
pub struct Doc {
    pub nodes: Vec<Node>,
    pub ids: HashMap<String, usize>,
    pub root: usize,
}

impl Doc {
    pub fn node(&self, i: usize) -> Option<&Node> {
        self.nodes.get(i)
    }
    pub fn attr(&self, i: usize, name: &str) -> Option<&str> {
        self.nodes.get(i)?.attrs.get(name).map(String::as_str)
    }
    pub fn by_id(&self, id: &str) -> Option<usize> {
        self.ids.get(id).copied()
    }
    /// The element a `href`/`xlink:href` of `#id` points at.
    pub fn href_target(&self, i: usize) -> Option<usize> {
        let h = self.attr(i, "href")?.trim();
        self.by_id(h.strip_prefix('#')?)
    }
}

const PRESENTATION: &[&str] = &[
    "alignment-baseline",
    "baseline-shift",
    "clip",
    "clip-path",
    "clip-rule",
    "color",
    "color-interpolation",
    "color-interpolation-filters",
    "cursor",
    "direction",
    "display",
    "dominant-baseline",
    "fill",
    "fill-opacity",
    "fill-rule",
    "filter",
    "flood-color",
    "flood-opacity",
    "font-family",
    "font-size",
    "font-size-adjust",
    "font-stretch",
    "font-style",
    "font-variant",
    "font-weight",
    "image-rendering",
    "isolation",
    "letter-spacing",
    "lighting-color",
    "marker-end",
    "marker-mid",
    "marker-start",
    "mask",
    "mix-blend-mode",
    "opacity",
    "overflow",
    "paint-order",
    "pointer-events",
    "shape-rendering",
    "stop-color",
    "stop-opacity",
    "stroke",
    "stroke-dasharray",
    "stroke-dashoffset",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-opacity",
    "stroke-width",
    "text-anchor",
    "text-decoration",
    "text-rendering",
    "unicode-bidi",
    "visibility",
    "word-spacing",
    "writing-mode",
    "transform",
    "transform-origin",
];

pub fn parse(text: &str, limits: &Limits) -> Result<Doc> {
    let mut doc = Doc::default();
    let mut reader = Reader::new(text, limits);
    let mut stack: Vec<usize> = Vec::new();
    // Depth of elements in a foreign namespace that are being skipped.
    let mut skip = 0usize;
    let mut sheets: Vec<String> = Vec::new();
    let mut in_style = false;
    let mut root: Option<usize> = None;
    loop {
        match reader.read_event()? {
            Event::Start { name, ns, attrs } => {
                if skip > 0 {
                    skip += 1;
                    continue;
                }
                if ns.as_deref().is_some_and(|n| n != SVG_NS) {
                    skip = 1;
                    continue;
                }
                if doc.nodes.len() as u64 >= limits.max_entries.min(2_000_000) {
                    return Err(Error::limit("max_entries", "too many SVG elements"));
                }
                let local = name.rsplit(':').next().unwrap_or(&name).to_string();
                let mut node = Node {
                    tag: local.clone(),
                    parent: stack.last().copied(),
                    ..Node::default()
                };
                for a in attrs {
                    let key = if a.name == "xlink:href" || a.name.ends_with(":href") {
                        "href".to_string()
                    } else if a.name.contains(':') && !a.name.starts_with("xml:") {
                        continue;
                    } else {
                        a.name.clone()
                    };
                    node.attrs.insert(key, a.value);
                }
                for p in PRESENTATION {
                    if let Some(v) = node.attrs.get(*p) {
                        node.style.insert((*p).to_string(), v.trim().to_string());
                    }
                }
                let id = doc.nodes.len();
                if let Some(i) = node.attrs.get("id") {
                    doc.ids.entry(i.clone()).or_insert(id);
                }
                if let Some(&p) = stack.last() {
                    if let Some(parent) = doc.nodes.get_mut(p) {
                        parent.children.push(id);
                    }
                } else if root.is_none() {
                    root = Some(id);
                }
                in_style = local == "style"
                    && node
                        .attrs
                        .get("type")
                        .is_none_or(|t| t.is_empty() || t == "text/css");
                doc.nodes.push(node);
                stack.push(id);
                if in_style {
                    sheets.push(String::new());
                }
            }
            Event::End { .. } => {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                stack.pop();
                in_style = false;
            }
            Event::Text(t) | Event::CData(t) => {
                if skip > 0 {
                    continue;
                }
                if in_style {
                    if let Some(s) = sheets.last_mut() {
                        s.push_str(&t);
                    }
                    continue;
                }
                if let Some(&p) = stack.last() {
                    let id = doc.nodes.len();
                    doc.nodes.push(Node {
                        tag: "#text".to_string(),
                        text: t,
                        parent: Some(p),
                        ..Node::default()
                    });
                    if let Some(parent) = doc.nodes.get_mut(p) {
                        parent.children.push(id);
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    doc.root = root.ok_or_else(|| Error::corrupt("SVG has no root element"))?;
    if doc.nodes.get(doc.root).map(|n| n.tag.as_str()) != Some("svg") {
        return Err(Error::corrupt("the root element is not <svg>"));
    }
    let rules: Vec<crate::css::Rule> = sheets
        .iter()
        .flat_map(|s| crate::css::parse_sheet(s))
        .collect();
    apply_cascade(&mut doc, &rules);
    Ok(doc)
}

fn apply_cascade(doc: &mut Doc, rules: &[crate::css::Rule]) {
    let n = doc.nodes.len();
    for i in 0..n {
        if doc.nodes[i].tag == "#text" {
            continue;
        }
        let mut matched: Vec<(u32, usize, &crate::css::Decl)> = Vec::new();
        for (ri, r) in rules.iter().enumerate() {
            for sel in &r.selectors {
                if crate::css::matches(doc, i, sel) {
                    for d in &r.decls {
                        matched.push((sel.specificity, ri, d));
                    }
                }
            }
        }
        matched.sort_by_key(|(s, r, _)| (*s, *r));
        let mut important: Vec<(String, String)> = Vec::new();
        let mut set: Vec<(String, String)> = Vec::new();
        for (_, _, d) in matched {
            if d.important {
                important.push((d.name.clone(), d.value.clone()));
            } else {
                set.push((d.name.clone(), d.value.clone()));
            }
        }
        if let Some(inline) = doc.nodes[i].attrs.get("style").cloned() {
            for d in crate::css::parse_decls(&inline) {
                set.push((d.name, d.value));
            }
        }
        let node = &mut doc.nodes[i];
        for (k, v) in set.into_iter().chain(important) {
            node.style.insert(k, v);
        }
    }
}
