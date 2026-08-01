//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::{
    build_join_spill_with_ctes, build_projection_row_with_ctes, coerce_to_column_type,
    column_type_name, doc_id_value, expand_star_columns, prefix_row, projection_columns,
    validate_vector_dimensions, value_to_tensor, value_to_vector, BTreeMap, BTreeSet, BinaryOp,
    ColumnType, CteScope, DocId, Document, Engine, ForeignKey, ForeignKeyAction, ForeignKeyMatch,
    ResultRow, RowIndependentUpdateValues, SQLError, SQLParam, SQLResult, Value, DOC_ID_COLUMN,
    MERGE_ACTION_COLUMN,
};
use uqa_execution::ScalarExpr;
use uqa_planner::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
    ProjectionPlan, SourcePlan, UpdatePlan,
};

use super::scalar::{eval_lowered_expression, eval_physical_scalar, PhysicalEvalContext};
use super::ScopedEngineHook;

fn dml_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

fn insert_identity_columns(
    engine: &Engine,
    table: &str,
    action: &str,
) -> Result<(Option<String>, String), SQLError> {
    let auto_increment = engine
        .auto_increment_column(table)
        .map_err(|err| dml_storage_error(action, err))?;
    let id_column = if auto_increment.is_some() {
        auto_increment.clone()
    } else {
        engine
            .try_describe_table(table)
            .map_err(|err| dml_storage_error(action, err))?
            .and_then(|columns| columns.into_iter().find(|column| column.primary_key))
            .map(|column| column.name)
    }
    .unwrap_or_else(|| "id".into());
    Ok((auto_increment, id_column))
}

fn validate_mutation_columns<'a>(
    engine: &Engine,
    table: &str,
    columns: impl IntoIterator<Item = &'a str>,
    action: &str,
) -> Result<(), SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|err| dml_storage_error(action, err))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    // Programmatically-created document tables intentionally have no SQL
    // schema and retain their open-field behavior. SQL CREATE TABLE always
    // supplies definitions, and those targets must reject misspelled or
    // repeated mutation columns instead of persisting arbitrary fields.
    if definitions.is_empty() {
        return Ok(());
    }
    let known: BTreeSet<&str> = definitions
        .iter()
        .map(|definition| definition.name.as_str())
        .collect();
    let mut seen = BTreeSet::new();
    for column in columns {
        if !seen.insert(column) {
            return Err(SQLError::TypeMismatch(format!(
                "{action}: column `{column}` is specified more than once"
            )));
        }
        if !known.contains(column) {
            return Err(SQLError::UnknownColumn(format!("{table}.{column}")));
        }
    }
    Ok(())
}

fn eval_mutation_expr(
    engine: &Engine,
    ctes: &CteScope,
    expression: &ScalarExpr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    let context = PhysicalEvalContext::new(row, params)
        .with_function_hook(&hook)
        .with_subquery_runner(&hook);
    eval_physical_scalar(expression, &ctes.scalar_subqueries, &context)
}

const MERGE_PAIR_DOC_ID: &str = "__uqa_merge_pair_doc_id";

struct MergePairing {
    doc_id: Option<DocId>,
    target_row: ResultRow,
    source_row: Option<ResultRow>,
}

fn merge_pair_schema(target_columns: &[String], source_columns: &[String]) -> Vec<String> {
    std::iter::once(MERGE_PAIR_DOC_ID.to_string())
        .chain(
            target_columns
                .iter()
                .enumerate()
                .map(|(index, _)| format!("__uqa_merge_target_{index}")),
        )
        .chain(
            source_columns
                .iter()
                .enumerate()
                .map(|(index, _)| format!("__uqa_merge_source_{index}")),
        )
        .collect()
}

fn encode_merge_pair(
    doc_id: Option<DocId>,
    target_row: &ResultRow,
    source_row: &ResultRow,
    target_columns: &[String],
    source_columns: &[String],
) -> ResultRow {
    let mut encoded = ResultRow::new();
    encoded.insert(
        MERGE_PAIR_DOC_ID.into(),
        doc_id.map_or(Value::Null, |doc_id| Value::Str(doc_id.to_string())),
    );
    for (index, column) in target_columns.iter().enumerate() {
        encoded.insert(
            format!("__uqa_merge_target_{index}"),
            target_row.get(column).cloned().unwrap_or(Value::Null),
        );
    }
    for (index, column) in source_columns.iter().enumerate() {
        encoded.insert(
            format!("__uqa_merge_source_{index}"),
            source_row.get(column).cloned().unwrap_or(Value::Null),
        );
    }
    encoded
}

fn decode_merge_pair(
    encoded: &ResultRow,
    target_columns: &[String],
    source_columns: &[String],
) -> Result<MergePairing, SQLError> {
    let doc_id = match encoded.get(MERGE_PAIR_DOC_ID) {
        Some(Value::Null) | None => None,
        Some(Value::Str(doc_id)) => Some(doc_id.parse::<DocId>().map_err(|error| {
            SQLError::Internal(format!(
                "invalid spilled MERGE document id `{doc_id}`: {error}"
            ))
        })?),
        Some(value) => {
            return Err(SQLError::Internal(format!(
                "invalid spilled MERGE document id value {value:?}"
            )))
        }
    };
    let target_row = target_columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            (
                column.clone(),
                encoded
                    .get(&format!("__uqa_merge_target_{index}"))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect();
    let source_row = source_columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            (
                column.clone(),
                encoded
                    .get(&format!("__uqa_merge_source_{index}"))
                    .cloned()
                    .unwrap_or(Value::Null),
            )
        })
        .collect();
    Ok(MergePairing {
        doc_id,
        target_row,
        source_row: Some(source_row),
    })
}

fn merge_source_index_row(index: usize) -> ResultRow {
    ResultRow::from([("source_index".into(), Value::Str(index.to_string()))])
}

mod conflict;
mod constraints;
mod delete;
mod insert;
mod merge;
mod update;
mod update_from;
mod vectors;

pub(in crate::sql) use conflict::*;
pub(in crate::sql) use constraints::*;
pub(in crate::sql) use delete::*;
pub(in crate::sql) use insert::*;
pub(in crate::sql) use merge::*;
pub(in crate::sql) use update::*;
pub(in crate::sql) use update_from::*;
pub(in crate::sql) use vectors::*;
