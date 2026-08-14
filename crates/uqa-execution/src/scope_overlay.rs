//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional overlay for a correlated outer query scope.

use uqa_sql::ResultRow;

use crate::{Batch, ExecResult, PhysicalOperator, PhysicalRow, RowSchema};

/// Attach one outer row as hidden lookup state while keeping only the current relation's columns visible. The outer value fragment is shared by every row in a child batch, so correlated evaluation does not rebuild a merged map for each inner row.
pub struct ScopeOverlay<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    outer: PhysicalRow,
}

impl<'a> ScopeOverlay<'a> {
    pub fn new(child: Box<dyn PhysicalOperator + 'a>, outer: ResultRow) -> Self {
        let (columns, values): (Vec<_>, Vec<_>) = outer.into_iter().unzip();
        let schema = RowSchema::with_outer_scope(child.row_schema(), &columns);
        Self {
            child,
            schema,
            outer: PhysicalRow::from_values(values),
        }
    }

    /// Attach an outer row together with the statically bound schema that
    /// produced it. This preserves declared identities such as `smallint`,
    /// `varchar`, and `real` even though their runtime values use wider core
    /// carriers.
    pub fn new_with_outer_schema(
        child: Box<dyn PhysicalOperator + 'a>,
        outer: ResultRow,
        outer_schema: &RowSchema,
    ) -> Self {
        let (columns, values): (Vec<_>, Vec<_>) = outer.into_iter().unzip();
        let typed_columns = columns
            .iter()
            .map(|column| (column.clone(), outer_schema.type_of(column).cloned()))
            .collect::<Vec<_>>();
        let schema = RowSchema::with_typed_outer_scope(child.row_schema(), &typed_columns);
        Self {
            child,
            schema,
            outer: PhysicalRow::from_values(values),
        }
    }
}

impl PhysicalOperator for ScopeOverlay<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.child.estimated_cardinality()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        let rows = batch
            .rows
            .into_iter()
            .map(|row| PhysicalRow::concat_left_owned(row, &self.outer))
            .collect();
        Ok(Some(Batch::from_physical_rows(self.schema.clone(), rows)))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_core::Value;
    use uqa_sql::expr::RowLookup;

    use super::*;
    use crate::TableScan;

    fn one_row(columns: &[&str], values: &[i64]) -> Box<dyn PhysicalOperator> {
        let row = columns
            .iter()
            .zip(values)
            .map(|(column, value)| ((*column).to_string(), Value::Int(*value)))
            .collect();
        Box::new(TableScan::from_rows(
            columns.iter().map(|column| (*column).to_string()).collect(),
            vec![row],
        ))
    }

    #[test]
    fn current_scope_shadows_outer_names_without_exposing_outer_star_columns() {
        let child = one_row(&["inner.id", "inner.value"], &[1, 2]);
        let outer = BTreeMap::from([
            ("id".into(), Value::Int(9)),
            ("outer.id".into(), Value::Int(9)),
            ("outer.note".into(), Value::Int(10)),
        ]);
        let mut overlay = ScopeOverlay::new(child, outer);
        overlay.open().unwrap();
        let batch = overlay.next().unwrap().unwrap();
        assert_eq!(batch.schema.columns(), ["inner.id", "inner.value"]);
        let view = batch.schema.view(&batch.rows[0]);
        assert_eq!(view.column("id"), Some(&Value::Int(1)));
        assert_eq!(view.column("note"), Some(&Value::Int(10)));
        assert_eq!(
            view.qualified_column("outer", "id", "outer.id"),
            Some(&Value::Int(9))
        );
    }

    #[test]
    fn ambiguous_current_and_outer_names_are_not_resolved_arbitrarily() {
        let child = one_row(&["left.id", "right.id"], &[1, 2]);
        let outer = BTreeMap::from([("id".into(), Value::Int(9))]);
        let mut overlay = ScopeOverlay::new(child, outer);
        overlay.open().unwrap();
        let batch = overlay.next().unwrap().unwrap();
        assert!(batch.schema.view(&batch.rows[0]).column_is_ambiguous("id"));

        let child = one_row(&["inner.value"], &[1]);
        let outer = BTreeMap::from([
            ("left.id".into(), Value::Int(9)),
            ("right.id".into(), Value::Int(10)),
        ]);
        let mut overlay = ScopeOverlay::new(child, outer);
        overlay.open().unwrap();
        let batch = overlay.next().unwrap().unwrap();
        assert!(batch.schema.view(&batch.rows[0]).column_is_ambiguous("id"));
    }

    #[test]
    fn typed_outer_scope_preserves_declared_sql_identity() {
        let child = one_row(&["inner.id"], &[1]);
        let outer = BTreeMap::from([("outer.value".into(), Value::Int(7))]);
        let outer_schema = RowSchema::with_types(
            vec!["outer.value".into()],
            vec![Some(uqa_sql::ast::ColumnType::SmallInteger)],
        );
        let overlay = ScopeOverlay::new_with_outer_schema(child, outer, &outer_schema);
        assert_eq!(
            overlay.row_schema().type_of("outer.value"),
            Some(&uqa_sql::ast::ColumnType::SmallInteger)
        );
        assert_eq!(
            overlay.row_schema().type_of("value"),
            Some(&uqa_sql::ast::ColumnType::SmallInteger)
        );
    }
}
