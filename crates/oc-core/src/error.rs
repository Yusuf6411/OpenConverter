//! Typed errors with stable codes (MASTER_PROMPT §9.5).
use std::fmt;

/// Stable machine-readable error code used by the CLI exit codes, UI and API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    UnsupportedFormat,
    UnsupportedVariant,
    CodecNotInBuild,
    CorruptFile,
    TruncatedFile,
    LimitExceeded,
    EncryptedNeedPassword,
    WrongPassword,
    DrmProtected,
    Cancelled,
    Timeout,
    WorkerCrashed,
    IoError,
    OutputExists,
    InvalidOptions,
    InternalError,
}

impl ErrorCode {
    /// The stable snake_case identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnsupportedFormat => "unsupported_format",
            Self::UnsupportedVariant => "unsupported_variant",
            Self::CodecNotInBuild => "codec_not_in_build",
            Self::CorruptFile => "corrupt_file",
            Self::TruncatedFile => "truncated_file",
            Self::LimitExceeded => "limit_exceeded",
            Self::EncryptedNeedPassword => "encrypted_need_password",
            Self::WrongPassword => "wrong_password",
            Self::DrmProtected => "drm_protected",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::WorkerCrashed => "worker_crashed",
            Self::IoError => "io_error",
            Self::OutputExists => "output_exists",
            Self::InvalidOptions => "invalid_options",
            Self::InternalError => "internal_error",
        }
    }

    /// Process exit code for the CLI. 0 and 2 are reserved for success and usage errors.
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::UnsupportedFormat => 10,
            Self::UnsupportedVariant => 11,
            Self::CodecNotInBuild => 12,
            Self::CorruptFile => 13,
            Self::TruncatedFile => 14,
            Self::LimitExceeded => 15,
            Self::EncryptedNeedPassword => 16,
            Self::WrongPassword => 17,
            Self::DrmProtected => 18,
            Self::Cancelled => 19,
            Self::Timeout => 20,
            Self::WorkerCrashed => 21,
            Self::IoError => 22,
            Self::OutputExists => 23,
            Self::InvalidOptions => 24,
            Self::InternalError => 70,
        }
    }
}

/// An error with a stable code, a human message and optional detail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    /// Byte offset into the input where the problem was found, when known.
    pub offset: Option<u64>,
    /// Extra structured detail, e.g. the limit name or the option key.
    pub detail: Option<String>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            offset: None,
            detail: None,
        }
    }
    #[must_use]
    pub fn at(mut self, offset: u64) -> Self {
        self.offset = Some(offset);
        self
    }
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
    pub fn corrupt(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::CorruptFile, message)
    }
    pub fn truncated(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::TruncatedFile, message)
    }
    pub fn limit(which: &str, message: impl Into<String>) -> Self {
        Self::new(ErrorCode::LimitExceeded, message).with_detail(which)
    }
    pub fn cancelled() -> Self {
        Self::new(ErrorCode::Cancelled, "operation cancelled")
    }
    pub fn invalid_option(key: &str, why: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidOptions, why).with_detail(key)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)?;
        if let Some(o) = self.offset {
            write!(f, " (at byte {o})")?;
        }
        Ok(())
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::truncated("unexpected end of input")
        } else {
            Self::new(ErrorCode::IoError, e.kind().to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_includes_code_and_offset() {
        let e = Error::corrupt("bad header").at(12);
        assert_eq!(e.to_string(), "corrupt_file: bad header (at byte 12)");
    }

    #[test]
    fn exit_codes_are_unique() {
        use ErrorCode::*;
        let all = [
            UnsupportedFormat,
            UnsupportedVariant,
            CodecNotInBuild,
            CorruptFile,
            TruncatedFile,
            LimitExceeded,
            EncryptedNeedPassword,
            WrongPassword,
            DrmProtected,
            Cancelled,
            Timeout,
            WorkerCrashed,
            IoError,
            OutputExists,
            InvalidOptions,
            InternalError,
        ];
        let mut codes: Vec<u8> = all.iter().map(|c| c.exit_code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all.len());
    }

    #[test]
    fn eof_maps_to_truncated() {
        let e: Error = std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into();
        assert_eq!(e.code, ErrorCode::TruncatedFile);
    }
}
