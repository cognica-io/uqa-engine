//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve each public error carrier when transaction cleanup also fails.

use uqa_sql::{SQLBatchError, SQLError};
use uqa_storage::StorageBackendError;

pub(crate) trait TransactionError: std::fmt::Display {
    fn with_transaction_cleanup(self, cleanup: SQLError, operation: &'static str) -> Self;
}

impl TransactionError for SQLError {
    fn with_transaction_cleanup(self, cleanup: SQLError, operation: &'static str) -> Self {
        self.with_cleanup(cleanup, operation)
    }
}

impl TransactionError for SQLBatchError {
    fn with_transaction_cleanup(mut self, cleanup: SQLError, operation: &'static str) -> Self {
        self.error = self.error.with_cleanup(cleanup, operation);
        self
    }
}

impl TransactionError for String {
    fn with_transaction_cleanup(self, cleanup: SQLError, operation: &'static str) -> Self {
        format!("{self}; {operation}: {cleanup}")
    }
}

#[derive(Debug, thiserror::Error)]
#[error("{source}; {operation}: {cleanup}")]
struct CleanupSource<E> {
    #[source]
    source: E,
    cleanup: SQLError,
    operation: &'static str,
}

impl TransactionError for StorageBackendError {
    fn with_transaction_cleanup(self, cleanup: SQLError, operation: &'static str) -> Self {
        Self::backend(
            "transaction cleanup",
            CleanupSource {
                source: self,
                cleanup,
                operation,
            },
        )
    }
}

impl TransactionError for uqa_sql::catalog::sequence_functions::value_error::SequenceValueError {
    fn with_transaction_cleanup(self, cleanup: SQLError, operation: &'static str) -> Self {
        Self::from(self.into_sql_error().with_cleanup(cleanup, operation))
    }
}

impl TransactionError for uqa_graph::cypher::CypherError {
    fn with_transaction_cleanup(self, cleanup: SQLError, operation: &'static str) -> Self {
        Self::from(StorageBackendError::backend(
            "transaction cleanup",
            CleanupSource {
                source: self,
                cleanup,
                operation,
            },
        ))
    }
}

impl TransactionError for crate::migration::PythonMigrationError {
    fn with_transaction_cleanup(self, cleanup: SQLError, operation: &'static str) -> Self {
        Self::from(StorageBackendError::backend(
            "transaction cleanup",
            CleanupSource {
                source: self,
                cleanup,
                operation,
            },
        ))
    }
}

/// Unresolved physical completion still takes precedence; otherwise retain the failed operation's typed diagnostic and metadata.
pub(super) fn attach<E: TransactionError>(
    error: E,
    cleanup: SQLError,
    operation: &'static str,
    map_transaction_error: impl FnOnce(SQLError) -> E,
) -> E {
    if cleanup.sqlstate() == Some("08007") {
        map_transaction_error(cleanup.with_context(format!("original operation error: {error}")))
    } else {
        error.with_transaction_cleanup(cleanup, operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_cleanup_keeps_the_primary_resource_cause() {
        let original = StorageBackendError::Memory(uqa_core::memory::MemoryError::SizeOverflow);
        let error = original.with_transaction_cleanup(
            SQLError::Internal("catalog restore failed".into()),
            "rollback",
        );
        let sql = uqa_execution::storage_errors::storage_error("read", &error);
        assert_eq!(sql.sqlstate(), Some("53200"));
        assert!(error.to_string().contains("catalog restore failed"));
    }

    #[test]
    fn unresolved_completion_keeps_its_diagnostic_and_the_original_context() {
        let original = SQLError::Routine {
            sqlstate: "22012".into(),
            message: "original division failure".into(),
        };
        let unresolved = SQLError::Routine {
            sqlstate: "08007".into(),
            message: "retained transaction requires resolution".into(),
        };
        let error = attach(original, unresolved, "rollback", std::convert::identity);
        assert_eq!(error.sqlstate(), Some("08007"));
        assert_eq!(
            error.to_string(),
            "retained transaction requires resolution"
        );
        assert!(error
            .context()
            .unwrap()
            .contains("original division failure"));
    }
}
