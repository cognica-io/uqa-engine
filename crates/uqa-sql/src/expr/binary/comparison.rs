//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL NULL and row comparison rules share the native controlled value owners.

use super::super::enums::{EnumComparisonState, EnumLabelCatalog};
use super::{BinaryOp, Result, SQLError, Value};
use std::cmp::Ordering;
use uqa_core::memory::ProductionControl;

#[cfg(test)]
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

/// Observe scalar and nested enums through the caller's catalog, preserving private scalar/row call state, shared type support state and ordinary three-valued rules.
pub fn eval_comparison_truth_with_enum_catalog(
    op: BinaryOp,
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
    enums: Option<&dyn super::super::enums::EnumLabelCatalog>,
    state: Option<&super::super::enums::EnumComparisonState>,
) -> Result<Option<bool>> {
    control.check()?;
    if matches!(op, BinaryOp::Equal | BinaryOp::NotEqual) {
        return values_equal_nullable_with_catalog(left, right, control, enums)
            .map(|equal| equal.map(|equal| if op == BinaryOp::Equal { equal } else { !equal }));
    }
    if !matches!(
        op,
        BinaryOp::Less | BinaryOp::LessEqual | BinaryOp::Greater | BinaryOp::GreaterEqual
    ) {
        return Err(SQLError::Internal(format!(
            "non-comparison operator {op:?} reached comparison evaluation"
        )));
    }
    let order = compare_nullable_with_catalog(left, right, control, enums, state)?;
    Ok(order.map(|order| match op {
        BinaryOp::Less => order.is_lt(),
        BinaryOp::LessEqual => order.is_le(),
        BinaryOp::Greater => order.is_gt(),
        BinaryOp::GreaterEqual => order.is_ge(),
        _ => unreachable!("only comparison operators reach comparison evaluation"),
    }))
}

/// Two-valued equality treats SQL UNKNOWN as no match for CASE, NULLIF and membership probes.
#[cfg(test)]
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

#[cfg(test)]
pub(in crate::expr) fn values_equal_nullable(a: &Value, b: &Value) -> Result<Option<bool>> {
    values_equal_nullable_with_control(a, b, &ProductionControl::uncontrolled())
}

pub fn values_equal_nullable_with_control(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
) -> Result<Option<bool>> {
    values_equal_nullable_with_catalog(a, b, control, None)
}

