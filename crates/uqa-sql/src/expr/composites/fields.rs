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
    let decoded;
    let value = if let Value::Datum(datum) = value {
        decoded = crate::expr::datums::read_with_catalog_and_control(datum, engine, control)?;
        &*decoded
    } else {
        value
    };
    let Value::Record(record) = value else {
        return Err(SQLError::Internal(
            "bound composite field has no record value".into(),
        ));
    };
    let selected = record
        .iter()
        .find(|(key, _)| key == name)
        .map_or(&Value::Null, |(_, value)| value);
    Ok(control.copy_value(selected)?)
}
