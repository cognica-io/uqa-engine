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
    /// Hide a derived input's SQL namespace while sharing its physical values through positional structural references.
    pub(crate) fn capturing_output(
        child: Box<dyn PhysicalOperator + 'a>,
        relation: uqa_sql::ast::InternalRelationId,
    ) -> Self {
        let schema = RowSchema::capture_output(child.row_schema(), relation);
        let width = child.row_schema().physical_width();
        let compact_slots =
            (schema.physical_width() > width).then(|| (0..width).map(Some).chain([None]).collect());
        Self {
            child,
            schema,
            ordering: Vec::new(),
            rebind_lock_qualifier: None,
            discard_lock_origins: false,
            compact_slots,
        }
    }

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

    /// Map public outputs to their analyzed descriptor and retain structural score provenance. Analysis owns public collations and record descriptors; the producer owns shared value slots and opaque score identities.
    pub(crate) fn with_bound_output(
        child: Box<dyn PhysicalOperator + 'a>,
        columns: &[(String, usize)],
        mut schema: RowSchema,
    ) -> ExecResult<Self> {
        if !schema.is_identity_layout()
            || schema.physical_width() != schema.len()
            || columns.len() != schema.len()
            || columns.iter().enumerate().any(|(output, (name, input))| {
                schema.columns()[output] != *name
                    || *input >= child.row_schema().len()
                    || schema.column_type(output) != child.row_schema().column_type(*input)
            })
        {
            return Err(uqa_sql::SQLError::Internal(
                "query output does not match its analyzed columns and types".into(),
            )
            .into());
        }
        let input_positions = columns
            .iter()
            .map(|(_, position)| Some(*position))
            .collect::<Vec<_>>();
        let ordering = remap_ordering(child.output_ordering(), &input_positions);
        let mut slots = columns
            .iter()
            .map(|(_, position)| child.row_schema().physical_slot(*position))
            .collect::<Vec<_>>();
        if child.row_schema().score_sources().next().is_some() {
            let score_columns = child
                .row_schema()
                .score_sources()
                .map(|(_, column)| column)
                .collect::<std::collections::BTreeSet<_>>();
            let retired = child
                .row_schema()
                .internal_columns_with_types()
                .into_iter()
                .filter_map(|(column, _, _)| (!score_columns.contains(&column)).then_some(column))
                .collect::<Vec<_>>();
            let scores =
                RowSchema::remap_typed_identities_without_input_aliases(child.row_schema(), &[]);
            let scores = RowSchema::without_internal_attributes(&scores, &retired);
            let (scores, score_slots) = scores.canonical_projection();
            schema = RowSchema::join(&schema, &scores, []);
            slots.extend(score_slots.into_iter().map(Some));
        }
        let identity_layout = child.row_schema().physical_width() == slots.len()
            && slots
                .iter()
                .enumerate()
                .all(|(position, slot)| *slot == Some(position));
        Ok(Self {
            child,
            schema,
            ordering,
            rebind_lock_qualifier: None,
            discard_lock_origins: false,
            compact_slots: (!identity_layout).then_some(slots),
        })
    }

    /// Establish a source namespace even when the selected row has no named columns.
    #[must_use]
    pub(crate) fn with_relation_qualifier(mut self, qualifier: &str) -> Self {
        self.schema = RowSchema::with_relation_qualifier(&self.schema, qualifier);
        self
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

    /// A derived-table demand boundary changes its physical layout without retiring row-lock identities needed by an enclosing locking clause.
    #[must_use]
    pub fn preserving_lock_origins(mut self) -> Self {
        self.discard_lock_origins = false;
        self
    }

    fn select_batch(&self, batch: Batch) -> Batch {
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
        Batch::from_physical_rows(self.schema.clone(), rows)
    }
}

impl ColumnSelection<'_> {
    fn pull(&mut self, demand: Option<std::num::NonZeroUsize>) -> ExecResult<Option<Batch>> {
        let Some(batch) = (match demand {
            Some(rows) => self.child.next_with_demand(rows)?,
            None => self.child.next()?,
        }) else {
            return Ok(None);
        };
        Ok(Some(self.select_batch(batch)))
    }
}

