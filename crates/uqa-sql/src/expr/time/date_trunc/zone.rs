//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Zone selection and calendar truncation retain the selected timestamp carrier.

use uqa_core::TemporalTimeZone;

use super::{decode_unit, truncate_timestamp, Unit};
use crate::expr::context::EvalContext;
use crate::expr::conversion::value_to_string;
use crate::expr::time::{micros_from_naive, naive_from_micros};
use crate::expr::{datetime_out_of_range, Result, SQLError};
use uqa_core::{memory::ProductionControl, TemporalValue, Value};

pub(in crate::expr) fn truncate_explicit_zone(
    units: &str,
    value: &Value,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
    // PostgreSQL's text_to_cstring_buffer uses TZ_STRLEN_MAX before lookup and diagnostics.
    let mut end = name.len().min(255);
    while !name.is_char_boundary(end) {
        end -= 1;
    }
    let name = &name[..end];
    let zone = TemporalTimeZone::named_or_abbreviation(name).ok_or_else(|| unknown_zone(name))?;
    truncate(units, value, &zone, control)
}

pub(in crate::expr) fn truncate_session_zone(
    args: &[Value],
    context: &EvalContext<'_>,
) -> Option<Result<Value>> {
    let [unit, value @ Value::Temporal(TemporalValue::TimestampTz { .. })] = args else {
        return None;
    };
    Some((|| {
        if matches!(unit, Value::Null) {
            return Ok(Value::Null);
        }
        let units = value_to_string(unit)?;
        let name = context
            .engine
            .map(|engine| engine.runtime_parameter("TimeZone"))
            .transpose()?
            .flatten();
        let name = name.as_deref().unwrap_or("UTC");
        let zone = TemporalTimeZone::named(name).ok_or_else(|| unknown_zone(name))?;
        truncate(&units, value, &zone, &ProductionControl::uncontrolled())
    })())
}

fn truncate(
    units: &str,
    value: &Value,
    zone: &TemporalTimeZone,
    control: &ProductionControl<'_>,
) -> Result<Value> {
    control.check()?;
    let Value::Temporal(TemporalValue::TimestampTz { micros }) = value else {
        return Err(SQLError::TypeMismatch(
            "date_trunc with a time zone requires timestamp with time zone".into(),
        ));
    };
    let unit = decode_unit(units, "timestamp with time zone")?;
    let out_of_range = || datetime_out_of_range("timestamp");
    let offset = zone
        .offset_at(micros.div_euclid(1_000_000))
        .ok_or_else(out_of_range)?;
    let local = micros
        .checked_add(i64::from(offset) * 1_000_000)
        .ok_or_else(out_of_range)?;
    let truncated = micros_from_naive(truncate_timestamp(unit, naive_from_micros(local)?)?);
    let offset = if matches!(
        unit,
        Unit::Hour | Unit::Minute | Unit::Second | Unit::Milliseconds | Unit::Microseconds
    ) {
        offset
    } else {
        zone.offset_for_local(truncated.div_euclid(1_000_000))
            .ok_or_else(out_of_range)?
    };
    let micros = truncated
        .checked_sub(i64::from(offset) * 1_000_000)
        .filter(|micros| TemporalValue::timestamp_micros_in_range(*micros))
        .ok_or_else(out_of_range)?;
    Ok(Value::Temporal(TemporalValue::TimestampTz { micros }))
}

fn unknown_zone(name: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("time zone \"{name}\" not recognized"),
    }
}

#[cfg(test)]
mod tests;
