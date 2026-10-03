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
use uqa_storage::document_store::Document;
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
/// The rows of `table` with `column` converted from `source_ty` to `target_ty`, by its `USING` expression when the type change has one. A type change rewrites the table with them; their constraints are checked before they replace the stored rows.
pub fn converted_column_rows(
    context: &ColumnRewriteContext<'_>,
    table: &str,
    column: &str,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
    using: Option<&Expr>,
) -> Result<Vec<(DocId, Document)>, SQLError> {
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
    let doc_ids = context.reads.live_table_doc_ids(table)?;
    let mut rows = Vec::with_capacity(doc_ids.len());
    for doc_id in doc_ids {
        let Some(mut doc) = context.reads.get_document(table, doc_id)? else {
            continue;
        };
        let converted = if let Some(expression) = using {
            let value = context
                .expressions
                .evaluate_row(expression, &doc, &schema, &[])?;
            Some(convert_value_to_column_type_with_context(
                context.types,
                value,
                target_ty,
            )?)
        } else {
            doc.get(column)
                .cloned()
                .map(|value| {
                    convert_declared_value_to_column_type(
                        context.types,
                        value,
                        source_ty,
                        target_ty,
                    )
                })
                .transpose()?
        };
        if let Some(converted) = converted {
            doc.insert(column.to_string(), converted);
        }
        rows.push((doc_id, doc));
    }
    Ok(rows)
}

pub mod backfill;
pub mod generated;

pub mod addition;

pub mod alteration;

pub mod removal;
