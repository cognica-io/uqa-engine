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
    let expected = match name {
        "nextval" | "currval" => 1,
        "setval" => 2,
        other => {
            return Err(SQLError::Unsupported(format!(
                "unknown sequence function `{other}`"
            )));
        }
    };
    if args.len() != expected {
        return Err(SQLError::BadArity {
            name: name.to_string(),
            expected: expected.to_string(),
            actual: args.len(),
        });
    }
    let seq_name = value_to_string(&args[0]);
    let result: std::result::Result<i64, String> = match name {
        "nextval" => engine.nextval(&seq_name),
        "currval" => engine.currval(&seq_name),
        "setval" => {
            let n = to_i64(&args[1])?;
            engine.setval(&seq_name, n)
        }
        _ => unreachable!("sequence function name was validated above"),
    };
    let v = result.map_err(SQLError::Unsupported)?;
    Ok(Value::Int(v))
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
        super::scalar_geospatial::eval_geospatial_functions,
    ];
    for dispatch in dispatchers {
        if let Some(result) = dispatch(name, args) {
            return result;
        }
    }
    Err(SQLError::UnknownFunction(name.to_string()))
}
