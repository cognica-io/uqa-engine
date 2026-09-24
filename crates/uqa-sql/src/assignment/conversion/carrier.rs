//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type-directed normalization of predecessor legacy-vector carriers without repeating domain constraints.

use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, LegacyVectorKind, Value,
};

/// Whether this declared type contains catalog vectors, including domain and array wrappers.
pub fn contains_legacy_vectors(ty: &ColumnType) -> bool {
    element_kind(ty).is_some()
}

/// Return a replacement only when the declared type contains legacy vectors whose runtime representation is not canonical. Domain checks have already run when the stored value was admitted; normalization changes its carrier, not its domain membership.
pub fn normalize_legacy_vector_carrier_with_control(
    value: &Value,
    ty: &ColumnType,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Value>>, SQLError> {
    control.check()?;
    if matches!(value, Value::Null) {
        return Ok(None);
    }
    match ty {
        ColumnType::Domain { base, .. } => {
            normalize_legacy_vector_carrier_with_control(value, base, control)
        }
        ColumnType::Int2Vector | ColumnType::OidVector => {
            let name = super::column_type_name(ty);
            if matches!(value, Value::LegacyVector(vector) if vector.kind().type_name() == name) {
                return Ok(None);
            }
            crate::expr::cast_value_from_with_control(value, name, Some(name), control).map(Some)
        }
        ColumnType::Array(element) => {
            let Some(kind) = element_kind(element) else {
                return Ok(None);
            };
            let (elements, bounds) = match value {
                Value::Array(array) => (array.elements(), Some(array.lower_bounds())),
                Value::List(elements) => (elements.as_slice(), None),
                _ => return Err(invalid_array()),
            };
            if bounds.is_some() && canonical_elements(elements, kind, control)? {
                return Ok(None);
            }
            let (elements, collapsed) = normalize_elements(elements, kind, control)?;
            let array = if let Some(bounds) = bounds {
                let count = bounds
                    .len()
                    .checked_sub(usize::from(collapsed))
                    .ok_or_else(invalid_array)?;
                let mut output = ProductionVec::new(*control);
                for bound in &bounds[..count] {
                    output.push_copy(*bound)?;
                }
                ArrayValue::with_lower_bounds_with_control(elements, output.finish()?, control)?
            } else {
                ArrayValue::try_new_with_control(elements, control)?
            }
            .ok_or_else(invalid_array)?;
            let (array, memory) = array.into_parts();
            Ok(Some(control.finish(Value::Array(array), memory)?))
        }
        _ => Ok(None),
    }
}

fn element_kind(ty: &ColumnType) -> Option<LegacyVectorKind> {
    match ty {
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => element_kind(base),
        ColumnType::Int2Vector => Some(LegacyVectorKind::SmallInteger),
        ColumnType::OidVector => Some(LegacyVectorKind::Oid),
        _ => None,
    }
}

fn canonical_elements(
    values: &[Value],
    kind: LegacyVectorKind,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    for value in values {
        control.check()?;
        match value {
            Value::Null => {}
            Value::LegacyVector(vector) if vector.kind() == kind => {}
            Value::List(nested)
                if !nested.is_empty() && canonical_elements(nested, kind, control)? => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn normalize_elements(
    values: &[Value],
    kind: LegacyVectorKind,
    control: &ProductionControl<'_>,
) -> Result<(Produced<Vec<Value>>, bool), SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    let mut collapsed = false;
    for value in values {
        control.check()?;
        let value = match value {
            Value::Null => control.copy_value(value)?,
            Value::List(nested) => {
                let mut flat = true;
                for item in nested {
                    control.check()?;
                    flat &= matches!(item, Value::Int(_));
                }
                if flat {
                    collapsed = true;
                    crate::expr::cast_value_from_with_control(
                        value,
                        kind.type_name(),
                        Some(kind.type_name()),
                        control,
                    )?
                } else {
                    let (values, child_collapsed) = normalize_elements(nested, kind, control)?;
                    collapsed |= child_collapsed;
                    let (values, memory) = values.into_parts();
                    control.finish(Value::List(values), memory)?
                }
            }
            _ => crate::expr::cast_value_from_with_control(
                value,
                kind.type_name(),
                Some(kind.type_name()),
                control,
            )?,
        };
        output.push_produced(value)?;
    }
    Ok((output.finish()?, collapsed))
}

fn invalid_array() -> SQLError {
    SQLError::TypeMismatch("invalid stored array of legacy vectors".into())
}

#[cfg(test)]
mod tests;

pub(in crate::assignment) fn normalize_existing(
    value: Value,
    ty: &ColumnType,
) -> Result<Value, SQLError> {
    normalize_legacy_vector_carrier_with_control(&value, ty, &ProductionControl::uncontrolled())?
        .map_or(Ok(value), |value| {
            value
                .into_uncontrolled()
                .map_err(|_| SQLError::Internal("ordinary carrier normalization".into()))
        })
}
