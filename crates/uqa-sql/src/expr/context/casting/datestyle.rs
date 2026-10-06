//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read the live session date order for casts, including `set_config` earlier in the same row.

use super::{ColumnType, EngineHook, Result};
use uqa_core::{memory::ProductionControl, Value};

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
