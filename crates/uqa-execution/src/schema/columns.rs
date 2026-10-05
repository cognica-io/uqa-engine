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
    columns::AssignmentColumnCatalog, conversion::convert_declared_value_to_column_type,
    AssignmentContext,
};
use uqa_sql::semantics::partition::PartitionExpressions;
use uqa_sql::{
    ast::ColumnType,
    schema::columns::type_transform::{assign_type_transform_value, AnalyzedTypeTransform},
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
    pub cancellation: &'a uqa_core::CancellationToken,
    pub columns: &'a dyn AssignmentColumnCatalog,
    pub reads: &'a dyn MutationRead,
    pub types: &'a dyn AssignmentContext,
    pub expressions: &'a dyn PartitionExpressions,
    pub writes: &'a dyn ColumnRewritePublication,
}
/// The rows of `table` with `column` converted from `source_ty` to `target_ty`, by its `USING` expression when the type change has one. A type change rewrites the table with them; their constraints are checked before they replace the stored rows.
pub fn converted_column_rows<S: Clone + 'static>(
    context: &alteration::ColumnAlterContext<'_, S>,
    table: &str,
    column: &str,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
    transform: Option<&AnalyzedTypeTransform>,
) -> Result<rows::RewriteRows, SQLError> {
    let rewrite = &context.rewrite;
    let allowance = context.generated.keys.constraints.memory.work_mem_bytes()?;
    let memory = uqa_core::memory::MemoryBudget::new(allowance / 2);
    let read_memory = uqa_core::memory::MemoryBudget::new(allowance - allowance / 2);
    let control =
        uqa_storage::read_control::StorageReadControl::new(&read_memory, rewrite.cancellation);
    let mut original = rows::capture(rewrite.reads, table, &memory, &control)?;
    let mut rows = rows::RewriteRows::new(&memory);
    for position in 0..original.len() {
        rewrite.cancellation.check()?;
        let rows::RewriteRow {
            original_id: doc_id,
            document: mut doc,
            ..
        } = original.get(position)?;
        let converted = if let Some(transform) = transform {
            let value = crate::query::catalog_expression::eval_expression_plan_with_schema(
                context.generated.assignment.expressions.expressions,
                crate::query::CteScope::default(),
                transform.plan.clone(),
                &doc,
                transform.row_schema(),
                &[],
            )?;
            Some(assign_type_transform_value(
                rewrite.types,
                value,
                target_ty,
                transform.source_type.as_ref(),
            )?)
        } else {
            doc.get(column)
                .cloned()
                .map(|value| {
                    convert_declared_value_to_column_type(
                        rewrite.types,
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
        rows.push(doc_id, doc)?;
    }
    Ok(rows)
}

pub mod rows;

pub mod backfill;
pub mod generated;

pub mod addition;

pub mod alteration;

pub mod deletion;

pub mod removal;
