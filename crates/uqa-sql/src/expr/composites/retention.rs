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
    if needs_retention(&value, control)? {
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

fn needs_retention(value: &Value, control: &ProductionControl<'_>) -> Result<bool, SQLError> {
    let mut stack = ProductionSmallVec::<[&[Value]; 8]>::new(*control);
    let mut values = std::slice::from_ref(value);
    loop {
        control.check()?;
        if let Some((value, remaining)) = values.split_first() {
            values = remaining;
            let children = match value {
                Value::Record(record) if record.type_oid().is_some() => return Ok(true),
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
