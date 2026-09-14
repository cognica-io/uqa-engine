//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt native storage and scoring failures to SQL errors.

use super::{SQLError, StorageBackendError};

pub(crate) fn storage_sql_error(action: &str, error: impl Into<StorageBackendError>) -> SQLError {
    let error = error.into();
    SQLError::Internal(format!("{action}: {error}"))
}

pub(super) fn scoring_sql_error(error: uqa_scoring::TextSearchError) -> SQLError {
    match error {
        uqa_scoring::TextSearchError::Parameters(error) => {
            SQLError::TypeMismatch(error.to_string())
        }
        error => SQLError::Internal(error.to_string()),
    }
}
