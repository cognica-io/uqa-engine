//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL setting lookup semantics over a host-owned logical session.

use super::{EvalContext, Result, SQLError, Value};

pub(super) fn current_setting(args: &[Value], ctx: &EvalContext<'_>) -> Result<Value> {
    if !matches!(args.len(), 1 | 2) {
        return Err(SQLError::BadArity {
            name: "current_setting".into(),
            expected: "1 or 2".into(),
            actual: args.len(),
        });
    }
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return Ok(Value::Null);
    }
    let (name, missing_ok) = match args {
        [Value::Str(name)] => (name, false),
        [Value::Str(name), Value::Bool(missing_ok)] => (name, *missing_ok),
        _ => {
            return Err(SQLError::TypeMismatch(
                "current_setting requires text and an optional boolean".into(),
            ));
        }
    };
    let engine = ctx.engine.ok_or_else(|| {
        SQLError::Unsupported("current_setting requires a logical engine session".into())
    })?;
    match engine.runtime_parameter(name)? {
        Some(value) => Ok(Value::Str(value)),
        None if missing_ok => Ok(Value::Null),
        None => Err(SQLError::Routine {
            sqlstate: "42704".into(),
            message: format!("unrecognized configuration parameter \"{name}\""),
        }),
    }
}

#[cfg(test)]
mod tests;
