//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select a bound attribute through the tuple's actual descriptor before observing its bytes.

use crate::{ast::CompositeFieldBinding, expr::EngineHook, SQLError};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};

#[cfg(test)]
mod tests;

pub(in crate::expr) fn select_with_control(
    field: &CompositeFieldBinding,
    arguments: &[Value],
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let [value, Value::Str(name)] = arguments else {
        return Err(SQLError::Internal(
            "field selection takes a value and a field name".into(),
        ));
    };
    if matches!(value, Value::Null) || field.dropped {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    }
    if let Value::Datum(datum) = value {
        if let Some(layout) = datum.record_fields() {
            return select_expanded_field(datum, layout, field, engine, control);
        }
    }
    let actual_oid = match value {
        Value::Record(record) => record.type_oid(),
        Value::Datum(datum) => Some(crate::expr::datums::record_type_oid(datum, control)?),
        _ => None,
    };
    let descriptor = actual_oid
        .map(|oid| super::descriptor(engine.and_then(EngineHook::composite_types), oid))
        .transpose()?;
    let name = if let Some(descriptor) = &descriptor {
        let Some(attribute) = descriptor
            .attributes
            .iter()
            .find(|attribute| attribute.number == field.number)
        else {
            return Ok(control.finish(Value::Null, control.empty_reservation())?);
        };
        super::validate_field_type(field, &attribute.ty)?;
        &attribute.name
    } else {
        super::validate_field_result(field)?;
        name
    };
    if let Value::Datum(datum) = value {
        return crate::expr::datums::read_composite_field_with_control(
            datum,
            name,
            engine.and_then(EngineHook::composite_types),
            control,
        );
    }
    let Value::Record(record) = value else {
        return Err(SQLError::Internal(
            "bound composite field has no record value".into(),
        ));
    };
    let selected = record
        .iter()
        .find(|(key, _)| key.as_str() == name)
        .map_or(&Value::Null, |(_, value)| value);
    Ok(control.copy_value(selected)?)
}

/// Expanded procedural records keep their field descriptor even when the named
/// type changes. Text output still observes the original tuple through the catalog.
fn select_expanded_field(
    datum: &uqa_core::DatumValue,
    layout: &[uqa_core::RecordDatumField],
    field: &CompositeFieldBinding,
    engine: Option<&dyn EngineHook>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    use super::{CompositeAttribute, CompositeTypeDescriptor, DroppedCompositeAttribute};
    use uqa_core::memory::ProductionVec;
    let index = usize::try_from(field.number)
        .ok()
        .and_then(|number| number.checked_sub(1));
    let Some(selected) = index
        .and_then(|index| layout.get(index))
        .filter(|slot| !slot.dropped)
    else {
        return Ok(control.finish(Value::Null, control.empty_reservation())?);
    };
    if crate::catalog::type_metadata::pg_type_oid(&field.result_type) as u32 != selected.type_oid {
        let catalog = engine.map(crate::expr::EngineValueCatalog);
        let ty = crate::expr::value_catalog::type_by_oid_with_control(
            selected.type_oid,
            catalog
                .as_ref()
                .map(|catalog| catalog as &dyn crate::expr::SQLValueCatalog),
            control,
        )?;
        super::validate_field_type(field, &ty)?;
    }
    let mut dropped = ProductionVec::new(*control);
    for (index, slot) in layout.iter().enumerate() {
        control.check()?;
        let number = i16::try_from(index + 1)
            .map_err(|_| SQLError::Internal("invalid expanded record width".into()))?;
        if number != field.number {
            dropped.push_copy(DroppedCompositeAttribute {
                number,
                length: slot.length,
                alignment: slot.alignment,
            })?;
        }
    }
    let mut attributes = ProductionVec::new(*control);
    let (ty, memory) = field.result_type.clone_with_control(control)?.into_parts();
    attributes.push_produced(control.finish(
        CompositeAttribute {
            name: String::new(),
            ty,
            number: field.number,
            collation_oid: None,
        },
        memory,
    )?)?;
    let (attributes, attribute_memory) = attributes.finish()?.into_parts();
    let (dropped, dropped_memory) = dropped.finish()?.into_parts();
    let _descriptor_memory = control.combine(attribute_memory, dropped_memory);
    let _header = control.reserve(size_of::<CompositeTypeDescriptor>() + 2 * size_of::<usize>())?;
    let descriptor = std::sync::Arc::new(CompositeTypeDescriptor {
        type_oid: datum.type_oid(),
        relation_oid: 0,
        attributes,
        dropped,
    });
    crate::expr::datums::read_composite_field_in_descriptor(datum, "", &descriptor, control)
}
