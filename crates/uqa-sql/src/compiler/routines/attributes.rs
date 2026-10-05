//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The attribute clauses of `CREATE FUNCTION` and `ALTER FUNCTION`, read as the grammar gives them; `PostgreSQL` checks them later, once the routine's schema or the altered routine is known.

use super::super::{extract_string, NodeEnum, Result, SQLError};
use crate::ast::{FunctionParallel, RoutineAttributeClause, RoutineTransformType};
use pg_query::protobuf::{DefElem, TypeName};

/// The clause a `DefElem` of a routine statement writes.
pub(super) fn routine_attribute_clause(
    element: &DefElem,
    keyword: &str,
) -> Result<RoutineAttributeClause> {
    Ok(match element.defname.to_ascii_lowercase().as_str() {
        "as" => RoutineAttributeClause::As,
        "language" => RoutineAttributeClause::Language,
        "transform" => RoutineAttributeClause::Transform,
        "window" => RoutineAttributeClause::Window,
        "volatility" => RoutineAttributeClause::Volatility,
        "strict" => RoutineAttributeClause::Strict,
        "security" => RoutineAttributeClause::Security,
        "leakproof" => RoutineAttributeClause::Leakproof,
        "set" => RoutineAttributeClause::Set,
        "cost" => RoutineAttributeClause::Cost,
        "rows" => RoutineAttributeClause::Rows,
        "support" => RoutineAttributeClause::Support,
        "parallel" => RoutineAttributeClause::Parallel,
        other => {
            return Err(SQLError::Internal(format!(
                "{keyword}: option \"{other}\" not recognized"
            )))
        }
    })
}

/// The number a COST or ROWS clause gives, read as `defGetNumeric` reads it and stored in a `float4`, as `pg_proc` stores it.
pub(super) fn def_elem_float4(element: &DefElem, context: &str) -> Result<f32> {
    let value = match element.arg.as_ref().and_then(|arg| arg.node.as_ref()) {
        Some(NodeEnum::Integer(value)) => f64::from(value.ival),
        Some(NodeEnum::Float(value)) => value.fval.parse::<f64>().map_err(|_| {
            SQLError::Internal(format!("{context} expects a number, got `{}`", value.fval))
        })?,
        other => {
            return Err(SQLError::TypeMismatch(format!(
                "{context} expects a number, got {other:?}"
            )))
        }
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "PostgreSQL rounds COST and ROWS to float4"
    )]
    Ok(value as f32)
}

/// The PARALLEL setting a clause names, or the name itself when it is not SAFE, RESTRICTED or UNSAFE.
pub(super) fn compile_parallel(
    element: &DefElem,
) -> Result<std::result::Result<FunctionParallel, String>> {
    let value = super::def_elem_string(element)?;
    Ok(match value.as_str() {
        "unsafe" => Ok(FunctionParallel::Unsafe),
        "restricted" => Ok(FunctionParallel::Restricted),
        "safe" => Ok(FunctionParallel::Safe),
        _ => Err(value),
    })
}

/// The strings of an AS clause.
pub(super) fn compile_as_items(element: &DefElem, keyword: &str) -> Result<Vec<String>> {
    match element.arg.as_ref().and_then(|arg| arg.node.as_ref()) {
        Some(NodeEnum::List(list)) => list.items.iter().map(extract_string).collect(),
        Some(NodeEnum::String(value)) => Ok(vec![value.sval.clone()]),
        other => Err(SQLError::TypeMismatch(format!(
            "{keyword}: AS expects a string body, got {other:?}"
        ))),
    }
}

/// The types a `TRANSFORM FOR TYPE` clause names.
pub(super) fn compile_transform_types(
    element: &DefElem,
    keyword: &str,
) -> Result<Vec<RoutineTransformType>> {
    let Some(NodeEnum::List(list)) = element.arg.as_ref().and_then(|arg| arg.node.as_ref()) else {
        return Err(SQLError::TypeMismatch(format!(
            "{keyword}: TRANSFORM expects a type list"
        )));
    };
    list.items
        .iter()
        .map(|item| {
            let Some(NodeEnum::TypeName(type_name)) = item.node.as_ref() else {
                return Err(SQLError::TypeMismatch(format!(
                    "{keyword}: TRANSFORM expects type names"
                )));
            };
            Ok(RoutineTransformType {
                type_name: super::compile_function_type_name(type_name)?.name,
                written: written_type_name(type_name)?,
            })
        })
        .collect()
}

/// A type as `TypeNameToString` spells it in messages: its names as written joined by dots, `%TYPE` for a column's type, and `[]` once for any array bounds.
pub(super) fn written_type_name(type_name: &TypeName) -> Result<String> {
    let mut text = type_name
        .names
        .iter()
        .map(extract_string)
        .collect::<Result<Vec<_>>>()?
        .join(".");
    if type_name.pct_type {
        text.push_str("%TYPE");
    }
    if !type_name.array_bounds.is_empty() {
        text.push_str("[]");
    }
    Ok(text)
}
