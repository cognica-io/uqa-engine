//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::SQLError;

/// An atomic batch failure with its original SQL error and, when known, zero-based member.
/// Transaction begin, commit and rollback failures are not attributed to a member.
#[derive(Debug, thiserror::Error)]
#[error("{error}")]
pub struct SQLBatchError {
    #[source]
    pub error: SQLError,
    pub statement_index: Option<usize>,
}

impl SQLBatchError {
    pub const fn statement(error: SQLError, statement_index: usize) -> Self {
        Self {
            error,
            statement_index: Some(statement_index),
        }
    }

    pub const fn transaction(error: SQLError) -> Self {
        Self {
            error,
            statement_index: None,
        }
    }

    pub fn into_error(self) -> SQLError {
        self.error
    }
}
