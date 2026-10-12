//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate a containing call after its original children have been analyzed.

use super::functions::{
    validate_ordered_function, validate_scalar_function, validate_window_function,
    ScalarFunctionValidation,
};
use crate::{
    routines::RoutineResolution, schema::ScalarTypeSchema, FunctionTypeResolver, SQLError,
    SQLParam, ScalarExpr,
};

pub(in crate::binding) fn validate_call(
    routines: &dyn RoutineResolution,
    expression: &ScalarExpr,
    schema: &dyn ScalarTypeSchema,
    params: &[SQLParam],
    resolver: &dyn FunctionTypeResolver,
) -> Result<(), SQLError> {
    match expression {
        ScalarExpr::Func {
            order_syntax,
            name,
            binding,
            args,
            distinct,
            order_by,
            filter,
        } => {
            let within_group = super::super::ordered_calls::uses_ordered_arguments(
                *order_syntax,
                name,
                binding.as_ref(),
                order_by.len(),
            );
            if within_group || super::super::ordered_calls::is_ordered_set(name) {
                return validate_ordered_function(
                    routines,
                    ScalarFunctionValidation {
                        name,
                        binding: binding.as_ref(),
                        args,
                        order_by,
                        expression,
                        schema,
                        params,
                        resolver,
                    },
                    within_group,
                    *distinct,
                    filter.is_some(),
                );
            }
            validate_scalar_function(
                routines,
                ScalarFunctionValidation {
                    name,
                    binding: binding.as_ref(),
                    args,
                    order_by,
                    expression,
                    schema,
                    params,
                    resolver,
                },
            )?;
            if crate::semantics::is_builtin_aggregate(expression)
                || routines.has_registered_aggregate_function(name)
            {
                return Ok(());
            }
            // `ParseFuncOrColumn` names the first aggregate modifier an ordinary function cannot take.
            let modifier = if *distinct {
                Some("DISTINCT")
            } else if !order_by.is_empty() {
                Some("ORDER BY")
            } else if filter.is_some() {
                Some("FILTER")
            } else {
                None
            };
            modifier.map_or(Ok(()), |modifier| {
                Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!(
                        "{modifier} specified, but {name} is not an aggregate function"
                    ),
                })
            })
        }
        ScalarExpr::WindowCall {
            name,
            args,
            filter,
            modifiers,
            ..
        } => validate_window_function(
            routines,
            name,
            (args, filter.is_some(), *modifiers),
            schema,
            params,
            resolver,
        ),
        _ => Err(SQLError::Internal(
            "call validation requires a function".into(),
        )),
    }
}
