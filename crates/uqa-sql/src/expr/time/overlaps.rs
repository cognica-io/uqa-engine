//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::BinaryOp;
use crate::error::{Result, SQLError};
use crate::expr::cast_value_from_with_control;
use uqa_core::{memory::ProductionControl, TemporalValue, Value};

pub(in crate::expr) fn overlaps(args: &[Value], control: &ProductionControl<'_>) -> Result<Value> {
    let [a, b, c, d] = args else {
        return Err(SQLError::TypeMismatch(
            "overlaps takes 4 args (start1, end1, start2, end2)".into(),
        ));
    };
    let target = args
        .iter()
        .find_map(|value| match value {
            Value::Temporal(TemporalValue::Time { .. }) => Some("time"),
            Value::Temporal(TemporalValue::TimeTz { .. }) => Some("timetz"),
            _ => None,
        })
        .unwrap_or("timestamptz");
    let period = |start: &Value, end: &Value| -> Result<_> {
        let start = cast_value_from_with_control(start, target, None, control)?;
        let end = if matches!(end, Value::Temporal(TemporalValue::Interval { .. })) {
            if matches!(*start, Value::Null) {
                Value::Null
            } else {
                super::temporal_arith_with_control(&start, end, BinaryOp::Add, None, control)?
            }
        } else {
            (*cast_value_from_with_control(end, target, None, control)?).clone()
        };
        Ok(match (&*start, end) {
            (Value::Temporal(a), Value::Temporal(b)) if *a > b => Some((b, Some(a.clone()))),
            (Value::Temporal(a), Value::Temporal(b)) => Some((a.clone(), Some(b))),
            (Value::Null, Value::Temporal(b)) => Some((b, None)),
            (Value::Temporal(a), Value::Null) => Some((a.clone(), None)),
            _ => None,
        })
    };
    let left = period(a, b)?;
    let right = period(c, d)?;
    let (Some((a, b)), Some((c, d))) = (left, right) else {
        return Ok(Value::Null);
    };
    // A known start inside the other period proves overlap even if its end is unknown.
    let (later, later_end, earlier_end) = match a.cmp(&c) {
        std::cmp::Ordering::Less => (c, d, b),
        std::cmp::Ordering::Greater => (a, b, d),
        std::cmp::Ordering::Equal => {
            return Ok(if b.is_some() && d.is_some() {
                Value::Bool(true)
            } else {
                Value::Null
            })
        }
    };
    Ok(match earlier_end {
        Some(end) if later < end => Value::Bool(true),
        Some(_) if later_end.is_some() => Value::Bool(false),
        _ => Value::Null,
    })
}
