//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL NULL and row comparison rules share the native controlled value owners.

use super::{BinaryOp, Result, SQLError, Value};
use std::cmp::Ordering;
use uqa_core::memory::ProductionControl;

pub(in crate::expr) fn eval_comparison_op(op: BinaryOp, l: &Value, r: &Value) -> Result<Value> {
    Ok(eval_comparison_truth(op, l, r)?
        .map(Value::Bool)
        .unwrap_or(Value::Null))
}

/// Compare two values without allocating an intermediate [`Value::Bool`]. `None` represents SQL UNKNOWN, including an undecided anonymous row comparison.
#[inline]
pub fn eval_comparison_truth(op: BinaryOp, l: &Value, r: &Value) -> Result<Option<bool>> {
    eval_comparison_truth_with_control(op, l, r, &ProductionControl::uncontrolled())
}

/// Evaluate the same three-valued comparison while admitting the native numeric, JSONB and array comparison workspace to the caller's allowance.
pub fn eval_comparison_truth_with_control(
    op: BinaryOp,
    l: &Value,
    r: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<bool>> {
    control.check()?;
    let out = match op {
        BinaryOp::Equal => values_equal_nullable_with_control(l, r, control)?,
        BinaryOp::NotEqual => values_equal_nullable_with_control(l, r, control)?.map(|v| !v),
        BinaryOp::Less => compare_nullable_with_control(l, r, control)?.map(|v| v.is_lt()),
        BinaryOp::LessEqual => compare_nullable_with_control(l, r, control)?.map(|v| v.is_le()),
        BinaryOp::Greater => compare_nullable_with_control(l, r, control)?.map(|v| v.is_gt()),
        BinaryOp::GreaterEqual => compare_nullable_with_control(l, r, control)?.map(|v| v.is_ge()),
        _ => {
            return Err(SQLError::Internal(format!(
                "non-comparison operator {op:?} reached comparison evaluation"
            )))
        }
    };
    Ok(out)
}

/// Two-valued equality treats SQL UNKNOWN as no match for CASE, NULLIF and membership probes.
pub(in crate::expr) fn values_equal(a: &Value, b: &Value) -> bool {
    values_equal_nullable(a, b) == Some(true)
}

pub fn values_equal_with_control(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    Ok(values_equal_nullable_with_control(a, b, control)? == Some(true))
}

pub(in crate::expr) fn values_equal_nullable(a: &Value, b: &Value) -> Option<bool> {
    values_equal_nullable_with_control(a, b, &ProductionControl::uncontrolled())
        .expect("uncontrolled equality has no resource failure")
}

pub fn values_equal_nullable_with_control(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<bool>> {
    control.check()?;
    let equal = match (a, b) {
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Temporal(x), Value::Str(y)) | (Value::Str(y), Value::Temporal(x)) => Some(
            x.parse_same_kind_with_control(y, control)?
                .is_some_and(|parsed| x.cmp(&parsed).is_eq()),
        ),
        (Value::FixedChar(x), Value::Str(y)) | (Value::Str(y), Value::FixedChar(x)) => {
            Some(compare_fixed_text(x, y, control)?.is_eq())
        }
        // Anonymous rows use SQL three-valued equality: a definite mismatch wins over an earlier NULL field. Native arrays and stored records instead use total element equality through the value owner below.
        (Value::Row(xs), Value::Row(ys)) => {
            if xs.len() != ys.len() {
                return Ok(Some(false));
            }
            let mut unknown = false;
            for (x, y) in xs.iter().zip(ys) {
                match values_equal_nullable_with_control(x, y, control)? {
                    Some(false) => return Ok(Some(false)),
                    Some(true) => {}
                    None => unknown = true,
                }
            }
            if unknown {
                None
            } else {
                Some(true)
            }
        }
        _ => Some(a.cmp_with_control(b, control)?.is_eq()),
    };
    Ok(equal)
}

/// Compare with the existing two-valued selector convention that SQL UNKNOWN sorts as equal.
pub fn compare_with_control(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    Ok(compare_nullable_with_control(a, b, control)?.unwrap_or(Ordering::Equal))
}

pub(in crate::expr) fn compare_nullable(a: &Value, b: &Value) -> Result<Option<Ordering>> {
    compare_nullable_with_control(a, b, &ProductionControl::uncontrolled())
}

pub fn compare_nullable_with_control(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<Ordering>> {
    control.check()?;
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Ok(None),
        (
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
            Value::Int(_) | Value::Float(_) | Value::Decimal(_),
        )
        | (Value::Bool(_), Value::Decimal(_))
        | (Value::Decimal(_), Value::Bool(_))
        | (Value::Str(_), Value::Str(_))
        | (Value::FixedChar(_), Value::FixedChar(_))
        | (Value::JsonB(_), Value::JsonB(_))
        | (Value::Temporal(_), Value::Temporal(_))
        | (Value::Bool(_), Value::Bool(_))
        | (Value::Array(_), Value::Array(_))
        | (Value::List(_), Value::List(_))
        | (Value::Record(_), Value::Record(_)) => Ok(Some(a.cmp_with_control(b, control)?)),
        (Value::FixedChar(x), Value::Str(y)) | (Value::Str(x), Value::FixedChar(y)) => {
            Ok(Some(compare_fixed_text(x, y, control)?))
        }
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind_with_control(y, control)?
            .map(|parsed| Some(x.cmp(&parsed)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind_with_control(x, control)?
            .map(|parsed| Some(parsed.cmp(y)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        // Ordering is lexicographic; reaching NULL before a definite comparison leaves it unknown.
        (Value::Row(xs), Value::Row(ys)) => {
            for (x, y) in xs.iter().zip(ys) {
                match compare_nullable_with_control(x, y, control)? {
                    Some(Ordering::Equal) => {}
                    Some(other) => return Ok(Some(other)),
                    None => return Ok(None),
                }
            }
            Ok(Some(xs.len().cmp(&ys.len())))
        }
        (lhs, rhs) => Err(SQLError::TypeMismatch(format!(
            "cannot compare {lhs:?} with {rhs:?}"
        ))),
    }
}

fn compare_fixed_text(
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    fn trim<'a>(text: &'a str, control: &ProductionControl<'_>) -> Result<&'a [u8]> {
        let mut bytes = text.as_bytes();
        let mut checked = 0;
        while bytes.last() == Some(&b' ') {
            if checked % 4096 == 0 {
                control.check()?;
            }
            bytes = &bytes[..bytes.len() - 1];
            checked += 1;
        }
        Ok(bytes)
    }
    let left = trim(left, control)?;
    let right = trim(right, control)?;
    for (left, right) in left.chunks(4096).zip(right.chunks(4096)) {
        control.check()?;
        let ordering = left.cmp(right);
        if !ordering.is_eq() {
            return Ok(ordering);
        }
    }
    control.check()?;
    Ok(left.len().cmp(&right.len()))
}

#[cfg(test)]
mod tests;
