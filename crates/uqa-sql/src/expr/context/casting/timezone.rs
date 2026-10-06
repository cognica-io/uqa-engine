//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Typed local timestamps use the invoking session's zone before the ordinary cast applies its type modifier.

use uqa_core::{memory::ProductionControl, TemporalTimeZone, TemporalValue, Value};

use super::{ColumnType, EngineHook, Result, SQLError};

pub(super) fn cast_local_timestamp(
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
    if !matches!(
        base,
        ColumnType::TimestampTz | ColumnType::TimestampTzPrecision(_)
    ) {
        return Ok(None);
    }
    let out_of_range = || crate::expr::datetime_out_of_range("timestamp");
    let local = match value {
        Value::Temporal(TemporalValue::Timestamp { micros }) => *micros,
        Value::Temporal(TemporalValue::Date { days }) => i64::from(*days)
            .checked_mul(86_400_000_000)
            .ok_or_else(out_of_range)?,
        _ => return Ok(None),
    };
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
    let zone = TemporalTimeZone::named(name).ok_or_else(|| SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("time zone \"{name}\" not recognized"),
    })?;
    let offset = zone
        .offset_for_local(local.div_euclid(1_000_000))
        .ok_or_else(out_of_range)?;
    let micros = local
        .checked_sub(i64::from(offset) * 1_000_000)
        .filter(|micros| TemporalValue::timestamp_micros_in_range(*micros))
        .ok_or_else(out_of_range)?;
    Ok(Some(Value::Temporal(TemporalValue::TimestampTz { micros })))
}
