//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::ast::BinaryOp;
use crate::error::{Result, SQLError};
use crate::expr::{cast_value_from_with_control, EvalContext};
use crate::type_resolution::runtime_value_type_with_control;
use crate::ColumnType;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
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
    // A retained field's OID determines the overload without reading its payload.
    let kind = |value: &Value| {
        if matches!(value, Value::Temporal(_) | Value::Datum(_)) {
            runtime_value_type_with_control(value, None, control)
        } else {
            Ok(None)
        }
    };
    let types = [kind(a)?, kind(b)?, kind(c)?, kind(d)?];
    let kinds = types.each_ref().map(Option::as_deref);
    let target = endpoint_type(&kinds);
    let endpoint = |value: &Value, ty: Option<&ColumnType>| {
        if target == "timestamptz" && matches!(ty, Some(ColumnType::Date | ColumnType::Timestamp)) {
            let local = cast_value_from_with_control(value, "timestamp", None, control)?;
            super::eval_timezone(std::slice::from_ref(&*local), context, control)
        } else {
            cast_value_from_with_control(value, target, None, control)
        }
    };
    let period = |start: &Value, start_ty, end: &Value, end_ty| -> Result<_> {
        let start = endpoint(start, start_ty)?;
        let end = if end_ty == Some(&ColumnType::Interval) {
            if matches!(*start, Value::Null) {
                Value::Null
            } else {
                let interval = cast_value_from_with_control(end, "interval", None, control)?;
                super::temporal_arith_with_control(
                    &start,
                    &interval,
                    BinaryOp::Add,
                    context.and_then(|ctx| ctx.engine),
                    control,
                )?
            }
        } else {
            endpoint(end, end_ty)?.into_parts().0
        };
        Ok(match (&*start, end) {
            (Value::Temporal(a), Value::Temporal(b)) if *a > b => Some((b, Some(a.clone()))),
            (Value::Temporal(a), Value::Temporal(b)) => Some((a.clone(), Some(b))),
            (Value::Null, Value::Temporal(b)) => Some((b, None)),
            (Value::Temporal(a), Value::Null) => Some((a.clone(), None)),
            _ => None,
        })
    };
    let left = period(a, kinds[0], b, kinds[1])?;
    let right = period(c, kinds[2], d, kinds[3])?;
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

fn endpoint_type(kinds: &[Option<&ColumnType>; 4]) -> &'static str {
    if kinds.contains(&Some(&ColumnType::TimestampTz)) {
        "timestamptz"
    } else if kinds.contains(&Some(&ColumnType::Date))
        || kinds.contains(&Some(&ColumnType::Timestamp))
    {
        "timestamp"
    } else if kinds.contains(&Some(&ColumnType::TimeTz)) {
        "timetz"
    } else if kinds.contains(&Some(&ColumnType::Time)) {
        "time"
    } else {
        "timestamptz"
    }
}
