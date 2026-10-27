//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain admitted composite tuples before their producing catalog scope ends.

use super::{descriptor, retained, CompositeTypeCatalog, CompositeTypeDescriptor};
use crate::SQLError;
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionSmallVec},
    DatumValue, Value,
};

/// Transfer an admitted value into independent physical retention. Named composites use their producer's current descriptor exactly once; existing Datums remain unread, including deferred invalid field addresses. Array bounds and anonymous producer metadata survive recursive retention. The caller must still own the producing statement's catalog scope.
pub fn retain_value_with_control(
    value: Produced<Value>,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if needs_transformation(
        &value,
        control,
        |value| matches!(value, Value::Record(record) if record.type_oid().is_some()),
    )? {
        value.transform_with_control(control, |value| {
            let Value::Record(record) = value else {
                return Ok(None);
            };
            let Some(oid) = record.type_oid() else {
                return Ok(None);
            };
            let descriptor = descriptor(catalog, oid)?;
            retain_record(record, &descriptor, catalog, control).map(Some)
        })
    } else {
        Ok(value)
    }
}

pub(super) fn retain_record(
    record: &uqa_core::RecordValue,
    descriptor: &CompositeTypeDescriptor,
    catalog: Option<&dyn CompositeTypeCatalog>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    let bytes = retained::record_bytes_with_control(record, descriptor, catalog, control)?;
    let (datum, memory) =
        DatumValue::with_control(descriptor.type_oid, 0, bytes, control)?.into_parts();
    Ok(control.finish(Value::Datum(datum), memory)?)
}

/// A table tuple stores the flat physical value, while procedural record copies
/// retain their expanded field binding. This transition never reads a payload.
pub fn flatten_value_with_control(
    value: Produced<Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    if !needs_transformation(
        &value,
        control,
        |value| matches!(value, Value::Datum(datum) if datum.record_fields().is_some()),
    )? {
        return Ok(value);
    }
    value.transform_with_control(control, |value| {
        if let Value::Datum(datum) = value {
            if datum.record_fields().is_some() {
                return control
                    .copy_value(&Value::Datum(datum.clone().without_record_fields()))
                    .map(Some)
                    .map_err(Into::into);
            }
        }
        Ok(None)
    })
}

fn needs_transformation(
    value: &Value,
    control: &ProductionControl<'_>,
    selected: impl Fn(&Value) -> bool,
) -> Result<bool, SQLError> {
    let mut stack = ProductionSmallVec::<[&[Value]; 8]>::new(*control);
    let mut values = std::slice::from_ref(value);
    loop {
        control.check()?;
        if let Some((value, remaining)) = values.split_first() {
            values = remaining;
            if selected(value) {
                return Ok(true);
            }
            let children = match value {
                Value::Record(record) => record.values(),
                Value::Row(row) => row.values(),
                Value::Array(array) => array.elements(),
                Value::List(values) => values,
                _ => continue,
            };
            if !children.is_empty() {
                if !remaining.is_empty() {
                    stack.push(remaining)?;
                }
                values = children;
            }
        } else if let Some(parent) = stack.pop() {
            values = parent;
        } else {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests;
