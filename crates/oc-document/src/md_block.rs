//! CommonMark block structure, following the reference algorithm (open containers are matched
//! line by line, then new blocks are tried), with GitHub tables and task lists. Inline content
//! is parsed afterwards by `md_inline`.
use crate::md_inline::{self, RefMap};
use crate::model::{Align, Block, Cell, Inline, ListItem};

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Document,
    Quote,
    List {
        ordered: bool,
        start: u32,
        delim: u8,
        bullet: u8,
        tight: bool,
    },
    Item {
        marker_offset: usize,
        padding: usize,
    },
    Paragraph,
    Heading(u8),
    Code {
        fenced: bool,
        fence_char: u8,
        fence_len: usize,
        fence_offset: usize,
    },
    Html(u8),
    Rule,
    Table {
        aligns: Vec<Align>,
        header: String,
    },
}

struct Node {
    kind: Kind,
    parent: usize,
    children: Vec<usize>,
    content: String,
    info: String,
    open: bool,
    last_line_blank: bool,
    start_line: usize,
}

struct Parser {
    nodes: Vec<Node>,
    tip: usize,
    oldtip: usize,
    last_matched: usize,
    refs: RefMap,
    line: Vec<u8>,
    line_no: usize,
    offset: usize,
    column: usize,
    first_nonspace: usize,
    first_nonspace_column: usize,
    indent: usize,
    blank: bool,
    partially_consumed_tab: bool,
    all_matched: bool,
    tables: bool,
}

/// Containers nested deeper than this are read as text.
const MAX_DEPTH: usize = 64;

fn is_space_or_tab(b: u8) -> bool {
    b == b' ' || b == b'\t'
}

fn is_line_end(b: u8) -> bool {
    b == b'\n' || b == b'\r' || b == 0
}

impl Parser {
    fn new(tables: bool) -> Self {
        let root = Node {
            kind: Kind::Document,
            parent: 0,
            children: vec![],
            content: String::new(),
            info: String::new(),
            open: true,
            last_line_blank: false,
            start_line: 0,
        };
        Self {
            nodes: vec![root],
            tip: 0,
            oldtip: 0,
            last_matched: 0,
            refs: RefMap::default(),
            line: vec![],
            line_no: 0,
            offset: 0,
            column: 0,
            first_nonspace: 0,
            first_nonspace_column: 0,
            indent: 0,
            blank: false,
            partially_consumed_tab: false,
            all_matched: true,
            tables,
        }
    }

    /// Nesting depth of a node (containers only), used to stop pathological input.
    fn depth(&self, mut id: usize) -> usize {
        let mut d = 0;
        while id != 0 {
            d += 1;
            id = self.nodes[id].parent;
        }
        d
    }

    fn peek(&self, i: usize) -> u8 {
        self.line.get(i).copied().unwrap_or(0)
    }

    fn find_first_nonspace(&mut self) {
        self.first_nonspace = self.offset;
        self.first_nonspace_column = self.column;
        let mut chars_to_tab = 4 - (self.column % 4);
        loop {
            match self.peek(self.first_nonspace) {
                b' ' => {
                    self.first_nonspace += 1;
                    self.first_nonspace_column += 1;
                    chars_to_tab -= 1;
                    if chars_to_tab == 0 {
                        chars_to_tab = 4;
                    }
                }
                b'\t' => {
                    self.first_nonspace += 1;
                    self.first_nonspace_column += chars_to_tab;
                    chars_to_tab = 4;
                }
                _ => break,
            }
        }
        self.indent = self.first_nonspace_column - self.column;
        self.blank = is_line_end(self.peek(self.first_nonspace));
    }

    fn advance_offset(&mut self, mut count: usize, columns: bool) {
        while count > 0 {
            let c = self.peek(self.offset);
            if c == 0 {
                break;
            }
            if c == b'\t' {
                let chars_to_tab = 4 - (self.column % 4);
                if columns {
                    self.partially_consumed_tab = chars_to_tab > count;
                    let n = chars_to_tab.min(count);
                    self.column += n;
                    if !self.partially_consumed_tab {
                        self.offset += 1;
                    }
                    count -= n;
                } else {
                    self.partially_consumed_tab = false;
                    self.column += chars_to_tab;
                    self.offset += 1;
                    count -= 1;
                }
            } else {
                self.partially_consumed_tab = false;
                self.offset += 1;
                self.column += 1;
                count -= 1;
            }
        }
    }

