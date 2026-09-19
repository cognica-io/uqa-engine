//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage failure context for mutation execution.
use uqa_core::DocId;
use uqa_sql::SQLError;

pub use uqa_sql::catalog::errors::dml_storage_error;

/// Keep cancellation and memory limits typed when identity reservations cross the storage boundary.
pub fn identifier_storage_error(
    action: &str,
    error: &uqa_storage::StorageBackendError,
) -> SQLError {
    match error {
        uqa_storage::StorageBackendError::Cancelled(error) => SQLError::Cancelled(*error),
        uqa_storage::StorageBackendError::Memory(_) => SQLError::Routine {
            sqlstate: "53200".into(),
            message: format!("{action}: {error}"),
        },
        error => uqa_sql::catalog::errors::storage_error(action, error),
    }
}

pub fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}
