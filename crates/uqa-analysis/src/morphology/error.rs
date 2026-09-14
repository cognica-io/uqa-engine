//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Structural failures translated into each language's public dictionary error.

#[derive(Debug, thiserror::Error)]
pub(crate) enum DictionaryError {
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
