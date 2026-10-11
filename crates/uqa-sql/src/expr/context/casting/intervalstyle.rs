//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Live interval input and output for catalog-aware casts.

use super::{ColumnType, EngineHook, Result};
use crate::expr::IntervalStyleScope;
use uqa_core::{memory::ProductionControl, Value};

pub(super) fn cast_scope(
    value: &Value,
    target: Option<&ColumnType>,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Option<IntervalStyleScope>> {
    let Some(target) = target else {
        return Ok(None);
    };
    let may_read_text = matches!(
        value,
        Value::Str(_)
            | Value::FixedChar(_)
            | Value::Array(_)
            | Value::Row(_)
            | Value::Record(_)
            | Value::Datum(_)
    );
    if may_read_text && input_can_contain_interval(target)
        || super::is_string_type(target)
            && crate::expr::interval_style::output_can_contain_interval(value)
    {
        crate::expr::interval_style::live_scope(engine, control)
    } else {
        Ok(None)
    }
}

fn input_can_contain_interval(target: &ColumnType) -> bool {
    match target {
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => {
            input_can_contain_interval(base)
        }
        ColumnType::Interval
        | ColumnType::IntervalWithFields { .. }
        | ColumnType::Composite(_)
        | ColumnType::Record => true,
        _ => false,
    }
}
