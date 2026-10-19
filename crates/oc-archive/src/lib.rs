//! Archive formats written for OpenConverter: TAR, ZIP, 7z, CPIO, AR, ISO 9660, CAB and
//! single-file compressed streams, all over one in-memory [`Archive`] model.
#![forbid(unsafe_code)]

pub mod ar;
pub mod cab;
pub mod cpio;
pub mod crypto;
pub mod entry;
pub mod format;
pub mod iso;
pub mod sevenz;
pub mod safety;
pub mod tar;
pub mod zip;

pub use entry::{Archive, Entry, EntryKind};
pub use format::{Codec, Format, WriteOpts};

use oc_core::{Limits, Progress};

/// Everything a reader or writer needs besides the bytes.
pub struct Context<'a> {
    pub limits: &'a Limits,
    pub progress: &'a dyn Progress,
    pub password: Option<&'a str>,
}
