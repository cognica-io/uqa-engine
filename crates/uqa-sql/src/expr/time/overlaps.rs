//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::BinaryOp;
use crate::error::{Result, SQLError};
use crate::expr::{cast_value_from_with_control, EvalContext};
use uqa_core::{
    memory::{Produced, ProductionControl},
    TemporalValue, Value,
};

pub(in crate::expr) fn overlaps(
    args: &[Value],
    context: Option<&EvalContext<'_>>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    control.check()?;
    let finish = |value| Ok(control.finish(value, control.empty_reservation())?);
    let [a, b, c, d] = args else {
        return Err(SQLError::TypeMismatch(
            "overlaps takes 4 args (start1, end1, start2, end2)".into(),
        ));
    };
    let target = endpoint_type(args);
    let endpoint = |value: &Value| {
        if target == "timestamptz"
            && matches!(
                value,
                Value::Temporal(TemporalValue::Date { .. } | TemporalValue::Timestamp { .. })
            )
        {
            let local = cast_value_from_with_control(value, "timestamp", None, control)?;
            super::eval_timezone(std::slice::from_ref(&*local), context, control)
        } else {
            cast_value_from_with_control(value, target, None, control)
        }
    };
    let period = |start: &Value, end: &Value| -> Result<_> {
        let start = endpoint(start)?;
        let end = if matches!(end, Value::Temporal(TemporalValue::Interval { .. })) {
            if matches!(*start, Value::Null) {
                Value::Null
            } else {
                super::temporal_arith_with_control(
                    &start,
                    end,
                    BinaryOp::Add,
                    context.and_then(|ctx| ctx.engine),
                    control,
                )?
            }
        } else {
            endpoint(end)?.into_parts().0
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
        return finish(Value::Null);
    };
    // A known start inside the other period proves overlap even if its end is unknown.
    let (later, later_end, earlier_end) = match a.cmp(&c) {
        std::cmp::Ordering::Less => (c, d, b),
        std::cmp::Ordering::Greater => (a, b, d),
        std::cmp::Ordering::Equal => {
            return finish(if b.is_some() && d.is_some() {
                Value::Bool(true)
            } else {
                Value::Null
            })
        }
    };
    finish(match earlier_end {
        Some(end) if later < end => Value::Bool(true),
        Some(_) if later_end.is_some() => Value::Bool(false),
        _ => Value::Null,
    })
}

fn endpoint_type(args: &[Value]) -> &'static str {
    let mut temporals = args.iter().filter_map(|value| match value {
        Value::Temporal(value) => Some(value),
        _ => None,
    });
    if temporals
        .clone()
        .any(|v| matches!(v, TemporalValue::TimestampTz { .. }))
    {
        "timestamptz"
    } else if temporals.clone().any(|v| {
        matches!(
            v,
            TemporalValue::Date { .. } | TemporalValue::Timestamp { .. }
        )
    }) {
        "timestamp"
    } else if temporals
        .clone()
        .any(|v| matches!(v, TemporalValue::TimeTz { .. }))
    {
        "timetz"
    } else if temporals.any(|v| matches!(v, TemporalValue::Time { .. })) {
        "time"
    } else {
        "timestamptz"
    }
}