impl PhysicalOperator for ColumnSelection<'_> {
    fn row_schema(&self) -> &RowSchema {
        &self.schema
    }

    fn supports_batched_consumption(&self) -> bool {
        self.child.supports_batched_consumption()
    }

    fn estimated_cardinality(&self) -> Option<u64> {
        self.child.estimated_cardinality()
    }

    fn output_ordering(&self) -> &[PhysicalOrder] {
        &self.ordering
    }

    fn backward_scan_support(&self) -> crate::BackwardScanSupport {
        self.child.backward_scan_support()
    }

    fn open(&mut self) -> ExecResult<()> {
        self.child.open()
    }

    fn next(&mut self) -> ExecResult<Option<Batch>> {
        self.pull(None)
    }

    fn next_with_demand(&mut self, rows: std::num::NonZeroUsize) -> ExecResult<Option<Batch>> {
        self.pull(Some(rows))
    }

    fn next_direction(
        &mut self,
        direction: crate::PhysicalScanDirection,
    ) -> ExecResult<Option<Batch>> {
        let Some(batch) = self.child.next_direction(direction)? else {
            return Ok(None);
        };
        Ok(Some(self.select_batch(batch)))
    }

    fn rewind(&mut self) -> ExecResult<()> {
        self.child.rewind()
    }

    fn close(&mut self) -> ExecResult<()> {
        self.child.close()
    }
}

#[cfg(test)]
mod tests {
    use crate::RowSchemaExecution;
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
        assert_eq!(remap_ordering(&ordering, &[Some(0), Some(1)]).len(), 0);
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
        assert_eq!(batches[0].rows[0].lock_origins().len(), 0);
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

    #[test]
    fn captured_output_shares_values_without_exposing_input_names() {
        let relation = uqa_sql::ast::InternalRelationId::allocate();
        let source = RowSchema::with_types(
            vec!["unused".into(), "payload".into()],
            vec![
                Some(uqa_sql::ColumnType::Integer),
                Some(uqa_sql::ColumnType::Text),
            ],
        );
        let selected = RowSchema::select(&source, &[("payload".into(), "payload".into())]);
        let row =
            crate::PhysicalRow::from_values(vec![Value::Int(7), Value::Str("shared".repeat(1000))]);
        let retained = row.clone();
        let mut output = ColumnSelection::capturing_output(
            Box::new(TableScan::from_physical_rows(selected, vec![row])),
            relation,
        );
        let batches = crate::physical::run_to_batches(&mut output).unwrap();
        assert_eq!(batches.len(), 1);
        let batch = &batches[0];
        assert!(batch.schema.is_empty());
        assert_eq!(batch.schema.position("payload"), None);
        let slot = batch.schema.internal_slot(relation.column(0)).unwrap();
        assert!(std::ptr::eq(
            batch.rows[0].value(slot).unwrap(),
            retained.value(1).unwrap()
        ));
        assert_eq!(
            batch.schema.internal_type(relation.column(0)),
            Some(&uqa_sql::ColumnType::Text)
        );
        let selected = RowSchema::select(&source, &[("missing".into(), "absent".into())]);
        let mut output = ColumnSelection::capturing_output(
            Box::new(TableScan::from_physical_rows(selected, vec![retained])),
            relation,
        );
        let batches = crate::physical::run_to_batches(&mut output).unwrap();
        let slot = batches[0].schema.internal_slot(relation.column(0)).unwrap();
        assert_eq!(batches[0].rows[0].value(slot), Some(&Value::Null));
    }

