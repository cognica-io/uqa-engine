//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The final statement of a SQL function body checked against the declared result, as `PostgreSQL`'s `check_sql_fn_retval` checks it at creation and before execution.

use super::declaration::RoutineTypeCatalog;
use crate::{
    ast::{ColumnType, CreateFunction, FunctionReturns},
    binding::statements::AnalyzedResult,
    SQLError,
};

mod anonymous;
mod production;
pub use production::{
    declared_sql_function_result_with_control, sql_function_composite_columns_with_control,
    sql_function_result_layout_with_control,
};
pub(crate) mod retention;
pub use anonymous::{
    validate_anonymous_record_result, validate_procedural_record_identity,
    validate_trigger_record_result,
};

/// Whether the routine returns nothing, one output value, or the complete output tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SQLFunctionResultKind {
    Void,
    Value,
    Tuple,
}

/// A live attribute in the declared function result, in physical attribute order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLFunctionResultColumn {
    pub name: String,
    pub ty: ColumnType,
}

/// The SQL-owned result decision. A lone row value is distinct from a tuple whose individual columns need assignment coercion. Domains remain scalar, even when their base type is composite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SQLFunctionResultLayout {
    pub kind: SQLFunctionResultKind,
    pub declared_type: ColumnType,
    /// Descriptor of a lone returned row, captured before the statement executes.
    pub source_record: Option<Vec<Option<ColumnType>>>,
    /// Named composite attributes or OUT parameters; absent for scalar and unspecified record results.
    pub columns: Option<Vec<SQLFunctionResultColumn>>,
}

/// Check the final statement without exposing its execution layout to callers that only validate a definition.
pub fn check_sql_function_result(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    last: Option<&AnalyzedResult>,
) -> Result<(), SQLError> {
    sql_function_result_layout(types, def, last).map(|_| ())
}

/// Decide whether the final statement returns a single value or its complete row, using the same assignment rules at creation and invocation.
pub fn sql_function_result_layout(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
    last: Option<&AnalyzedResult>,
) -> Result<SQLFunctionResultLayout, SQLError> {
    sql_function_result_layout_with_control(
        types,
        def,
        last,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|layout| {
        layout
            .into_uncontrolled()
            .expect("ordinary function result layout")
    })
}

/// Resolve the result declaration independently of a final statement.
pub fn declared_sql_function_result(
    types: &dyn RoutineTypeCatalog,
    def: &CreateFunction,
) -> Result<SQLFunctionResultLayout, SQLError> {
    declared_sql_function_result_with_control(
        types,
        def,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|layout| {
        layout
            .into_uncontrolled()
            .expect("ordinary function declaration layout")
    })
}

/// Read the live descriptor of a named row value. An anonymous row's descriptor must come from expression analysis, rather than inspecting its values.
pub fn sql_function_composite_columns(
    types: &dyn RoutineTypeCatalog,
    source: Option<&ColumnType>,
) -> Result<Option<Vec<Option<ColumnType>>>, SQLError> {
    sql_function_composite_columns_with_control(
        types,
        source,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|columns| {
        columns.map(|columns| {
            columns
                .into_uncontrolled()
                .expect("ordinary composite result columns")
        })
    })
}

/// A whole returned record must match the caller's tuple descriptor; its fields are not assignment-coerced as independently selected columns are.
pub fn validate_sql_function_record(
    types: &dyn RoutineTypeCatalog,
    source: &[Option<ColumnType>],
    target: &[ColumnType],
) -> Result<(), SQLError> {
    if source.len() != target.len() {
        return Err(record_mismatch(format!(
            "Returned row contains {} attribute{}, but query expects {}.",
            source.len(),
            if source.len() == 1 { "" } else { "s" },
            target.len()
        )));
    }
    for (index, (source, target)) in source.iter().zip(target).enumerate() {
        let Some(source) = source else {
            return Err(record_mismatch(format!(
                "Returned type unknown at ordinal position {}, but query expects {}.",
                index + 1,
                types.format_type(target)?
            )));
        };
        let source_oid = crate::catalog::type_metadata::pg_type_oid(source);
        let target_oid = crate::catalog::type_metadata::pg_type_oid(target);
        let source_modifier = crate::catalog::type_metadata::pg_type_modifier(source);
        let target_modifier = crate::catalog::type_metadata::pg_type_modifier(target);
        if source_oid != target_oid || (target_modifier >= 0 && source_modifier != target_modifier)
        {
            return Err(record_mismatch(format!(
                "Returned type {} at ordinal position {}, but query expects {}.",
                types.format_type(source)?,
                index + 1,
                types.format_type(target)?
            )));
        }
    }
    Ok(())
}

fn record_mismatch(detail: String) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42804".into(),
        message: "function return row and query-specified return row do not match".into(),
        detail: Some(detail),
        hint: None,
    }
}

/// A SQL set-returning function materializes one record descriptor for all non-null rows before the caller checks the declared result. Inspect borrowed values without copying their fields.
pub fn validate_sql_function_record_rows(result: &crate::SQLResult) -> Result<(), SQLError> {
    let mut descriptor = None;
    for index in 0..result.rows.len() {
        if let Some(uqa_core::Value::Row(row)) = result.value_at(index, 0) {
            if let Some(fields) = row.field_types() {
                if descriptor.is_some_and(|previous| previous != fields) {
                    return Err(SQLError::Routine {
                        sqlstate: "42804".into(),
                        message: "rows returned by function are not all of the same row type"
                            .into(),
                    });
                }
                descriptor = Some(fields);
            }
        }
    }
    Ok(())
}

/// Validate the descriptor carried by the selected runtime row, including CASE branches and materialized intermediate rows whose record shapes differ.
pub fn validate_sql_function_record_identity(
    types: &dyn RoutineTypeCatalog,
    source: &[uqa_core::RecordFieldType],
    target: &[ColumnType],
) -> Result<(), SQLError> {
    if source.len() != target.len() {
        return Err(record_mismatch(format!(
            "Returned row contains {} attribute{}, but query expects {}.",
            source.len(),
            if source.len() == 1 { "" } else { "s" },
            target.len(),
        )));
    }
    for (index, (source, target)) in source.iter().zip(target).enumerate() {
        let target_modifier = crate::catalog::type_metadata::pg_type_modifier(target);
        if i64::from(source.oid) == crate::catalog::type_metadata::pg_type_oid(target)
            && (target_modifier < 0 || i64::from(source.type_modifier) == target_modifier)
        {
            continue;
        }
        let source_name = if source.oid == 705 {
            "unknown".into()
        } else {
            types.format_type_oid(source.oid)?
        };
        return Err(record_mismatch(format!(
            "Returned type {source_name} at ordinal position {}, but query expects {}.",
            index + 1,
            types.format_type(target)?,
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
