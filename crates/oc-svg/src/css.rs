//! A small CSS subset: type, class, id and attribute selectors with descendant, child and
//! adjacent-sibling combinators, and plain declarations.
use crate::dom::Doc;

#[derive(Debug, Clone)]
pub struct Decl {
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub decls: Vec<Decl>,
}

#[derive(Debug, Clone, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, Option<(char, String)>)>,
    first_child: bool,
}

#[derive(Debug, Clone)]
pub struct Selector {
    /// Compounds left to right with the combinator that precedes each (the first has none).
    parts: Vec<(char, Compound)>,
    pub specificity: u32,
}

pub fn parse_decls(s: &str) -> Vec<Decl> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quote: Option<char> = None;
    let mut items: Vec<String> = Vec::new();
    for c in s.chars() {
        match (c, quote) {
            ('"' | '\'', None) => {
                quote = Some(c);
                cur.push(c);
            }
            (q, Some(open)) if q == open => {
                quote = None;
                cur.push(c);
            }
            ('(', None) => {
                depth += 1;
                cur.push(c);
            }
            (')', None) => {
                depth -= 1;
                cur.push(c);
            }
            (';', None) if depth <= 0 => items.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    items.push(cur);
    for it in items {
        let Some((k, v)) = it.split_once(':') else {
            continue;
        };
        let mut v = v.trim().to_string();
        let important = v.to_ascii_lowercase().ends_with("!important");
        if important {
            v.truncate(v.len() - "!important".len());
            v = v.trim().to_string();
        }
        let k = k.trim().to_ascii_lowercase();
        if !k.is_empty() && !v.is_empty() {
            out.push(Decl {
                name: k,
                value: v,
                important,
            });
        }
    }
    out
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("/*") {
        out.push_str(rest.get(..i).unwrap_or(""));
        match rest.get(i + 2..).and_then(|r| r.find("*/")) {
            Some(j) => rest = rest.get(i + 2 + j + 2..).unwrap_or(""),
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

pub fn parse_sheet(src: &str) -> Vec<Rule> {
    let text = strip_comments(src);
    let mut rules = Vec::new();
    let mut rest = text.as_str();
    while let Some(open) = rest.find('{') {
        let head = rest.get(..open).unwrap_or("").trim();
        // find the matching close brace
        let mut depth = 1;
        let mut end = None;
        for (i, c) in rest.get(open + 1..).unwrap_or("").char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + 1 + i);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let body = rest.get(open + 1..end).unwrap_or("");
        rest = rest.get(end + 1..).unwrap_or("");
        if head.starts_with('@') {
            continue;
        }
        let selectors: Vec<Selector> = head.split(',').filter_map(parse_selector).collect();
        if !selectors.is_empty() {
            rules.push(Rule {
                selectors,
                decls: parse_decls(body),
            });
        }
    }
    rules
}

fn parse_selector(s: &str) -> Option<Selector> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut parts: Vec<(char, Compound)> = Vec::new();
    let mut comb = ' ';
    let mut cur = Compound::default();
    let mut has = false;
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0usize;
    let mut spec = 0u32;
    let ident = |i: &mut usize| -> String {
        let st = *i;
        while *i < chars.len()
            && (chars[*i].is_alphanumeric() || chars[*i] == '-' || chars[*i] == '_')
        {
            *i += 1;
        }
        chars[st..*i].iter().collect()
    };
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '>' | '+' | '~' => {
                if has {
                    parts.push((comb, std::mem::take(&mut cur)));
                    has = false;
                    comb = ' ';
                }
                if c != ' ' && c != '\t' && c != '\n' {
                    comb = c;
                }
                i += 1;
            }
            '*' => {
                has = true;
                i += 1;
            }
            '.' => {
                i += 1;
                cur.classes.push(ident(&mut i));
                spec += 10;
                has = true;
            }
            '#' => {
                i += 1;
                cur.id = Some(ident(&mut i));
                spec += 100;
                has = true;
            }
            '[' => {
                let close = chars[i..].iter().position(|c| *c == ']')? + i;
                let inner: String = chars[i + 1..close].iter().collect();
                i = close + 1;
                spec += 10;
                has = true;
                let (name, op) = match inner.find(['=', '~', '^', '$', '*', '|']) {
                    Some(p) => {
                        let name = inner[..p].trim().to_string();
                        let rest = &inner[p..];
                        let (op, val) = if let Some(v) = rest.strip_prefix('=') {
                            ('=', v)
                        } else {
                            (rest.chars().next()?, rest.get(2..)?)
                        };
                        let val = val
                            .trim()
                            .trim_matches(|c| c == '"' || c == '\'')
                            .to_string();
                        (name, Some((op, val)))
                    }
                    None => (inner.trim().to_string(), None),
                };
                cur.attrs.push((name, op));
            }
            ':' => {
                i += 1;
                let name = ident(&mut i);
                if name == "first-child" {
                    cur.first_child = true;
                    spec += 10;
                    has = true;
                } else if name == "root" {
                    cur.attrs.push(("#root".to_string(), None));
                    has = true;
                } else {
                    return None; // pseudo-classes and elements we cannot evaluate never match
                }
            }
            c if c.is_alphabetic() || c == '_' => {
                cur.tag = Some(ident(&mut i));
                spec += 1;
                has = true;
            }
            _ => return None,
        }
    }
    if has {
        parts.push((comb, cur));
    }
    if parts.is_empty() {
        return None;
    }
    Some(Selector {
        parts,
        specificity: spec,
    })
}

