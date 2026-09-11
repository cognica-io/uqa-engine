//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Normalize array-valued rows at the foreign data boundary.
#[cfg(test)]
mod tests;
use crate::{ColumnType, Row};
use uqa_core::{ArrayValue, Value};
pub fn column_type_is_array(column_type: &ColumnType) -> bool {
    match column_type {
        ColumnType::Array(_) => true,
        ColumnType::Domain { base, .. } => column_type_is_array(base),
        _ => false,
    }
}
pub fn normalize_array_columns(
    mut row: Row,
    array_columns: &[String],
) -> std::result::Result<Row, String> {
    for column in array_columns {
        let Some(value) = row.get_mut(column) else {
            continue;
        };
        let normalized = match std::mem::take(value) {
            Value::Null => Value::Null,
            Value::Array(array) => Value::Array(array),
            Value::List(elements) => {
                Value::Array(ArrayValue::try_new(elements).ok_or_else(|| {
                    format!("foreign array column `{column}` contains non-rectangular dimensions")
                })?)
            }
            other => {
                return Err(format!(
                    "foreign array column `{column}` requires an array value, got {other:?}"
                ));
            }
        };
        *value = normalized;
    }
    Ok(row)
}