    fn can_contain(&self, parent: usize, child: &Kind) -> bool {
        match &self.nodes[parent].kind {
            Kind::Document | Kind::Quote | Kind::Item { .. } => !matches!(child, Kind::Item { .. }),
            Kind::List { .. } => matches!(child, Kind::Item { .. }),
            _ => false,
        }
    }

    fn add_child(&mut self, mut parent: usize, kind: Kind) -> usize {
        while !self.can_contain(parent, &kind) {
            let up = self.nodes[parent].parent;
            self.finalize(parent);
            parent = up;
        }
        let id = self.nodes.len();
        self.nodes.push(Node {
            kind,
            parent,
            children: vec![],
            content: String::new(),
            info: String::new(),
            open: true,
            last_line_blank: false,
            start_line: self.line_no,
        });
        self.nodes[parent].children.push(id);
        id
    }

    fn accepts_lines(&self, id: usize) -> bool {
        matches!(
            self.nodes[id].kind,
            Kind::Paragraph
                | Kind::Heading(_)
                | Kind::Code { .. }
                | Kind::Html(_)
                | Kind::Table { .. }
        )
    }

    /// Pull reference definitions off the front of a paragraph's text.
    fn take_definitions(&mut self, content: &str) -> String {
        let mut s = content.to_string();
        while s.starts_with('[') {
            let n = md_inline::parse_reference_definition(&s, &mut self.refs);
            if n == 0 {
                break;
            }
            s = s[n..].to_string();
        }
        s
    }

