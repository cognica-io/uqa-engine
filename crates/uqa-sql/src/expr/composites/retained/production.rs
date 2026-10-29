//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Descriptor projection constructs independently admitted values while preserving the original lazy tuple interpretation.

use super::owners::DescriptorLookup;
use crate::{ColumnType, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    ArrayValue, RecordValue, Value,
};

pub(crate) fn project_value_with_control(
    value: &Value,
    ty: &ColumnType,
    original: &dyn DescriptorLookup,
    current: &dyn DescriptorLookup,
    interpret_datums: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    match (ty, value) {
        (_, Value::Null) => Ok(control.finish(Value::Null, control.empty_reservation())?),
        (ColumnType::Domain { base, .. }, _) => {
            project_value_with_control(value, base, original, current, interpret_datums, control)
        }
        (ColumnType::Composite(reference), Value::Record(fields)) => {
            let oid = fields.type_oid().unwrap_or(reference.oid);
            let (Some(before), Some(after)) = (original.lookup(oid), current.lookup(oid)) else {
                return Ok(control.copy_value(value)?);
            };
            if interpret_datums {
                if let Some(projected) =
                    super::tuple::project_with_control(fields, before, after, original, control)?
                {
                    return nested_fields(projected, before, after, original, current, control);
                }
            }
            let mut output = ProductionVec::new(*control);
            for attribute in &after.attributes {
                control.check()?;
                let retained = before
                    .attributes
                    .iter()
                    .find(|old| old.number == attribute.number)
                    .and_then(|old| {
                        fields
                            .iter()
                            .find(|(name, _)| name.as_str() == old.name)
                            .map(|(_, value)| (old, value))
                    });
                let value = if let Some((old, value)) = retained {
                    let reinterpreted = if interpret_datums {
                        super::super::datum::reinterpret_with_control(
                            value,
                            &old.ty,
                            &attribute.ty,
                            control,
                        )?
                    } else {
                        None
                    };
                    project_value_with_control(
                        reinterpreted.as_deref().unwrap_or(value),
                        &attribute.ty,
                        original,
                        current,
                        interpret_datums,
                        control,
                    )?
                } else {
                    control.finish(Value::Null, control.empty_reservation())?
                };
                push_field(&mut output, &attribute.name, value, control)?;
            }
            record(output.finish()?, Some(oid), control)
        }
        (ColumnType::Array(element), Value::Array(array)) => {
            let elements = array_elements(
                array.elements(),
                element,
                original,
                current,
                interpret_datums,
                control,
            )?;
            let mut bounds = ProductionVec::new(*control);
            bounds.extend_from_slice(array.lower_bounds())?;
            let array =
                ArrayValue::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
                    .ok_or_else(|| {
                        SQLError::Internal(
                            "retained composite projection changed array shape".into(),
                        )
                    })?;
            let (array, memory) = array.into_parts();
            Ok(control.finish(
                Value::Array(array.with_element_type_oid(match value {
                    Value::Array(source) => source.element_type_oid(),
                    _ => unreachable!(),
                })),
                memory,
            )?)
        }
        _ => Ok(control.copy_value(value)?),
    }
}

fn array_elements(
    values: &[Value],
    element: &ColumnType,
    original: &dyn DescriptorLookup,
    current: &dyn DescriptorLookup,
    interpret: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    for value in values {
        let value = if let Value::List(values) = value {
            let (values, memory) =
                array_elements(values, element, original, current, interpret, control)?
                    .into_parts();
            control.finish(Value::List(values), memory)?
        } else {
            project_value_with_control(value, element, original, current, interpret, control)?
        };
        output.push_produced(value)?;
    }
    Ok(output.finish()?)
}

fn nested_fields(
    projected: Produced<Value>,
    before: &super::CompositeTypeDescriptor,
    after: &super::CompositeTypeDescriptor,
    original: &dyn DescriptorLookup,
    current: &dyn DescriptorLookup,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let Value::Record(fields) = &*projected else {
        unreachable!("tuple projector returns a record");
    };
    if !after.attributes.iter().any(|attribute| {
        before
            .attributes
            .iter()
            .find(|old| old.number == attribute.number)
            .is_some_and(|old| contains_composite(&old.ty))
    }) {
        return Ok(projected);
    }
    let mut output = ProductionVec::new(*control);
    for (attribute, (name, value)) in after.attributes.iter().zip(fields) {
        let old = before
            .attributes
            .iter()
            .find(|old| old.number == attribute.number);
        let value = if let Some(old) = old.filter(|old| contains_composite(&old.ty)) {
            project_value_with_control(value, &old.ty, original, current, true, control)?
        } else {
            control.copy_value(value)?
        };
        push_field(&mut output, name, value, control)?;
    }
    record(output.finish()?, fields.type_oid(), control)
}

fn contains_composite(mut ty: &ColumnType) -> bool {
    while let ColumnType::Domain { base, .. } | ColumnType::Array(base) = ty {
        ty = base;
    }
    matches!(ty, ColumnType::Composite(_))
}

pub(super) fn push_field(
    output: &mut ProductionVec<'_, (String, Value)>,
    name: &str,
    value: Produced<Value>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let name = control.copy_text(name)?;
    let (name, name_memory) = name.into_parts();
    let (value, value_memory) = value.into_parts();
    output.push_produced(
        control.finish((name, value), control.combine(name_memory, value_memory))?,
    )?;
    Ok(())
}

pub(super) fn record(
    fields: Produced<Vec<(String, Value)>>,
    oid: Option<u32>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let (record, memory) = RecordValue::with_control(fields, oid, control)?.into_parts();
    Ok(control.finish(Value::Record(record), memory)?)
}
