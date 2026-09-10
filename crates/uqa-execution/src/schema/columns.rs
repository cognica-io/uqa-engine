//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Rewrite stored column values after a declaration changes their SQL type.
use crate::mutation::constraints::context::MutationRead;
use crate::mutation::publication::DocumentVectors;
use std::collections::BTreeMap;
use uqa_core::{DocId, Value};
use uqa_sql::assignment::vectors::index_vectors_for_type;
use uqa_sql::assignment::{
    columns::{AssignmentColumnCatalog, ColumnCatalogError},
    conversion::{
        convert_declared_value_to_column_type, convert_value_to_column_type_with_context,
    },
    AssignmentContext,
};
use uqa_sql::semantics::partition::PartitionExpressions;
use uqa_sql::{
    ast::{ColumnType, Expr},
    SQLError,
};
/// Publish converted fields through the caller's storage and index update path.
pub trait ColumnRewritePublication {
    fn update_fields(
        &self,
        table: &str,
        id: DocId,
        values: BTreeMap<String, Value>,
        vectors: DocumentVectors,
    ) -> Result<bool, SQLError>;
}
pub struct ColumnRewriteContext<'a> {
    pub columns: &'a dyn AssignmentColumnCatalog,
    pub reads: &'a dyn MutationRead,
    pub types: &'a dyn AssignmentContext,
    pub expressions: &'a dyn PartitionExpressions,
    pub writes: &'a dyn ColumnRewritePublication,
}
fn ddl_storage_error(action: &str, error: ColumnCatalogError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, error.as_ref())
}
type RowUpdateVectors = DocumentVectors;
pub fn rewrite_column_values_to_type(
    context: &ColumnRewriteContext<'_>,
    table: &str,
    column: &str,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
    using: Option<&Expr>,
) -> Result<(), SQLError> {
    let definitions = context
        .columns
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let schema = crate::RowSchema::with_types(
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect(),
        definitions
            .iter()
            .map(|definition| {
                Some(if definition.name == column {
                    source_ty.clone()
                } else {
                    definition.ty.clone()
                })
            })
            .collect(),
    );
    for doc_id in context.reads.live_table_doc_ids(table)? {
        let Some(doc) = context.reads.get_document(table, doc_id)? else {
            continue;
        };
        let converted = if let Some(expression) = using {
            let value = context
                .expressions
                .evaluate_row(expression, &doc, &schema, &[])?;
            convert_value_to_column_type_with_context(context.types, value, target_ty)?
        } else {
            let Some(value) = doc.get(column).cloned() else {
                continue;
            };
            convert_declared_value_to_column_type(context.types, value, source_ty, target_ty)?
        };
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), converted.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if matches!(target_ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) {
            vectors.insert(
                column.to_string(),
                index_vectors_for_type(&converted, target_ty)?,
            );
        }
        context
            .writes
            .update_fields(table, doc_id, updates, vectors)?;
    }
    Ok(())
}

pub mod backfill;
pub mod generated;

pub mod addition;

pub mod alteration;

pub mod removal;
