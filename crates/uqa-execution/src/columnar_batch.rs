//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional, column-oriented batches for public result transfer.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ResultRow;

use crate::RowSchema;

/// A column vector preserves its position in the declared output schema.
///
/// The current physical row carrier is the map-backed [`ResultRow`], so two
/// differently-valued expressions with the same output label have already
/// collapsed before this conversion boundary. Duplicate schema labels remain
/// visible as separate slots, but they cannot recover values lost upstream.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnVector {
    pub name: String,
    pub values: Vec<Value>,
}

/// Column-oriented transfer batch. Physical operators may remain row-oriented
/// internally; API consumers can process one bounded batch without retaining
/// the complete result set or rebuilding columns themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnarBatch {
    columns: Vec<ColumnVector>,
    row_count: usize,
}

impl ColumnarBatch {
    /// Move map-backed rows into positional columns. A missing projected value
    /// has SQL `NULL` semantics.
    pub fn from_rows(schema: &[String], rows: Vec<ResultRow>) -> Self {
        let row_count = rows.len();
        let occurrences = schema.iter().fold(BTreeMap::new(), |mut counts, name| {
            *counts.entry(name.as_str()).or_insert(0_usize) += 1;
            counts
        });
        let mut columns = schema
            .iter()
            .map(|name| ColumnVector {
                name: name.clone(),
                values: Vec::with_capacity(row_count),
            })
            .collect::<Vec<_>>();
        for mut row in rows {
            let mut remaining = occurrences.clone();
            for column in &mut columns {
                let remaining_for_name = remaining
                    .get_mut(column.name.as_str())
                    .expect("column occurrence was counted");
                *remaining_for_name -= 1;
                let value = if *remaining_for_name == 0 {
                    row.remove(&column.name)
                } else {
                    row.get(&column.name).cloned()
                };
                column.values.push(value.unwrap_or(Value::Null));
            }
        }
        Self { columns, row_count }
    }

    pub fn schema(&self) -> RowSchema {
        RowSchema::new(
            self.columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        )
    }

    pub fn columns(&self) -> &[ColumnVector] {
        &self.columns
    }

    pub fn len(&self) -> usize {
        self.row_count
    }

    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }

    /// Convert back to the legacy named-row representation. Duplicate output
    /// labels necessarily collapse because [`ResultRow`] is a map.
    pub fn into_rows(self) -> Vec<ResultRow> {
        let mut rows = (0..self.row_count)
            .map(|_| BTreeMap::new())
            .collect::<Vec<ResultRow>>();
        for column in self.columns {
            for (row, value) in rows.iter_mut().zip(column.values) {
                row.insert(column.name.clone(), value);
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversion_is_positional_and_fills_missing_values_with_null() {
        let mut first = ResultRow::new();
        first.insert("a".into(), Value::Int(1));
        first.insert("b".into(), Value::Str("x".into()));
        let mut second = ResultRow::new();
        second.insert("a".into(), Value::Int(2));

        let batch = ColumnarBatch::from_rows(&["b".into(), "a".into()], vec![first, second]);
        assert_eq!(batch.len(), 2);
        assert_eq!(
            batch.columns()[0].values,
            vec![Value::Str("x".into()), Value::Null]
        );
        assert_eq!(
            batch.columns()[1].values,
            vec![Value::Int(1), Value::Int(2)]
        );
    }

    #[test]
    fn duplicate_schema_labels_remain_visible_as_separate_slots() {
        let mut row = ResultRow::new();
        row.insert("value".into(), Value::Int(7));
        let batch = ColumnarBatch::from_rows(&["value".into(), "value".into()], vec![row]);
        assert_eq!(batch.columns().len(), 2);
        assert_eq!(batch.columns()[0].values, batch.columns()[1].values);
    }
}
