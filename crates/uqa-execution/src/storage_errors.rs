//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed transaction and resource diagnostics across execution storage capabilities.

use uqa_sql::SQLError;

/// Keep cancellation, memory limits and serialization conflicts typed across storage reads, observations and mutations.
pub fn storage_error(action: &str, error: &uqa_storage::StorageBackendError) -> SQLError {
    use uqa_storage::mvcc::TransactionOutcome;
    match error.transaction_outcome() {
        Some(TransactionOutcome::Aborted(transaction)) => {
            return SQLError::Routine {
                sqlstate: "25000".into(),
                message: format!(
                    "{action} cannot commit transaction {transaction:?}: it was already aborted"
                ),
            };
        }
        Some(TransactionOutcome::Indeterminate(transaction)) => {
            return SQLError::Routine {
                sqlstate: "08007".into(),
                message: format!("transaction {transaction:?} requires commit resolution: {error}"),
            };
        }
        Some(TransactionOutcome::Committed(_)) => {
            return SQLError::Internal(format!("{action} failed after storage committed: {error}"));
        }
        None => {}
    }
    let mut cause: Option<&(dyn std::error::Error + 'static)> = Some(error);
    while let Some(source) = cause {
        let cancelled = match source.downcast_ref::<uqa_storage::StorageBackendError>() {
            Some(uqa_storage::StorageBackendError::Cancelled(cancelled)) => Some(cancelled),
            _ => source.downcast_ref::<uqa_core::QueryCancelled>(),
        };
        if let Some(cancelled) = cancelled {
            return SQLError::Cancelled(*cancelled);
        }
        if matches!(
            source.downcast_ref::<uqa_storage::StorageBackendError>(),
            Some(uqa_storage::StorageBackendError::Memory(_))
        ) || source.is::<uqa_core::memory::MemoryError>()
        {
            return SQLError::Routine {
                sqlstate: "53200".into(),
                message: format!("{action}: {error}"),
            };
        }
        if matches!(
            source.downcast_ref::<uqa_storage::mvcc::VersionError>(),
            Some(
                uqa_storage::mvcc::VersionError::WriteConflict { .. }
                    | uqa_storage::mvcc::VersionError::ReadConflict { .. }
                    | uqa_storage::mvcc::VersionError::SerializationConflict { .. }
            )
        ) {
            return SQLError::Routine {
                sqlstate: "40001".into(),
                message: format!("{action} could not serialize storage changes: {error}"),
            };
        }
        if matches!(
            source.downcast_ref::<uqa_graph::GraphStoreError>(),
            Some(uqa_graph::GraphStoreError::SerializationFailure(_))
        ) || matches!(
            source.downcast_ref::<uqa_graph::cypher::CypherError>(),
            Some(uqa_graph::cypher::CypherError::SerializationFailure(_))
        ) {
            return SQLError::Routine {
                sqlstate: "40001".into(),
                message: format!("{action}: {error}"),
            };
        }
        cause = source.source();
    }
    uqa_sql::catalog::errors::storage_error(action, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_storage::{mvcc::VersionError, StorageBackendError};

    #[test]
    fn graph_diagnostics_keep_completion_evidence_ahead_of_nested_conflicts() {
        use uqa_storage::mvcc::{CommitFailure, DatabaseId, StorageTransactionId};
        let transaction = StorageTransactionId::new(DatabaseId::from_bytes([1; 16]), 7).unwrap();
        for (error, state) in [
            (
                StorageBackendError::backend(
                    "receipt",
                    CommitFailure::Indeterminate {
                        transaction,
                        source: VersionError::WriteConflict {
                            mutation: 0,
                            expected: None,
                            actual: None,
                        }
                        .into_storage_error(),
                    },
                ),
                "08007",
            ),
            (
                VersionError::AlreadyAborted(transaction).into_storage_error(),
                "25000",
            ),
        ] {
            let cypher =
                uqa_graph::cypher::CypherError::from(uqa_graph::GraphStoreError::from(error));
            let error = StorageBackendError::backend("graph", cypher);
            assert_eq!(
                storage_error("complete graph", &error).sqlstate(),
                Some(state)
            );
        }
    }

    #[test]
    fn graph_and_cypher_keep_transaction_and_resource_diagnostics() {
        for cypher in [false, true] {
            for (error, state) in [
                (StorageBackendError::from(uqa_core::QueryCancelled), "57014"),
                (
                    StorageBackendError::backend("fixture", uqa_core::QueryCancelled),
                    "57014",
                ),
                (
                    StorageBackendError::from(uqa_core::memory::MemoryError::SizeOverflow),
                    "53200",
                ),
                (
                    StorageBackendError::backend(
                        "fixture",
                        uqa_core::memory::MemoryError::SizeOverflow,
                    ),
                    "53200",
                ),
                (
                    VersionError::WriteConflict {
                        mutation: 1,
                        expected: None,
                        actual: None,
                    }
                    .into_storage_error(),
                    "40001",
                ),
            ] {
                let graph = uqa_graph::GraphStoreError::from(StorageBackendError::backend(
                    "fixture", error,
                ));
                let error = if cypher {
                    StorageBackendError::backend(
                        "cypher",
                        uqa_graph::cypher::CypherError::from(graph),
                    )
                } else {
                    StorageBackendError::backend("graph", graph)
                };
                let actual = storage_error("execute graph", &error);
                assert_eq!(actual.sqlstate(), Some(state), "{actual}");
                if state == "57014" {
                    assert!(matches!(actual, SQLError::Cancelled(_)));
                }
            }
        }
        for error in [
            StorageBackendError::backend(
                "graph",
                uqa_graph::GraphStoreError::SerializationFailure("stale entity".into()),
            ),
            StorageBackendError::backend(
                "cypher",
                uqa_graph::cypher::CypherError::SerializationFailure("stale entity".into()),
            ),
        ] {
            assert_eq!(
                storage_error("graph mutation", &error).sqlstate(),
                Some("40001")
            );
        }
    }

    #[test]
    fn transaction_diagnostics_survive_provider_error_wrappers() {
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
                    VersionError::SerializationConflict {
                        transaction: uqa_storage::mvcc::SerializableTransactionId::new(
                            uqa_storage::mvcc::DatabaseId::from_bytes([1; 16]),
                            [2; 16],
                            1,
                        )
                        .unwrap(),
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
                let result = storage_error("reserve identity", &error);
                assert_eq!(result.sqlstate(), Some(state), "{result}");
                if state == "57014" {
                    assert!(matches!(result, SQLError::Cancelled(_)));
                }
            }
        }
        let error = storage_error(
            "observe identity",
            &StorageBackendError::Other("unavailable".into()),
        );
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert!(error.to_string().contains("observe identity"));
    }
}
