//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage failure context for mutation execution.
use uqa_core::DocId;
use uqa_sql::SQLError;

pub use uqa_sql::catalog::errors::dml_storage_error;

pub use crate::storage_errors::storage_error as identifier_storage_error;

pub fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

/// A row that an UPDATE, DELETE or MERGE reached was modified by a statement that the command's own triggers or functions started, which `PostgreSQL` finds under a later command id than the command's (`TM_SelfModified`). `operation` names what the command does to the row as the error does: `updated`, `deleted`, or `updated or deleted`.
pub fn triggered_modification_error(operation: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "27000".into(),
        message: format!(
            "tuple to be {operation} was already modified by an operation triggered by the current command"
        ),
        detail: None,
        hint: Some(
            "Consider using an AFTER trigger instead of a BEFORE trigger to propagate changes to other rows."
                .into(),
        ),
    }
}
