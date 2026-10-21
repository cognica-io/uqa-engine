//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read the live session date order for casts, including `set_config` earlier in the same row.

use super::{ColumnType, EngineHook, Result};
use uqa_core::{
    memory::{Produced, ProductionControl},
    TemporalValue, Value,
};

/// DATE-to-string casts call `date_out` with the live style before the destination applies its length or padding.
pub(super) fn date_output(
    value: &Value,
    target_name: &str,
    target: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>> {
    let Some(output) = date_text(value, target, engine, control)? else {
        return Ok(None);
    };
    let (text, memory) = output.into_parts();
    if matches!(target, Some(ColumnType::Text)) {
        return Ok(Some(control.finish(Value::Str(text), memory)?));
    }
    super::cast_value_from_with_control(&Value::Str(text), target_name, Some("text"), control)
        .map(Some)
}

pub(super) fn date_text(
    value: &Value,
    target: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<String>>> {
    let (Value::Temporal(date @ TemporalValue::Date { .. }), Some(engine)) = (value, engine) else {
        return Ok(None);
    };
    if !target.is_some_and(super::is_string_type) {
        return Ok(None);
    }
    control.check()?;
    let Some(setting) = engine.runtime_parameter("DateStyle")? else {
        return Ok(None);
    };
    let memory = control.reserve(setting.capacity())?;
    let setting = control.finish(setting, memory)?;
    control.check()?;
    date.to_sql_string_in_date_style_with_control(
        crate::semantics::parameters::datestyle::date_style(&setting),
        crate::semantics::parameters::datestyle::date_order(&setting),
        control,
    )
    .map(Some)
    .map_err(Into::into)
}

pub(super) fn input_scope(
    value: &Value,
    target: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<crate::expr::DateOrderScope>> {
    let Some(engine) = engine.filter(|_| target.is_some_and(uses_date_order)) else {
        return Ok(None);
    };
    if !contains_text(value, control)? {
        return Ok(None);
    }
    control.check()?;
    let Some(setting) = engine.runtime_parameter("DateStyle")? else {
        return Ok(None);
    };
    let memory = control.reserve(setting.capacity())?;
    let setting = control.finish(setting, memory)?;
    control.check()?;
    Ok(Some(crate::expr::DateOrderScope::enter(
        crate::semantics::parameters::datestyle::date_order(&setting),
    )))
}

fn uses_date_order(target: &ColumnType) -> bool {
    match target {
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => uses_date_order(base),
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimePrecision(_)
        | ColumnType::TimeTz
        | ColumnType::TimeTzPrecision(_)
        | ColumnType::Timestamp
        | ColumnType::TimestampPrecision(_)
        | ColumnType::TimestampTz
        | ColumnType::TimestampTzPrecision(_)
        | ColumnType::Range(
            crate::ast::RangeSubtype::Date
            | crate::ast::RangeSubtype::Timestamp
            | crate::ast::RangeSubtype::TimestampTz,
        )
        | ColumnType::Multirange(
            crate::ast::RangeSubtype::Date
            | crate::ast::RangeSubtype::Timestamp
            | crate::ast::RangeSubtype::TimestampTz,
        ) => true,
        _ => false,
    }
}

fn contains_text(value: &Value, control: &ProductionControl<'_>) -> Result<bool> {
    control.check()?;
    match value {
        Value::Str(_) | Value::FixedChar(_) => Ok(true),
        Value::Array(array) => {
            for element in array.elements() {
                if contains_text(element, control)? {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}
