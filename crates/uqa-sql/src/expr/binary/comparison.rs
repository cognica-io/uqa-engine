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
pub(in crate::expr) fn values_equal(a: &Value, b: &Value) -> Result<bool> {
    Ok(values_equal_nullable(a, b)? == Some(true))
}

pub fn values_equal_with_control(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    Ok(values_equal_nullable_with_control(a, b, control)? == Some(true))
}

pub(in crate::expr) fn values_equal_nullable(a: &Value, b: &Value) -> Result<Option<bool>> {
    values_equal_nullable_with_control(a, b, &ProductionControl::uncontrolled())
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
        _ => Some(equal_sql_values(a, b, control)?),
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
        | (Value::LegacyVector(_), Value::LegacyVector(_))
        | (Value::List(_), Value::List(_))
        | (Value::Record(_), Value::Record(_)) => Ok(Some(compare_sql_values(a, b, control)?)),
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

fn compare_sql_values(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    // Primitive mixed float/integer and float/numeric operators select float8 inputs. Physical keys and already-bound container elements keep their exact carrier order.
    if matches!(
        (left, right),
        (Value::Float(_), Value::Int(_) | Value::Decimal(_))
            | (Value::Int(_) | Value::Decimal(_), Value::Float(_))
    ) {
        let left =
            super::super::cast_value_from_with_control(left, "double precision", None, control)?;
        let right =
            super::super::cast_value_from_with_control(right, "double precision", None, control)?;
        return Ok(left.cmp(&right));
    }
    compare_typed_values_with_control(left, right, control)
}

/// Compare already-bound values, preserving type operator failures and total container NULL semantics. Callers supply top-level NULL placement and must apply operator-selected casts first.
pub fn compare_typed_values_with_control(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    control.check()?;
    match (left, right) {
        (Value::Null, Value::Null) => return Ok(Ordering::Equal),
        (Value::Null, _) => return Ok(Ordering::Greater),
        (_, Value::Null) => return Ok(Ordering::Less),
        (Value::Array(left), Value::Array(right)) => {
            return left.cmp_by_with_control(right, control, compare_typed_values_with_control);
        }
        (Value::Record(left), Value::Record(right)) => {
            return compare_sequence(
                left.iter().map(|(_, v)| v),
                right.iter().map(|(_, v)| v),
                control,
            );
        }
        (Value::Row(left), Value::Row(right)) | (Value::List(left), Value::List(right)) => {
            return compare_sequence(left.iter(), right.iter(), control);
        }
        _ => {}
    }
    for value in [left, right] {
        if let Value::LegacyVector(vector) = value {
            validate_legacy_vector_comparison(vector)?;
        }
    }
    left.cmp_with_control(right, control).map_err(Into::into)
}

/// Validate the layout required by the `oidvector` scalar equality, ordering and hashing operators. Array operators on `int2vector` permit dimensionless arrays.
pub fn validate_legacy_vector_comparison(vector: &uqa_core::LegacyVectorValue) -> Result<()> {
    if vector.kind() == uqa_core::LegacyVectorKind::Oid && !vector.has_vector_layout() {
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "array is not a valid oidvector".into(),
        });
    }
    Ok(())
}

/// Declared SQL keys whose runtime values may require a fallible comparison operator.
pub fn type_comparison_can_fail(ty: &crate::ast::ColumnType) -> bool {
    use crate::ast::ColumnType;
    match ty {
        ColumnType::OidVector | ColumnType::Record => true,
        ColumnType::Array(element) | ColumnType::Domain { base: element, .. } => {
            type_comparison_can_fail(element)
        }
        _ => false,
    }
}

/// Whether an opaque value key can suppress a SQL operator failure. A single such input remains legal until an operator compares it.
pub fn value_comparison_can_fail(value: &Value) -> bool {
    match value {
        Value::LegacyVector(vector) => {
            vector.kind() == uqa_core::LegacyVectorKind::Oid && !vector.has_vector_layout()
        }
        Value::Array(array) => array.elements().iter().any(value_comparison_can_fail),
        Value::Row(values) | Value::List(values) => values.iter().any(value_comparison_can_fail),
        Value::Record(fields) => fields
            .iter()
            .any(|(_, value)| value_comparison_can_fail(value)),
        _ => false,
    }
}

fn equal_sql_values(left: &Value, right: &Value, control: &ProductionControl<'_>) -> Result<bool> {
    control.check()?;
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            left.eq_by_with_control(right, control, equal_sql_values)
        }
        (Value::Record(left), Value::Record(right)) => equal_sequence(
            left.iter().map(|(_, v)| v),
            right.iter().map(|(_, v)| v),
            control,
        ),
        (Value::Row(left), Value::Row(right)) | (Value::List(left), Value::List(right)) => {
            equal_sequence(left.iter(), right.iter(), control)
        }
        _ => Ok(compare_sql_values(left, right, control)?.is_eq()),
    }
}

fn equal_sequence<'a>(
    mut left: impl Iterator<Item = &'a Value>,
    mut right: impl Iterator<Item = &'a Value>,
    control: &ProductionControl<'_>,
) -> Result<bool> {
    loop {
        control.check()?;
        match (left.next(), right.next()) {
            (Some(left), Some(right)) if equal_sql_values(left, right, control)? => {}
            (None, None) => return Ok(true),
            _ => return Ok(false),
        }
    }
}

fn compare_sequence<'a>(
    mut left: impl Iterator<Item = &'a Value>,
    mut right: impl Iterator<Item = &'a Value>,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    loop {
        control.check()?;
        let ordering = match (left.next(), right.next()) {
            (Some(left), Some(right)) => compare_typed_values_with_control(left, right, control)?,
            (Some(_), None) => Ordering::Greater,
            (None, Some(_)) => Ordering::Less,
            (None, None) => return Ok(Ordering::Equal),
        };
        if !ordering.is_eq() {
            return Ok(ordering);
        }
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
