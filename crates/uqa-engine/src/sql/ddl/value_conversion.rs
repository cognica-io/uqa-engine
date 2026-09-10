//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind declared column metadata and publish rewritten storage values.

use super::{
    ddl_storage_error, index_vectors_for_type, BTreeMap, ColumnType, Engine, RowUpdateVectors,
    SQLError, Value,
};
pub(crate) use uqa_sql::assignment::conversion::convert_value_to_column_type_with_context as convert_value_to_column_type_with_engine;
pub(crate) use uqa_sql::assignment::conversion::*;
use uqa_sql::ast::Expr;

/// Coerce a write value to fit the column's declared type.
pub(in crate::sql) fn coerce_to_column_type(
    engine: &Engine,
    table: &str,
    column: &str,
    value: Value,
) -> Result<Value, SQLError> {
    uqa_sql::assignment::columns::coerce_to_column_type(engine, engine, table, column, value)
}

pub(super) fn rewrite_column_values_to_type(
    engine: &Engine,
    table: &str,
    column: &str,
    source_ty: &ColumnType,
    target_ty: &ColumnType,
    using: Option<&Expr>,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| ddl_storage_error("ALTER COLUMN TYPE", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let schema = uqa_execution::RowSchema::with_types(
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
    for doc_id in engine.live_table_doc_ids(table)? {
        let Some(doc) = engine.get_document(table, doc_id)? else {
            continue;
        };
        let converted = if let Some(expression) = using {
            let value = crate::sql::scalar::eval_lowered_expression_with_schema(
                engine,
                expression,
                &doc,
                &schema,
                &[],
            )?;
            convert_value_to_column_type_with_engine(engine, value, target_ty)?
        } else {
            let Some(value) = doc.get(column).cloned() else {
                continue;
            };
            convert_declared_value_to_column_type(engine, value, source_ty, target_ty)?
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
        engine.update_document_fields_with_vector_values(table, doc_id, updates, vectors)?;
    }
    Ok(())
}
