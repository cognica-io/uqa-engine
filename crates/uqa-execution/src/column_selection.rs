//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema-only projection used after an expression-producing stage.

use crate::{Batch, ExecResult, PhysicalOperator, PhysicalOrder, RowSchema};

/// Select already-computed columns without evaluating expressions again.
///
/// This is intentionally distinct from [`crate::Project`]: SQL `ORDER BY`
/// may reference both source columns and SELECT aliases, so the expression
/// projection first appends aliases, Sort consumes that augmented row, and
/// `ColumnSelection` removes the non-output source columns afterwards. Volatile
/// projection expressions are therefore evaluated exactly once.
pub struct ColumnSelection<'a> {
    child: Box<dyn PhysicalOperator + 'a>,
    schema: RowSchema,
    /// `(output_name, input_name)` pairs. Keeping the physical input name
    /// separate lets a preceding projection expose SELECT-list expressions
    /// under collision-free internal names while this final, non-evaluating
    /// operator restores the public SQL column names.
    ordering: Vec<PhysicalOrder>,
}

impl<'a> ColumnSelection<'a> {
    pub fn new(child: Box<dyn PhysicalOperator + 'a>, columns: Vec<String>) -> Self {
        let columns = columns
            .into_iter()
            .map(|column| (column.clone(), column))
            .collect();
        Self::with_mapping(child, columns)
    }

    pub fn with_mapping(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, String)>,
    ) -> Self {
        let ordering = child
            .output_ordering()
            .iter()
            .map_while(|order| {
                let output = columns
                    .iter()
                    .find(|(_, input)| input == &order.column)?
                    .0
                    .clone();
                Some(PhysicalOrder {
                    column: output,
                    ..order.clone()
                })
            })
            .collect();
        let schema = RowSchema::select(child.row_schema(), &columns);
        Self {
            child,
            schema,
            ordering,
        }
    }
}

impl PhysicalOperator for ColumnSelection<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.child.estimated_cardinality()
    }

    fn output_ordering(&self) -> &[PhysicalOrder] {
        &self.ordering
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next()? else {
            return Ok(None);
        };
        Ok(Some(Batch::from_physical_rows(
            self.schema.clone(),
            batch.rows,
        )))
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use uqa_core::Value;

    use super::*;
    use crate::physical::run_to_rows;
    use crate::scan::TableScan;

    #[test]
    fn selects_computed_columns_without_leaking_sort_inputs() {
        let row = BTreeMap::from([
            ("source".to_string(), Value::Int(1)),
            ("alias".to_string(), Value::Int(2)),
        ]);
        let scan = TableScan::from_rows(vec!["source".into(), "alias".into()], vec![row]);
        let mut selection = ColumnSelection::new(Box::new(scan), vec!["alias".into()]);
        let (schema, rows) = run_to_rows(&mut selection).unwrap();
        assert_eq!(schema, vec!["alias"]);
        assert_eq!(rows[0], BTreeMap::from([("alias".into(), Value::Int(2))]));
    }

    #[test]
    fn renames_collision_free_physical_columns() {
        let row = BTreeMap::from([
            ("source".to_string(), Value::Int(1)),
            ("__projection_0".to_string(), Value::Int(2)),
        ]);
        let scan = TableScan::from_rows(vec!["source".into(), "__projection_0".into()], vec![row]);
        let mut selection = ColumnSelection::with_mapping(
            Box::new(scan),
            vec![("source".into(), "__projection_0".into())],
        );
        let (schema, rows) = run_to_rows(&mut selection).unwrap();
        assert_eq!(schema, vec!["source"]);
        assert_eq!(rows[0], BTreeMap::from([("source".into(), Value::Int(2))]));
    }
}
