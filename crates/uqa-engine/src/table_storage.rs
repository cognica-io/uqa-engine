//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AnalyzerPhase, Arc, BTreeMap, CommandExactIndex, DocId, Document, Engine, FieldName,
    IVFIndexParams, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult,
    TableState, Value,
};
use crate::CatalogIndexRow;

/// Answer of the value-index conflict probe in [`Engine::find_conflict`].
enum IndexConflictProbe {
    /// No conflict column has a usable value index; fall back to the
    /// evaluated document scan.
    Unanswerable,
    /// The index answered: no existing row matches the conflict target.
    NoConflict,
    /// The index answered: this existing row matches the conflict target.
    Conflict(DocId),
}

fn table_not_found(table: &str) -> StorageBackendError {
    StorageBackendError::Other(format!("table `{table}` does not exist"))
}

pub(crate) use uqa_sql::schema::dependencies::rewrites::upgrade_legacy_schema_function_dispatches;
use uqa_sql::schema::dependencies::rewrites::{
    schema_expr_references_relation, stored_relation_reference_matches,
};
pub(crate) use uqa_sql::schema::dependencies::schema_expr_references_column;

pub(crate) fn rename_schema_expr_column(
    expression: &mut uqa_sql::ast::Expr,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    uqa_sql::schema::dependencies::rewrites::rename_schema_expr_column(expression, from, to)
        .map_err(StorageBackendError::Other)
}
fn rename_schema_expr_relation(
    expression: &mut uqa_sql::ast::Expr,
    from: &RelationIdentity,
    to: &str,
) -> StorageBackendResult<()> {
    uqa_sql::schema::dependencies::rewrites::rename_schema_expr_relation(expression, from, to)
        .map_err(StorageBackendError::Other)
}
fn rename_schema_expr_qualified_column(
    expression: &mut uqa_sql::ast::Expr,
    table: &RelationIdentity,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    uqa_sql::schema::dependencies::rewrites::rename_schema_expr_qualified_column(
        expression, table, from, to,
    )
    .map_err(StorageBackendError::Other)
}

mod columns;
mod constraints;
pub(crate) use constraints::{
    foreign_keys_match_without_object_id, materialize_constraint_metadata,
    table_next_id_metadata_key,
};
mod dependencies;
mod documents;
mod fts;
mod persistent;
mod table_lifecycle;

/// A persistent document-store write failed. Surfacing this as a
/// statement error makes the enclosing transaction roll back, so the
/// on-disk state never keeps a half-applied rewrite.
pub(crate) fn document_store_write_error(err: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("document store write failed: {err}"))
}

pub(crate) fn document_store_read_error(action: &str, err: &StorageBackendError) -> SQLError {
    SQLError::Internal(format!("{action} failed: {err}"))
}

#[cfg(test)]
mod tests;

mod truncate;

mod reads;
