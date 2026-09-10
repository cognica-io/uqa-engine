//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Date, time, timestamp, and interval conversion.

use uqa_core::{TemporalValue, Value};

use crate::ast::{ColumnType, IntervalFields};
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
    precision: Option<&str>,
) -> Result<Value> {
    let mut value = match v {
        Value::Temporal(value) => cast_temporal_kind(value, target)
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
        other => parse(&value_to_string(other))
            .map(Value::Temporal)
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot cast {v:?} to {ty}"))),
    }?;
    if let Some(precision) = precision {
        round_temporal(&mut value, precision, ty)?;
    }
    Ok(value)
}

fn round_temporal(value: &mut Value, precision: &str, ty: &str) -> Result<()> {
    let precision = precision
        .parse::<u32>()
        .map_err(|_| SQLError::TypeMismatch(format!("invalid temporal precision: {precision}")))?
        .min(6);
    let Value::Temporal(value) = value else {
        return Ok(());
    };
    let (micros, epoch) = match value {
        TemporalValue::Time { micros }
        | TemporalValue::TimeTz { micros, .. }
        | TemporalValue::Interval { micros, .. } => (micros, 0),
        // PostgreSQL rounds signed timestamps around its 2000 epoch, including ties before that epoch.
        TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros } => {
            (micros, 946_684_800_000_000_i128)
        }
        _ => return Ok(()),
    };
    let scale = 10_i128.pow(6 - precision);
    let offset = i128::from(*micros) - epoch;
    let rounded = if offset >= 0 {
        (offset + scale / 2) / scale * scale
    } else {
        -((-offset + scale / 2) / scale * scale)
    };
    *micros = i64::try_from(rounded + epoch).map_err(|_| SQLError::Routine {
        sqlstate: if ty == "interval" { "22015" } else { "22008" }.into(),
        message: format!("{ty} out of range"),
    })?;
    Ok(())
}

pub(super) fn cast_interval(value: &Value, ty: &str) -> Result<Value> {
    let mut value = cast_temporal(
        value,
        TemporalCastTarget::Interval,
        TemporalValue::parse_interval,
        "interval",
        None,
    )?;
    let ColumnType::IntervalWithFields { fields, precision } = ColumnType::from_sql_name(ty)?
    else {
        return Ok(value);
    };
    let Value::Temporal(TemporalValue::Interval {
        months,
        days,
        micros,
    }) = &mut value
    else {
        return Ok(value);
    };
    match fields {
        IntervalFields::Year => {
            *months = *months / 12 * 12;
            *days = 0;
            *micros = 0;
        }
        IntervalFields::Month | IntervalFields::YearToMonth => {
            *days = 0;
            *micros = 0;
        }
        IntervalFields::Day => *micros = 0,
        IntervalFields::Hour | IntervalFields::DayToHour => {
            *micros = *micros / 3_600_000_000 * 3_600_000_000;
        }
        IntervalFields::Minute | IntervalFields::DayToMinute | IntervalFields::HourToMinute => {
            *micros = *micros / 60_000_000 * 60_000_000;
        }
        IntervalFields::All
        | IntervalFields::Second
        | IntervalFields::DayToSecond
        | IntervalFields::HourToSecond
        | IntervalFields::MinuteToSecond => {}
    }
    if let Some(precision) = precision {
        round_temporal(&mut value, &precision.to_string(), "interval")?;
    }
    Ok(value)
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
        | (TemporalCastTarget::Time, TemporalValue::TimeTz { micros, .. }) => {
            Some(TemporalValue::Time { micros: *micros })
        }
        (
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
        (TemporalCastTarget::TimeTz, TemporalValue::Time { micros }) => {
            Some(TemporalValue::TimeTz {
                micros: *micros,
                offset_minutes: 0,
            })
        }
        (TemporalCastTarget::TimeTz, TemporalValue::TimestampTz { micros }) => {
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
