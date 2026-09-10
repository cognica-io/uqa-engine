//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Project stored fields and transaction metadata through a SQL row type.

use uqa_core::Value;
use uqa_sql::semantics::XMIN_COLUMN;
use uqa_storage::{document_store::Document, DocumentMetadata, StoredDocument};

pub fn projection_uses_tuple_xmin(column: &str, definitions: &[uqa_sql::ast::ColumnDef]) -> bool {
    column == XMIN_COLUMN
        && !definitions
            .iter()
            .any(|definition| definition.name == XMIN_COLUMN)
}

pub fn projections_use_tuple_xmin(
    columns: &[String],
    definitions: &[uqa_sql::ast::ColumnDef],
) -> bool {
    columns
        .iter()
        .any(|column| projection_uses_tuple_xmin(column, definitions))
}

pub fn project_document_column(
    document: &Document,
    metadata: DocumentMetadata,
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Value {
    if !projection_uses_tuple_xmin(column, definitions) {
        return document.get(column).cloned().unwrap_or(Value::Null);
    }
    if definitions.is_empty() {
        if let Some(value) = document.get(column) {
            return value.clone();
        }
    }
    metadata
        .tuple_xmin()
        .map_or(Value::Null, |xmin| Value::Int(i64::from(xmin)))
}

pub fn project_stored_document_column(
    document: &StoredDocument,
    column: &str,
    definitions: &[uqa_sql::ast::ColumnDef],
) -> Value {
    project_document_column(document.fields(), document.metadata(), column, definitions)
}
