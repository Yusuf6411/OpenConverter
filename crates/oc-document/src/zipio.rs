//! ZIP packages (DOCX, ODT, EPUB): reading all members and writing a list of members.
use oc_core::{Limits, NoProgress, Result};

pub struct Package {
    pub entries: Vec<(String, Vec<u8>)>,
}

impl Package {
    pub fn read(data: &[u8], limits: &Limits) -> Result<Package> {
        let cx = oc_archive::Context {
            limits,
            progress: &NoProgress,
            password: None,
            list_only: false,
        };
        let a = oc_archive::format::read(oc_archive::Format::Zip, data, &cx, None)?;
        Ok(Package {
            entries: a
                .entries
                .into_iter()
                .filter(|e| e.kind == oc_archive::EntryKind::File)
                .map(|e| (e.path, e.data))
                .collect(),
        })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, d)| d.as_slice())
    }

    pub fn get_ci(&self, name: &str) -> Option<&[u8]> {
        self.entries
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, d)| d.as_slice())
    }

    pub fn text(&self, name: &str) -> Option<String> {
        self.get(name).map(|d| String::from_utf8_lossy(d).into_owned())
    }
}

/// Write members in order; names listed in `stored` are not compressed (EPUB's `mimetype`).
pub fn write(entries: Vec<(String, Vec<u8>)>, limits: &Limits) -> Result<Vec<u8>> {
    let cx = oc_archive::Context {
        limits,
        progress: &NoProgress,
        password: None,
        list_only: false,
    };
    let list = entries
        .into_iter()
        .map(|(n, d)| oc_archive::Entry::file(n, d))
        .collect();
    oc_archive::format::write(
        oc_archive::Format::Zip,
        &oc_archive::Archive {
            entries: list,
            comment: None,
        },
        &oc_archive::format::WriteOpts::default(),
        &cx,
    )
}
