//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema-only projection used after an expression-producing stage.

use std::sync::Arc;

use uqa_core::Value;

use crate::{
    Batch, ColumnIdentity, ExecResult, PhysicalOperator, PhysicalOrder, RowProjectionValue,
    RowSchema,
};

fn remap_ordering(
    ordering: &[PhysicalOrder],
    input_positions: &[Option<usize>],
) -> Vec<PhysicalOrder> {
    ordering
        .iter()
        .map_while(|order| {
            let position = input_positions
                .iter()
                .position(|input| *input == Some(order.position))?;
            Some(PhysicalOrder {
                position,
                ..order.clone()
            })
        })
        .collect()
}

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
    rebind_lock_qualifier: Option<Arc<str>>,
    discard_lock_origins: bool,
    compact_slots: Option<Vec<Option<usize>>>,
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
        let input_positions = columns
            .iter()
            .map(|(_, input)| child.row_schema().position(input))
            .collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let schema = RowSchema::select(child.row_schema(), &columns);
        Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: false,
            compact_slots: None,
        }
    }

    /// Select and rename logical input positions without resolving them by
    /// name. SQL result shaping uses this when repeated public labels must
    /// remain distinct even though their qualified input names differ.
    pub fn with_positions(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, usize)>,
    ) -> Self {
        let input_positions = columns
            .iter()
            .map(|(_, position)| Some(*position))
            .collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let schema = RowSchema::remap_positions(child.row_schema(), &columns, &[]);
        Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: false,
            compact_slots: None,
        }
    }

    /// Select logical input positions and assign explicit structured SQL identities without encoding qualifiers into output labels.
    pub fn with_identities(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, ColumnIdentity, usize)>,
    ) -> Self {
        let input_positions = columns
            .iter()
            .map(|(_, _, position)| Some(*position))
            .collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let columns = columns
            .into_iter()
            .map(|(label, identity, position)| {
                let ty = child.row_schema().column_type(position).cloned();
                (label, identity, position, ty)
            })
            .collect::<Vec<_>>();
        let schema = RowSchema::remap_typed_identities(child.row_schema(), &columns, &[]);
        Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: false,
            compact_slots: None,
        }
    }

    /// Select positions under identities that replace, rather than extend, the child's SQL namespace.
    pub fn with_fresh_identities(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, ColumnIdentity, usize)>,
    ) -> Self {
        let input_positions = columns
            .iter()
            .map(|(_, _, position)| Some(*position))
            .collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let columns = columns
            .into_iter()
            .map(|(label, identity, position)| {
                let ty = child.row_schema().column_type(position).cloned();
                (label, identity, position, ty)
            })
            .collect::<Vec<_>>();
        let schema =
            RowSchema::remap_typed_identities_without_input_aliases(child.row_schema(), &columns);
        Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: false,
            compact_slots: None,
        }
    }

    /// Select logical input positions and compact them to one canonical positional layout. Use this only at an explicit state boundary, such as recursive working-table materialization, where independently planned inputs must share an identical physical schema.
    pub fn compacting_with_positions(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, usize)>,
    ) -> Self {
        let input_positions = columns
            .iter()
            .map(|(_, position)| Some(*position))
            .collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let names = columns
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        let types = columns
            .iter()
            .map(|(_, position)| child.row_schema().column_type(*position).cloned())
            .collect::<Vec<_>>();
        let slots = columns
            .iter()
            .map(|(_, position)| child.row_schema().physical_slot(*position))
            .collect::<Vec<_>>();
        let identity_layout = child.row_schema().physical_width() == slots.len()
            && slots
                .iter()
                .enumerate()
                .all(|(position, slot)| *slot == Some(position));
        Self {
            child,
            schema: RowSchema::with_types(names, types),
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: true,
            compact_slots: (!identity_layout).then_some(slots),
        }
    }

    /// Attribute inner lock origins to this source qualifier so `FOR UPDATE OF` a view, CTE, or subquery does not lock sibling join inputs.
    #[must_use]
    pub fn rebinding_lock_origins(mut self, qualifier: impl Into<String>) -> Self {
        let qualifier = qualifier.into();
        if !qualifier.is_empty() {
            self.rebind_lock_qualifier = Some(Arc::from(qualifier));
        }
        self
    }

    /// Remove row-lock identities at a relational row-identity barrier.
    #[must_use]
    pub fn discarding_lock_origins(mut self) -> Self {
        self.rebind_lock_qualifier = None;
        self.discard_lock_origins = true;
        self
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
        let mut rows = match self.compact_slots.as_ref() {
            Some(slots) => batch
                .rows
                .into_iter()
                .map(|row| {
                    row.project_with_values(slots.iter().map(|slot| {
                        slot.map_or(
                            RowProjectionValue::Owned(Value::Null),
                            RowProjectionValue::InputSlot,
                        )
                    }))
                })
                .collect(),
            None => batch.rows,
        };
        if self.discard_lock_origins {
            for row in &mut rows {
                row.discard_lock_origins_mut();
            }
        } else if let Some(qualifier) = self.rebind_lock_qualifier.as_ref() {
            for row in &mut rows {
                row.rebind_lock_origin_qualifiers_mut(Arc::clone(qualifier));
            }
        }
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

    #[test]
    fn renames_repeated_columns_by_position() {
        let scan = TableScan::from_rows(
            vec!["left.value".into(), "right.value".into()],
            vec![BTreeMap::from([
                ("left.value".into(), Value::Int(1)),
                ("right.value".into(), Value::Int(2)),
            ])],
        );
        let mut selection = ColumnSelection::with_positions(
            Box::new(scan),
            vec![("value".into(), 0), ("value".into(), 1)],
        );
        let batches = crate::physical::run_to_batches(&mut selection).unwrap();
        assert_eq!(batches[0].schema.columns(), ["value", "value"]);
        let row = batches[0].schema.view(&batches[0].rows[0]);
        assert_eq!(row.value_at(0), Some(&Value::Int(1)));
        assert_eq!(row.value_at(1), Some(&Value::Int(2)));
    }

    #[test]
    fn ordering_is_remapped_by_selected_position() {
        let ordering = vec![PhysicalOrder {
            position: 2,
            descending: false,
            nulls_first: None,
            nullable: false,
        }];
        let remapped = remap_ordering(&ordering, &[Some(2), Some(0)]);
        assert_eq!(remapped[0].position, 0);
        assert!(remap_ordering(&ordering, &[Some(0), Some(1)]).is_empty());
    }

    #[test]
    fn row_identity_barrier_discards_lock_origins_in_place() {
        let schema = RowSchema::new(vec!["id".into()]);
        let row = crate::PhysicalRow::from_values(vec![Value::Int(1)])
            .with_lock_origin(crate::RowLockOrigin::new("accounts", "public.accounts", 1));
        let scan = TableScan::from_physical_rows(schema, vec![row]);
        let mut barrier = ColumnSelection::with_positions(Box::new(scan), vec![("id".into(), 0)])
            .discarding_lock_origins();

        let batches = crate::physical::run_to_batches(&mut barrier).unwrap();
        assert!(batches[0].rows[0].lock_origins().is_empty());
    }

    #[test]
    fn explicit_compaction_canonicalizes_a_wider_physical_layout() {
        let source = RowSchema::new(vec!["unused".into(), "value".into()]);
        let selected = RowSchema::select(&source, &[("value".into(), "value".into())]);
        let row = crate::PhysicalRow::from_values(vec![Value::Int(1), Value::Int(7)]);
        let scan = TableScan::from_physical_rows(selected, vec![row]);
        let mut compact =
            ColumnSelection::compacting_with_positions(Box::new(scan), vec![("value".into(), 0)]);

        compact.open().unwrap();
        let batch = compact.next().unwrap().unwrap();
        compact.close().unwrap();

        assert_eq!(batch.schema.physical_width(), 1);
        assert_eq!(
            batch.schema.view(&batch.rows[0]).get("value"),
            Some(&Value::Int(7))
        );
    }
}
