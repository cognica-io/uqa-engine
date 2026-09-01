//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Date, time, timestamp, and interval conversion.

use uqa_core::{TemporalValue, Value};

use crate::error::{Result, SQLError};

use super::{canonical_cast_source, undefined_cast, value_to_string};

#[derive(Clone, Copy)]
pub(super) enum TemporalCastTarget {
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
}

pub(super) fn cast_temporal(
    v: &Value,
    target: TemporalCastTarget,
    parse: fn(&str) -> Option<TemporalValue>,
    ty: &str,
) -> Result<Value> {
    match v {
        Value::Temporal(value) => cast_temporal_kind(value, target)
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
        other => parse(&value_to_string(other))
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
    }
}

pub(super) fn cast_date(v: &Value, source_ty: Option<&str>) -> Result<Value> {
    match v {
        Value::Temporal(value) => cast_temporal_kind(value, TemporalCastTarget::Date)
            .map(Value::Temporal)
            .ok_or_else(|| undefined_cast(&canonical_cast_source(source_ty, v), "date")),
        Value::Str(text) | Value::FixedChar(text) => TemporalValue::try_parse_date(text)
            .map(Value::Temporal)
            .map_err(|error| {
                let field_overflow = matches!(
                    error.kind(),
                    chrono::format::ParseErrorKind::OutOfRange
                        | chrono::format::ParseErrorKind::Impossible
                );
                SQLError::Routine {
                    sqlstate: if field_overflow { "22008" } else { "22007" }.into(),
                    message: if field_overflow {
                        format!("date/time field value out of range: \"{text}\"")
                    } else {
                        format!("invalid input syntax for type date: \"{text}\"")
                    },
                }
            }),
        _ => Err(undefined_cast(&canonical_cast_source(source_ty, v), "date")),
    }
}

fn cast_temporal_kind(value: &TemporalValue, target: TemporalCastTarget) -> Option<TemporalValue> {
    const MICROS_PER_DAY: i64 = 86_400_000_000;
    match (target, value) {
        (TemporalCastTarget::Date, TemporalValue::Date { days }) => {
            Some(TemporalValue::Date { days: *days })
        }
        (
            TemporalCastTarget::Date,
            TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros },
        ) => Some(TemporalValue::Date {
            days: i32::try_from(micros.div_euclid(MICROS_PER_DAY)).ok()?,
        }),
        (TemporalCastTarget::Time, TemporalValue::Time { micros })
        | (TemporalCastTarget::Time, TemporalValue::TimeTz { micros, .. })
        | (
            TemporalCastTarget::Time,
            TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros },
        )
        | (TemporalCastTarget::Time, TemporalValue::Interval { micros, .. }) => {
            Some(TemporalValue::Time {
                micros: micros.rem_euclid(MICROS_PER_DAY),
            })
        }
        (
            TemporalCastTarget::TimeTz,
            TemporalValue::TimeTz {
                micros,
                offset_minutes,
            },
        ) => Some(TemporalValue::TimeTz {
            micros: *micros,
            offset_minutes: *offset_minutes,
        }),
        (TemporalCastTarget::TimeTz, TemporalValue::Time { micros })
        | (TemporalCastTarget::TimeTz, TemporalValue::TimestampTz { micros }) => {
            Some(TemporalValue::TimeTz {
                micros: micros.rem_euclid(MICROS_PER_DAY),
                offset_minutes: 0,
            })
        }
        (TemporalCastTarget::Timestamp, TemporalValue::Timestamp { micros })
        | (TemporalCastTarget::Timestamp, TemporalValue::TimestampTz { micros }) => {
            Some(TemporalValue::Timestamp { micros: *micros })
        }
        (TemporalCastTarget::Timestamp, TemporalValue::Date { days }) => {
            Some(TemporalValue::Timestamp {
                micros: i64::from(*days).checked_mul(MICROS_PER_DAY)?,
            })
        }
        (TemporalCastTarget::TimestampTz, TemporalValue::TimestampTz { micros })
        | (TemporalCastTarget::TimestampTz, TemporalValue::Timestamp { micros }) => {
            Some(TemporalValue::TimestampTz { micros: *micros })
        }
        (TemporalCastTarget::TimestampTz, TemporalValue::Date { days }) => {
            Some(TemporalValue::TimestampTz {
                micros: i64::from(*days).checked_mul(MICROS_PER_DAY)?,
            })
        }
        (
            TemporalCastTarget::Interval,
            TemporalValue::Interval {
                months,
                days,
                micros,
            },
        ) => Some(TemporalValue::Interval {
            months: *months,
            days: *days,
            micros: *micros,
        }),
        (TemporalCastTarget::Interval, TemporalValue::Time { micros }) => {
            Some(TemporalValue::Interval {
                months: 0,
                days: 0,
                micros: *micros,
            })
        }
        _ => None,
    }
}
