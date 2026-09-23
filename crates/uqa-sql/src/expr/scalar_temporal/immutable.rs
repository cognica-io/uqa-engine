//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal functions accepted by generated expressions share admitted scalar producers with ordinary evaluation.

use super::super::{
    age_between,
    conversion::{to_f64_with_control, to_i64_with_control, value_to_string_with_control},
    float_to_i64_rounded, make_timestamp, out_of_range,
    time::{coerce_temporal_with_control, date_trunc_value, extract_from_value},
    uuid::{extract_uuid_timestamp, extract_uuid_version},
    Result, SQLError, TemporalValue, Value,
};
use super::undefined_uuid_extraction;
use uqa_core::memory::{Produced, ProductionControl, ProductionString};

#[expect(
    clippy::too_many_lines,
    reason = "temporal dispatch preserves arity, NULL and error precedence"
)]
pub(in crate::expr) fn eval_temporal_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    if !matches!(
        name,
        "to_timestamp"
            | "extract"
            | "date_part"
            | "age"
            | "date_trunc"
            | "make_timestamp"
            | "make_date"
            | "make_interval"
            | "justify_hours"
            | "isfinite"
            | "uuid_extract_version"
            | "uuid_extract_timestamp"
    ) {
        return None;
    }
    Some((|| -> Result<Produced<Value>> {
        control.check()?;
        let inline = |value| -> Result<Produced<Value>> {
            Ok(control.finish(value, control.empty_reservation())?)
        };
        let integer = |value| to_i64_with_control(value, control);
        let float = |value| to_f64_with_control(value, control);
        match name {
            "to_timestamp" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("to_timestamp takes 1 arg".into()));
                }
                let secs = float(&args[0])?;
                inline(Value::Temporal(TemporalValue::TimestampTz {
                    micros: float_to_i64_rounded(secs * 1e6, "timestamp")?,
                }))
            }
            "extract" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "extract takes 2 args (field, ts)".into(),
                    ));
                }
                let field = field_name(&args[0], control)?;
                extract_from_value(&field, &args[1], true, control)
            }
            "date_part" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "date_part takes 2 args (field, ts)".into(),
                    ));
                }
                let field = field_name(&args[0], control)?;
                extract_from_value(&field, &args[1], false, control)
            }
            "age" => {
                let (a, b) = match args.len() {
                    2 => (
                        coerce_temporal_with_control(&args[0], control)?,
                        coerce_temporal_with_control(&args[1], control)?,
                    ),
                    _ => return Err(SQLError::TypeMismatch("age takes 1-2 args".into())),
                };
                inline(age_between(&a, &b)?)
            }
            "date_trunc" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("date_trunc takes 2 args".into()));
                }
                let unit = field_name(&args[0], control)?;
                inline(date_trunc_value(&unit, &args[1], control)?)
            }
            "make_timestamp" => {
                if !(6..=7).contains(&args.len()) {
                    return Err(SQLError::TypeMismatch(
                        "make_timestamp takes 6-7 args".into(),
                    ));
                }
                let year = i32::try_from(integer(&args[0])?).map_err(|_| out_of_range("date"))?;
                let month = u32::try_from(integer(&args[1])?).map_err(|_| out_of_range("date"))?;
                let day = u32::try_from(integer(&args[2])?).map_err(|_| out_of_range("date"))?;
                let hour = u32::try_from(integer(&args[3])?).map_err(|_| out_of_range("time"))?;
                let minute = u32::try_from(integer(&args[4])?).map_err(|_| out_of_range("time"))?;
                let second = float(&args[5])?;
                inline(make_timestamp(year, month, day, hour, minute, second)?)
            }
            "make_date" => {
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("make_date takes 3 args".into()));
                }
                let year = i32::try_from(integer(&args[0])?).map_err(|_| out_of_range("date"))?;
                let month = u32::try_from(integer(&args[1])?).map_err(|_| out_of_range("date"))?;
                let day = u32::try_from(integer(&args[2])?).map_err(|_| out_of_range("date"))?;
                let epoch = chrono::DateTime::<chrono::Utc>::UNIX_EPOCH.date_naive();
                let date = chrono::NaiveDate::from_ymd_opt(year, month, day).ok_or_else(|| {
                    SQLError::Routine {
                        sqlstate: "22008".into(),
                        message: format!(
                            "date field value out of range: {year:04}-{month:02}-{day:02}"
                        ),
                    }
                })?;
                inline(Value::Temporal(TemporalValue::Date {
                    days: i32::try_from(date.signed_duration_since(epoch).num_days())
                        .map_err(|_| out_of_range("date"))?,
                }))
            }
            "make_interval" => {
                // make_interval(years, months, weeks, days, hours, mins,
                // secs) -> PostgreSQL's months/days/micros interval model.
                let years = args.first().map(integer).transpose()?.unwrap_or(0);
                let months = args.get(1).map(integer).transpose()?.unwrap_or(0);
                let weeks = args.get(2).map(integer).transpose()?.unwrap_or(0);
                let days = args.get(3).map(integer).transpose()?.unwrap_or(0);
                let hours = args.get(4).map(integer).transpose()?.unwrap_or(0);
                let mins = args.get(5).map(integer).transpose()?.unwrap_or(0);
                let secs = args.get(6).map(float).transpose()?.unwrap_or(0.0);
                let total_months = years
                    .checked_mul(12)
                    .and_then(|value| value.checked_add(months))
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| out_of_range("interval"))?;
                let total_days = weeks
                    .checked_mul(7)
                    .and_then(|value| value.checked_add(days))
                    .and_then(|value| i32::try_from(value).ok())
                    .ok_or_else(|| out_of_range("interval"))?;
                let whole_micros = hours
                    .checked_mul(3_600)
                    .and_then(|value| {
                        mins.checked_mul(60)
                            .and_then(|mins| value.checked_add(mins))
                    })
                    .and_then(|value| value.checked_mul(1_000_000))
                    .ok_or_else(|| out_of_range("interval"))?;
                let fractional_micros = float_to_i64_rounded(secs * 1e6, "interval")?;
                let micros = whole_micros
                    .checked_add(fractional_micros)
                    .ok_or_else(|| out_of_range("interval"))?;
                inline(Value::Temporal(TemporalValue::Interval {
                    months: total_months,
                    days: total_days,
                    micros,
                }))
            }
            "justify_hours" => {
                if let Some(Value::Temporal(TemporalValue::Interval {
                    months,
                    days,
                    micros,
                })) = args.first()
                {
                    let extra_days = micros.div_euclid(86_400_000_000);
                    let extra_days =
                        i32::try_from(extra_days).map_err(|_| out_of_range("interval"))?;
                    let days = days
                        .checked_add(extra_days)
                        .ok_or_else(|| out_of_range("interval"))?;
                    return inline(Value::Temporal(TemporalValue::Interval {
                        months: *months,
                        days,
                        micros: micros.rem_euclid(86_400_000_000),
                    }));
                }
                Err(SQLError::TypeMismatch(
                    "justify_hours takes an interval".into(),
                ))
            }
            "isfinite" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("isfinite takes 1 arg".into()));
                }
                match &args[0] {
                    Value::Float(f) => inline(Value::Bool(f.is_finite())),
                    // The temporal model has no infinity values, so every
                    // date / timestamp / interval is finite.
                    Value::Int(_) | Value::Decimal(_) | Value::Str(_) | Value::Temporal(_) => {
                        inline(Value::Bool(true))
                    }
                    Value::Null => inline(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "isfinite: unsupported {other:?}"
                    ))),
                }
            }
            "uuid_extract_version" => match args {
                [uuid @ (Value::Str(_) | Value::FixedChar(_))] => {
                    inline(extract_uuid_version(uuid, control)?)
                }
                _ => Err(undefined_uuid_extraction(name, args)),
            },
            "uuid_extract_timestamp" => match args {
                [uuid @ (Value::Str(_) | Value::FixedChar(_))] => {
                    inline(extract_uuid_timestamp(uuid, control)?)
                }
                _ => Err(undefined_uuid_extraction(name, args)),
            },
            _ => unreachable!("temporal family checked before dispatch"),
        }
    })())
}

fn field_name(value: &Value, control: &ProductionControl<'_>) -> Result<Produced<String>> {
    let value = value_to_string_with_control(value, control)?;
    let mut output = ProductionString::new(*control);
    output.reserve(value.len())?;
    for character in value.chars() {
        output.push(character.to_ascii_lowercase())?;
    }
    Ok(output.finish()?)
}

#[cfg(test)]
mod tests;
