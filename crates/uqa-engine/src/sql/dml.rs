//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL DML execution, constraints, referential actions, and RETURNING rows.

use super::scalar::{eval_lowered_expression, eval_physical_scalar, PhysicalEvalContext};
use super::ScopedEngineHook;
use super::{
    bind_projection_output_schema, build_join_spill_with_ctes,
    build_projection_physical_row_with_ctes, coerce_to_column_type, column_type_name, doc_id_value,
    validate_vector_dimensions, value_to_tensor, value_to_vector, BTreeMap, BTreeSet, BinaryOp,
    ColumnType, CteScope, DocId, Document, Engine, ForeignKey, ForeignKeyAction, ForeignKeyMatch,
    RowIndependentUpdateValues, SQLError, SQLParam, SQLResult, Value, DOC_ID_COLUMN,
    MERGE_ACTION_COLUMN,
};
use uqa_execution::{ColumnIdentity, OwnedPhysicalRow, PhysicalRow, RowSchema, ScalarExpr};
use uqa_planner::{
    ConflictActionPlan, ConflictPlan, DeletePlan, InsertPlan, MergePlan, MergeWhenPlan,
    ProjectionPlan, SourcePlan, UpdatePlan,
};

fn dml_storage_error(action: &str, err: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {err}"))
}

fn missing_document_error(action: &str, table: &str, doc_id: DocId) -> SQLError {
    SQLError::Internal(format!(
        "{action}: document {doc_id} listed by table `{table}` disappeared during the statement"
    ))
}

