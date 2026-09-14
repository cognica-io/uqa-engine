//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dictionary validation failures remain distinct from empty analysis results.

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error("invalid Kuromoji {section} at byte {offset}: {reason}")]
    Invalid {
        section: &'static str,
        offset: usize,
        reason: &'static str,
    },
    #[error("unsupported Kuromoji bundle version {0}")]
    Version(u32),
    #[error("Kuromoji dictionary checksum mismatch in section {0}")]
    Checksum(u32),
    #[error("Kuromoji dictionary needs {required} {resource}, exceeding limit {limit}")]
    Limit {
        resource: &'static str,
        required: usize,
        limit: usize,
    },
    #[error("Kuromoji dictionary allocation failed: {0}")]
    Allocation(#[from] std::collections::TryReserveError),
    #[error("invalid UTF-8 in Kuromoji dictionary: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid UTF-16 in Kuromoji dictionary: {0}")]
    Utf16(#[from] std::string::FromUtf16Error),
    #[error("invalid Kuromoji provenance manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[cfg(feature = "kuromoji-tools")]
    #[error("Kuromoji dictionary tool I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub type DictionaryResult<T> = Result<T, DictionaryError>;

impl From<crate::morphology::error::DictionaryError> for DictionaryError {
    fn from(error: crate::morphology::error::DictionaryError) -> Self {
        use crate::morphology::error::DictionaryError as Shared;

        match error {
            Shared::Manifest(error) => Self::Manifest(error),
            Shared::Version(version) => Self::Version(version),
            Shared::Checksum(section) => Self::Checksum(section),
            Shared::Invalid {
                section,
                offset,
                reason,
            } => Self::Invalid {
                section,
                offset,
                reason,
            },
            Shared::Limit {
                resource,
                required,
                limit,
            } => Self::Limit {
                resource,
                required,
                limit,
            },
            Shared::Allocation(error) => Self::Allocation(error),
            Shared::Utf8(error) => Self::Utf8(error),
        }
    }
}

pub(super) fn invalid(section: &'static str, reason: &'static str) -> DictionaryError {
    DictionaryError::Invalid {
        section,
        offset: 0,
        reason,
    }
}

pub(super) fn check_limit(
    resource: &'static str,
    required: usize,
    limit: usize,
) -> DictionaryResult<()> {
    crate::morphology::error::check_limit(resource, required, limit).map_err(Into::into)
}

#[cfg(feature = "kuromoji-tools")]
impl From<crate::morphology::neutral::Error> for DictionaryError {
    fn from(error: crate::morphology::neutral::Error) -> Self {
        use crate::morphology::neutral::Error;
        match error {
            Error::Dictionary(error) => error.into(),
            Error::Io(error) => Self::Io(error),
            Error::Utf16(error) => Self::Utf16(error),
            Error::Manifest(error) => Self::Manifest(error),
        }
    }
}
