//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named, positional, and explicit variadic call-argument normalization.

use std::borrow::Cow;

use uqa_core::Value;

use crate::ast::{Expr, FunctionBinding, FunctionDispatch};
use crate::error::{Result, SQLError};

use super::context::EvalContext;
use super::evaluator::eval;

pub(super) fn normalized_function_name(name: &str) -> Cow<'_, str> {
    let stripped = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if stripped.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(stripped.to_ascii_lowercase())
    } else {
        Cow::Borrowed(stripped)
    }
}

fn binding_dispatch(binding: Option<&FunctionBinding>) -> Option<FunctionDispatch> {
    binding.and_then(|binding| binding.dispatch)
}

fn direct_variadic_argument_value(argument: &Expr) -> Option<&Expr> {
    let Expr::Func { binding, args, .. } = argument else {
        return None;
    };
    if binding_dispatch(binding.as_ref()) != Some(FunctionDispatch::VariadicArgument) {
        return None;
    }
    let [value] = args.as_slice() else {
        return None;
    };
    Some(value)
}

fn named_argument_value(argument: &Expr) -> Option<&Expr> {
    let Expr::Func { binding, args, .. } = argument else {
        return None;
    };
    if binding_dispatch(binding.as_ref()) == Some(FunctionDispatch::NamedArgument) {
        args.get(1)
    } else {
        None
    }
}

/// Wrap the last actual argument of an explicit `VARIADIC` invocation while preserving a named-argument marker at the top level.
#[must_use]
pub fn wrap_variadic_argument(mut argument: Expr) -> Expr {
    if variadic_argument_value(&argument).is_some() {
        return argument;
    }
    if let Expr::Func { binding, args, .. } = &mut argument {
        if binding_dispatch(binding.as_ref()) == Some(FunctionDispatch::NamedArgument)
            && args.len() == 2
        {
            let value = args.remove(1);
            args.push(variadic_argument_marker(value));
            return argument;
        }
    }
    variadic_argument_marker(argument)
}

fn variadic_argument_marker(value: Expr) -> Expr {
    let binding = FunctionBinding::dispatched(FunctionDispatch::VariadicArgument);
    Expr::Func {
        name: binding.name.clone(),
        binding: Some(binding),
        args: vec![value],
        distinct: false,
        order_by: Vec::new(),
        filter: None,
    }
}

/// Return the value expression carried by an explicit `VARIADIC` marker, including one nested inside a named argument.
#[must_use]
pub fn variadic_argument_value(argument: &Expr) -> Option<&Expr> {
    let value = named_argument_value(argument).unwrap_or(argument);
    direct_variadic_argument_value(value)
}

/// Return a call argument's value expression after stripping named and explicit `VARIADIC` syntax markers.
#[must_use]
pub fn call_argument_value(argument: &Expr) -> &Expr {
    let value = named_argument_value(argument).unwrap_or(argument);
    direct_variadic_argument_value(value).unwrap_or(value)
}

/// Enforce `PostgreSQL` function-call ordering before overload resolution.
/// Positional arguments must precede named arguments, and each explicit name
/// may occur only once.
pub fn validate_named_argument_order<'a>(
    argument_names: impl IntoIterator<Item = Option<&'a str>>,
) -> Result<()> {
    let mut saw_named = false;
    let mut named = Vec::new();
    for argument_name in argument_names {
        let Some(argument_name) = argument_name else {
            if saw_named {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: "positional argument cannot follow named argument".into(),
                });
            }
            continue;
        };
        saw_named = true;
        if named.contains(&argument_name) {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!("argument name \"{argument_name}\" used more than once"),
            });
        }
        named.push(argument_name);
    }
    Ok(())
}

/// Return the `PostgreSQL` 18 strictness contract for a built-in scalar call when its implemented overload is known.
pub fn evaluate_call_args(
    args: &[Expr],
    ctx: &EvalContext<'_>,
) -> Result<Vec<(Option<String>, Value)>> {
    args.iter()
        .map(|arg| match arg {
            Expr::Func {
                binding,
                args: inner,
                ..
            } if binding_dispatch(binding.as_ref()) == Some(FunctionDispatch::NamedArgument) => {
                let Some(Expr::Literal(Value::Str(arg_name))) = inner.first() else {
                    return Err(SQLError::Internal("named argument without a name".into()));
                };
                let value_expr = inner
                    .get(1)
                    .ok_or_else(|| SQLError::Internal("named argument without a value".into()))?;
                Ok((
                    Some(arg_name.clone()),
                    evaluate_call_argument_value(value_expr, ctx)?,
                ))
            }
            other => Ok((None, evaluate_call_argument_value(other, ctx)?)),
        })
        .collect()
}

fn evaluate_call_argument_value(argument: &Expr, ctx: &EvalContext<'_>) -> Result<Value> {
    if let Expr::Func { binding, args, .. } = argument {
        if binding_dispatch(binding.as_ref()) == Some(FunctionDispatch::VariadicArgument) {
            let [value] = args.as_slice() else {
                return Err(SQLError::Internal(
                    "VARIADIC argument marker must contain one value".into(),
                ));
            };
            return eval(value, ctx);
        }
    }
    eval(argument, ctx)
}