fn compound_matches(doc: &Doc, i: usize, c: &Compound) -> bool {
    let Some(n) = doc.node(i) else { return false };
    if n.tag == "#text" {
        return false;
    }
    if let Some(t) = &c.tag {
        if &n.tag != t {
            return false;
        }
    }
    if let Some(id) = &c.id {
        if n.attrs.get("id") != Some(id) {
            return false;
        }
    }
    if !c.classes.is_empty() {
        let cls = n.attrs.get("class").map_or("", String::as_str);
        let have: Vec<&str> = cls.split_whitespace().collect();
        if !c.classes.iter().all(|k| have.contains(&k.as_str())) {
            return false;
        }
    }
    for (name, op) in &c.attrs {
        if name == "#root" {
            if n.parent.is_some() {
                return false;
            }
            continue;
        }
        let Some(v) = n.attrs.get(name) else {
            return false;
        };
        if let Some((o, want)) = op {
            let ok = match o {
                '=' => v == want,
                '~' => v.split_whitespace().any(|w| w == want),
                '^' => v.starts_with(want.as_str()),
                '$' => v.ends_with(want.as_str()),
                '*' => v.contains(want.as_str()),
                '|' => v == want || v.starts_with(&format!("{want}-")),
                _ => false,
            };
            if !ok {
                return false;
            }
        }
    }
    if c.first_child {
        let Some(p) = n.parent.and_then(|p| doc.node(p)) else {
            return true;
        };
        let first = p
            .children
            .iter()
            .find(|&&k| doc.node(k).is_some_and(|x| x.tag != "#text"));
        if first != Some(&i) {
            return false;
        }
    }
    true
}

pub fn matches(doc: &Doc, i: usize, sel: &Selector) -> bool {
    fn go(doc: &Doc, i: usize, parts: &[(char, Compound)]) -> bool {
        let Some(((comb, comp), before)) = parts.split_last() else {
            return true;
        };
        if !compound_matches(doc, i, comp) {
            return false;
        }
        if before.is_empty() {
            return true;
        }
        let node = match doc.node(i) {
            Some(n) => n,
            None => return false,
        };
        match comb {
            '>' => node.parent.is_some_and(|p| go(doc, p, before)),
            '+' | '~' => {
                let Some(p) = node.parent.and_then(|p| doc.node(p)) else {
                    return false;
                };
                let elems: Vec<usize> = p
                    .children
                    .iter()
                    .copied()
                    .filter(|&k| doc.node(k).is_some_and(|x| x.tag != "#text"))
                    .collect();
                let Some(pos) = elems.iter().position(|&k| k == i) else {
                    return false;
                };
                if *comb == '+' {
                    pos > 0 && go(doc, elems[pos - 1], before)
                } else {
                    elems[..pos].iter().any(|&k| go(doc, k, before))
                }
            }
            _ => {
                let mut cur = node.parent;
                while let Some(p) = cur {
                    if go(doc, p, before) {
                        return true;
                    }
                    cur = doc.node(p).and_then(|n| n.parent);
                }
                false
            }
        }
    }
    go(doc, i, &sel.parts)
}
