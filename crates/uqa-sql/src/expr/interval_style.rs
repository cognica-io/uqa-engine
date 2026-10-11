//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read live interval presentation at SQL input/output boundaries, including earlier `set_config` calls.

use super::{EngineHook, IntervalStyleScope};
use crate::error::Result;
use uqa_core::{memory::ProductionControl, TemporalValue, Value};

pub(crate) fn live_scope(
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<IntervalStyleScope>> {
    let Some(engine) = engine else {
        return Ok(None);
    };
    control.check()?;
    let Some(style) = engine.temporal_interval_style()? else {
        return Ok(None);
    };
    control.check()?;
    Ok(Some(IntervalStyleScope::enter(style)))
}

pub(crate) fn output_can_contain_interval(value: &Value) -> bool {
    matches!(
        value,
        Value::Temporal(TemporalValue::Interval { .. })
            | Value::Array(_)
            | Value::Row(_)
            | Value::Record(_)
            | Value::Datum(_)
            | Value::List(_)
            | Value::Map(_)
    )
}

pub(crate) fn call_scope(
    name: &str,
    arguments: &[(Option<String>, Value)],
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<IntervalStyleScope>> {
    if !arguments
        .iter()
        .any(|(_, value)| output_can_contain_interval(value))
    {
        return Ok(None);
    }
    let name = super::call_arguments::normalized_function_name_with_control(name, control)?;
    if super::enums::call_applies_output_functions(&name, arguments) {
        live_scope(engine, control)
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
