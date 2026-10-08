//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host-facing results observe deferred physical datums and carry enums as their current labels, through SQL's type-output owners.

use super::SQLResult;
use crate::expr::enums::{contains_enum_carrier, render_enum_labels, EnumLabelCatalog};
use crate::SQLError;
use uqa_core::Value;

/// Read deferred datums and replace enum carriers in named and positional results by their current labels. This output boundary preserves declared column types and recursive carrier metadata.
pub fn render_result_enum_labels(
    catalog: Option<&dyn EnumLabelCatalog>,
    result: &mut SQLResult,
) -> Result<(), SQLError> {
    for row in &mut result.rows {
        for value in row.values_mut() {
            if contains_datum(value) {
                *value = read_datums(value)?;
            }
            if contains_enum_carrier(value) {
                *value = render_enum_labels(catalog, value)?;
            }
        }
    }
    for row in result.positional_rows.iter_mut().flatten() {
        for value in row {
            if contains_datum(value) {
                *value = read_datums(value)?;
            }
            if contains_enum_carrier(value) {
                *value = render_enum_labels(catalog, value)?;
            }
        }
    }
    Ok(())
}

fn contains_datum(value: &Value) -> bool {
    match value {
        Value::Datum(_) => true,
        Value::Array(array) => array.elements().iter().any(contains_datum),
        Value::List(values) => values.iter().any(contains_datum),
        Value::Row(values) => values.iter().any(contains_datum),
        Value::Record(fields) => fields.iter().any(|(_, value)| contains_datum(value)),
        Value::Map(values) => values.values().any(contains_datum),
        _ => false,
    }
}

fn read_datums(value: &Value) -> Result<Value, SQLError> {
    Ok(match value {
        Value::Datum(datum) => crate::expr::datums::read(datum)?,
        Value::Array(array) => Value::Array(
            uqa_core::ArrayValue::with_lower_bounds(
                array
                    .elements()
                    .iter()
                    .map(read_datums)
                    .collect::<Result<_, _>>()?,
                array.lower_bounds().to_vec(),
            )
            .ok_or_else(|| SQLError::Internal("datum output changed array shape".into()))?,
        ),
        Value::List(values) => {
            Value::List(values.iter().map(read_datums).collect::<Result<_, _>>()?)
        }
        Value::Row(values) => Value::Row(
            values
                .clone()
                .with_values(values.iter().map(read_datums).collect::<Result<_, _>>()?)?,
        ),
        Value::Record(fields) => Value::Record(
            fields
                .iter()
                .map(|(name, value)| Ok((name.clone(), read_datums(value)?)))
                .collect::<Result<_, SQLError>>()?,
        ),
        Value::Map(values) => Value::Map(
            values
                .iter()
                .map(|(name, value)| Ok((name.clone(), read_datums(value)?)))
                .collect::<Result<_, SQLError>>()?,
        ),
        other => other.clone(),
    })
}
