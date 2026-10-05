//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Date, time, timestamp, and interval conversion: text is read by the type's input function against the statement's transaction clock, a temporal value of another kind converts as the catalog's casts convert it, and any other source has no cast.

use uqa_core::{memory::ProductionControl, TemporalInputError, TemporalValue, Value};

use crate::ast::{ColumnType, IntervalFields};
use crate::error::{Result, SQLError};

use super::{canonical_cast_source_with_control, undefined_cast};

#[derive(Clone, Copy)]
pub(super) enum TemporalCastTarget {
    Date,
    Time,
    TimeTz,
    Timestamp,
    TimestampTz,
    Interval,
}

impl TemporalCastTarget {
    /// The type name the input function's diagnostics print (`invalid input syntax for type time`).
    fn input_name(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Time => "time",
            Self::TimeTz => "time with time zone",
            Self::Timestamp => "timestamp",
            Self::TimestampTz => "timestamp with time zone",
            Self::Interval => "interval",
        }
    }

    /// The type name `format_type` prints in a cast diagnostic (`cannot cast type integer to time without time zone`).
    fn cast_name(self) -> &'static str {
        match self {
            Self::Time => "time without time zone",
            Self::Timestamp => "timestamp without time zone",
            other => other.input_name(),
        }
    }

    /// The input function, reading `text` with `now_micros` as the transaction start the special values name.
    fn read(
        self,
        text: &str,
        now_micros: i64,
        control: &ProductionControl<'_>,
    ) -> Result<std::result::Result<TemporalValue, TemporalInputError>> {
        Ok(match self {
            Self::Date => TemporalValue::date_input_with_control(text, now_micros, control)?,
            Self::Time => TemporalValue::time_input_with_control(text, now_micros, control)?,
            Self::TimeTz => TemporalValue::time_tz_input_with_control(text, now_micros, control)?,
            Self::Timestamp => {
                TemporalValue::timestamp_input_with_control(text, now_micros, control)?
            }
            Self::TimestampTz => {
                TemporalValue::timestamp_tz_input_with_control(text, now_micros, control)?
            }
            Self::Interval => TemporalValue::interval_input_with_control(text, control)?,
        })
    }
}

/// Cast `v` to the temporal type `target`, with `precision` the type modifier's fractional digits. Text reaches the type through its input function, as `coerce_type` and the I/O conversion casts read it, with the diagnostics of `DateTimeParseError`; a temporal value of another kind converts as the catalog's casts convert it; any other value has no cast to the type.
pub(super) fn cast_temporal(
    v: &Value,
    source_ty: Option<&str>,
    target: TemporalCastTarget,
    precision: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    let converted = match v {
        Value::Temporal(value) => cast_temporal_kind(value, target),
        Value::Str(text) | Value::FixedChar(text) => Some(
            target
                .read(text, crate::expr::transaction_timestamp_or_clock(), control)?
                .map_err(|error| input_error(error, target.input_name(), text))?,
        ),
        _ => None,
    };
    let Some(mut value) = converted.map(Value::Temporal) else {
        return Err(undefined_cast(
            &canonical_cast_source_with_control(source_ty, v, control)?,
            target.cast_name(),
        ));
    };
    if let Some(precision) = precision {
        round_temporal(&mut value, precision)?;
    }
    Ok(value)
}

/// `AdjustTimestampForTypmod`, `AdjustTimeForTypmod` and `AdjustIntervalForTypmod`: round the fractional seconds to `precision` digits.
fn round_temporal(value: &mut Value, precision: &str) -> Result<()> {
    let precision = precision
        .parse::<u32>()
        .map_err(|_| SQLError::TypeMismatch(format!("invalid temporal precision: {precision}")))?
        .min(6);
    let Value::Temporal(value) = value else {
        return Ok(());
    };
    let (micros, epoch, overflow) = match value {
        TemporalValue::Time { micros } | TemporalValue::TimeTz { micros, .. } => {
            (micros, 0, ("22008", "time"))
        }
        TemporalValue::Interval { micros, .. } => (micros, 0, ("22015", "interval")),
        // PostgreSQL rounds signed timestamps around its 2000 epoch, including ties before that epoch.
        TemporalValue::Timestamp { micros } | TemporalValue::TimestampTz { micros } => {
            (micros, 946_684_800_000_000_i128, ("22008", "timestamp"))
        }
        TemporalValue::Date { .. } => return Ok(()),
    };
    let scale = 10_i128.pow(6 - precision);
    let offset = i128::from(*micros) - epoch;
    let rounded = if offset >= 0 {
        (offset + scale / 2) / scale * scale
    } else {
        -((-offset + scale / 2) / scale * scale)
    };
    *micros = i64::try_from(rounded + epoch).map_err(|_| SQLError::Routine {
        sqlstate: overflow.0.into(),
        message: format!("{} out of range", overflow.1),
    })?;
    Ok(())
}

/// Cast to `interval` or to `interval` with fields, truncating the fields the declaration excludes as `AdjustIntervalForTypmod` truncates them.
pub(super) fn cast_interval(
    value: &Value,
    source_ty: Option<&str>,
    ty: &str,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    let mut value = cast_temporal(
        value,
        source_ty,
        TemporalCastTarget::Interval,
        None,
        control,
    )?;
    let column_type = ColumnType::from_sql_name_with_control(ty, control)?;
    let ColumnType::IntervalWithFields { fields, precision } = &*column_type else {
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
        round_temporal(&mut value, &control.format(format_args!("{precision}"))?)?;
    }
    Ok(value)
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

/// The diagnostic `DateTimeParseError` reports for input the type `ty` rejects: `22007` for text that is not a value, `22008` for a field or value out of range with the `DateStyle` hint when the month or day order could be the cause, `22009` for a UTC offset out of range, `22023` for an unknown time zone and `22015` for an interval field that does not fit.
fn input_error(error: TemporalInputError, ty: &str, text: &str) -> SQLError {
    let (sqlstate, message, hint) = match error {
        TemporalInputError::InvalidSyntax => (
            "22007",
            format!("invalid input syntax for type {ty}: \"{text}\""),
            None,
        ),
        TemporalInputError::FieldOverflow { date_style } => (
            "22008",
            format!("date/time field value out of range: \"{text}\""),
            date_style.then(|| "Perhaps you need a different \"DateStyle\" setting.".to_string()),
        ),
        TemporalInputError::OutOfRange => {
            let name = if ty == "timestamp with time zone" {
                "timestamp"
            } else {
                ty
            };
            ("22008", format!("{name} out of range: \"{text}\""), None)
        }
        TemporalInputError::ZoneDisplacement => (
            "22009",
            format!("time zone displacement out of range: \"{text}\""),
            None,
        ),
        TemporalInputError::UnknownZone(zone) => (
            "22023",
            format!("time zone \"{zone}\" not recognized"),
            None,
        ),
        TemporalInputError::IntervalFieldOverflow => (
            "22015",
            format!("interval field value out of range: \"{text}\""),
            None,
        ),
    };
    SQLError::Diagnostic {
        sqlstate: sqlstate.into(),
        message,
        detail: None,
        hint,
    }
}