    fn finalize(&mut self, id: usize) {
        if !self.nodes[id].open {
            return;
        }
        self.nodes[id].open = false;
        if self.tip == id {
            self.tip = self.nodes[id].parent;
        }
        match self.nodes[id].kind.clone() {
            Kind::Paragraph => {
                let c = std::mem::take(&mut self.nodes[id].content);
                let rest = self.take_definitions(&c);
                self.nodes[id].content = rest;
            }
            Kind::Code { fenced, .. } => {
                let c = std::mem::take(&mut self.nodes[id].content);
                if fenced {
                    let (first, rest) = match c.find('\n') {
                        Some(p) => (c[..p].to_string(), c[p + 1..].to_string()),
                        None => (c.clone(), String::new()),
                    };
                    self.nodes[id].info = md_inline::unescape_string(first.trim());
                    self.nodes[id].content = rest;
                } else {
                    let mut lines: Vec<&str> = c.split('\n').collect();
                    // the split leaves one empty tail after the final newline
                    if lines.last() == Some(&"") {
                        lines.pop();
                    }
                    while lines
                        .last()
                        .is_some_and(|l| l.chars().all(|c| c == ' ' || c == '\t'))
                    {
                        lines.pop();
                    }
                    let mut s = lines.join("\n");
                    if !lines.is_empty() {
                        s.push('\n');
                    }
                    self.nodes[id].content = s;
                }
            }
            Kind::List { .. } => {
                let items = self.nodes[id].children.clone();
                let mut tight = true;
                'outer: for (n, &it) in items.iter().enumerate() {
                    if self.ends_with_blank_line(it) && n + 1 < items.len() {
                        tight = false;
                        break;
                    }
                    let subs = self.nodes[it].children.clone();
                    for (m, &s) in subs.iter().enumerate() {
                        if (n + 1 < items.len() || m + 1 < subs.len())
                            && self.ends_with_blank_line(s)
                        {
                            tight = false;
                            break 'outer;
                        }
                    }
                }
                if let Kind::List { tight: t, .. } = &mut self.nodes[id].kind {
                    *t = tight;
                }
            }
            _ => {}
        }
    }

    fn ends_with_blank_line(&self, mut id: usize) -> bool {
        loop {
            if self.nodes[id].last_line_blank {
                return true;
            }
            match &self.nodes[id].kind {
                Kind::List { .. } | Kind::Item { .. } => match self.nodes[id].children.last() {
                    Some(&c) => id = c,
                    None => return false,
                },
                _ => return false,
            }
        }
    }

    fn add_line(&mut self, to: usize) {
        if self.partially_consumed_tab {
            self.offset += 1;
            let chars_to_tab = 4 - (self.column % 4);
            for _ in 0..chars_to_tab {
                self.nodes[to].content.push(' ');
            }
        }
        let rest =
            String::from_utf8_lossy(&self.line[self.offset.min(self.line.len())..]).to_string();
        self.nodes[to].content.push_str(&rest);
    }

    fn close_unmatched(&mut self) {
        let mut t = self.oldtip;
        while t != self.last_matched {
            let p = self.nodes[t].parent;
            self.finalize(t);
            t = p;
        }
        self.oldtip = self.last_matched;
    }

    /// 0 = the container continues on this line, 1 = it does not, 2 = line fully consumed.
    fn continues(&mut self, id: usize) -> u8 {
        match self.nodes[id].kind.clone() {
            Kind::Quote => {
                if self.indent <= 3 && self.peek(self.first_nonspace) == b'>' {
                    self.advance_offset(self.first_nonspace + 1 - self.offset, false);
                    if is_space_or_tab(self.peek(self.offset)) {
                        self.advance_offset(1, true);
                    }
                    0
                } else {
                    1
                }
            }
            Kind::Item {
                marker_offset,
                padding,
            } => {
                if self.indent >= marker_offset + padding {
                    self.advance_offset(marker_offset + padding, true);
                    0
                } else if self.blank && !self.nodes[id].children.is_empty() {
                    self.advance_offset(self.first_nonspace - self.offset, false);
                    0
                } else {
                    1
                }
            }
            Kind::List { .. } | Kind::Document => 0,
            Kind::Heading(_) | Kind::Rule => 1,
            Kind::Code {
                fenced,
                fence_char,
                fence_len,
                fence_offset,
            } => {
                if !fenced {
                    if self.indent >= 4 {
                        self.advance_offset(4, true);
                        0
                    } else if self.blank {
                        self.advance_offset(self.first_nonspace - self.offset, false);
                        0
                    } else {
                        1
                    }
                } else {
                    let mut n = 0;
                    let mut i = self.first_nonspace;
                    if self.indent <= 3 && self.peek(i) == fence_char {
                        while self.peek(i) == fence_char {
                            n += 1;
                            i += 1;
                        }
                        while is_space_or_tab(self.peek(i)) {
                            i += 1;
                        }
                        if n >= fence_len && is_line_end(self.peek(i)) {
                            self.finalize(id);
                            return 2;
                        }
                    }
                    let mut skip = fence_offset;
                    while skip > 0 && is_space_or_tab(self.peek(self.offset)) {
                        self.advance_offset(1, true);
                        skip -= 1;
                    }
                    0
                }
            }
            Kind::Html(t) => {
                if self.blank && (t == 6 || t == 7) {
                    1
                } else {
                    0
                }
            }
            Kind::Paragraph => {
                if self.blank {
                    1
                } else {
                    0
                }
            }
            Kind::Table { .. } => {
                if self.blank {
                    1
                } else {
                    0
                }
            }
        }
    }

    fn parse_line(&mut self, text: &str) {
        self.line_no += 1;
        self.line = text.as_bytes().to_vec();
        self.line.push(b'\n');
        self.offset = 0;
        self.column = 0;
        self.partially_consumed_tab = false;
        self.blank = false;
        self.oldtip = self.tip;
        // match open containers
        let mut container = 0usize;
        self.all_matched = true;
        loop {
            let Some(&last) = self.nodes[container].children.last() else {
                break;
            };
            if !self.nodes[last].open {
                break;
            }
            let prev = container;
            container = last;
            self.find_first_nonspace();
            match self.continues(container) {
                0 => {}
                1 => {
                    container = prev;
                    self.all_matched = false;
                    break;
                }
                _ => return,
            }
        }
        self.last_matched = container;
        let maybe_lazy = matches!(self.nodes[self.tip].kind, Kind::Paragraph);
        let mut maybe_lazy_flag = maybe_lazy;
        // try new block starts
        while !matches!(
            self.nodes[container].kind,
            Kind::Code { .. } | Kind::Html(_)
        ) {
            self.find_first_nonspace();
            let indented = self.indent >= 4;
            let c = self.peek(self.first_nonspace);
            let cont_is_para = matches!(self.nodes[container].kind, Kind::Paragraph);
            if !indented && c == b'>' && self.depth(container) < MAX_DEPTH {
                self.advance_offset(self.first_nonspace + 1 - self.offset, false);
                if is_space_or_tab(self.peek(self.offset)) {
                    self.advance_offset(1, true);
                }
                self.close_unmatched();
                container = self.add_child(container, Kind::Quote);
            } else if !indented && c == b'#' && self.scan_atx().is_some() {
                let level = self.scan_atx().unwrap_or(1);
                self.advance_offset(self.first_nonspace + level as usize - self.offset, false);
                self.close_unmatched();
                container = self.add_child(container, Kind::Heading(level));
                // content is the rest of the line, minus an optional closing sequence
                let rest = String::from_utf8_lossy(&self.line[self.offset..]).to_string();
                let t = chop_closing_hashes(rest.trim_end_matches('\n'));
                self.nodes[container].content = format!("{t}\n");
                self.advance_offset(self.line.len() - self.offset, false);
            } else if !indented && (c == b'`' || c == b'~') && self.scan_fence().is_some() {
                let (n, ch) = self.scan_fence().unwrap_or((3, b'`'));
                self.close_unmatched();
                container = self.add_child(
                    container,
                    Kind::Code {
                        fenced: true,
                        fence_char: ch,
                        fence_len: n,
                        fence_offset: self.first_nonspace - self.offset,
                    },
                );
                self.advance_offset(self.first_nonspace + n - self.offset, false);
            } else if !indented
                && c == b'<'
                && html_start(&self.line[self.first_nonspace..], cont_is_para).is_some()
            {
                let t = html_start(&self.line[self.first_nonspace..], cont_is_para).unwrap_or(6);
                self.close_unmatched();
                container = self.add_child(container, Kind::Html(t));
            } else if !indented
                && cont_is_para
                && (c == b'=' || c == b'-')
                && self.scan_setext().is_some()
                && !self.nodes[container].content.is_empty()
                && self.setext_applies(container)
            {
                let level = self.scan_setext().unwrap_or(1);
                let content = self.nodes[container].content.clone();
                let rest = self.take_definitions(&content);
                self.close_unmatched();
                self.nodes[container].kind = Kind::Heading(level);
                self.nodes[container].content = rest;
                self.advance_offset(self.line.len() - self.offset, false);
            } else if !indented && !(cont_is_para && !self.all_matched) && self.is_thematic_break()
            {
                self.close_unmatched();
                container = self.add_child(container, Kind::Rule);
                self.advance_offset(self.line.len() - self.offset, false);
            } else if self.tables
                && !indented
                && cont_is_para
                && self.all_matched
                && self.try_table(&mut container)
            {
                // consumed the delimiter row
            } else if self.indent < 4
                && self.depth(container) < MAX_DEPTH
                && self.try_list_item(&mut container, cont_is_para)
            {
            } else if indented && !maybe_lazy_flag && !self.blank {
                self.advance_offset(4, true);
                self.close_unmatched();
                container = self.add_child(
                    container,
                    Kind::Code {
                        fenced: false,
                        fence_char: 0,
                        fence_len: 0,
                        fence_offset: 0,
                    },
                );
            } else {
                break;
            }
            if self.accepts_lines(container) {
                break;
            }
            maybe_lazy_flag = false;
        }
        // add the rest of the line
        self.find_first_nonspace();
        let tip_is_para = matches!(self.nodes[self.tip].kind, Kind::Paragraph);
        if !self.all_matched
            && !self.blank
            && tip_is_para
            && self.tip != container
            && self.oldtip_is_lazy(container)
        {
            // lazy paragraph continuation
            self.advance_offset(self.first_nonspace - self.offset, false);
            let t = self.tip;
            self.add_line(t);
        } else {
            self.close_unmatched();
            if self.blank {
                if let Some(&last) = self.nodes[container].children.last() {
                    self.nodes[last].last_line_blank = true;
                }
            }
            let ct = self.nodes[container].kind.clone();
            let last_line_blank = self.blank
                && !matches!(ct, Kind::Quote | Kind::Heading(_) | Kind::Rule)
                && !matches!(&ct, Kind::Code { fenced: true, .. })
                && !(matches!(ct, Kind::Item { .. })
                    && self.nodes[container].children.is_empty()
                    && self.nodes[container].start_line == self.line_no);
            self.nodes[container].last_line_blank = last_line_blank;
            let mut p = container;
            while p != 0 {
                p = self.nodes[p].parent;
                self.nodes[p].last_line_blank = false;
            }
            if self.accepts_lines(container) {
                self.add_line(container);
                if let Kind::Html(t) = ct {
                    if (1..=5).contains(&t)
                        && html_end(t, &self.line[self.first_nonspace.min(self.line.len())..])
                    {
                        self.finalize(container);
                    }
                }
            } else if self.offset < self.line.len() && !self.blank {
                let c = self.add_child(container, Kind::Paragraph);
                self.advance_offset(self.first_nonspace - self.offset, false);
                self.add_line(c);
                container = c;
            }
            self.tip = container;
        }
        // The tip is the deepest open node.
        if self.nodes[self.tip].open || self.tip == 0 {
        } else {
            self.tip = self.nodes[self.tip].parent;
        }
    }

    fn oldtip_is_lazy(&self, _container: usize) -> bool {
        true
    }

    fn setext_applies(&self, container: usize) -> bool {
        // A paragraph holding only reference definitions is not a heading.
        let mut probe = RefMap::default();
        let mut s = self.nodes[container].content.as_str();
        while s.starts_with('[') {
            let n = md_inline::parse_reference_definition(s, &mut probe);
            if n == 0 {
                break;
            }
            s = &s[n..];
        }
        !s.trim().is_empty()
    }

    fn scan_atx(&self) -> Option<u8> {
        let mut i = self.first_nonspace;
        let mut level = 0u8;
        while self.peek(i) == b'#' {
            level += 1;
            i += 1;
            if level > 6 {
                return None;
            }
        }
        if level == 0 {
            return None;
        }
        let c = self.peek(i);
        if is_space_or_tab(c) || is_line_end(c) {
            Some(level)
        } else {
            None
        }
    }

    fn scan_fence(&self) -> Option<(usize, u8)> {
        let ch = self.peek(self.first_nonspace);
        let mut i = self.first_nonspace;
        let mut n = 0;
        while self.peek(i) == ch {
            n += 1;
            i += 1;
        }
        if n < 3 {
            return None;
        }
        if ch == b'`' {
            // the info string may not contain backticks
            let rest = &self.line[i..];
            if rest.contains(&b'`') {
                return None;
            }
        }
        Some((n, ch))
    }

    fn scan_setext(&self) -> Option<u8> {
        let ch = self.peek(self.first_nonspace);
        let mut i = self.first_nonspace;
        while self.peek(i) == ch {
            i += 1;
        }
        while is_space_or_tab(self.peek(i)) {
            i += 1;
        }
        if is_line_end(self.peek(i)) {
            Some(if ch == b'=' { 1 } else { 2 })
        } else {
            None
        }
    }

    fn is_thematic_break(&self) -> bool {
        let c = self.peek(self.first_nonspace);
        if c != b'*' && c != b'_' && c != b'-' {
            return false;
        }
        let mut n = 0;
        for &b in &self.line[self.first_nonspace..] {
            if b == c {
                n += 1;
            } else if !(b == b' ' || b == b'\t' || b == b'\n' || b == b'\r') {
                return false;
            }
        }
        n >= 3
    }

    fn try_list_item(&mut self, container: &mut usize, interrupts_paragraph: bool) -> bool {
        let mut i = self.first_nonspace;
        let c = self.peek(i);
        let (ordered, start, delim, bullet);
        if c == b'*' || c == b'+' || c == b'-' {
            i += 1;
            let nc = self.peek(i);
            if !(is_space_or_tab(nc) || is_line_end(nc)) {
                return false;
            }
            if interrupts_paragraph {
                let mut j = i;
                while is_space_or_tab(self.peek(j)) {
                    j += 1;
                }
                if is_line_end(self.peek(j)) {
                    return false;
                }
            }
            ordered = false;
            start = 1;
            delim = 0;
            bullet = c;
        } else if c.is_ascii_digit() {
            let mut n = 0u32;
            let mut digits = 0;
            while digits < 10 && self.peek(i).is_ascii_digit() {
                n = n
                    .saturating_mul(10)
                    .saturating_add(u32::from(self.peek(i) - b'0'));
                digits += 1;
                i += 1;
            }
            if digits > 9 {
                return false;
            }
            if interrupts_paragraph && n != 1 {
                return false;
            }
            let d = self.peek(i);
            if d != b'.' && d != b')' {
                return false;
            }
            i += 1;
            let nc = self.peek(i);
            if !(is_space_or_tab(nc) || is_line_end(nc)) {
                return false;
            }
            if interrupts_paragraph {
                let mut j = i;
                while is_space_or_tab(self.peek(j)) {
                    j += 1;
                }
                if is_line_end(self.peek(j)) {
                    return false;
                }
            }
            ordered = true;
            start = n;
            delim = d;
            bullet = 0;
        } else {
            return false;
        }
        let matched = i - self.first_nonspace;
        let marker_offset = self.indent;
        self.advance_offset(self.first_nonspace + matched - self.offset, false);
        let save_offset = self.offset;
        let save_column = self.column;
        let save_tab = self.partially_consumed_tab;
        while self.column - save_column <= 5 && is_space_or_tab(self.peek(self.offset)) {
            self.advance_offset(1, true);
        }
        let spaces = self.column - save_column;
        let padding;
        if spaces >= 5 || spaces < 1 || is_line_end(self.peek(self.offset)) {
            padding = matched + 1;
            self.offset = save_offset;
            self.column = save_column;
            self.partially_consumed_tab = save_tab;
            if spaces > 0 {
                self.advance_offset(1, true);
            }
        } else {
            padding = matched + spaces;
        }
        self.close_unmatched();
        let same = matches!(&self.nodes[*container].kind,
            Kind::List { ordered: o, delim: d, bullet: b, .. } if *o == ordered && *d == delim && *b == bullet);
        if !same {
            *container = self.add_child(
                *container,
                Kind::List {
                    ordered,
                    start,
                    delim,
                    bullet,
                    tight: true,
                },
            );
        }
        *container = self.add_child(
            *container,
            Kind::Item {
                marker_offset,
                padding,
            },
        );
        true
    }

    /// A GitHub table starts when a delimiter row follows a paragraph line with as many cells.
    fn try_table(&mut self, container: &mut usize) -> bool {
        let line = String::from_utf8_lossy(&self.line[self.first_nonspace..]).to_string();
        let Some(aligns) = parse_delimiter_row(line.trim_end_matches('\n')) else {
            return false;
        };
        let id = *container;
        let content = self.nodes[id].content.clone();
        let body = content.trim_end_matches('\n');
        let (before, header) = match body.rfind('\n') {
            Some(p) => (&body[..p], &body[p + 1..]),
            None => ("", body),
        };
        if split_row(header).len() != aligns.len() {
            return false;
        }
        let before = before.to_string();
        let header = header.to_string();
        self.close_unmatched();
        let parent = self.nodes[id].parent;
        if before.is_empty() {
            // the paragraph becomes the table
            self.nodes[id].kind = Kind::Table { aligns, header };
            self.nodes[id].content = String::new();
        } else {
            self.nodes[id].content = format!("{before}\n");
            self.finalize(id);
            let t = self.add_child(parent, Kind::Table { aligns, header });
            *container = t;
        }
        self.advance_offset(self.line.len() - self.offset, false);
        true
    }
}

