//! Structured notes attached to every conversion result (MASTER_PROMPT §9.5).

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    /// Data or fidelity was lost.
    Loss,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub severity: Severity,
    /// Stable code such as `alpha_flattened` or `metadata_dropped`.
    pub code: &'static str,
    pub message: String,
    pub detail: Option<String>,
}

impl Note {
    pub fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            detail: None,
        }
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Ordered collection of notes; identical (code, detail) pairs are recorded once.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notes(Vec<Note>);

impl Notes {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, note: Note) {
        if !self
            .0
            .iter()
            .any(|n| n.code == note.code && n.detail == note.detail)
        {
            self.0.push(note);
        }
    }
    pub fn extend(&mut self, other: Notes) {
        for n in other.0 {
            self.push(n);
        }
    }
    pub fn iter(&self) -> impl Iterator<Item = &Note> {
        self.0.iter()
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn has_loss(&self) -> bool {
        self.0.iter().any(|n| n.severity == Severity::Loss)
    }
    pub fn worst(&self) -> Option<Severity> {
        self.0.iter().map(|n| n.severity).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_are_collapsed() {
        let mut n = Notes::new();
        n.push(Note::new(
            Severity::Loss,
            "alpha_flattened",
            "alpha removed",
        ));
        n.push(Note::new(
            Severity::Loss,
            "alpha_flattened",
            "alpha removed",
        ));
        n.push(Note::new(Severity::Info, "metadata_dropped", "x").with_detail("exif"));
        n.push(Note::new(Severity::Info, "metadata_dropped", "x").with_detail("xmp"));
        assert_eq!(n.len(), 3);
        assert!(n.has_loss());
        assert_eq!(n.worst(), Some(Severity::Loss));
    }
}
