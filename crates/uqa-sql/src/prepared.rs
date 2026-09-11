//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Prepared definitions, result descriptors, and SQL EXECUTE argument contracts.

use crate::{ColumnType, RowSchema, SQLError, SQLParam};
use uqa_core::Value;
pub mod arguments;

pub fn declared_parameter_types(
    resolver: &dyn crate::FunctionTypeResolver,
    logical_plan: &mut crate::plan::UnifiedPlan,
    declared: &[ColumnType],
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let mut parameter_types = declared
        .iter()
        .map(|ty| match ty {
            crate::ast::ColumnType::Named(name) if is_unknown_type(name)? => Ok(None),
            _ => crate::type_resolution::resolve_declared_column_type(resolver, ty).map(Some),
        })
        .collect::<Result<Vec<_>, _>>()?;
    logical_plan.rewrite_scalar_expressions(&mut |expression| {
        if let crate::ScalarExpr::Param(index) = expression {
            parameter_types.resize(parameter_types.len().max(*index), None);
        }
    });
    Ok(parameter_types)
}

fn is_unknown_type(name: &str) -> Result<bool, crate::SQLError> {
    Ok(crate::parse_regtype_name(name)?.is_some_and(|parsed| {
        parsed.array_dimensions == 0
            && !parsed.has_type_modifiers
            && match parsed.names.as_slice() {
                [local] => local == "unknown",
                [schema, local] => schema == "pg_catalog" && local == "unknown",
                _ => false,
            }
    }))
}

pub fn analyze_prepared_plan(
    routines: &dyn crate::routines::RoutineResolution,
    plan: &crate::plan::UnifiedPlan,
    parameter_types: &[Option<ColumnType>],
    scope: &crate::binding::context::BindingContext<'_>,
) -> Result<Option<RowSchema>, SQLError> {
    let params = parameter_types
        .iter()
        .map(|ty| match ty {
            Some(ty) => SQLParam::typed_scalar(Value::Null, ty.clone()),
            None => SQLParam::Scalar(Value::Null),
        })
        .collect::<Vec<_>>();
    match plan {
        crate::plan::UnifiedPlan::Query(query) => {
            crate::binding::analyze_query_plan_schema(routines, query, &params, scope, None)
                .map(Some)
        }
        crate::plan::UnifiedPlan::Command(command) => {
            crate::binding::analyze_prepared_command_schema(routines, command, &params, scope)
        }
    }
}

pub fn prepared_result_schema_matches(left: Option<&RowSchema>, right: Option<&RowSchema>) -> bool {
    match (left, right) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            left.columns() == right.columns()
                && left.column_types().len() == right.column_types().len()
                && left
                    .column_types()
                    .iter()
                    .zip(right.column_types())
                    .all(|(left, right)| {
                        prepared_type_identity(left.as_ref())
                            == prepared_type_identity(right.as_ref())
                    })
        }
        _ => false,
    }
}

fn prepared_type_identity(ty: Option<&ColumnType>) -> Option<(u32, i32)> {
    ty.map(|ty| {
        if let ColumnType::Domain { oid, .. } = ty {
            (*oid, -1)
        } else {
            let metadata = crate::catalog::result_type::postgres_result_type(ty);
            (metadata.type_oid, metadata.type_modifier)
        }
    })
}

pub fn statement_error(sqlstate: &str, name: &str, reason: &str) -> SQLError {
    error(sqlstate, format!("prepared statement \"{name}\" {reason}"))
}

pub fn execute_parameter_types(
    name: &str,
    types: Option<Vec<Option<ColumnType>>>,
    argument_count: usize,
) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let types = types.ok_or_else(|| statement_error("26000", name, "does not exist"))?;
    // SQL EXECUTE ignores argument lists for parameterless definitions; protocol Bind checks its own arity.
    if types.is_empty() {
        return Ok(Vec::new());
    }
    if argument_count != types.len() {
        return Err(error(
            "42601",
            format!("wrong number of parameters for prepared statement \"{name}\""),
        ));
    }
    Ok(types)
}

pub(super) fn error(sqlstate: &str, message: impl Into<String>) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