/// Remove an optional closing run of `#` from an ATX heading's text.
fn chop_closing_hashes(s: &str) -> String {
    let t = s.trim_matches(|c| c == ' ' || c == '\t');
    let b = t.as_bytes();
    let mut end = b.len();
    while end > 0 && b[end - 1] == b'#' {
        end -= 1;
    }
    if end == b.len() {
        return t.to_string();
    }
    if end == 0 {
        return String::new();
    }
    if is_space_or_tab(b[end - 1]) {
        return t[..end].trim_end_matches([' ', '\t']).to_string();
    }
    t.to_string()
}

fn html_start(rest: &[u8], in_paragraph: bool) -> Option<u8> {
    if rest.first() != Some(&b'<') {
        return None;
    }
    let s = String::from_utf8_lossy(rest);
    let lower = s.to_ascii_lowercase();
    let after = |name: &str| -> bool {
        lower.strip_prefix(name).is_some_and(|r| {
            r.chars()
                .next()
                .is_none_or(|c| c == ' ' || c == '\t' || c == '>' || c == '\n' || c == '\r')
        })
    };
    if after("<script") || after("<pre") || after("<style") || after("<textarea") {
        return Some(1);
    }
    if lower.starts_with("<!--") {
        return Some(2);
    }
    if lower.starts_with("<?") {
        return Some(3);
    }
    if lower.starts_with("<![cdata[") {
        return Some(5);
    }
    if lower.starts_with("<!")
        && lower
            .as_bytes()
            .get(2)
            .is_some_and(|b| b.is_ascii_alphabetic())
    {
        return Some(4);
    }
    // block-level tag names
    let body = lower.strip_prefix("</").or_else(|| lower.strip_prefix('<'));
    if let Some(b) = body {
        let name: String = b
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        let tail = &b[name.len()..];
        if BLOCK_TAGS.contains(&name.as_str())
            && (tail.is_empty()
                || tail.starts_with([' ', '\t', '\n', '\r', '>'])
                || tail.starts_with("/>"))
        {
            return Some(6);
        }
    }
    if !in_paragraph {
        if let Some(n) = md_inline::scan_html_tag(&s, true) {
            let tail = &s[n..];
            if tail
                .chars()
                .all(|c| c == ' ' || c == '\t' || c == '\n' || c == '\r')
            {
                let name: String = lower
                    .trim_start_matches("</")
                    .trim_start_matches('<')
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect();
                if !matches!(name.as_str(), "script" | "style" | "pre" | "textarea")
                    || lower.starts_with("</")
                {
                    return Some(7);
                }
            }
        }
    }
    None
}