    #[test]
    fn bound_output_preserves_metadata_shared_values_and_lock_origins() {
        use uqa_sql::ColumnType;

        let fields: uqa_sql::schema::RecordFields =
            Arc::from([Some(ColumnType::SmallInteger), Some(ColumnType::Text)]);
        let bound = RowSchema::with_types(
            vec!["first".into(), "second".into(), "record".into()],
            vec![
                Some(ColumnType::Text),
                Some(ColumnType::Text),
                Some(ColumnType::Record),
            ],
        )
        .with_column_collations([Some(950), Some(951), None])
        .with_record_fields([(2, Arc::clone(&fields))]);
        let input = RowSchema::with_types(
            vec!["hidden".into(), "text".into(), "record".into()],
            vec![
                Some(ColumnType::Integer),
                Some(ColumnType::Text),
                Some(ColumnType::Record),
            ],
        );
        let row = crate::PhysicalRow::from_values(vec![
            Value::Int(99),
            Value::Str("shared".repeat(1_000)),
            Value::Record(vec![("n".into(), Value::Int(7)), ("s".into(), Value::Null)].into()),
        ])
        .with_lock_origin(crate::RowLockOrigin::new("items", "public.items", 1));
        let retained = row.clone();
        let mut output = ColumnSelection::with_bound_output(
            Box::new(TableScan::from_physical_rows(input, vec![row])),
            &[
                ("first".into(), 1),
                ("second".into(), 1),
                ("record".into(), 2),
            ],
            bound.clone(),
        )
        .unwrap();
        assert_eq!(output.row_schema(), &bound);
        let batches = crate::physical::run_to_batches(&mut output).unwrap();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].schema, bound);
        let result = &batches[0].rows[0];
        for (output, input) in [(0, 1), (1, 1), (2, 2)] {
            assert!(std::ptr::eq(
                result.value(output).unwrap(),
                retained.value(input).unwrap()
            ));
        }
        assert_eq!(result.lock_origins().len(), 1);
        assert!(Arc::ptr_eq(
            batches[0].schema.record_fields(2).unwrap(),
            &fields
        ));
    }

    #[test]
    fn bound_output_rejects_changed_columns_types_and_nonpositional_descriptors() {
        use uqa_sql::ColumnType;

        let input = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Integer)]);
        let other_type = RowSchema::with_types(vec!["value".into()], vec![Some(ColumnType::Text)]);
        let wider = RowSchema::with_types(
            vec!["hidden".into(), "value".into()],
            vec![Some(ColumnType::Integer); 2],
        );
        let nonpositional = RowSchema::select(&wider, &[("value".into(), "value".into())]);
        for (columns, bound) in [
            (vec![("renamed".into(), 0)], input.clone()),
            (vec![], input.clone()),
            (vec![("value".into(), 1)], input.clone()),
            (vec![("value".into(), 0)], other_type),
            (vec![("value".into(), 0)], nonpositional),
        ] {
            let child = Box::new(TableScan::from_physical_rows(input.clone(), vec![]));
            assert!(ColumnSelection::with_bound_output(child, &columns, bound).is_err());
        }
    }

    #[test]
    fn bound_output_preserves_scores_without_exposing_internal_columns() {
        use uqa_sql::ColumnType;

        let public = RowSchema::with_types(vec!["body".into()], vec![Some(ColumnType::Text)])
            .with_column_collations([Some(950)]);
        let first = uqa_sql::ast::InternalRelationId::allocate().column(0);
        let second = uqa_sql::ast::InternalRelationId::allocate().column(0);
        let physical = RowSchema::from_physical_layout(uqa_sql::schema::PhysicalLayout {
            columns: vec!["unused".into(), "body".into()],
            identities: vec![
                ColumnIdentity::unqualified("unused"),
                ColumnIdentity::unqualified("body"),
            ],
            types: vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
            slots: vec![Some(0), Some(1)],
            physical_width: 4,
            aliases: vec![],
            internal: vec![
                (first, Some(2), Some(ColumnType::DoublePrecision)),
                (second, Some(3), Some(ColumnType::DoublePrecision)),
            ],
            score_sources: vec![
                (Some("left".into()), first),
                (Some("right".into()), second),
                (Some("echo".into()), first),
            ],
            wildcard_hidden: std::collections::HashSet::new(),
        })
        .unwrap();
        let row = crate::PhysicalRow::from_values(vec![
            Value::Int(99),
            Value::Str("rust".into()),
            Value::Float(1.25),
            Value::Float(2.5),
        ]);
        let retained = row.clone();
        let mut output = ColumnSelection::with_bound_output(
            Box::new(TableScan::from_physical_rows(physical, vec![row])),
            &[("body".into(), 1)],
            public,
        )
        .unwrap();
        assert_eq!(output.schema(), ["body"]);
        assert_eq!(output.row_schema().column_collation(0), Some(950));
        assert_eq!(
            output.row_schema().score_sources().collect::<Vec<_>>(),
            [
                (Some("left"), first),
                (Some("right"), second),
                (Some("echo"), first)
            ]
        );
        assert_eq!(output.row_schema().physical_width(), 3);
        let batches = crate::physical::run_to_batches(&mut output).unwrap();
        for (column, slot) in [(first, 2), (second, 3)] {
            let output_slot = batches[0].schema.internal_slot(column).unwrap();
            assert!(std::ptr::eq(
                batches[0].rows[0].value(output_slot).unwrap(),
                retained.value(slot).unwrap(),
            ));
        }
    }
}
