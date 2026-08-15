//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional overlay for a correlated outer query scope.

use crate::{Batch, ExecResult, OwnedPhysicalRow, PhysicalOperator, PhysicalRow, RowSchema};

/// Attach one outer row as hidden lookup state while keeping only the current relation's columns visible. The outer value fragment is shared by every row in a child batch, so correlated evaluation does not rebuild a merged map for each inner row.
pub struct ScopeOverlay<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    outer: PhysicalRow,
}

impl<'a> ScopeOverlay<'a> {
    /// Attach an already-positional outer row without materializing names or duplicating values for lookup aliases.
    pub fn new(child: Box<dyn PhysicalOperator + 'a>, outer: OwnedPhysicalRow) -> Self {
        let schema = RowSchema::with_outer_schema(child.row_schema(), &outer.schema);
        Self {
            child,
            schema,
            outer: outer.row,
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
    use uqa_core::Value;
    use uqa_sql::expr::RowLookup;

    use super::*;
    use crate::{ColumnIdentity, ColumnSelection, TableScan};

    fn one_row(qualifier: &str, columns: &[&str], values: &[i64]) -> Box<dyn PhysicalOperator> {
        let row = columns
            .iter()
            .zip(values)
            .map(|(column, value)| ((*column).to_string(), Value::Int(*value)))
            .collect();
        let scan: Box<dyn PhysicalOperator> = Box::new(TableScan::from_rows(
            columns.iter().map(|column| (*column).to_string()).collect(),
            vec![row],
        ));
        let mapping = columns
            .iter()
            .enumerate()
            .map(|(position, column)| {
                (
                    (*column).to_string(),
                    ColumnIdentity::qualified(qualifier, *column),
                    position,
                )
            })
            .collect();
        Box::new(ColumnSelection::with_identities(scan, mapping))
    }

    #[test]
    fn current_scope_shadows_outer_names_without_exposing_outer_star_columns() {
        let child = one_row("inner", &["id", "value"], &[1, 2]);
        let outer_schema = RowSchema::with_qualified_types(
            "outer",
            vec!["id".into(), "note".into()],
            vec![None, None],
        );
        let outer = OwnedPhysicalRow::new(
            outer_schema,
            PhysicalRow::from_values(vec![Value::Int(9), Value::Int(10)]),
        );
        let mut overlay = ScopeOverlay::new(child, outer);
        overlay.open().unwrap();
        let batch = overlay.next().unwrap().unwrap();
        assert_eq!(batch.schema.columns(), ["id", "value"]);
        let view = batch.schema.view(&batch.rows[0]);
        assert_eq!(view.column("id"), Some(&Value::Int(1)));
        assert_eq!(view.column("note"), Some(&Value::Int(10)));
        assert_eq!(view.qualified_column("outer", "id"), Some(&Value::Int(9)));
    }

    #[test]
    fn ambiguous_current_and_outer_names_are_not_resolved_arbitrarily() {
        let row = [
            ("left slot".into(), Value::Int(1)),
            ("right slot".into(), Value::Int(2)),
        ]
        .into_iter()
        .collect();
        let scan: Box<dyn PhysicalOperator> = Box::new(TableScan::from_rows(
            vec!["left slot".into(), "right slot".into()],
            vec![row],
        ));
        let child: Box<dyn PhysicalOperator> = Box::new(ColumnSelection::with_identities(
            scan,
            vec![
                ("id".into(), ColumnIdentity::qualified("left", "id"), 0),
                ("id".into(), ColumnIdentity::qualified("right", "id"), 1),
            ],
        ));
        let outer = OwnedPhysicalRow::new(
            RowSchema::new(vec!["id".into()]),
            PhysicalRow::from_values(vec![Value::Int(9)]),
        );
        let mut overlay = ScopeOverlay::new(child, outer);
        overlay.open().unwrap();
        let batch = overlay.next().unwrap().unwrap();
        assert!(batch.schema.view(&batch.rows[0]).column_is_ambiguous("id"));

        let child = one_row("inner", &["value"], &[1]);
        let outer_schema = RowSchema::with_identities(
            vec!["left slot".into(), "right slot".into()],
            vec![
                ColumnIdentity::qualified("left", "id"),
                ColumnIdentity::qualified("right", "id"),
            ],
            vec![None, None],
        );
        let outer = OwnedPhysicalRow::new(
            outer_schema,
            PhysicalRow::from_values(vec![Value::Int(9), Value::Int(10)]),
        );
        let mut overlay = ScopeOverlay::new(child, outer);
        overlay.open().unwrap();
        let batch = overlay.next().unwrap().unwrap();
        assert!(batch.schema.view(&batch.rows[0]).column_is_ambiguous("id"));
    }

    #[test]
    fn typed_outer_scope_preserves_declared_sql_identity() {
        let child = one_row("inner", &["id"], &[1]);
        let outer_schema = RowSchema::with_qualified_types(
            "outer",
            vec!["value".into()],
            vec![Some(uqa_sql::ast::ColumnType::SmallInteger)],
        );
        let outer =
            OwnedPhysicalRow::new(outer_schema, PhysicalRow::from_values(vec![Value::Int(7)]));
        let overlay = ScopeOverlay::new(child, outer);
        assert_eq!(
            overlay.row_schema().qualified_type("outer", "value"),
            Some(&uqa_sql::ast::ColumnType::SmallInteger)
        );
        assert_eq!(
            overlay.row_schema().type_of("value"),
            Some(&uqa_sql::ast::ColumnType::SmallInteger)
        );
    }

    #[test]
    fn outer_scope_preserves_hidden_structured_aliases_without_extra_value_slots() {
        let child = one_row("inner", &["id"], &[1]);
        let outer_schema = RowSchema::with_identity_aliases(
            &RowSchema::new(vec!["payload".into()]),
            &[(ColumnIdentity::qualified("outer.dot", "column.dot"), 0)],
        );
        let outer =
            OwnedPhysicalRow::new(outer_schema, PhysicalRow::from_values(vec![Value::Int(9)]));
        let mut overlay = ScopeOverlay::new(child, outer);
        assert_eq!(overlay.row_schema().physical_width(), 2);
        overlay.open().unwrap();
        let batch = overlay.next().unwrap().unwrap();
        let view = batch.schema.view(&batch.rows[0]);
        assert_eq!(
            view.qualified_column("outer.dot", "column.dot"),
            Some(&Value::Int(9))
        );
        assert_eq!(view.qualified_column("outer", "dot.column.dot"), None);
    }
}
