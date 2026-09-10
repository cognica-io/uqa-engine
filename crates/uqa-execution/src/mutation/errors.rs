//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage failure context for mutation execution.
use uqa_core::DocId;
use uqa_sql::SQLError;

pub use uqa_sql::catalog::errors::dml_storage_error;

pub fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}
