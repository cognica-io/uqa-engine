//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read an already admitted constant through the current descriptor. Added fields are NULL even when their domains reject fresh NULL inputs; the retained value and its input expression remain unchanged.

use crate::{expr::EngineHook, ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, Value,
};

pub fn contains_records(value: &Value) -> bool {
    inspect_records(value, &mut || Ok(())).expect("uncontrolled value inspection")
}

fn inspect_records(
    value: &Value,
    check: &mut impl FnMut() -> Result<(), SQLError>,
) -> Result<bool, SQLError> {
    check()?;
    let values = match value {
        Value::Record(_) => return Ok(true),
        Value::Array(array) => array.elements(),
        Value::List(values) => values.as_slice(),
        _ => return Ok(false),
    };
    for value in values {
        if inspect_records(value, check)? {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn evaluate_with_control(
    value: &Value,
    ty: &str,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    if !inspect_records(value, &mut || control.check().map_err(Into::into))? {
        return control.copy_value(value).map_err(Into::into);
    }
    let Some(engine) = engine else {
        return control.copy_value(value).map_err(Into::into);
    };
    let ty = engine
        .resolve_type_name(ty)
        .map_err(SQLError::Internal)?
        .map(|ty| ty.retain_external_with_control(control))
        .transpose()?;
    control.check()?;
    match ty.as_deref() {
        Some(ty) => materialize(value, ty, engine, control),
        None => control.copy_value(value).map_err(Into::into),
    }
}

fn materialize(
    value: &Value,
    ty: &ColumnType,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    match (ty, value) {
        (ColumnType::Domain { base, .. }, _) => materialize(value, base, engine, control),
        (ColumnType::Composite(reference), Value::Record(fields)) => {
            let descriptor = super::descriptor(engine.composite_types(), reference.oid)?;
            control.check()?;
            let mut output = ProductionVec::new(*control);
            output.reserve(descriptor.attributes.len())?;
            for (index, attribute) in descriptor.attributes.iter().enumerate() {
                let value = fields
                    .get(index)
                    .filter(|(name, _)| *name == attribute.name)
                    .or_else(|| fields.iter().find(|(name, _)| *name == attribute.name))
                    .map_or(&Value::Null, |(_, value)| value);
                let value = materialize(value, &attribute.ty, engine, control)?;
                let name = control.copy_text(&attribute.name)?;
                let (value, memory) = value.into_parts();
                let (name, name_memory) = name.into_parts();
                output.push_produced(
                    control.finish((name, value), control.combine(memory, name_memory))?,
                )?;
            }
            let (fields, memory) = output.finish()?.into_parts();
            control
                .finish(Value::Record(fields), memory)
                .map_err(Into::into)
        }
        (ColumnType::Array(element), Value::Array(array)) => {
            let element_type_oid = array.element_type_oid();
            let elements = array_elements(array.elements(), element, engine, control)?;
            let mut bounds = ProductionVec::new(*control);
            for bound in array.lower_bounds() {
                bounds.push_copy(*bound)?;
            }
            let array =
                ArrayValue::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "retained composite constant changed array dimensions".into(),
                        )
                    })?;
            let (array, memory) = array.into_parts();
            control
                .finish(
                    Value::Array(array.with_element_type_oid(element_type_oid)),
                    memory,
                )
                .map_err(Into::into)
        }
        _ => control.copy_value(value).map_err(Into::into),
    }
}

fn array_elements(
    values: &[Value],
    element: &ColumnType,
    engine: &dyn EngineHook,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>, SQLError> {
    let mut element = element;
    while let ColumnType::Array(inner) = element {
        element = inner;
    }
    let mut output = ProductionVec::new(*control);
    output.reserve(values.len())?;
    for value in values {
        let value = if let Value::List(values) = value {
            let (values, memory) = array_elements(values, element, engine, control)?.into_parts();
            control.finish(Value::List(values), memory)?
        } else {
            materialize(value, element, engine, control)?
        };
        output.push_produced(value)?;
    }
    output.finish().map_err(Into::into)
}

#[cfg(test)]
mod tests;
