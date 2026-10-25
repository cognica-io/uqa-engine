//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed timestamp casts use the invoking session's zone before applying the target type modifier.

use uqa_core::{memory::ProductionControl, TemporalTimeZone, TemporalValue, Value};

use super::{ColumnType, EngineHook, Result, SQLError};

pub(super) fn cast_timestamp(
    value: &Value,
    target: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<Value>> {
    let Some(target) = target else {
        return Ok(None);
    };
    let mut base = target;
    while let ColumnType::Domain {
        base: underlying, ..
    } = base
    {
        base = underlying;
    }
    if let Value::Temporal(
        value @ TemporalValue::TimestampTz {
            postgres_micros: micros,
        },
    ) = value
    {
        if matches!(
            base,
            ColumnType::Date | ColumnType::Timestamp | ColumnType::TimestampPrecision(_)
        ) {
            if let Some(infinity) = value.infinity() {
                control.check()?;
                return Ok(Some(Value::Temporal(if matches!(base, ColumnType::Date) {
                    infinity.date()
                } else {
                    infinity.timestamp()
                })));
            }
            let zone = session_zone(engine, control)?;
            let offset = zone
                .offset_at_postgres_micros(*micros)
                .ok_or_else(|| crate::expr::datetime_out_of_range("timestamp"))?;
            let local = i128::from(*micros) + i128::from(offset) * 1_000_000;
            let value = if matches!(base, ColumnType::Date) {
                let days = i32::try_from(
                    local.div_euclid(86_400_000_000) + i128::from(uqa_core::POSTGRES_EPOCH_DAYS),
                )
                .map_err(|_| crate::expr::datetime_out_of_range("date"))?;
                TemporalValue::Date { days }
            } else {
                let micros = i64::try_from(local)
                    .ok()
                    .filter(|micros| TemporalValue::timestamp_micros_in_range(*micros))
                    .ok_or_else(|| crate::expr::datetime_out_of_range("timestamp"))?;
                TemporalValue::Timestamp {
                    postgres_micros: micros,
                }
            };
            return Ok(Some(Value::Temporal(value)));
        }
    }
    if !matches!(
        base,
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_)
    ) {
        return Ok(None);
    }
    let out_of_range = || crate::expr::datetime_out_of_range("timestamp");
    if let Value::Temporal(value @ (TemporalValue::Date { .. } | TemporalValue::Timestamp { .. })) =
        value
    {
        if let Some(infinity) = value.infinity() {
            control.check()?;
            return Ok(Some(Value::Temporal(infinity.timestamp_tz())));
        }
    }
    let local = match value {
        Value::Temporal(TemporalValue::Timestamp {
            postgres_micros: micros,
        }) => *micros,
        Value::Temporal(TemporalValue::Date { days }) => (i64::from(*days)
            - i64::from(uqa_core::POSTGRES_EPOCH_DAYS))
        .checked_mul(86_400_000_000)
        .ok_or_else(out_of_range)?,
        _ => return Ok(None),
    };
    let zone = session_zone(engine, control)?;
    let offset = zone
        .offset_for_local(local.div_euclid(1_000_000) + uqa_core::POSTGRES_EPOCH_MICROS / 1_000_000)
        .ok_or_else(out_of_range)?;
    let micros = local
        .checked_sub(i64::from(offset) * 1_000_000)
        .filter(|micros| TemporalValue::timestamp_micros_in_range(*micros))
        .ok_or_else(out_of_range)?;
    Ok(Some(Value::Temporal(TemporalValue::TimestampTz {
        postgres_micros: micros,
    })))
}

fn session_zone(
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<TemporalTimeZone> {
    control.check()?;
    let name = engine
        .map(|engine| engine.runtime_parameter("TimeZone"))
        .transpose()?
        .flatten()
        .map(|name| {
            let memory = control.reserve(name.capacity())?;
            Ok::<_, SQLError>(control.finish(name, memory)?)
        })
        .transpose()?;
    control.check()?;
    let name = name.as_ref().map_or("UTC", |name| name.as_str());
    TemporalTimeZone::named(name).ok_or_else(|| SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("time zone \"{name}\" not recognized"),
    })
}
