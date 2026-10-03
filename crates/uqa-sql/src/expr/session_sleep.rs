//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_sleep`, `pg_sleep_for` and `pg_sleep_until`: sleep in the session until a time passes, ending at once when the statement is canceled or times out.

use super::{EvalContext, Result, SQLError, Value};
use std::time::Duration;
use uqa_core::TemporalValue;

/// Evaluate a sleep function, or `None` for another name.
pub(super) fn eval_session_sleep(
    name: &str,
    args: &[Value],
    ctx: &EvalContext<'_>,
) -> Option<Result<Value>> {
    if !matches!(name, "pg_sleep" | "pg_sleep_for" | "pg_sleep_until") {
        return None;
    }
    Some(sleep(name, args, ctx))
}

fn sleep(name: &str, args: &[Value], ctx: &EvalContext<'_>) -> Result<Value> {
    let [argument] = args else {
        return Err(SQLError::BadArity {
            name: name.into(),
            expected: "1".into(),
            actual: args.len(),
        });
    };
    if matches!(argument, Value::Null) {
        return Ok(Value::Null);
    }
    let seconds = match (name, argument) {
        ("pg_sleep", value) => super::conversion::to_f64(value)?,
        (
            "pg_sleep_for",
            Value::Temporal(TemporalValue::Interval {
                months,
                days,
                micros,
            }),
        ) => {
            let now = super::current_time::clock_timestamp_micros();
            let until = super::time::timestamp_plus_interval(now, *months, *days, *micros)?;
            (until - now) as f64 / 1_000_000.0
        }
        ("pg_sleep_until", Value::Temporal(TemporalValue::TimestampTz { micros })) => {
            (*micros - super::current_time::clock_timestamp_micros()) as f64 / 1_000_000.0
        }
        (_, other) => {
            return Err(SQLError::TypeMismatch(format!(
                "{name} does not accept {other:?}"
            )))
        }
    };
    let duration = if seconds > 0.0 {
        Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
    } else {
        Duration::ZERO
    };
    let engine = ctx.engine.ok_or_else(|| {
        SQLError::Unsupported(format!("{name} requires a logical engine session"))
    })?;
    engine.sleep(duration)?;
    Ok(Value::Void)
}
