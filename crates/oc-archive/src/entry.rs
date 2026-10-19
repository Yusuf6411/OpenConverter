//! The in-memory archive model every reader produces and every writer consumes.
use oc_core::{Error, Limits, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
}

#[derive(Debug, Clone)]
pub struct Entry {
    /// Forward-slash separated, no trailing slash, exactly as stored in the archive.
    pub path: String,
    pub kind: EntryKind,
    /// File contents (empty for directories; symlinks keep their target in `link`).
    pub data: Vec<u8>,
    /// Unix permission bits (lower 12 bits).
    pub mode: u32,
    /// Seconds since the Unix epoch.
    pub mtime: Option<i64>,
    pub link: Option<String>,
    /// Whether the member was stored encrypted (informational).
    pub encrypted: bool,
}

impl Entry {
    pub fn file(path: impl Into<String>, data: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::File,
            data,
            mode: 0o644,
            mtime: None,
            link: None,
            encrypted: false,
        }
    }
    pub fn dir(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::Dir,
            data: Vec::new(),
            mode: 0o755,
            mtime: None,
            link: None,
            encrypted: false,
        }
    }
    pub fn symlink(path: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            kind: EntryKind::Symlink,
            data: Vec::new(),
            mode: 0o777,
            mtime: None,
            link: Some(target.into()),
            encrypted: false,
        }
    }
    pub fn size(&self) -> u64 {
        self.data.len() as u64
    }
}

#[derive(Debug, Clone, Default)]
pub struct Archive {
    pub entries: Vec<Entry>,
    pub comment: Option<String>,
}

impl Archive {
    pub fn total_size(&self) -> u64 {
        self.entries.iter().map(Entry::size).sum()
    }
}

/// Running guard used by readers to enforce entry-count and output-size limits.
pub struct Budget<'a> {
    limits: &'a Limits,
    entries: u64,
    bytes: u64,
}

impl<'a> Budget<'a> {
    pub fn new(limits: &'a Limits) -> Self {
        Self {
            limits,
            entries: 0,
            bytes: 0,
        }
    }
    pub fn limits(&self) -> &'a Limits {
        self.limits
    }
    pub fn entry(&mut self) -> Result<()> {
        self.entries += 1;
        self.limits.check_entries(self.entries)
    }
    /// Bytes still allowed for further output.
    pub fn remaining(&self) -> u64 {
        self.limits.max_output.saturating_sub(self.bytes)
    }
    pub fn add_output(&mut self, n: u64) -> Result<()> {
        self.bytes = self.bytes.saturating_add(n);
        if self.bytes > self.limits.max_output {
            return Err(Error::limit(
                "max_output",
                "archive expands beyond the output size limit",
            ));
        }
        Ok(())
    }
    /// Limits for decoding one member of `declared` bytes from `compressed` input bytes.
    pub fn member_limits(&self, declared: u64) -> Limits {
        let mut l = self.limits.clone();
        l.max_output = declared.min(self.remaining());
        l
    }
}
