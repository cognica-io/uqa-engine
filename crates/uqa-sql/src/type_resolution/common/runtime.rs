//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Runtime type observation borrows carriers and retains an inline scalar plus array depth. Only a consumer requesting an owned type allocates array boxes.

use super::{category_choice, common_scalar_type, numeric_type, CategoryChoice};
use crate::{ColumnType, SQLError, SQLParam};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

enum ParameterType<'a> {
    Declared(&'a ColumnType),
    Inferred(Option<RuntimeType>),
}

fn parameter_type<'a>(
    parameter: &'a SQLParam,
    control: &ProductionControl<'_>,
) -> Result<ParameterType<'a>, SQLError> {
    control.check()?;
    let scalar = match parameter {
        SQLParam::TypedScalar { ty, .. } | SQLParam::DeferredError { ty, .. } => {
            return Ok(ParameterType::Declared(ty));
        }
        SQLParam::Scalar(value) => {
            return Ok(ParameterType::Inferred(RuntimeType::infer(value, control)?))
        }
        SQLParam::Vector(values) => u32::try_from(values.len()).ok().map(ColumnType::Vector),
        SQLParam::TypedTensor(value) => Some(ColumnType::Tensor(value.dimensions())),
        SQLParam::Tensor(values) => values
            .first()
            .and_then(|values| u32::try_from(values.len()).ok())
            .map(ColumnType::Tensor),
    };
    Ok(ParameterType::Inferred(scalar.map(RuntimeType::scalar)))
}

pub(crate) fn parameter_type_with_control(
    parameter: &SQLParam,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    match parameter_type(parameter, control)? {
        ParameterType::Declared(ty) => Ok(Some(ty.clone_with_control(control)?)),
        ParameterType::Inferred(ty) => ty.map(|ty| ty.into_produced(control)).transpose(),
    }
}

/// Compare the same shape used by inference without allocating a type or reading a deferred parameter value. Matching precedes SQL analysis and must not acquire optimizer inputs or introduce allocation failures before it.
pub(crate) fn parameter_type_matches(parameter: &SQLParam, expected: Option<&ColumnType>) -> bool {
    match parameter_type(parameter, &ProductionControl::uncontrolled())
        .expect("ordinary runtime type observation cannot be cancelled or limited")
    {
        ParameterType::Declared(ty) => expected == Some(ty),
        ParameterType::Inferred(None) => expected.is_none(),
        ParameterType::Inferred(Some(ty)) => expected.is_some_and(|expected| ty.matches(expected)),
    }
}

#[derive(PartialEq)]
pub(super) struct RuntimeType {
    // Runtime value inference selects only builtin inline scalar types, never names, domains or catalog descriptors.
    scalar: ColumnType,
    arrays: usize,
}

impl RuntimeType {
    fn scalar(scalar: ColumnType) -> Self {
        Self { scalar, arrays: 0 }
    }

    fn array(mut self) -> Self {
        self.arrays += 1;
        self
    }

    fn matches(&self, mut expected: &ColumnType) -> bool {
        for _ in 0..self.arrays {
            let ColumnType::Array(element) = expected else {
                return false;
            };
            expected = element;
        }
        self.scalar == *expected
    }

