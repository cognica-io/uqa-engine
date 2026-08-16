//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional, column-oriented batches for public result transfer.

use std::collections::BTreeMap;

use uqa_core::Value;
use uqa_sql::ResultRow;

use crate::{Batch, RowSchema};

/// A column vector preserves its position in the declared output schema.
///
/// Duplicate labels remain distinct because conversion from a physical batch
/// reads each logical position directly. The named-row compatibility
/// constructor cannot recover values that were already collapsed by a map.
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
    /// Convert a positional physical batch without crossing the legacy
    /// map-backed row boundary. Repeated output labels are matched to repeated
    /// logical schema positions in order, so differently-valued duplicate
    /// columns remain distinct for wire and cursor consumers.
    pub fn from_batch(schema: &[String], batch: Batch) -> Self {
        let row_count = batch.rows.len();
        let mut occurrences = BTreeMap::<&str, usize>::new();
        let positions = schema
            .iter()
            .map(|name| {
                let occurrence = occurrences.entry(name.as_str()).or_default();
                let position = batch
                    .schema
                    .columns()
                    .iter()
                    .enumerate()
                    .filter(|(_, input)| input == &name)
                    .nth(*occurrence)
                    .map(|(position, _)| position);
                *occurrence += 1;
                position
            })
            .collect::<Vec<_>>();
        let mut columns = schema
            .iter()
            .map(|name| ColumnVector {
                name: name.clone(),
                values: Vec::with_capacity(row_count),
            })
            .collect::<Vec<_>>();
        for row in &batch.rows {
            let view = batch.schema.view(row);
            for (column, position) in columns.iter_mut().zip(&positions) {
                column.values.push(
                    position
                        .and_then(|position| view.value_at(position))
                        .cloned()
                        .unwrap_or(Value::Null),
                );
            }
        }
        Self { columns, row_count }
    }

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
        if occurrences.values().all(|count| *count == 1) {
            for mut row in rows {
                for column in &mut columns {
                    column
                        .values
                        .push(row.remove(&column.name).unwrap_or(Value::Null));
                }
            }
            return Self { columns, row_count };
        }
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

    /// Convert the column vectors to row-major positional values without
    /// passing through a name-keyed map.
    pub fn into_positional_rows(self) -> Vec<Vec<Value>> {
        let mut rows = (0..self.row_count)
            .map(|_| Vec::with_capacity(self.columns.len()))
            .collect::<Vec<_>>();
        for column in self.columns {
            for (row, value) in rows.iter_mut().zip(column.values) {
                row.push(value);
            }
        }
        rows
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

    #[test]
    fn physical_batch_preserves_different_duplicate_values() {
        let schema = RowSchema::new(vec!["value".into(), "value".into()]);
        let batch = Batch::from_physical_rows(
            schema,
            vec![crate::PhysicalRow::from_values(vec![
                Value::Int(1),
                Value::Int(2),
            ])],
        );
        let batch = ColumnarBatch::from_batch(&["value".into(), "value".into()], batch);
        assert_eq!(batch.columns()[0].values, [Value::Int(1)]);
        assert_eq!(batch.columns()[1].values, [Value::Int(2)]);
    }

    #[test]
    fn positional_rows_preserve_duplicate_values() {
        let schema = RowSchema::new(vec!["value".into(), "value".into()]);
        let batch = Batch::from_physical_rows(
            schema,
            vec![crate::PhysicalRow::from_values(vec![
                Value::Int(1),
                Value::Int(2),
            ])],
        );
        assert_eq!(
            ColumnarBatch::from_batch(&["value".into(), "value".into()], batch)
                .into_positional_rows(),
            [vec![Value::Int(1), Value::Int(2)]]
        );
    }
}
