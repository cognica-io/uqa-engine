//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invoke a catalog-bound validator as an internal callback, preserving the caller's transaction and function state.

use super::context::RoutineInvocationContext;
use uqa_core::Value;
use uqa_sql::{
    ast::ColumnType, catalog::foreign_wrapper::ForeignWrapperFunction,
    routines::resolution::RoutineCallKind, SQLError,
};

/// The caller has authorized the catalog operation and retained the validator identity. `PostgreSQL`'s `OidFunctionCall2` does not recheck the routine's EXECUTE ACL or change the invoking role.
pub fn call_catalog_validator(
    context: &RoutineInvocationContext<'_>,
    function: &ForeignWrapperFunction,
    values: &[Value; 2],
) -> Result<(), SQLError> {
    let binding = &function.binding;
    if !binding.builtin
        && context
            .lookup
            .lookup_bound_sql_functions_by_binding(binding)
            .is_none()
    {
        return Err(SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!("cache lookup failed for function {}", function.oid),
        });
    }
    let types = [
        Some(ColumnType::Array(Box::new(ColumnType::Text))),
        Some(ColumnType::Oid),
    ];
    let matched = context
        .overloads
        .resolve_static_sql_routine_match(
            &binding.name,
            Some(binding),
            &[None, None],
            &types,
            false,
            RoutineCallKind::Function,
        )?
        .ok_or_else(|| SQLError::Routine {
            sqlstate: "XX000".into(),
            message: format!("cache lookup failed for function {}", function.oid),
        })?;
    if matched.function.def.returns_set() {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "set-valued function called in context that cannot accept a set".into(),
        });
    }
    let arguments = [(None, values[0].clone()), (None, values[1].clone())];
    let bound = crate::routines::arguments::materialize_arguments(
        context.runtime.expressions,
        &matched.function.def,
        &matched.invocation,
        &arguments,
    )?;
    if matched.function.def.strict && bound.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(());
    }
    super::execution::execute_catalog_routine(
        context,
        &matched.function,
        bound,
        &matched.invocation,
    )?;
    Ok(())
}
