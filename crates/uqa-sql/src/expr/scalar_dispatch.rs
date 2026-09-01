//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stateful sequence dispatch and built-in function-family routing.

use super::{to_i64, value_to_string, EvalContext, Result, SQLError, Value};

type ScalarFunctionDispatcher = fn(&str, &[Value]) -> Option<Result<Value>>;

// -------------------------------------------------------------------------
// Built-in scalar functions
// -------------------------------------------------------------------------

/// Dispatch table for built-in scalar SQL functions. Function
/// names are lower-cased before lookup.
pub(super) fn eval_sequence_function(
    name: &str,
    args: &[Value],
    ctx: &EvalContext<'_>,
) -> Result<Value> {
    let engine = ctx.engine.ok_or_else(|| {
        SQLError::Unsupported(format!(
            "sequence function `{name}` requires an engine hook on the EvalContext"
        ))
    })?;
    let (valid_arity, expected) = match name {
        "nextval" | "currval" => (args.len() == 1, "1"),
        "lastval" => (args.is_empty(), "0"),
        "setval" => (matches!(args.len(), 2 | 3), "2 or 3"),
        other => {
            return Err(SQLError::Unsupported(format!(
                "unknown sequence function `{other}`"
            )));
        }
    };
    if !valid_arity {
        return Err(SQLError::BadArity {
            name: name.to_string(),
            expected: expected.into(),
            actual: args.len(),
        });
    }
    if args.iter().any(|argument| matches!(argument, Value::Null)) {
        return Ok(Value::Null);
    }
    let value = match name {
        "nextval" => engine.nextval(&value_to_string(&args[0])),
        "currval" => engine.currval(&value_to_string(&args[0])),
        "lastval" => engine.lastval(),
        "setval" => {
            let seq_name = value_to_string(&args[0]);
            let n = to_i64(&args[1])?;
            let is_called = match args.get(2) {
                None => true,
                Some(Value::Bool(is_called)) => *is_called,
                Some(value) => {
                    return Err(SQLError::TypeMismatch(format!(
                        "setval requires a boolean third argument, got {value:?}"
                    )));
                }
            };
            engine.setval(&seq_name, n, is_called)
        }
        _ => unreachable!("sequence function name was validated above"),
    }?;
    Ok(Value::Int(value))
}

pub(super) fn eval_scalar_function(name: &str, args: &[Value]) -> Result<Value> {
    let name = name.strip_prefix("pg_catalog.").unwrap_or(name);
    if super::builtin_scalar_function_strictness(name, args.len()) == Some(true)
        && args.iter().any(|argument| matches!(argument, Value::Null))
    {
        return Ok(Value::Null);
    }
    let dispatchers: &[ScalarFunctionDispatcher] = &[
        super::scalar_core::eval_core_functions,
        super::scalar_math::eval_math_functions,
        super::scalar_temporal::eval_temporal_functions,
        super::scalar_json::eval_json_functions,
        super::scalar_array::eval_array_functions,
        super::scalar_postgres::eval_postgres_functions,
        super::scalar_range::eval_range_functions,
        super::scalar_geospatial::eval_geospatial_functions,
    ];
    for dispatch in dispatchers {
        if let Some(result) = dispatch(name, args) {
            return result;
        }
    }
    Err(SQLError::UnknownFunction(name.to_string()))
}
