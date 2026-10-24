//! Format descriptors (MASTER_PROMPT §9.4, §9.6).
use crate::options::OptionSchema;

/// Stable lowercase format identifier, e.g. `png`, `mkv`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FormatId(pub &'static str);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Image,
    Audio,
    Video,
    Document,
    Pdf,
    Sheet,
    Subtitle,
    Font,
    Archive,
    Text,
}

/// Who wrote the code behind a route. Lower is preferred by the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ownership {
    /// The whole route is our code.
    Own,
    /// Our code plus a vetted infrastructure component.
    Partial,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub read: bool,
    pub write: bool,
    pub animation: bool,
    pub alpha: bool,
    pub metadata: bool,
    pub multi_page: bool,
    pub streams: bool,
}

/// A magic-byte rule with a confidence score used by detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sniffer {
    pub offset: usize,
    pub magic: &'static [u8],
    /// 0..=100, higher wins.
    pub confidence: u8,
}

impl Sniffer {
    pub fn matches(&self, data: &[u8]) -> bool {
        let end = self.offset.checked_add(self.magic.len());
        end.and_then(|e| data.get(self.offset..e))
            .is_some_and(|s| s == self.magic)
    }
}

#[derive(Debug, Clone)]
pub struct FormatDescriptor {
    pub id: FormatId,
    pub name: &'static str,
    pub category: Category,
    pub extensions: &'static [&'static str],
    pub mime: &'static [&'static str],
    pub sniffers: Vec<Sniffer>,
    pub capabilities: Capabilities,
    pub writer_options: OptionSchema,
    /// Options that shape how this format is read (a page, a password, a resolution); they
    /// are accepted whatever the target is.
    pub reader_options: OptionSchema,
    pub ownership: Ownership,
    pub spec: &'static str,
}

impl FormatDescriptor {
    /// Best sniffer confidence for `data`, if any rule matches.
    pub fn sniff(&self, data: &[u8]) -> Option<u8> {
        self.sniffers
            .iter()
            .filter(|s| s.matches(data))
            .map(|s| s.confidence)
            .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffer_is_bounds_safe() {
        let s = Sniffer {
            offset: 4,
            magic: b"ftyp",
            confidence: 90,
        };
        assert!(s.matches(b"\0\0\0\x18ftypmp42"));
        assert!(!s.matches(b"\0\0\0"));
        let huge = Sniffer {
            offset: usize::MAX,
            magic: b"x",
            confidence: 1,
        };
        assert!(!huge.matches(b"x"));
    }
}
