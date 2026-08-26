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
    /// Physical ordering after the selected positions have been remapped.
    /// Executor-only projection attributes are addressed structurally and
    /// never enter the public SQL column namespace.
    ordering: Vec<PhysicalOrder>,
    rebind_lock_qualifier: Option<Arc<str>>,
    discard_lock_origins: bool,
    compact_slots: Option<Vec<Option<usize>>>,
}

impl<'a> ColumnSelection<'a> {
    /// Keep the visible layout unchanged while retiring consumed executor-only
    /// attributes from the schema namespace.
    pub fn dropping_internal_attributes(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: &[uqa_sql::ast::InternalColumnRef],
    ) -> Self {
        let schema = RowSchema::without_internal_attributes(child.row_schema(), columns);
        let ordering = child.output_ordering().to_vec();
        Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: false,
            compact_slots: None,
        }
    }

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

    /// Select flattened physical slots, including executor-only internal
    /// attributes that deliberately have no SQL-visible logical position.
    pub fn with_physical_positions(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: Vec<(String, usize)>,
    ) -> Self {
        let input_positions = columns
            .iter()
            .map(|(_, physical)| {
                (0..child.row_schema().len())
                    .find(|logical| child.row_schema().physical_slot(*logical) == Some(*physical))
            })
            .collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let columns = columns
            .into_iter()
            .map(|(label, physical)| {
                let ty = child.row_schema().physical_type(physical).cloned();
                (
                    label.clone(),
                    ColumnIdentity::unqualified(label),
                    physical,
                    ty,
                )
            })
            .collect::<Vec<_>>();
        let schema = RowSchema::remap_typed_physical_identities(child.row_schema(), &columns, &[]);
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

    /// Hide recursive-CTE generated columns from `*` while retaining them as real qualified and unqualified lookup aliases. `PostgreSQL` adds SEARCH/CYCLE state after expanding the recursive term's wildcard, but still permits the recursive term to name the generated cycle columns explicitly.
    pub fn hiding_trailing_columns(
        child: Box<dyn PhysicalOperator + 'a>,
        visible: usize,
        qualifier: &str,
    ) -> Self {
        let visible = visible.min(child.row_schema().len());
        let input_positions = (0..visible).map(Some).collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let columns = (0..visible)
            .map(|position| {
                let label = child.row_schema().columns()[position].clone();
                let identity = if qualifier.is_empty() {
                    ColumnIdentity::unqualified(&label)
                } else {
                    ColumnIdentity::qualified(qualifier, &label)
                };
                let ty = child.row_schema().column_type(position).cloned();
                (label, identity, position, ty)
            })
            .collect::<Vec<_>>();
        let mut aliases = Vec::new();
        for position in visible..child.row_schema().len() {
            let name = child.row_schema().columns()[position].clone();
            aliases.push((ColumnIdentity::unqualified(&name), position));
            if !qualifier.is_empty() {
                aliases.push((ColumnIdentity::qualified(qualifier, name), position));
            }
        }
        let schema = RowSchema::remap_typed_identities(child.row_schema(), &columns, &aliases);
        Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: true,
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
        let selected = RowSchema::remap_positions(child.row_schema(), &columns, &[]);
        let (schema, source_slots) = selected.canonical_projection();
        let identity_layout = child.row_schema().physical_width() == source_slots.len()
            && source_slots
                .iter()
                .enumerate()
                .all(|(position, slot)| *slot == position);
        Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: true,
            compact_slots: (!identity_layout).then(|| source_slots.into_iter().map(Some).collect()),
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

    /// Attribute every structurally carried retrieval score to this relation alias without introducing a hidden SQL column name.
    #[must_use]
    pub fn rebinding_score_sources(mut self, qualifier: impl Into<String>) -> Self {
        let qualifier = qualifier.into();
        self.schema = RowSchema::with_rebound_score_sources(
            &self.schema,
            (!qualifier.is_empty()).then_some(qualifier.as_str()),
        );
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
