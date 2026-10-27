//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Timestamp zone conversion shares Core's bundled offset selection.

use crate::expr::{datetime_out_of_range, EvalContext, Result, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    TemporalTimeZone, TemporalValue, Value,
};

pub(in crate::expr) fn eval_timezone(
    args: &[Value],
    context: Option<&EvalContext<'_>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    if !matches!(args.len(), 1 | 2) {
        return Err(SQLError::UnknownFunction("timezone".into()));
    }
    let finish = |value| Ok(control.finish(value, control.empty_reservation())?);
    if args.iter().any(|value| matches!(value, Value::Null)) {
        return finish(Value::Null);
    }
    let Some(Value::Temporal(value)) = args.last() else {
        return Err(SQLError::UnknownFunction("timezone".into()));
    };
    let (micros, to_utc) = match value {
        TemporalValue::Timestamp {
            postgres_micros: micros,
        } => (*micros, true),
        TemporalValue::TimestampTz {
            postgres_micros: micros,
        } => (*micros, false),
        _ => return Err(SQLError::UnknownFunction("timezone".into())),
    };
    // PostgreSQL returns non-finite timestamps before consulting a text zone
    // or validating an interval zone. Strict NULL evaluation still precedes it.
    if let Some(infinity) = value.infinity() {
        return finish(Value::Temporal(if to_utc {
            infinity.timestamp_tz()
        } else {
            infinity.timestamp()
        }));
    }
    let seconds_to_subtract = if args.len() == 1 {
        let name = context
            .and_then(|context| context.engine)
            .map(|engine| engine.runtime_parameter("TimeZone"))
            .transpose()?
            .flatten();
        let name = name.as_deref().unwrap_or("UTC");
        let zone = TemporalTimeZone::named(name).ok_or_else(|| unknown_zone(name))?;
        directional_offset(zone, micros, to_utc)?
    } else {
        match &args[0] {
            Value::Str(name) => {
                let name = zone_name(name);
                let zone = TemporalTimeZone::abbreviation(name)
                    .or_else(|| TemporalTimeZone::named(name))
                    .ok_or_else(|| unknown_zone(name))?;
                directional_offset(zone, micros, to_utc)?
            }
            Value::Temporal(zone @ TemporalValue::Interval { .. }) => {
                interval_offset(zone, to_utc, control)?
            }
            _ => return Err(SQLError::UnknownFunction("timezone".into())),
        }
    };
    let micros = micros
        .checked_sub(i64::from(seconds_to_subtract) * 1_000_000)
        .filter(|micros| TemporalValue::timestamp_micros_in_range(*micros))
        .ok_or_else(|| datetime_out_of_range("timestamp"))?;
    finish(Value::Temporal(if to_utc {
        TemporalValue::TimestampTz {
            postgres_micros: micros,
        }
    } else {
        TemporalValue::Timestamp {
            postgres_micros: micros,
        }
    }))
}

fn interval_offset(
    zone: &TemporalValue,
    to_utc: bool,
    control: &ProductionControl<'_>,
) -> Result<i32> {
    let TemporalValue::Interval {
        months,
        days,
        micros,
    } = zone
    else {
        unreachable!("caller selects interval arguments")
    };
    let reason = if !zone.is_finite() {
        Some("must be finite")
    } else if *months != 0 || *days != 0 {
        Some("must not include months or days")
    } else {
        None
    };
    if let Some(reason) = reason {
        let text = zone.to_sql_string_with_control(control)?;
        return Err(SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("interval time zone \"{}\" {reason}", text.as_str()),
        });
    }
    // timestamp_izone/timestamptz_izone truncate fractional seconds,
    // apply direction, then narrow the signed offset to C int.
    let seconds = micros / 1_000_000;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "PostgreSQL narrows the signed interval offset to int32"
    )]
    let seconds = (if to_utc { seconds } else { -seconds }) as i32;
    Ok(seconds)
}

fn directional_offset(zone: TemporalTimeZone, micros: i64, to_utc: bool) -> Result<i32> {
    let seconds = micros.div_euclid(1_000_000) + uqa_core::POSTGRES_EPOCH_MICROS / 1_000_000;
    let offset = if to_utc {
        zone.offset_for_local(seconds)
    } else {
        zone.offset_at_postgres_micros(micros)
    }
    .ok_or_else(|| datetime_out_of_range("timestamp"))?;
    Ok(if to_utc { offset } else { -offset })
}

pub(super) fn zone_name(name: &str) -> &str {
    // PostgreSQL's text_to_cstring_buffer clips to TZ_STRLEN_MAX bytes.
    let mut end = name.len().min(255);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    &name[..end]
}

pub(super) fn unknown_zone(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("time zone \"{name}\" not recognized"),
    }
}

#[cfg(test)]
mod tests;
