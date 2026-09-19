//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage failure context for mutation execution.
use uqa_core::DocId;
use uqa_sql::SQLError;

pub use uqa_sql::catalog::errors::dml_storage_error;

/// Keep cancellation, memory limits and serialization conflicts typed when identity reservations cross the storage boundary.
pub fn identifier_storage_error(
    action: &str,
    error: &uqa_storage::StorageBackendError,
) -> SQLError {
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(source) = cause {
        match source.downcast_ref::<uqa_storage::StorageBackendError>() {
            Some(uqa_storage::StorageBackendError::Cancelled(cancelled)) => {
                return SQLError::Cancelled(*cancelled);
            }
            Some(uqa_storage::StorageBackendError::Memory(_)) => {
                return SQLError::Routine {
                    sqlstate: "53200".into(),
                    message: format!("{action}: {error}"),
                };
            }
            _ => {}
        }
        if matches!(
            source.downcast_ref::<uqa_storage::mvcc::VersionError>(),
            Some(
                uqa_storage::mvcc::VersionError::WriteConflict { .. }
                    | uqa_storage::mvcc::VersionError::ReadConflict { .. }
            )
        ) {
            return SQLError::Routine {
                sqlstate: "40001".into(),
                message: format!("{action} could not serialize storage changes: {error}"),
            };
        }
        cause = source.source();
    }
    uqa_sql::catalog::errors::storage_error(action, error)
}

pub fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_storage::{mvcc::VersionError, StorageBackendError};

    #[test]
    fn identifier_diagnostics_survive_provider_error_wrappers() {
        for wrapped in [false, true] {
            let errors = [
                (StorageBackendError::from(uqa_core::QueryCancelled), "57014"),
                (
                    StorageBackendError::from(uqa_core::memory::MemoryError::SizeOverflow),
                    "53200",
                ),
                (
                    VersionError::WriteConflict {
                        mutation: 0,
                        expected: None,
                        actual: None,
                    }
                    .into_storage_error(),
                    "40001",
                ),
                (
                    VersionError::ReadConflict {
                        dependency: 0,
                        expected: None,
                        actual: None,
                    }
                    .into_storage_error(),
                    "40001",
                ),
                (
                    StorageBackendError::backend(
                        "fixture",
                        SQLError::Routine {
                            sqlstate: "22003".into(),
                            message: "identity out of range".into(),
                        },
                    ),
                    "22003",
                ),
            ];
            for (error, state) in errors {
                let error = if wrapped {
                    StorageBackendError::backend("wrapped", error)
                } else {
                    error
                };
                let result = identifier_storage_error("reserve identity", &error);
                assert_eq!(result.sqlstate(), Some(state), "{result}");
                if state == "57014" {
                    assert!(matches!(result, SQLError::Cancelled(_)));
                }
            }
        }
        let error = identifier_storage_error(
            "observe identity",
            &StorageBackendError::Other("unavailable".into()),
        );
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert!(error.to_string().contains("observe identity"));
    }
}
