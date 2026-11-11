//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural failures translated into each language's public dictionary error.

#[derive(Debug, thiserror::Error)]
pub(crate) enum DictionaryError {
    #[error(transparent)]
    Input(#[from] uqa_core::image_io::ImageIoError),
    #[error("invalid provenance manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("unsupported bundle version {0}")]
    Version(u32),
    #[error("dictionary checksum mismatch in section {0}")]
    Checksum(u32),
    #[error("invalid {section} at byte {offset}: {reason}")]
    Invalid {
        section: &'static str,
        offset: usize,
        reason: &'static str,
    },
    #[error("dictionary needs {required} {resource}, exceeding limit {limit}")]
    Limit {
        resource: &'static str,
        required: usize,
        limit: usize,
    },
    #[error("dictionary allocation failed: {0}")]
    Allocation(#[from] std::collections::TryReserveError),
    #[error("invalid UTF-8 in dictionary: {0}")]
    Utf8(#[from] std::str::Utf8Error),
    #[error("invalid UTF-8 in dictionary: {diagnostic}", diagnostic = utf8_diagnostic(*offset, error))]
    Utf8Offset {
        offset: usize,
        #[source]
        error: std::str::Utf8Error,
    },
}

pub(crate) fn utf8_diagnostic(offset: usize, error: &std::str::Utf8Error) -> Utf8Diagnostic {
    Utf8Diagnostic {
        valid_up_to: offset + error.valid_up_to(),
        error_len: error.error_len(),
    }
}

pub(crate) struct Utf8Diagnostic {
    valid_up_to: usize,
    error_len: Option<usize>,
}

impl std::fmt::Display for Utf8Diagnostic {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.error_len {
            Some(length) => write!(
                output,
                "invalid utf-8 sequence of {length} bytes from index {}",
                self.valid_up_to
            ),
            None => write!(
                output,
                "incomplete utf-8 byte sequence from index {}",
                self.valid_up_to
            ),
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

pub(crate) fn check_limit(
    resource: &'static str,
    required: usize,
    limit: usize,
) -> super::DictionaryResult<()> {
    if required > limit {
        return Err(DictionaryError::Limit {
            resource,
            required,
            limit,
        });
    }
    Ok(())
}
