use crate::{escape_attr, escape_text};

/// Streaming XML writer producing well-formed output.
#[derive(Debug, Default)]
pub struct Writer {
    out: String,
    open: Vec<String>,
    /// True while a start tag is still unclosed (`<a` waiting for `>` or `/>`).
    in_tag: bool,
}

impl Writer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn declaration(&mut self) {
        self.out
            .push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    }

    fn close_tag(&mut self) {
        if self.in_tag {
            self.out.push('>');
            self.in_tag = false;
        }
    }

    pub fn start(&mut self, name: &str) {
        self.close_tag();
        self.out.push('<');
        self.out.push_str(name);
        self.open.push(name.to_string());
        self.in_tag = true;
    }

    /// Add an attribute to the element just started. Ignored if no start tag is open.
    pub fn attr(&mut self, name: &str, value: &str) {
        if self.in_tag {
            self.out.push(' ');
            self.out.push_str(name);
            self.out.push_str("=\"");
            self.out.push_str(&escape_attr(value));
            self.out.push('"');
        }
    }

    pub fn text(&mut self, s: &str) {
        self.close_tag();
        self.out.push_str(&escape_text(s));
    }

    pub fn end(&mut self) {
        if let Some(name) = self.open.pop() {
            if self.in_tag {
                self.out.push_str("/>");
                self.in_tag = false;
            } else {
                self.out.push_str("</");
                self.out.push_str(&name);
                self.out.push('>');
            }
        }
    }

    /// Close every open element and return the document.
    pub fn finish(mut self) -> String {
        while !self.open.is_empty() {
            self.end();
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Event, Reader};
    use oc_core::Limits;

    #[test]
    fn writes_escaped_wellformed_output_that_reads_back() {
        let mut w = Writer::new();
        w.declaration();
        w.start("root");
        w.attr("q", "a\"b<c>&\n");
        w.start("empty");
        w.end();
        w.start("t");
        w.text("1 < 2 & 3 > 2");
        let doc = w.finish();
        assert!(doc.contains("<empty/>"));
        let mut r = Reader::new(&doc, &Limits::default());
        let mut texts = Vec::new();
        let mut qval = None;
        loop {
            match r.read_event() {
                Ok(Event::Text(t)) => texts.push(t),
                Ok(Event::Start { name, attrs, .. }) if name == "root" => {
                    qval = attrs.first().map(|a| a.value.clone());
                }
                Ok(Event::Eof) => break,
                Ok(_) => {}
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(texts, vec!["1 < 2 & 3 > 2".to_string()]);
        assert_eq!(qval.as_deref(), Some("a\"b<c>& "));
    }
}