    pub(super) fn into_produced(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<ColumnType>, SQLError> {
        let mut ty = control.finish(self.scalar, control.empty_reservation())?;
        for _ in 0..self.arrays {
            ty = ColumnType::array_with_control(ty, control)?;
        }
        Ok(ty)
    }

    fn merge(self, other: Self) -> Option<Self> {
        if self == other {
            return Some(self);
        }
        let arrays = match (self.arrays, other.arrays) {
            (0, 0) => 0,
            (0, _) | (_, 0) => return None,
            // Unequal SQL arrays meet at their innermost scalar types, irrespective of their value dimensions.
            _ => 1,
        };
        let scalar = if self.scalar == other.scalar {
            self.scalar
        } else if let Some(scalar) = common_scalar_type(&self.scalar, &other.scalar) {
            scalar
        } else {
            use super::super::overload_resolution::builtin_canonical_type_name;
            let name = |ty: &ColumnType| {
                builtin_canonical_type_name(ty).expect("runtime values select builtin scalar types")
            };
            match category_choice(name(&self.scalar), name(&other.scalar)) {
                CategoryChoice::Left => self.scalar,
                CategoryChoice::Right => other.scalar,
                CategoryChoice::Unmatched | CategoryChoice::Inconvertible => return None,
            }
        };
        Some(Self { scalar, arrays })
    }

    pub(super) fn infer(
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Self>, SQLError> {
        control.check()?;
        let scalar = match value {
            // OID-only carriers do not supply a declaration; observation never reads their opaque payloads.
            Value::Null | Value::Map(_) | Value::Enum(_) | Value::Datum(_) => None,
            Value::Void => Some(ColumnType::Void),
            Value::Row(_) | Value::Record(_) => Some(ColumnType::Record),
            Value::Bool(_) => Some(ColumnType::Boolean),
            Value::Int(value) if i32::try_from(*value).is_ok() => Some(ColumnType::Integer),
            Value::Int(_) => Some(ColumnType::BigInteger),
            Value::Float(_) => Some(ColumnType::DoublePrecision),
            Value::Decimal(_) => Some(numeric_type()),
            Value::Str(_) => Some(ColumnType::Text),
            Value::FixedChar(value) => {
                let mut count = 0_usize;
                for _ in value.chars() {
                    control.check()?;
                    count += 1;
                }
                u32::try_from(count).ok().map(ColumnType::Character)
            }
            Value::Bytes(_) => Some(ColumnType::Bytea),
            Value::BitString(_) => Some(ColumnType::Bit(None)),
            Value::Temporal(value) => Some(match value {
                uqa_core::TemporalValue::Date { .. } => ColumnType::Date,
                uqa_core::TemporalValue::Time { .. } => ColumnType::Time,
                uqa_core::TemporalValue::TimeTz { .. } => ColumnType::TimeTz,
                uqa_core::TemporalValue::Timestamp { .. } => ColumnType::Timestamp,
                uqa_core::TemporalValue::TimestampTz { .. } => ColumnType::TimestampTz,
                uqa_core::TemporalValue::Interval { .. } => ColumnType::Interval,
            }),
            Value::Json(_) => Some(ColumnType::Json),
            Value::JsonB(_) => Some(ColumnType::JsonB),
            Value::LegacyVector(vector) => Some(match vector.kind() {
                uqa_core::LegacyVectorKind::SmallInteger => ColumnType::Int2Vector,
                uqa_core::LegacyVectorKind::Oid => ColumnType::OidVector,
            }),
            Value::Array(array) => {
                let mut element = None;
                if !merge_array_elements(array.elements(), &mut element, control)? {
                    return Ok(None);
                }
                return Ok(element.map(Self::array));
            }
            Value::List(values) => {
                let mut element = None;
                for value in values {
                    if !merge_element(&mut element, Self::infer(value, control)?) {
                        return Ok(None);
                    }
                }
                return Ok(element.map(Self::array));
            }
        };
        Ok(scalar.map(Self::scalar))
    }
}

fn merge_element(element: &mut Option<RuntimeType>, next: Option<RuntimeType>) -> bool {
    *element = match (element.take(), next) {
        (None, other) | (other, None) => other,
        (Some(left), Some(right)) => match left.merge(right) {
            Some(merged) => Some(merged),
            None => return false,
        },
    };
    true
}

fn merge_array_elements(
    values: &[Value],
    element: &mut Option<RuntimeType>,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for value in values {
        control.check()?;
        if let Value::List(nested) = value {
            if !merge_array_elements(nested, element, control)? {
                return Ok(false);
            }
        } else if !merge_element(element, RuntimeType::infer(value, control)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
