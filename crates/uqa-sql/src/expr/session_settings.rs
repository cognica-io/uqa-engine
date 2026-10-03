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

/// `set_config(name, value, is_local)`: a NULL name is an error, a NULL value restores the reset setting and a NULL `is_local` assigns for the session.
pub(super) fn set_config(args: &[Value], ctx: &EvalContext<'_>) -> Result<Value> {
    let [name, value, local] = args else {
        return Err(SQLError::BadArity {
            name: "set_config".into(),
            expected: "3".into(),
            actual: args.len(),
        });
    };
    let name = match name {
        Value::Null => {
            return Err(SQLError::Routine {
                sqlstate: "22004".into(),
                message: "SET requires parameter name".into(),
            })
        }
        Value::Str(name) => name,
        _ => {
            return Err(SQLError::TypeMismatch(
                "set_config requires text, text and boolean".into(),
            ))
        }
    };
    let value = match value {
        Value::Null => None,
        Value::Str(value) => Some(value.as_str()),
        _ => {
            return Err(SQLError::TypeMismatch(
                "set_config requires text, text and boolean".into(),
            ))
        }
    };
    let local = match local {
        Value::Null => false,
        Value::Bool(local) => *local,
        _ => {
            return Err(SQLError::TypeMismatch(
                "set_config requires text, text and boolean".into(),
            ))
        }
    };
    let engine = ctx.engine.ok_or_else(|| {
        SQLError::Unsupported("set_config requires a logical engine session".into())
    })?;
    engine
        .set_runtime_parameter(name, value, local)
        .map(Value::Str)
}

#[cfg(test)]
mod tests;
