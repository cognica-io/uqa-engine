//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A projected routine selects and materializes one invocation before execution.

use super::{
    context::{RoutineCallRetention, RoutineInvocationContext},
    handlers::{
        body_descriptions, execute_resolved_scalar_function, execute_resolved_table_function,
    },
    resolution::{resolve_described_bound_routine, resolve_routine},
};
use crate::functions::SQLTableFunctionResult;
use uqa_core::Value;
use uqa_sql::{ast::FunctionBinding, SQLError};

pub enum RoutineProjectionResult {
    Scalar(Value),
    Table(SQLTableFunctionResult),
}

pub fn has_described_user_function(
    lookup: &dyn uqa_sql::routines::RoutineResolution,
    name: &str,
    binding: Option<&FunctionBinding>,
    retained: RoutineCallRetention<'_>,
) -> Result<bool, SQLError> {
    Ok(match binding {
        Some(binding) if binding.builtin => false,
        Some(binding) => {
            let current = (retained.scope)();
            if current
                .as_ref()
                .and_then(|scope| retained.descriptions.get(binding, scope))
                .and_then(
                    uqa_sql::routines::invocation::descriptions::RoutineCallDescription::function,
                )
                .is_some()
            {
                return Ok(true);
            }
            lookup
                .lookup_bound_sql_functions_by_binding(binding)
                .is_some()
        }
        None => lookup.lookup_visible_sql_functions(name)?.is_some(),
    })
}

pub fn call_user_projection_function(
    context: &RoutineInvocationContext<'_>,
    name: &str,
    binding: Option<&FunctionBinding>,
    args: &[(Option<String>, Value)],
    retained: Option<RoutineCallRetention<'_>>,
) -> Option<Result<RoutineProjectionResult, SQLError>> {
    let selected = match binding {
        Some(binding) => resolve_described_bound_routine(context, binding, args, retained),
        None => resolve_routine(context, name, args, None, "function", false),
    };
    let resolved = match selected {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return None,
        Err(error) => return Some(Err(error)),
    };
    let descriptions = binding
        .and_then(|binding| body_descriptions(context.runtime.expressions, binding, retained));
    Some(if resolved.function.def.returns_set() {
        execute_resolved_table_function(context, name, args, resolved, None, descriptions)
            .map(RoutineProjectionResult::Table)
    } else {
        execute_resolved_scalar_function(context, name, args, resolved, descriptions)
            .map(RoutineProjectionResult::Scalar)
    })
}
