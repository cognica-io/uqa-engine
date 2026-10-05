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
    fn temporal_coercion_uses_the_transaction_clock() {
        let _clock = TransactionClockScope::enter(90_123_456_789);
        for (input, micros) in [
            ("now", 90_123_456_789),
            ("today", 86_400_000_000),
            ("tomorrow", 172_800_000_000),
            ("yesterday", 0),
        ] {
            assert_eq!(
                coerce_temporal(&Value::Str(input.into())).unwrap(),
                TemporalValue::Timestamp { micros },
                "{input}"
            );
        }
    }

    #[test]
    fn temporal_comparisons_use_the_transaction_clock() {
        use crate::expr::{compare_with_control, values_equal_with_control};
        use uqa_core::memory::ProductionControl;
        let _clock = TransactionClockScope::enter(90_123_456_789);
        let control = ProductionControl::uncontrolled();
        let now = Value::Str("now".into());
        for temporal in [
            TemporalValue::Date { days: 1 },
            TemporalValue::Time {
                micros: 3_723_456_789,
            },
            TemporalValue::TimeTz {
                micros: 3_723_456_789,
                offset_minutes: 0,
            },
            TemporalValue::Timestamp {
                micros: 90_123_456_789,
            },
            TemporalValue::TimestampTz {
                micros: 90_123_456_789,
            },
        ] {
            let value = Value::Temporal(temporal);
            for (left, right) in [(&value, &now), (&now, &value)] {
                assert!(values_equal_with_control(left, right, &control).unwrap());
                assert_eq!(
                    compare_with_control(left, right, &control).unwrap(),
                    std::cmp::Ordering::Equal
                );
            }
        }
    }

    #[test]
    fn temporal_range_bounds_use_the_transaction_clock() {
        use crate::ast::RangeSubtype;
        use crate::expr::parse_range;
        let _clock = TransactionClockScope::enter(90_123_456_789);
        for (kind, expected) in [
            (
                RangeSubtype::Timestamp,
                TemporalValue::Timestamp {
                    micros: 90_123_456_789,
                },
            ),
            (
                RangeSubtype::TimestampTz,
                TemporalValue::TimestampTz {
                    micros: 90_123_456_789,
                },
            ),
        ] {
            let range = parse_range("[now,now]", kind).unwrap();
            assert_eq!(range.lower(), Some(&Value::Temporal(expected.clone())));
            assert_eq!(range.upper(), Some(&Value::Temporal(expected)));
            assert!(!range.is_empty());
        }
        let range = parse_range("[today,tomorrow)", RangeSubtype::Date).unwrap();
        assert_eq!(
            range.lower(),
            Some(&Value::Temporal(TemporalValue::Date { days: 1 }))
        );
        assert_eq!(
            range.upper(),
            Some(&Value::Temporal(TemporalValue::Date { days: 2 }))
        );
    }

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
