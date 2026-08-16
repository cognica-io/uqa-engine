//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Result rows returned by `Engine::sql`.

use std::collections::BTreeMap;

use uqa_core::Value;

use crate::ast::ColumnType;

pub type ResultRow = BTreeMap<String, Value>;

#[derive(Debug, Clone, Default)]
pub struct SQLResult {
    /// Column order as the SELECT clause specified.
    pub columns: Vec<String>,
    /// Statically bound SQL type for each output position. A missing entry
    /// represents a type that has not yet been resolved, never a type inferred
    /// from the first runtime value.
    pub column_types: Vec<Option<ColumnType>>,
    /// One row per result document, with the named columns in
    /// `columns`. Extra columns from `_score` etc. are included here
    /// too.
    pub rows: Vec<ResultRow>,
    /// Positional values for result sets whose output contains repeated column
    /// labels. `rows` remains available for named lookup, while this carrier
    /// preserves values that cannot be represented by a string-keyed map.
    #[doc(hidden)]
    pub positional_rows: Option<Vec<Vec<Value>>>,
    /// Number of rows touched by an INSERT / UPDATE / DELETE.
    pub affected_rows: u64,
}

impl SQLResult {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn from_rows(columns: Vec<String>, rows: Vec<ResultRow>) -> Self {
        let column_types = vec![None; columns.len()];
        Self {
            columns,
            column_types,
            rows,
            positional_rows: None,
            affected_rows: 0,
        }
    }

    pub fn from_rows_with_positions(
        columns: Vec<String>,
        rows: Vec<ResultRow>,
        positional_rows: Option<Vec<Vec<Value>>>,
    ) -> Self {
        let column_types = vec![None; columns.len()];
        Self::from_typed_rows_with_positions(columns, column_types, rows, positional_rows)
    }

    pub fn from_typed_rows_with_positions(
        columns: Vec<String>,
        column_types: Vec<Option<ColumnType>>,
        mut rows: Vec<ResultRow>,
        positional_rows: Option<Vec<Vec<Value>>>,
    ) -> Self {
        debug_assert_eq!(columns.len(), column_types.len());
        debug_assert!(positional_rows.as_ref().is_none_or(|values| {
            values.len() == rows.len() && values.iter().all(|row| row.len() == columns.len())
        }));
        if let Some(positional_rows) = positional_rows.as_ref() {
            let compatibility_labels = unique_compatibility_labels(&columns);
            for (row, positional) in rows.iter_mut().zip(positional_rows) {
                for (label, value) in compatibility_labels.iter().zip(positional) {
                    row.insert(label.clone(), value.clone());
                }
            }
        }
        Self {
            columns,
            column_types,
            rows,
            positional_rows,
            affected_rows: 0,
        }
    }

    /// Return a result value by row and output-column position.
    ///
    /// Positional access is the canonical way to distinguish repeated output
    /// labels. Named rows remain available for compatibility with existing
    /// callers.
    pub fn value_at(&self, row: usize, column: usize) -> Option<&Value> {
        self.positional_rows
            .as_ref()
            .and_then(|rows| rows.get(row))
            .and_then(|row| row.get(column))
            .or_else(|| {
                self.columns
                    .get(column)
                    .and_then(|name| self.rows.get(row)?.get(name))
            })
    }

    pub fn from_affected(affected: u64) -> Self {
        Self {
            affected_rows: affected,
            ..Self::default()
        }
    }
}

fn unique_compatibility_labels(columns: &[String]) -> Vec<String> {
    let mut labels = Vec::with_capacity(columns.len());
    for base in columns {
        let mut label = base.clone();
        let mut suffix = 1usize;
        while labels.contains(&label) {
            label = format!("{base}_{suffix}");
            suffix += 1;
        }
        labels.push(label);
    }
    labels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_postgresql_labels_keep_unique_named_compatibility_keys() {
        let result = SQLResult::from_rows_with_positions(
            vec!["value".into(), "value".into()],
            vec![ResultRow::from([("value".into(), Value::Int(6))])],
            Some(vec![vec![Value::Int(5), Value::Int(6)]]),
        );

        assert_eq!(result.columns, ["value", "value"]);
        assert_eq!(result.rows[0].get("value"), Some(&Value::Int(5)));
        assert_eq!(result.rows[0].get("value_1"), Some(&Value::Int(6)));
        assert_eq!(result.value_at(0, 0), Some(&Value::Int(5)));
        assert_eq!(result.value_at(0, 1), Some(&Value::Int(6)));
    }
}
