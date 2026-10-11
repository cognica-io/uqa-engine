//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `pg_sleep`, `pg_sleep_for` and `pg_sleep_until`: sleep in the session until a time passes, ending at once when the statement is canceled or times out.

use super::{EvalContext, Result, SQLError, Value};
use std::time::Duration;
use uqa_core::{memory::ProductionControl, TemporalValue};

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
    let seconds = sleep_seconds(
        name,
        argument,
        ctx.engine,
        &mut super::clock_timestamp_micros,
    )?;
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

fn sleep_seconds(
    name: &str,
    argument: &Value,
    engine: Option<&dyn super::EngineHook>,
    clock: &mut impl FnMut() -> i64,
) -> Result<f64> {
    match (name, argument) {
        ("pg_sleep", value) => super::conversion::to_f64(value),
        (
            "pg_sleep_for",
            Value::Temporal(TemporalValue::Interval {
                months,
                days,
                micros,
            }),
        ) => {
            let until = super::time::timestamp_tz_plus_interval(
                clock(),
                super::IntervalFields {
                    months: *months,
                    days: *days,
                    micros: *micros,
                },
                engine,
                &ProductionControl::uncontrolled(),
            )?;
            remaining_seconds(until, clock())
        }
        ("pg_sleep_until", Value::Temporal(TemporalValue::TimestampTz { micros })) => {
            remaining_seconds(*micros, clock())
        }
        (_, other) => Err(SQLError::TypeMismatch(format!(
            "{name} does not accept {other:?}"
        ))),
    }
}

fn remaining_seconds(until: i64, now: i64) -> Result<f64> {
    // PostgreSQL's SQL wrappers subtract numeric epoch values, then convert the
    // difference to float8. Raw timestamp subtraction loses infinities and can overflow.
    let control = ProductionControl::uncontrolled();
    let epoch = |micros| {
        super::time::extract_from_value(
            "epoch",
            &Value::Temporal(TemporalValue::TimestampTz { micros }),
            true,
            &control,
        )
    };
    let until = epoch(until)?;
    let now = epoch(now)?;
    let difference = super::eval_binary_values(crate::ast::BinaryOp::Subtract, &until, &now)?;
    super::conversion::to_f64(&difference)
}

#[cfg(test)]
mod tests;
