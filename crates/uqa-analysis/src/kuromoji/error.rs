//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dictionary validation failures remain distinct from empty analysis results.

#[derive(Debug, thiserror::Error)]
pub enum DictionaryError {
    #[error(transparent)]
    Input(#[from] uqa_core::image_io::ImageIoError),
    #[error("Kuromoji dictionary resource is unavailable: {0}")]
    ResourceMissing(String),
    #[error("Kuromoji resource hash mismatch: expected {expected}, received {actual}")]
    ResourceHashMismatch {
        expected: super::ResourceHash,
        actual: super::ResourceHash,
    },
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
    /// Retained input reports the validated byte prefix separately from the bounded fragment error.
    #[error("invalid UTF-8 in Kuromoji dictionary: {diagnostic}", diagnostic = crate::morphology::error::utf8_diagnostic(*offset, error))]
    Utf8Offset {
        offset: usize,
        #[source]
        error: std::str::Utf8Error,
    },
    #[error("invalid UTF-16 in Kuromoji dictionary: {0}")]
    Utf16(#[from] std::string::FromUtf16Error),
    #[error("invalid Kuromoji provenance manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("invalid Kuromoji provenance manifest: {0}")]
    ManifestDiagnostic(#[from] uqa_core::json::JsonDiagnostic),
    #[cfg(feature = "kuromoji-tools")]
    #[error("Kuromoji dictionary tool I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

pub type DictionaryResult<T> = Result<T, DictionaryError>;

crate::morphology::error::dictionary_error_conversions!(
    DictionaryError,
    super::ResourceHash,
    "kuromoji-tools"
);

pub(super) fn invalid(section: &'static str, reason: &'static str) -> DictionaryError {
    crate::morphology::error::invalid(section, reason).into()
}

pub(super) fn check_limit(
    resource: &'static str,
    required: usize,
    limit: usize,
) -> DictionaryResult<()> {
    crate::morphology::error::check_limit(resource, required, limit).map_err(Into::into)
}
