//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Container output admits current label text and all transformed buffers before allocation.

use super::{invalid_internal_value, labels, EnumLabelCatalog, Result, SQLError};
use uqa_core::{
    memory::{MemoryReservation, Produced, ProductionControl, ProductionVec},
    ArrayValue, EnumValue, RecordValue, RowValue, Value,
};

pub(crate) fn label_text_with_control(
    catalog: Option<&dyn EnumLabelCatalog>,
    value: &EnumValue,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>> {
    control.check()?;
    let labels = labels(catalog, value.type_oid())?;
    let label = labels
        .by_key(value.key())
        .ok_or_else(|| invalid_internal_value(value))?;
    Ok(control.copy_text(&label.label)?)
}

pub(crate) fn render_with_control(
    catalog: Option<&dyn EnumLabelCatalog>,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    map_with_control(value, control, &mut |value| {
        let (text, memory) = label_text_with_control(catalog, value, control)?.into_parts();
        Ok(control.finish(Value::Str(text), memory)?)
    })
}

pub(super) fn map_with_control(
    value: &Value,
    control: &ProductionControl<'_>,
    convert: &mut dyn FnMut(&EnumValue) -> Result<Produced<Value>>,
) -> Result<Produced<Value>> {
    control.check()?;
    match value {
        Value::Enum(value) => convert(value),
        Value::Array(array) => {
            let elements = values(convert, array.elements(), control)?;
            let mut bounds = ProductionVec::new(*control);
            for bound in array.lower_bounds() {
                bounds.push_copy(*bound)?;
            }
            let (result, memory) =
                ArrayValue::with_lower_bounds_with_control(elements, bounds.finish()?, control)?
                    .ok_or_else(|| {
                        SQLError::Internal("enum output changed array dimensions".into())
                    })?
                    .into_parts();
            Ok(control.finish(
                Value::Array(result.with_element_type_oid(array.element_type_oid())),
                memory,
            )?)
        }
        Value::List(elements) => {
            let (elements, memory) = values(convert, elements, control)?.into_parts();
            Ok(control.finish(Value::List(elements), memory)?)
        }
        Value::Row(row) => {
            let elements = values(convert, row.values(), control)?;
            let row = if let Some(descriptor) = row.descriptor() {
                RowValue::typed_shared_with_control(elements, descriptor.clone(), control)?
            } else {
                RowValue::new_with_control(elements, control)?
            };
            let (row, memory) = row.into_parts();
            Ok(control.finish(Value::Row(row), memory)?)
        }
        Value::Record(record) => {
            let elements = values(convert, record.values(), control)?;
            let (record, memory) =
                RecordValue::with_shared_control(elements, record.descriptor().clone(), control)?
                    .into_parts();
            Ok(control.finish(Value::Record(record), memory)?)
        }
        Value::Map(fields) => map(convert, fields, control),
        other => Ok(control.copy_value(other)?),
    }
}

fn values(
    convert: &mut dyn FnMut(&EnumValue) -> Result<Produced<Value>>,
    source: &[Value],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Value>>> {
    let mut values = ProductionVec::new(*control);
    values.reserve(source.len())?;
    for value in source {
        values.push_produced(map_with_control(value, control, convert)?)?;
    }
    Ok(values.finish()?)
}

fn map(
    convert: &mut dyn FnMut(&EnumValue) -> Result<Produced<Value>>,
    source: &std::collections::BTreeMap<String, Value>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>> {
    // Match Core's retained map-entry accounting. The partial map precedes its lease on every failure path.
    struct Owned {
        value: std::collections::BTreeMap<String, Value>,
        memory: Option<MemoryReservation>,
    }
    let mut output = Owned {
        value: std::collections::BTreeMap::new(),
        memory: control.empty_reservation(),
    };
    for (name, value) in source {
        let entry = control.reserve(size_of::<(String, Value)>())?;
        let name = control.copy_text(name)?;
        let value = map_with_control(value, control, convert)?;
        let (name, name_memory) = name.into_parts();
        let (value, value_memory) = value.into_parts();
        output.memory = control.combine(
            output.memory,
            control.combine(entry, control.combine(name_memory, value_memory)),
        );
        output.value.insert(name, value);
    }
    Ok(control.finish(Value::Map(output.value), output.memory)?)
}
