//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL current date/time values read from the owning execution context.

use super::{age_between, coerce_temporal, EvalContext, Result, SQLError, TemporalValue, Value};

/// Read the platform wall clock as Unix microseconds.
#[must_use]
pub fn clock_timestamp_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

pub(super) fn eval_current_time(
    name: &str,
    args: &[Value],
    context: Option<&EvalContext<'_>>,
) -> Option<Result<Value>> {
    if !(matches!(
        name,
        "now"
            | "transaction_timestamp"
            | "statement_timestamp"
            | "current_timestamp"
            | "current_date"
            | "current_time"
            | "localtime"
            | "localtimestamp"
    ) || name == "age" && args.len() == 1)
    {
        return None;
    }
    Some((|| {
        if name == "age" && matches!(args, [Value::Null]) {
            return Ok(Value::Null);
        }
        if name != "age" && !args.is_empty() {
            return Err(SQLError::BadArity {
                name: name.into(),
                expected: "0".into(),
                actual: args.len(),
            });
        }
        let micros = context
            .and_then(|context| context.engine)
            .and_then(|engine| {
                if name == "statement_timestamp" {
                    engine.statement_timestamp_micros()
                } else {
                    engine.transaction_timestamp_micros()
                }
            })
            .unwrap_or_else(clock_timestamp_micros);
        const MICROS_PER_DAY: i64 = 86_400_000_000;
        let value = match name {
            "current_date" => TemporalValue::Date {
                days: i32::try_from(micros.div_euclid(MICROS_PER_DAY)).map_err(|_| {
                    SQLError::Internal("current date exceeds its day carrier".into())
                })?,
            },
            "current_time" => TemporalValue::TimeTz {
                micros: micros.rem_euclid(MICROS_PER_DAY),
                offset_minutes: 0,
            },
            "localtime" => TemporalValue::Time {
                micros: micros.rem_euclid(MICROS_PER_DAY),
            },
            "localtimestamp" => TemporalValue::Timestamp { micros },
            "age" => {
                return age_between(
                    &TemporalValue::Timestamp {
                        micros: micros.div_euclid(MICROS_PER_DAY) * MICROS_PER_DAY,
                    },
                    &coerce_temporal(&args[0])?,
                );
            }
            _ => TemporalValue::TimestampTz { micros },
        };
        Ok(Value::Temporal(value))
    })())
}