fn dml_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
    doc_id: DocId,
    document: &Document,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut materialized = document.clone();
    crate::engine_generated::materialize_virtual_generated_columns(
        &definitions,
        &mut materialized,
    )?;
    let mut columns = if definitions.is_empty() {
        materialized.keys().cloned().collect::<Vec<_>>()
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    let values = columns
        .iter()
        .map(|column| {
            if column == DOC_ID_COLUMN
                || definitions.iter().any(|definition| {
                    definition.name == *column
                        && definition.primary_key
                        && definition.ty.is_integer()
                })
            {
                doc_id_value(doc_id)
            } else {
                Ok(materialized.get(column).cloned().unwrap_or(Value::Null))
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::from_values(values),
    ))
}

fn dml_null_target_row(
    engine: &Engine,
    table: &str,
    qualifier: &str,
) -> Result<OwnedPhysicalRow, SQLError> {
    let definitions = engine
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("DML row schema lookup", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let mut columns = if definitions.is_empty() {
        engine
            .try_table_columns(table)
            .map_err(|error| dml_storage_error("DML row schema lookup", error))?
    } else {
        definitions
            .iter()
            .map(|definition| definition.name.clone())
            .collect::<Vec<_>>()
    };
    let mut types = columns
        .iter()
        .map(|column| {
            definitions
                .iter()
                .find(|definition| definition.name == *column)
                .map(|definition| definition.ty.clone())
        })
        .collect::<Vec<_>>();
    if !columns.iter().any(|column| column == DOC_ID_COLUMN) {
        columns.push(DOC_ID_COLUMN.into());
        types.push(Some(ColumnType::BigInteger));
    }
    let width = columns.len();
    Ok(OwnedPhysicalRow::new(
        RowSchema::with_qualified_types(qualifier, columns, types),
        PhysicalRow::nulls(width),
    ))
}

fn dml_join_rows(left: &OwnedPhysicalRow, right: &OwnedPhysicalRow) -> OwnedPhysicalRow {
    OwnedPhysicalRow::new(
        RowSchema::join(&left.schema, &right.schema, std::iter::empty()),
        PhysicalRow::concat(&left.row, &right.row),
    )
}

fn validate_dml_expression_qualifiers(
    expression: &ScalarExpr,
    allowed: &BTreeSet<String>,
) -> Result<(), SQLError> {
    for qualifier in crate::sql::select::expr_qualifiers(expression) {
        if !allowed.contains(&qualifier) {
            return Err(SQLError::UnknownTable(qualifier));
        }
    }
    Ok(())
}

fn dml_append_hidden_qualified_row(
    base: &OwnedPhysicalRow,
    qualifier: &str,
    columns: &[String],
    types: &[Option<ColumnType>],
    values: Vec<Value>,
) -> OwnedPhysicalRow {
    let hidden = columns
        .iter()
        .enumerate()
        .map(|(position, _)| {
            (
                format!("\0uqa.dml.{qualifier}.{position}"),
                types.get(position).cloned().flatten(),
            )
        })
        .collect::<Vec<_>>();
    let schema = RowSchema::append_typed(&base.schema, &hidden);
    let offset = base.schema.len();
    let aliases = columns
        .iter()
        .enumerate()
        .map(|(position, column)| {
            (
                ColumnIdentity::qualified(qualifier, column),
                offset + position,
            )
        })
        .collect::<Vec<_>>();
    OwnedPhysicalRow::new(
        RowSchema::with_identity_aliases(&schema, &aliases),
        base.row.clone().append_values(values),
    )
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
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let hook = ScopedEngineHook::new(engine, ctes);
    if let Some(row) = row {
        let expression = uqa_execution::bind_type_introspection_with_resolver(
            expression.clone(),
            &row.schema,
            params,
            engine,
        );
        let view = row.view();
        let context = PhysicalEvalContext::from_row_lookup(&view, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook)
            .with_physical_outer_row(&row.schema, &row.row);
        eval_physical_scalar(&expression, &ctes.scalar_subqueries, &context)
    } else {
        let context = PhysicalEvalContext::new(None, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        eval_physical_scalar(expression, &ctes.scalar_subqueries, &context)
    }
}

struct MutationAssignmentTarget<'a> {
    table: &'a str,
    column: &'a str,
    action: &'a str,
}

fn eval_mutation_assignment(
    engine: &Engine,
    ctes: &CteScope,
    target: MutationAssignmentTarget<'_>,
    expression: &ScalarExpr,
    row: Option<&OwnedPhysicalRow>,
    params: &[SQLParam],
) -> Result<Option<Value>, SQLError> {
    let MutationAssignmentTarget {
        table,
        column,
        action,
    } = target;
    let generated = crate::sql::generated::generated_column_kind(engine, table, column)?;
    if matches!(expression, ScalarExpr::Default) {
        if generated.is_some() {
            return Ok(None);
        }
        let value = match engine
            .try_column_default_expr(table, column)
            .map_err(|error| dml_storage_error(action, error))?
        {
            Some(default) => eval_lowered_expression(engine, &default, None, params)?,
            None => Value::Null,
        };
        return coerce_to_column_type(engine, table, column, value).map(Some);
    }
    if generated.is_some() {
        return Err(SQLError::TypeMismatch(format!(
            "column `{column}` is a generated column; only DEFAULT may be assigned"
        )));
    }
    let value = eval_mutation_expr(engine, ctes, expression, row, params)?;
    coerce_to_column_type(engine, table, column, value).map(Some)
}

const MERGE_PAIR_DOC_ID: &str = "__uqa_merge_pair_doc_id";

struct MergePairing {
    doc_id: Option<DocId>,
    source_row: OwnedPhysicalRow,
}

fn merge_pair_schema(source: &RowSchema) -> RowSchema {
    let header =
        RowSchema::with_types(vec![MERGE_PAIR_DOC_ID.into()], vec![Some(ColumnType::Text)]);
    RowSchema::join(&header, source, std::iter::empty())
}

fn encode_merge_pair(
    doc_id: Option<DocId>,
    source_row: &OwnedPhysicalRow,
) -> uqa_execution::PhysicalRow {
    let header = PhysicalRow::from_values(vec![
        doc_id.map_or(Value::Null, |doc_id| Value::Str(doc_id.to_string()))
    ]);
    PhysicalRow::concat(&header, &source_row.row)
}

fn decode_merge_pair(encoded: OwnedPhysicalRow) -> Result<MergePairing, SQLError> {
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
    Ok(MergePairing {
        doc_id,
        source_row: encoded,
    })
}

fn merge_source_index_value(index: usize) -> Value {
    Value::Str(index.to_string())
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
