//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL current date/time values read from the owning execution context, and the transaction clock the thread's statement runs under.

use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

use super::{age_between, coerce_temporal, EvalContext, Result, SQLError, TemporalValue, Value};

thread_local! {
    /// The start of the transaction whose statement the current thread executes, in Unix microseconds, while a [`TransactionClockScope`] is entered.
    static TRANSACTION_CLOCK: Cell<Option<i64>> = const { Cell::new(None) };
}

/// Read the platform wall clock as Unix microseconds.
#[must_use]
pub fn clock_timestamp_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

/// The transaction start entered for the current thread, or `None` outside any statement.
#[must_use]
pub fn transaction_clock_micros() -> Option<i64> {
    TRANSACTION_CLOCK.get()
}

/// The transaction start the special date and time inputs `now`, `today`, `tomorrow` and `yesterday` resolve against, as `GetCurrentTransactionStartTimestamp` supplies it to the input functions; an evaluation outside any statement reads the wall clock.
#[must_use]
pub(crate) fn transaction_timestamp_or_clock() -> i64 {
    transaction_clock_micros().unwrap_or_else(clock_timestamp_micros)
}

/// Enters a transaction start for the current thread until it drops, restoring the clock entered before it. The engine enters its transaction timestamp at every statement boundary, and the parallel executor enters the dispatching thread's clock on each worker.
pub struct TransactionClockScope {
    previous: Option<i64>,
    _thread: PhantomData<Rc<()>>,
}

impl TransactionClockScope {
    #[must_use]
    pub fn enter(micros: i64) -> Self {
        Self {
            previous: TRANSACTION_CLOCK.replace(Some(micros)),
            _thread: PhantomData,
        }
    }
}

impl Drop for TransactionClockScope {
    fn drop(&mut self) {
        TRANSACTION_CLOCK.set(self.previous);
    }
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
        let statement = name == "statement_timestamp";
        let micros = context
            .and_then(|context| context.engine)
            .and_then(|engine| {
                if statement {
                    engine.statement_timestamp_micros()
                } else {
                    engine.transaction_timestamp_micros()
                }
            })
            .or_else(|| (!statement).then(transaction_clock_micros).flatten())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_clock_scopes_nest_and_restore() {
        assert_eq!(transaction_clock_micros(), None);
        {
            let _outer = TransactionClockScope::enter(10);
            assert_eq!(transaction_clock_micros(), Some(10));
            assert_eq!(transaction_timestamp_or_clock(), 10);
            {
                let _inner = TransactionClockScope::enter(20);
                assert_eq!(transaction_clock_micros(), Some(20));
            }
            assert_eq!(transaction_clock_micros(), Some(10));
        }
        assert_eq!(transaction_clock_micros(), None);
        assert!(transaction_timestamp_or_clock() > 1_700_000_000_000_000);
    }
}
