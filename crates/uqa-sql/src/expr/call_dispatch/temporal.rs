//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected timestamptz extraction observes the invoking session's current zone.

use super::{EvalContext, Result, SQLError};
use crate::expr::{conversion::value_to_string, time::extract_from_value_with_offset};
use uqa_core::{memory::ProductionControl, TemporalTimeZone, TemporalValue, Value};

pub(super) fn extract_session_zone(
    name: &str,
    args: &[Value],
    context: &EvalContext<'_>,
) -> Option<Result<Value>> {
    if !matches!(name, "extract" | "date_part") {
        return None;
    }
    let [field, value @ Value::Temporal(TemporalValue::TimestampTz { micros })] = args else {
        return None;
    };
    Some((|| {
        if matches!(field, Value::Null) {
            return Ok(Value::Null);
        }
        let field = value_to_string(field)?;
        let zone = context
            .engine
            .map(|engine| engine.runtime_parameter("TimeZone"))
            .transpose()?
            .flatten();
        let zone = zone.as_deref().unwrap_or("UTC");
        let zone = TemporalTimeZone::named(zone).ok_or_else(|| SQLError::Routine {
            sqlstate: "22023".into(),
            message: format!("time zone \"{zone}\" not recognized"),
        })?;
        let offset = zone
            .offset_at(micros.div_euclid(1_000_000))
            .ok_or_else(|| crate::expr::datetime_out_of_range("timestamp"))?;
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
