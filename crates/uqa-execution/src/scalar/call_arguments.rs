//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compiler-owned named and explicit variadic argument validation.

use uqa_core::Value;
use uqa_sql::ast::{FunctionBinding, FunctionDispatch};
use uqa_sql::SQLError;

use super::{eval_scalar, ScalarEvalContext, ScalarExpr, ScalarOrder};

pub fn eval_call_arguments(
    arguments: &[ScalarExpr],
    context: &ScalarEvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>, SQLError> {
    scalar_call_arguments(arguments)?
        .into_iter()
        .map(|argument| {
            Ok((
                argument.name.map(str::to_string),
                eval_scalar(argument.value, context)?,
            ))
        })
        .collect()
}

/// A physical call argument after removing the compiler's named and explicit `VARIADIC` syntax markers.
#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScalarCallArgument<'a> {
    pub name: Option<&'a str>,
    pub value: &'a ScalarExpr,
    pub explicit_variadic: bool,
}

/// Decode and validate all compiler-owned call-argument markers. `PostgreSQL` permits one explicit `VARIADIC` argument and requires it to be the final argument.
#[doc(hidden)]
pub fn scalar_call_arguments(
    arguments: &[ScalarExpr],
) -> Result<Vec<ScalarCallArgument<'_>>, SQLError> {
    let mut decoded = Vec::with_capacity(arguments.len());
    for argument in arguments {
        decoded.push(scalar_call_argument(argument)?);
    }
    validate_scalar_call_arguments(&decoded)?;
    Ok(decoded)
}

/// Validate cross-argument invariants after individual syntax markers have been decoded, returning whether the call used explicit `VARIADIC` syntax.
#[doc(hidden)]
pub fn validate_scalar_call_arguments(
    arguments: &[ScalarCallArgument<'_>],
) -> Result<bool, SQLError> {
    let variadic_positions = arguments
        .iter()
        .enumerate()
        .filter_map(|(position, argument)| argument.explicit_variadic.then_some(position))
        .collect::<Vec<_>>();
    if variadic_positions.len() > 1 {
        return Err(malformed_call_argument(
            "call contains more than one explicit VARIADIC argument",
        ));
    }
    if variadic_positions
        .first()
        .is_some_and(|position| position + 1 != arguments.len())
    {
        return Err(malformed_call_argument(
            "explicit VARIADIC argument must be the final call argument",
        ));
    }
    Ok(!variadic_positions.is_empty())
}

/// Decode one compiler-owned call-argument marker. Use [`scalar_call_arguments`] for a complete call so duplicate and ordering invariants are also checked.
#[doc(hidden)]
pub fn scalar_call_argument(expression: &ScalarExpr) -> Result<ScalarCallArgument<'_>, SQLError> {
    let ScalarExpr::Func {
        name,
        args,
        binding,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        return Ok(ScalarCallArgument {
            name: None,
            value: expression,
            explicit_variadic: false,
        });
    };
    if binding.as_ref().and_then(|binding| binding.dispatch)
        == Some(FunctionDispatch::NamedArgument)
    {
        validate_marker_shape(
            binding.as_ref(),
            FunctionDispatch::NamedArgument,
            *distinct,
            order_by,
            filter.as_deref(),
            name,
        )?;
        let [ScalarExpr::Literal(Value::Str(argument_name)), value] = args.as_slice() else {
            return Err(malformed_call_argument(
                "named argument marker must contain a string name and one value",
            ));
        };
        let (value, explicit_variadic) = direct_variadic_argument(value)?;
        if !explicit_variadic
            && matches!(
                value,
                ScalarExpr::Func { binding, .. }
                    if binding.as_ref().and_then(|binding| binding.dispatch)
                        == Some(FunctionDispatch::NamedArgument)
            )
        {
            return Err(malformed_call_argument(
                "call argument contains nested syntax markers",
            ));
        }
        return Ok(ScalarCallArgument {
            name: Some(argument_name),
            value,
            explicit_variadic,
        });
    }
    let (value, explicit_variadic) = direct_variadic_argument(expression)?;
    Ok(ScalarCallArgument {
        name: None,
        value,
        explicit_variadic,
    })
}

fn direct_variadic_argument(expression: &ScalarExpr) -> Result<(&ScalarExpr, bool), SQLError> {
    let ScalarExpr::Func {
        name,
        args,
        binding,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        return Ok((expression, false));
    };
    if binding.as_ref().and_then(|binding| binding.dispatch)
        != Some(FunctionDispatch::VariadicArgument)
    {
        return Ok((expression, false));
    }
    validate_marker_shape(
        binding.as_ref(),
        FunctionDispatch::VariadicArgument,
        *distinct,
        order_by,
        filter.as_deref(),
        name,
    )?;
    let [value] = args.as_slice() else {
        return Err(malformed_call_argument(
            "VARIADIC argument marker must contain exactly one value",
        ));
    };
    if matches!(
        value,
        ScalarExpr::Func { binding, .. }
            if matches!(
                binding.as_ref().and_then(|binding| binding.dispatch),
                Some(FunctionDispatch::VariadicArgument | FunctionDispatch::NamedArgument)
            )
    ) {
        return Err(malformed_call_argument(
            "call argument contains nested syntax markers",
        ));
    }
    Ok((value, true))
}

fn validate_marker_shape(
    binding: Option<&FunctionBinding>,
    expected_dispatch: FunctionDispatch,
    distinct: bool,
    order_by: &[ScalarOrder],
    filter: Option<&ScalarExpr>,
    name: &str,
) -> Result<(), SQLError> {
    if binding.is_none_or(|binding| {
        !binding.builtin
            || binding.dispatch != Some(expected_dispatch)
            || !binding.argument_types.is_empty()
            || binding.invocation.is_some()
            || binding.resolution_error.is_some()
    }) || distinct
        || !order_by.is_empty()
        || filter.is_some()
    {
        return Err(malformed_call_argument(&format!(
            "{name} syntax marker contains function-call metadata"
        )));
    }
    Ok(())
}

fn malformed_call_argument(message: &str) -> SQLError {
    SQLError::Internal(format!("malformed call argument: {message}"))
}
