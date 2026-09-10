//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Materialize stable defaults once and volatile defaults once per existing row.
use super::ColumnRewriteContext;
use crate::mutation::publication::DocumentVectors as RowUpdateVectors;
use std::collections::BTreeMap;
use uqa_core::Value;
use uqa_sql::{
    assignment::{columns::coerce_to_column_type, vectors::index_vectors_for_type},
    ast::ColumnType,
    semantics::volatility::VolatilityCatalog,
    SQLError,
};
use uqa_storage::StorageBackendResult;
pub trait ColumnBackfillState {
    fn column_type(&self, table: &str, column: &str) -> StorageBackendResult<Option<ColumnType>>;
    fn clear_missing_values(&self, table: &str) -> Result<(), SQLError>;
}
pub struct ColumnBackfillContext<'a> {
    pub rewrite: ColumnRewriteContext<'a>,
    pub state: &'a dyn ColumnBackfillState,
    pub volatility: &'a dyn VolatilityCatalog,
}
fn ddl_storage_error(action: &str, error: uqa_storage::StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, &error)
}
/// Apply the new column's default to rows that existed before `ADD COLUMN`. `PostgreSQL` stores one missing value for a non-volatile default, including on an empty table, while volatile defaults are evaluated independently for every existing row and do not populate `attmissingval`.
pub fn backfill_added_column(
    context: &ColumnBackfillContext<'_>,
    table: &str,
    column: &str,
    default_expr: Option<&uqa_sql::ast::Expr>,
    not_null: bool,
) -> Result<Option<Value>, SQLError> {
    let doc_ids = context.rewrite.reads.live_table_doc_ids(table)?;
    let Some(default_expr) = default_expr else {
        if not_null && !doc_ids.is_empty() {
            return Err(SQLError::Routine {
                sqlstate: "23502".into(),
                message: format!(
                    "column \"{column}\" of relation \"{table}\" contains null values"
                ),
            });
        }
        return Ok(None);
    };
    let column_type = context
        .state
        .column_type(table, column)
        .map_err(|err| ddl_storage_error("ALTER TABLE ADD COLUMN", err))?;
    let lowered = uqa_sql::plan::ExpressionPlan::lower(default_expr.clone());
    let volatile = uqa_sql::semantics::volatility::expr_contains_volatile_function(
        context.volatility,
        &lowered.scalar,
    );
    if volatile {
        for doc_id in doc_ids {
            let value = context
                .rewrite
                .expressions
                .evaluate_bound(default_expr, &[])?;
            let value = coerce_to_column_type(
                context.rewrite.types,
                context.rewrite.columns,
                table,
                column,
                value,
            )?;
            if not_null && value == Value::Null {
                return Err(SQLError::Routine {
                    sqlstate: "23502".into(),
                    message: format!(
                        "null value in column \"{column}\" of relation \"{table}\" violates not-null constraint"
                    ),
                });
            }
            let mut vectors: RowUpdateVectors = BTreeMap::new();
            if let Some(ty) = column_type
                .as_ref()
                .filter(|ty| matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)))
            {
                vectors.insert(column.to_string(), index_vectors_for_type(&value, ty)?);
            }
            context.rewrite.writes.update_fields(
                table,
                doc_id,
                BTreeMap::from([(column.to_string(), value)]),
                vectors,
            )?;
        }
        context.state.clear_missing_values(table)?;
        return Ok(None);
    }
    let default_value = coerce_to_column_type(
        context.rewrite.types,
        context.rewrite.columns,
        table,
        column,
        context
            .rewrite
            .expressions
            .evaluate_bound(default_expr, &[])?,
    )?;
    if not_null && default_value == Value::Null && !doc_ids.is_empty() {
        return Err(SQLError::Routine {
            sqlstate: "23502".into(),
            message: format!(
                "null value in column \"{column}\" of relation \"{table}\" violates not-null constraint"
            ),
        });
    }
    let vector_value = match column_type.as_ref() {
        Some(ty) if matches!(ty, ColumnType::Vector(_) | ColumnType::Tensor(_)) => {
            Some(index_vectors_for_type(&default_value, ty)?)
        }
        Some(_) | None => None,
    };
    for doc_id in doc_ids {
        let mut updates: BTreeMap<String, Value> = BTreeMap::new();
        updates.insert(column.to_string(), default_value.clone());
        let mut vectors: RowUpdateVectors = BTreeMap::new();
        if let Some(v) = vector_value.as_ref() {
            vectors.insert(column.to_string(), v.clone());
        }
        context
            .rewrite
            .writes
            .update_fields(table, doc_id, updates, vectors)?;
    }
    Ok((default_value != Value::Null).then_some(default_value))
}
