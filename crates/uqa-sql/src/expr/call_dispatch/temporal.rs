//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected timestamptz extraction observes the invoking session's current zone.

use super::{EvalContext, Result};
use crate::expr::{conversion::value_to_string, time::extract_from_value_with_offset};
use uqa_core::{memory::ProductionControl, TemporalValue, Value};

pub(super) fn extract_session_zone(
    name: &str,
    args: &[Value],
    context: &EvalContext<'_>,
) -> Option<Result<Value>> {
    if !matches!(name, "extract" | "date_part") {
        return None;
    }
    let [field, value @ Value::Temporal(
        temporal @ TemporalValue::TimestampTz {
            postgres_micros: micros,
        },
    )] = args
    else {
        return None;
    };
    Some((|| {
        if matches!(field, Value::Null) {
            return Ok(Value::Null);
        }
        let field = value_to_string(field)?;
        let zone =
            crate::expr::time::session_zone(context.engine, &ProductionControl::uncontrolled())?;
        let offset = if temporal.is_finite() {
            zone.offset_at_postgres_micros(*micros)
                .ok_or_else(|| crate::expr::datetime_out_of_range("timestamp"))?
        } else {
            0
        };
        let result = extract_from_value_with_offset(
            &field,
            value,
            name == "extract",
            offset,
            &ProductionControl::uncontrolled(),
        )?;
        Ok(result
            .into_uncontrolled()
            .expect("ordinary call dispatch uses uncontrolled production"))
    })())
}

#[cfg(test)]
mod tests;
