//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Host-facing results observe deferred physical datums and carry enums as their current labels, through SQL's type-output owners.

use super::SQLResult;
use crate::expr::datums::contains_datum;
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
                *value = read_datums(value, catalog)?;
            }
            if contains_enum_carrier(value) {
                *value = render_enum_labels(catalog, value)?;
            }
        }
    }
    for row in result.positional_rows.iter_mut().flatten() {
        for value in row {
            if contains_datum(value) {
                *value = read_datums(value, catalog)?;
            }
            if contains_enum_carrier(value) {
                *value = render_enum_labels(catalog, value)?;
            }
        }
    }
    Ok(())
}

fn read_datums(value: &Value, catalog: Option<&dyn EnumLabelCatalog>) -> Result<Value, SQLError> {
    Ok(match value {
        Value::Datum(datum) => read_datums(
            &*crate::expr::datums::read_with_value_catalog_and_control(
                datum,
                catalog,
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?,
            catalog,
        )?,
        Value::Array(array) => Value::Array(
            uqa_core::ArrayValue::with_lower_bounds(
                array
                    .elements()
                    .iter()
                    .map(|value| read_datums(value, catalog))
                    .collect::<Result<_, _>>()?,
                array.lower_bounds().to_vec(),
            )
            .ok_or_else(|| SQLError::Internal("datum output changed array shape".into()))?
            .with_element_type_oid(array.element_type_oid()),
        ),
        Value::List(values) => Value::List(
            values
                .iter()
                .map(|value| read_datums(value, catalog))
                .collect::<Result<_, _>>()?,
        ),
        Value::Row(values) => Value::Row(
            values.clone().with_values(
                values
                    .iter()
                    .map(|value| read_datums(value, catalog))
                    .collect::<Result<_, _>>()?,
            )?,
        ),
        Value::Record(fields) => Value::Record(
            fields
                .iter()
                .map(|(name, value)| Ok((name.clone(), read_datums(value, catalog)?)))
                .collect::<Result<_, SQLError>>()?,
        ),
        Value::Map(values) => Value::Map(
            values
                .iter()
                .map(|(name, value)| Ok((name.clone(), read_datums(value, catalog)?)))
                .collect::<Result<_, SQLError>>()?,
        ),
        other => other.clone(),
    })
}
