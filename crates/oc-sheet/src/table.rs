use oc_core::Limits;

/// Rows of text cells with an optional header row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub header: Option<Vec<String>>,
    pub rows: Vec<Vec<String>>,
    /// Set by readers that turned non-text scalars (numbers, booleans, null) into text.
    pub coerced_scalars: bool,
}

impl Table {
    pub fn new(header: Option<Vec<String>>, rows: Vec<Vec<String>>) -> Self {
        Self {
            header,
            rows,
            coerced_scalars: false,
        }
    }

    /// Number of columns: the longest row, header included.
    pub fn width(&self) -> usize {
        self.rows
            .iter()
            .map(Vec::len)
            .chain(self.header.as_ref().map(Vec::len))
            .max()
            .unwrap_or(0)
    }

    /// True if rows (or the header) have different lengths.
    pub fn is_ragged(&self) -> bool {
        let w = self.width();
        self.rows.iter().any(|r| r.len() != w) || self.header.as_ref().is_some_and(|h| h.len() != w)
    }

    /// Fails if padding every row to the table width would exceed the cell limit. A few long
    /// rows among many short ones can otherwise expand a small file enormously.
    pub fn check_padded_size(&self, limits: &TableLimits) -> oc_core::Result<()> {
        let rows = self.rows.len() as u64 + u64::from(self.header.is_some());
        if (self.width() as u64).saturating_mul(rows) > limits.max_cells {
            return Err(oc_core::Error::limit(
                "table",
                "padding short rows would make the table larger than the allowed maximum",
            ));
        }
        Ok(())
    }

    /// A row extended with empty cells to the table width.
    pub fn padded(&self, row: &[String]) -> Vec<String> {
        let mut r = row.to_vec();
        r.resize(self.width().max(row.len()), String::new());
        r
    }
}

/// Size caps for parsing, derived from the shared [`Limits`].
#[derive(Debug, Clone, Copy)]
pub struct TableLimits {
    pub max_cells: u64,
    pub max_field_bytes: u64,
    pub max_depth: u32,
}

impl Default for TableLimits {
    fn default() -> Self {
        Self::from(&Limits::default())
    }
}

impl From<&Limits> for TableLimits {
    fn from(l: &Limits) -> Self {
        Self {
            max_cells: l.max_entries,
            max_field_bytes: l.max_alloc.min(1 << 28),
            max_depth: l.max_depth,
        }
    }
}
