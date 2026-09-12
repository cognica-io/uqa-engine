//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dictionary validation failures remain distinct from empty analysis results.

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error("invalid Nori {section} at byte {offset}: {reason}")]
    Invalid {
        section: &'static str,
        offset: usize,
        reason: &'static str,
    },
    #[error("unsupported Nori bundle version {0}")]
    Version(u32),
    #[error("Nori dictionary checksum mismatch in section {0}")]
    Checksum(u32),
    #[error("Nori dictionary needs {required} {resource}, exceeding limit {limit}")]
    Limit {
        resource: &'static str,
        required: usize,
        limit: usize,
    },
    #[error("Nori dictionary allocation failed: {0}")]
    Allocation(#[from] std::collections::TryReserveError),
    #[error("invalid UTF-8 in Nori dictionary: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid UTF-16 in Nori dictionary: {0}")]
    Utf16(#[from] std::string::FromUtf16Error),
    #[error("invalid Nori provenance manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[cfg(feature = "nori-tools")]
    #[error("Nori dictionary tool I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub type DictionaryResult<T> = Result<T, DictionaryError>;

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
    if required > limit {
        return Err(DictionaryError::Limit {
            resource,
            required,
            limit,
        });
    }
    Ok(())
}
