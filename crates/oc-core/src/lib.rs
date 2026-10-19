//! Core types shared by every OpenConverter crate: errors, notes, limits,
//! progress/cancellation, option schemas, format descriptors and the route planner.
#![forbid(unsafe_code)]

pub mod error;
pub mod format;
pub mod limits;
pub mod note;
pub mod options;
pub mod planner;
pub mod progress;

pub use error::{Error, ErrorCode, Result};
pub use format::{Capabilities, Category, FormatDescriptor, FormatId, Ownership};
pub use limits::Limits;
pub use note::{Note, Notes, Severity};
pub use progress::{CancelToken, NoProgress, Progress};