const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "base",
    "basefont",
    "blockquote",
    "body",
    "caption",
    "center",
    "col",
    "colgroup",
    "dd",
    "details",
    "dialog",
    "dir",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "frame",
    "frameset",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "header",
    "hr",
    "html",
    "iframe",
    "legend",
    "li",
    "link",
    "main",
    "menu",
    "menuitem",
    "nav",
    "noframes",
    "ol",
    "optgroup",
    "option",
    "p",
    "param",
    "search",
    "section",
    "summary",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "title",
    "tr",
    "track",
    "ul",
];

fn html_end(kind: u8, rest: &[u8]) -> bool {
    let s = String::from_utf8_lossy(rest).to_ascii_lowercase();
    match kind {
        1 => ["</script>", "</pre>", "</style>", "</textarea>"]
            .iter()
            .any(|t| s.contains(t)),
        2 => s.contains("-->"),
        3 => s.contains("?>"),
        4 => s.contains('>'),
        5 => s.contains("]]>"),
        _ => false,
    }
}

/// `| a | b |` style rows: split on unescaped pipes, dropping an empty first and last cell.
pub fn split_row(row: &str) -> Vec<String> {
    let t = row.trim();
    let mut cells = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    let mut in_code = false;
    let _ = &mut in_code;
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('\\');
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    cells.push(cur);
    if t.starts_with('|') && cells.first().is_some_and(|c| c.trim().is_empty()) {
        cells.remove(0);
    }
    if t.ends_with('|') && !t.ends_with("\\|") && cells.last().is_some_and(|c| c.trim().is_empty())
    {
        cells.pop();
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

fn parse_delimiter_row(row: &str) -> Option<Vec<Align>> {
    if !row.contains('-') {
        return None;
    }
    let cells = split_row(row);
    if cells.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for c in &cells {
        let left = c.starts_with(':');
        let right = c.ends_with(':') && c.len() > 1;
        let core = c.trim_start_matches(':').trim_end_matches(':');
        if core.is_empty() || !core.chars().all(|ch| ch == '-') {
            return None;
        }
        out.push(match (left, right) {
            (true, true) => Align::Center,
            (true, false) => Align::Left,
            (false, true) => Align::Right,
            _ => Align::None,
        });
    }
    // a lone delimiter without pipes is ambiguous with a setext underline or rule
    if !row.contains('|') {
        return None;
    }
    Some(out)
}

pub struct Parsed {
    pub blocks: Vec<Block>,
}

pub fn parse(text: &str, tables: bool) -> Vec<Block> {
    let normalized = text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\0', "\u{FFFD}");
    let mut p = Parser::new(tables);
    let text = normalized.strip_prefix('\u{FEFF}').unwrap_or(&normalized);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for l in lines {
        p.parse_line(l);
    }
    // close everything
    let mut t = p.tip;
    loop {
        p.finalize(t);
        if t == 0 {
            break;
        }
        t = p.nodes[t].parent;
    }
    p.finalize(0);
    let root_children = p.nodes[0].children.clone();
    let refs = std::mem::take(&mut p.refs);
    convert_children(&p, &root_children, &refs, tables)
}

fn convert_children(p: &Parser, ids: &[usize], refs: &RefMap, tables: bool) -> Vec<Block> {
    let mut out = Vec::new();
    for &id in ids {
        let n = &p.nodes[id];
        match &n.kind {
            Kind::Document | Kind::Item { .. } => {}
            Kind::Quote => out.push(Block::Quote(convert_children(p, &n.children, refs, tables))),
            Kind::List {
                ordered,
                start,
                tight,
                ..
            } => {
                let mut items = Vec::new();
                for &it in &n.children {
                    let mut blocks = convert_children(p, &p.nodes[it].children, refs, tables);
                    let mut checked = None;
                    if tables {
                        if let Some(Block::Paragraph(inl)) = blocks.first_mut() {
                            checked = take_task_marker(inl);
                        }
                    }
                    items.push(ListItem { checked, blocks });
                }
                out.push(Block::List {
                    ordered: *ordered,
                    start: *start,
                    tight: *tight,
                    items,
                });
            }
            Kind::Paragraph => {
                let text = n.content.trim_end_matches(['\n', ' ', '\t']);
                if text.trim().is_empty() {
                    continue;
                }
                out.push(Block::Paragraph(md_inline::parse_inlines(
                    text.trim_start(),
                    refs,
                    tables,
                )));
            }
            Kind::Heading(l) => {
                let text = n.content.trim_end_matches('\n').trim();
                out.push(Block::Heading {
                    level: *l,
                    content: md_inline::parse_inlines(text, refs, tables),
                });
            }
            Kind::Code { .. } => out.push(Block::Code {
                info: n.info.clone(),
                text: n.content.clone(),
            }),
            Kind::Html(_) => {
                let mut s = n.content.clone();
                while s.ends_with('\n') {
                    s.pop();
                }
                out.push(Block::Html(s));
            }
            Kind::Rule => out.push(Block::Rule),
            Kind::Table { aligns, header } => {
                let mut rows = Vec::new();
                let cell = |s: &str, header: bool, i: usize| Cell {
                    header,
                    align: aligns.get(i).cloned().unwrap_or_default(),
                    content: md_inline::parse_inlines(&s.replace("\\|", "|"), refs, tables),
                };
                let mut r: Vec<Cell> = split_row(header)
                    .iter()
                    .enumerate()
                    .map(|(i, c)| cell(c, true, i))
                    .collect();
                r.truncate(aligns.len());
                rows.push(r);
                for line in n.content.lines() {
                    if line.trim().is_empty() {
                        continue;
                    }
                    let mut cells = split_row(line);
                    cells.resize(aligns.len(), String::new());
                    rows.push(
                        cells
                            .iter()
                            .enumerate()
                            .map(|(i, c)| cell(c, false, i))
                            .collect(),
                    );
                }
                out.push(Block::Table { rows });
            }
        }
    }
    out
}

fn take_task_marker(inl: &mut Vec<Inline>) -> Option<bool> {
    let Some(Inline::Text(t)) = inl.first_mut() else {
        return None;
    };
    let (checked, rest) = if let Some(r) = t.strip_prefix("[ ]") {
        (false, r)
    } else if let Some(r) = t.strip_prefix("[x]").or_else(|| t.strip_prefix("[X]")) {
        (true, r)
    } else {
        return None;
    };
    if !(rest.starts_with(' ') || rest.is_empty()) {
        return None;
    }
    let rest = rest.trim_start().to_string();
    if rest.is_empty() {
        inl.remove(0);
    } else {
        *t = rest;
    }
    Some(checked)
}
