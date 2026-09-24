//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Value comparisons preserve the native ordering while owning decimal, JSONB and array traversal workspace.

use super::{ArrayValue, DecimalValue, Value};
use crate::{memory::ProductionControl, ValueRetentionError};
use std::cmp::Ordering;

impl Value {
    /// Compare through the existing numeric and JSONB owners under the producer's allowance. Borrowed scalar comparisons allocate nothing; recursive carriers retain traversal and comparison scratch only for the call.
    pub fn cmp_with_control(
        &self,
        other: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Ordering, ValueRetentionError> {
        control.check()?;
        if control.budget().is_none() {
            return Ok(self.cmp(other));
        }
        let ordering = match (self, other) {
            (Self::Decimal(left), Self::Decimal(right)) => left.cmp_with_control(right, control)?,
            (Self::Int(left), Self::Decimal(right)) => {
                DecimalValue::from_i64_with_control(*left, control)?
                    .cmp_with_control(right, control)?
            }
            (Self::Decimal(left), Self::Int(right)) => left.cmp_with_control(
                &*DecimalValue::from_i64_with_control(*right, control)?,
                control,
            )?,
            (Self::Bool(left), Self::Decimal(right)) => {
                DecimalValue::from_i64_with_control(i64::from(*left), control)?
                    .cmp_with_control(right, control)?
            }
            (Self::Decimal(left), Self::Bool(right)) => left.cmp_with_control(
                &*DecimalValue::from_i64_with_control(i64::from(*right), control)?,
                control,
            )?,
            (Self::Float(left), Self::Decimal(right)) => {
                compare_float_decimal(*left, right, control)?
            }
            (Self::Decimal(left), Self::Float(right)) => {
                compare_float_decimal(*right, left, control)?.reverse()
            }
            (Self::Str(left), Self::Str(right)) | (Self::Json(left), Self::Json(right)) => {
                compare_bytes(left.as_bytes(), right.as_bytes(), control)?
            }
            (Self::FixedChar(left), Self::FixedChar(right)) => compare_bytes(
                trim_spaces(left, control)?.as_bytes(),
                trim_spaces(right, control)?.as_bytes(),
                control,
            )?,
            (Self::Bytes(left), Self::Bytes(right)) => compare_bytes(left, right, control)?,
            (Self::JsonB(left), Self::JsonB(right)) => {
                super::super::jsonb::compare_jsonb_text_with_control(left, right, control)?
            }
            (Self::List(left), Self::List(right)) | (Self::Row(left), Self::Row(right)) => {
                compare_sequence(left.iter(), right.iter(), control)?
            }
            (Self::Record(left), Self::Record(right)) => compare_sequence(
                left.iter().map(|(_, value)| value),
                right.iter().map(|(_, value)| value),
                control,
            )?,
            (Self::Array(left), Self::Array(right)) => compare_arrays(left, right, control)?,
            (Self::LegacyVector(left), Self::LegacyVector(right)) => {
                let prefix = left.compare_prefix(right);
                if prefix.is_eq() {
                    if left.kind() == crate::LegacyVectorKind::SmallInteger {
                        compare_arrays(left.as_array(), right.as_array(), control)?
                    } else {
                        compare_sequence(left.elements().iter(), right.elements().iter(), control)?
                    }
                } else {
                    prefix
                }
            }
            (Self::Map(left), Self::Map(right)) => {
                let mut left = left.iter();
                let mut right = right.iter();
                loop {
                    control.check()?;
                    match (left.next(), right.next()) {
                        (Some((a, x)), Some((b, y))) => {
                            let ordering = compare_bytes(a.as_bytes(), b.as_bytes(), control)?;
                            let ordering = if ordering.is_eq() {
                                x.cmp_with_control(y, control)?
                            } else {
                                ordering
                            };
                            if !ordering.is_eq() {
                                break ordering;
                            }
                        }
                        (Some(_), None) => break Ordering::Greater,
                        (None, Some(_)) => break Ordering::Less,
                        (None, None) => break Ordering::Equal,
                    }
                }
            }
            // Remaining native comparisons have inline carriers or only compare variant identities.
            _ => self.cmp(other),
        };
        control.check()?;
        Ok(ordering)
    }
}

fn compare_float_decimal(
    float: f64,
    decimal: &DecimalValue,
    control: &ProductionControl<'_>,
) -> Result<Ordering, ValueRetentionError> {
    DecimalValue::from_f64_exact_with_control(float, control)?.cmp_with_control(decimal, control)
}

pub(crate) fn compare_bytes(
    left: &[u8],
    right: &[u8],
    control: &ProductionControl<'_>,
) -> Result<Ordering, ValueRetentionError> {
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

fn trim_spaces<'a>(
    text: &'a str,
    control: &ProductionControl<'_>,
) -> Result<&'a str, ValueRetentionError> {
    let mut end = text.len();
    while end != 0 && text.as_bytes()[end - 1] == b' ' {
        if end.is_multiple_of(4096) {
            control.check()?;
        }
        end -= 1;
    }
    Ok(&text[..end])
}

fn compare_sequence<'a>(
    mut left: impl Iterator<Item = &'a Value>,
    mut right: impl Iterator<Item = &'a Value>,
    control: &ProductionControl<'_>,
) -> Result<Ordering, ValueRetentionError> {
    loop {
        control.check()?;
        match (left.next(), right.next()) {
            (Some(left), Some(right)) => {
                let ordering = compare_element(left, right, control)?;
                if !ordering.is_eq() {
                    return Ok(ordering);
                }
            }
            (Some(_), None) => return Ok(Ordering::Greater),
            (None, Some(_)) => return Ok(Ordering::Less),
            (None, None) => return Ok(Ordering::Equal),
        }
    }
}

fn compare_element(
    left: &Value,
    right: &Value,
    control: &ProductionControl<'_>,
) -> Result<Ordering, ValueRetentionError> {
    match (left, right) {
        (Value::Null, Value::Null) => Ok(Ordering::Equal),
        (Value::Null, _) => Ok(Ordering::Greater),
        (_, Value::Null) => Ok(Ordering::Less),
        _ => left.cmp_with_control(right, control),
    }
}

fn compare_arrays(
    left: &ArrayValue,
    right: &ArrayValue,
    control: &ProductionControl<'_>,
) -> Result<Ordering, ValueRetentionError> {
    left.cmp_by_with_control(right, control, Value::cmp_with_control)
}

#[cfg(test)]
mod tests;