fn values_equal_nullable_with_catalog(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
    enums: Option<&dyn super::super::enums::EnumLabelCatalog>,
) -> Result<Option<bool>> {
    control.check()?;
    if let Some(value) = super::super::enums::eval_comparison(BinaryOp::Equal, a, b, enums, None)? {
        return match value {
            Value::Null => Ok(None),
            Value::Bool(value) => Ok(Some(value)),
            _ => unreachable!("equality returns boolean or NULL"),
        };
    }
    if let Some(order) = super::super::datums::compare_jsonb_with_control(a, b, control)? {
        return Ok(Some(order.is_eq()));
    }
    let equal = match (a, b) {
        (Value::Null, _) | (_, Value::Null) => None,
        (Value::Datum(datum), _) => {
            return values_equal_nullable_with_catalog(
                &*super::super::datums::read_with_control(datum, control)?,
                b,
                control,
                enums,
            )
        }
        (_, Value::Datum(datum)) => {
            return values_equal_nullable_with_catalog(
                a,
                &*super::super::datums::read_with_control(datum, control)?,
                control,
                enums,
            )
        }
        (Value::Temporal(x), Value::Str(y)) | (Value::Str(y), Value::Temporal(x)) => Some(
            x.parse_same_kind_in_order_with_control(
                y,
                crate::expr::transaction_timestamp_or_clock(),
                crate::expr::temporal_date_order(),
                control,
            )?
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
                let equal = if matches!((x, y), (Value::Row(_), Value::Row(_))) {
                    Some(equal_values(x, y, control, enums, false)?)
                } else {
                    values_equal_nullable_with_catalog(x, y, control, enums)?
                };
                match equal {
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
        _ => Some(equal_values(a, b, control, enums, false)?),
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
    compare_nullable_with_catalog(a, b, control, None, None)
}

fn compare_nullable_with_catalog(
    a: &Value,
    b: &Value,
    control: &ProductionControl<'_>,
    enums: Option<&dyn EnumLabelCatalog>,
    state: Option<&EnumComparisonState>,
) -> Result<Option<Ordering>> {
    control.check()?;
    if matches!(a, Value::Null) || matches!(b, Value::Null) {
        return Ok(None);
    }
    if let Some(order) = super::super::enums::comparison_order(a, b, enums, state, false)? {
        return Ok(Some(order));
    }
    if let Some(order) = super::super::datums::compare_jsonb_with_control(a, b, control)? {
        return Ok(Some(order));
    }
    match (a, b) {
        (Value::Null, _) | (_, Value::Null) => Ok(None),
        (Value::Datum(datum), _) => compare_nullable_with_catalog(
            &*super::super::datums::read_with_control(datum, control)?,
            b,
            control,
            enums,
            state,
        ),
        (_, Value::Datum(datum)) => compare_nullable_with_catalog(
            a,
            &*super::super::datums::read_with_control(datum, control)?,
            control,
            enums,
            state,
        ),
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
        | (Value::Record(_), Value::Record(_) | Value::Row(_))
        | (Value::Row(_), Value::Record(_))
        | (Value::Enum(_), Value::Enum(_)) => Ok(Some(compare_sql_values(a, b, control, enums)?)),
        (Value::FixedChar(x), Value::Str(y)) | (Value::Str(x), Value::FixedChar(y)) => {
            Ok(Some(compare_fixed_text(x, y, control)?))
        }
        (Value::Temporal(x), Value::Str(y)) => x
            .parse_same_kind_in_order_with_control(
                y,
                crate::expr::transaction_timestamp_or_clock(),
                crate::expr::temporal_date_order(),
                control,
            )?
            .map(|parsed| Some(x.cmp(&parsed)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        (Value::Str(x), Value::Temporal(y)) => y
            .parse_same_kind_in_order_with_control(
                x,
                crate::expr::transaction_timestamp_or_clock(),
                crate::expr::temporal_date_order(),
                control,
            )?
            .map(|parsed| Some(parsed.cmp(y)))
            .ok_or_else(|| SQLError::TypeMismatch(format!("cannot compare {a:?} with {b:?}"))),
        // Ordering is lexicographic; reaching NULL before a definite comparison leaves it unknown.
        (Value::Row(xs), Value::Row(ys)) => compare_anonymous_row(xs, ys, control, enums, state),
        (lhs, rhs) => Err(SQLError::TypeMismatch(format!(
            "cannot compare {lhs:?} with {rhs:?}"
        ))),
    }
}

fn compare_anonymous_row(
    left: &[Value],
    right: &[Value],
    control: &ProductionControl<'_>,
    enums: Option<&dyn EnumLabelCatalog>,
    state: Option<&EnumComparisonState>,
) -> Result<Option<Ordering>> {
    for (index, (left, right)) in left.iter().zip(right).enumerate() {
        let field =
            if matches!(left, Value::Enum(_) | Value::Datum(_)) && !matches!(right, Value::Null) {
                state.map(|state| state.field(index))
            } else {
                None
            };
        let order = if matches!((left, right), (Value::Row(_), Value::Row(_))) {
            Some(compare_typed_values_with_catalog(
                left, right, control, enums,
            )?)
        } else {
            compare_nullable_with_catalog(left, right, control, enums, field.as_deref())?
        };
        match order {
            Some(Ordering::Equal) => {}
            other => return Ok(other),
        }
    }
    Ok(Some(left.len().cmp(&right.len())))
}

fn compare_sql_values(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
    enums: Option<&dyn EnumLabelCatalog>,
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
    compare_typed_values_with_catalog(left, right, control, enums)
}

/// Compare already-bound values, preserving type operator failures and total container NULL semantics. Callers supply top-level NULL placement and must apply operator-selected casts first.
pub fn compare_typed_values_with_control(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering> {
    compare_typed_values_with_catalog(left, right, control, None)
}

fn compare_typed_values_with_catalog(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
    enums: Option<&dyn EnumLabelCatalog>,
) -> Result<Ordering> {
    control.check()?;
    if let Some(order) = super::super::datums::compare_jsonb_with_control(left, right, control)? {
        return Ok(order);
    }
    match (left, right) {
        (Value::Null, Value::Null) => return Ok(Ordering::Equal),
        (Value::Null, _) => return Ok(Ordering::Greater),
        (_, Value::Null) => return Ok(Ordering::Less),
        _ => {}
    }
    if let Some(order) = super::super::enums::comparison_order(left, right, enums, None, true)? {
        return Ok(order);
    }
    match (left, right) {
        (Value::Datum(datum), _) => {
            return compare_typed_values_with_catalog(
                &*super::super::datums::read_with_control(datum, control)?,
                right,
                control,
                enums,
            )
        }
        (_, Value::Datum(datum)) => {
            return compare_typed_values_with_catalog(
                left,
                &*super::super::datums::read_with_control(datum, control)?,
                control,
                enums,
            )
        }
        (Value::Array(left), Value::Array(right)) => {
            validate_array_element_types(left, right)?;
            return left.cmp_by_with_control(right, control, |left, right, control| {
                compare_typed_values_with_catalog(left, right, control, enums)
            });
        }
        (Value::Record(left), Value::Record(right)) => {
            return compare_sequence(
                left.iter().map(|(_, v)| v),
                right.iter().map(|(_, v)| v),
                control,
                enums,
            );
        }
        // A composite value compared with an anonymous row uses the record operators, which order NULL fields after all others.
        (Value::Record(left), Value::Row(right)) => {
            return compare_sequence(left.iter().map(|(_, v)| v), right.iter(), control, enums);
        }
        (Value::Row(left), Value::Record(right)) => {
            return compare_sequence(left.iter(), right.iter().map(|(_, v)| v), control, enums);
        }
        (Value::Row(left), Value::Row(right)) => {
            return compare_sequence(left.iter(), right.iter(), control, enums);
        }
        (Value::List(left), Value::List(right)) => {
            return compare_sequence(left.iter(), right.iter(), control, enums);
        }
        // Enum operators are declared on one enum type; binding coerces every other operand to it.
        (Value::Enum(left), Value::Enum(right)) if left.type_oid() == right.type_oid() => {
            return Ok(left.key().cmp(right.key()));
        }
        (Value::Enum(_), _) | (_, Value::Enum(_)) => {
            return Err(SQLError::Internal(format!(
                "enum comparison reached operands of different types: {} and {}",
                comparison_operand_type(left),
                comparison_operand_type(right)
            )));
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

fn comparison_operand_type(value: &Value) -> String {
    match value {
        Value::Enum(label) => format!("enum type OID {}", label.type_oid()),
        other => super::super::diagnostics::value_type_name(other).to_owned(),
    }
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
        ColumnType::OidVector
        | ColumnType::Record
        | ColumnType::Composite(_)
        | ColumnType::Array(_) => true,
        ColumnType::Domain { base: element, .. } => type_comparison_can_fail(element),
        _ => false,
    }
}

/// Whether an opaque value key can suppress a SQL operator failure. A single such input remains legal until an operator compares it.
pub fn value_comparison_can_fail(value: &Value) -> bool {
    match value {
        Value::Datum(_) => true,
        Value::LegacyVector(vector) => {
            vector.kind() == uqa_core::LegacyVectorKind::Oid && !vector.has_vector_layout()
        }
        Value::Array(array) => {
            array.element_type_oid().is_some()
                || array.elements().iter().any(value_comparison_can_fail)
        }
        Value::Row(values) => values.iter().any(value_comparison_can_fail),
        Value::List(values) => values.iter().any(value_comparison_can_fail),
        Value::Record(fields) => fields
            .iter()
            .any(|(_, value)| value_comparison_can_fail(value)),
        _ => false,
    }
}

/// Compare already-bound group/hash keys without enum output. Physical enum
/// identities use the same equality as `enum_eq`, including inside containers.
pub fn equal_typed_values_with_enum_catalog(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
    enums: Option<&dyn super::super::enums::EnumLabelCatalog>,
) -> Result<bool> {
    equal_values(left, right, control, enums, true)
}

fn equal_values(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
    enums: Option<&dyn super::super::enums::EnumLabelCatalog>,
    typed: bool,
) -> Result<bool> {
    control.check()?;
    if let Some(order) = super::super::datums::compare_jsonb_with_control(left, right, control)? {
        return Ok(order.is_eq());
    }
    if matches!(left, Value::Null) || matches!(right, Value::Null) {
        return Ok(matches!((left, right), (Value::Null, Value::Null)));
    }
    if let Some(enums) = enums {
        if let (Some(left), Some(right)) = (
            super::super::enums::comparison_identity(enums, left)?,
            super::super::enums::comparison_identity(enums, right)?,
        ) {
            return Ok(left == right);
        }
    }
    match (left, right) {
        (Value::Datum(datum), _) => equal_values(
            &*super::super::datums::read_with_control(datum, control)?,
            right,
            control,
            enums,
            typed,
        ),
        (_, Value::Datum(datum)) => equal_values(
            left,
            &*super::super::datums::read_with_control(datum, control)?,
            control,
            enums,
            typed,
        ),
        (Value::Array(left), Value::Array(right)) => {
            validate_array_element_types(left, right)?;
            left.eq_by_with_control(right, control, |left, right, control| {
                equal_values(left, right, control, enums, typed)
            })
        }
        (Value::Record(left), Value::Record(right)) => equal_sequence(
            left.iter().map(|(_, v)| v),
            right.iter().map(|(_, v)| v),
            control,
            enums,
            typed,
        ),
        (Value::Record(left), Value::Row(right)) => equal_sequence(
            left.iter().map(|(_, v)| v),
            right.iter(),
            control,
            enums,
            typed,
        ),
        (Value::Row(left), Value::Record(right)) => equal_sequence(
            left.iter(),
            right.iter().map(|(_, v)| v),
            control,
            enums,
            typed,
        ),
        (Value::Row(left), Value::Row(right)) => {
            equal_sequence(left.iter(), right.iter(), control, enums, typed)
        }
        (Value::List(left), Value::List(right)) => {
            equal_sequence(left.iter(), right.iter(), control, enums, typed)
        }
        _ if typed => Ok(compare_typed_values_with_control(left, right, control)?.is_eq()),
        _ => Ok(compare_sql_values(left, right, control, enums)?.is_eq()),
    }
}

fn validate_array_element_types(
    left: &uqa_core::ArrayValue,
    right: &uqa_core::ArrayValue,
) -> Result<()> {
    if matches!((left.element_type_oid(), right.element_type_oid()), (Some(left), Some(right)) if left != right)
    {
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "cannot compare arrays of different element types".into(),
        });
    }
    Ok(())
}

fn equal_sequence<'a>(
    mut left: impl Iterator<Item = &'a Value>,
    mut right: impl Iterator<Item = &'a Value>,
    control: &ProductionControl<'_>,
    enums: Option<&dyn super::super::enums::EnumLabelCatalog>,
    typed: bool,
) -> Result<bool> {
    loop {
        control.check()?;
        match (left.next(), right.next()) {
            (Some(left), Some(right)) if equal_values(left, right, control, enums, typed)? => {}
            (None, None) => return Ok(true),
            _ => return Ok(false),
        }
    }
}

fn compare_sequence<'a>(
    mut left: impl Iterator<Item = &'a Value>,
    mut right: impl Iterator<Item = &'a Value>,
    control: &ProductionControl<'_>,
    enums: Option<&dyn EnumLabelCatalog>,
) -> Result<Ordering> {
    loop {
        control.check()?;
        let ordering = match (left.next(), right.next()) {
            (Some(left), Some(right)) => {
                compare_typed_values_with_catalog(left, right, control, enums)?
            }
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
