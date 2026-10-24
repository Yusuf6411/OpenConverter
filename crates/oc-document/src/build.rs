//! Shared assembly of flat paragraphs into lists, code blocks and quotes.
use crate::model::{Block, ListItem};

pub struct ListLevel {
    pub num: String,
    pub ordered: bool,
    pub start: u32,
    pub items: Vec<ListItem>,
}

#[derive(Default)]
pub struct Builder {
    pub out: Vec<Block>,
    pub lists: Vec<ListLevel>,
    pub code: Option<String>,
    pub quote: Vec<Block>,
}

impl Builder {
    pub fn close_lists(&mut self) {
        while let Some(top) = self.lists.pop() {
            let block = Block::List {
                ordered: top.ordered,
                start: top.start,
                tight: true,
                items: top.items,
            };
            match self.lists.last_mut() {
                Some(parent) => {
                    if parent.items.is_empty() {
                        parent.items.push(ListItem {
                            checked: None,
                            blocks: vec![],
                        });
                    }
                    if let Some(last) = parent.items.last_mut() {
                        last.blocks.push(block);
                    }
                }
                None => self.out.push(block),
            }
        }
    }

    pub fn flush_code(&mut self) {
        if let Some(c) = self.code.take() {
            self.out.push(Block::Code {
                info: String::new(),
                text: c,
            });
        }
    }

    pub fn flush_quote(&mut self) {
        if !self.quote.is_empty() {
            let q = std::mem::take(&mut self.quote);
            self.out.push(Block::Quote(q));
        }
    }

    pub fn flush_all(&mut self) {
        self.close_lists();
        self.flush_code();
        self.flush_quote();
    }

    pub fn push(&mut self, b: Block) {
        self.flush_all();
        self.out.push(b);
    }

    pub fn finish(mut self) -> Vec<Block> {
        self.flush_all();
        self.out
    }
}

impl Builder {
    /// Add a list item at nesting depth `ilvl` of the list identified by `key`.
    pub fn list_item(&mut self, ilvl: usize, key: &str, ordered: bool, start: u32, item: ListItem) {
        self.flush_code();
        self.flush_quote();
        while self.lists.len() > ilvl + 1 {
            self.close_top();
        }
        if let Some(cur) = self.lists.get(ilvl) {
            if cur.num != key || cur.ordered != ordered {
                while self.lists.len() > ilvl {
                    self.close_top();
                }
            }
        }
        while self.lists.len() <= ilvl {
            self.lists.push(ListLevel {
                num: key.to_string(),
                ordered,
                start,
                items: Vec::new(),
            });
            if self.lists.len() <= ilvl {
                // a skipped level gets an empty parent item
                if let Some(l) = self.lists.last_mut() {
                    l.items.push(ListItem {
                        checked: None,
                        blocks: vec![],
                    });
                }
            }
        }
        if let Some(l) = self.lists.get_mut(ilvl) {
            l.items.push(item);
        }
    }

    fn close_top(&mut self) {
        if let Some(top) = self.lists.pop() {
            let block = Block::List {
                ordered: top.ordered,
                start: top.start,
                tight: true,
                items: top.items,
            };
            match self.lists.last_mut() {
                Some(parent) => {
                    if parent.items.is_empty() {
                        parent.items.push(ListItem {
                            checked: None,
                            blocks: vec![],
                        });
                    }
                    if let Some(last) = parent.items.last_mut() {
                        last.blocks.push(block);
                    }
                }
                None => self.out.push(block),
            }
        }
    }
}
