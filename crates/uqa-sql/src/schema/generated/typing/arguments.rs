//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Decode the call arguments of a stored generation expression: named-argument and `VARIADIC` syntax markers around each value.

use crate::ast::{Expr, FunctionBinding};
use crate::SQLError;
use uqa_core::Value;

#[derive(Debug)]
pub(in crate::schema) struct GeneratedCallArgument<'a> {
    pub(in crate::schema) name: Option<String>,
    pub(in crate::schema) value: &'a Expr,
    pub(in crate::schema) explicit_variadic: bool,
}

pub(in crate::schema) fn generated_call_arguments(
    arguments: &[Expr],
) -> Result<Vec<GeneratedCallArgument<'_>>, SQLError> {
    let decoded = arguments
        .iter()
        .map(generated_call_argument)
        .collect::<Result<Vec<_>, _>>()?;
    let variadic_positions = decoded
        .iter()
        .enumerate()
        .filter_map(|(position, argument)| argument.explicit_variadic.then_some(position))
        .collect::<Vec<_>>();
    if variadic_positions.len() > 1 {
        return Err(malformed_generated_argument(
            "call contains more than one explicit VARIADIC argument",
        ));
    }
    if variadic_positions
        .first()
        .is_some_and(|position| *position + 1 != arguments.len())
    {
        return Err(malformed_generated_argument(
            "explicit VARIADIC argument must be the final call argument",
        ));
    }
    Ok(decoded)
}

fn generated_call_argument(expression: &Expr) -> Result<GeneratedCallArgument<'_>, SQLError> {
    let Expr::Func {
        name,
        args,
        binding,
        distinct,
        order_by,
        filter,
    } = expression
    else {
        return Ok(GeneratedCallArgument {
            name: None,
            value: expression,
            explicit_variadic: false,
        });
    };
    if binding.as_ref().and_then(|binding| binding.dispatch)
        == Some(crate::ast::FunctionDispatch::NamedArgument)
    {
        validate_generated_marker(
            binding.as_ref(),
            crate::ast::FunctionDispatch::NamedArgument,
            *distinct,
            order_by,
            filter.as_deref(),
            name,
        )?;
        let [Expr::Literal(Value::Str(argument_name)), value] = args.as_slice() else {
            return Err(malformed_generated_argument(
                "named argument marker must contain a string name and one value",
            ));
        };
        let (value, explicit_variadic) = generated_variadic_argument(value)?;
        if !explicit_variadic
            && matches!(
                value,
                Expr::Func { binding, .. }
                    if binding.as_ref().and_then(|binding| binding.dispatch)
                        == Some(crate::ast::FunctionDispatch::NamedArgument)
            )
        {
            return Err(malformed_generated_argument(
                "call argument contains nested syntax markers",
            ));
        }
        return Ok(GeneratedCallArgument {
            name: Some(argument_name.clone()),
            value,
            explicit_variadic,
        });
    }
    let (value, explicit_variadic) = generated_variadic_argument(expression)?;
    Ok(GeneratedCallArgument {
        name: None,
        value,
        explicit_variadic,
    })
}

fn generated_variadic_argument(expression: &Expr) -> Result<(&Expr, bool), SQLError> {
    let Expr::Func {
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
        != Some(crate::ast::FunctionDispatch::VariadicArgument)
    {
        return Ok((expression, false));
    }
    validate_generated_marker(
        binding.as_ref(),
        crate::ast::FunctionDispatch::VariadicArgument,
        *distinct,
        order_by,
        filter.as_deref(),
        name,
    )?;
    let [value] = args.as_slice() else {
        return Err(malformed_generated_argument(
            "VARIADIC argument marker must contain exactly one value",
        ));
    };
    if matches!(
        value,
        Expr::Func { binding, .. }
            if matches!(
                binding.as_ref().and_then(|binding| binding.dispatch),
                Some(
                    crate::ast::FunctionDispatch::VariadicArgument
                        | crate::ast::FunctionDispatch::NamedArgument
                )
            )
    ) {
        return Err(malformed_generated_argument(
            "call argument contains nested syntax markers",
        ));
    }
    Ok((value, true))
}

fn validate_generated_marker(
    binding: Option<&FunctionBinding>,
    expected_dispatch: crate::ast::FunctionDispatch,
    distinct: bool,
    order_by: &[crate::ast::OrderBy],
    filter: Option<&Expr>,
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
        return Err(malformed_generated_argument(&format!(
            "{name} syntax marker contains function-call metadata"
        )));
    }
    Ok(())
}

fn malformed_generated_argument(message: &str) -> SQLError {
    SQLError::TypeMismatch(format!(
        "malformed generated-column call argument: {message}"
    ))
}
