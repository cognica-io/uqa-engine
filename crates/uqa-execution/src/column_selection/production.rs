//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A relation alias preserves the child's physical row while admitting its replacement namespace and ordering.

use super::{ColumnSelection, PhysicalOperator};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
use uqa_sql::SQLError;

impl<'a> ColumnSelection<'a> {
    pub(crate) fn with_join_alias_with_control(
        child: Box<dyn PhysicalOperator + 'a>,
        alias: &str,
        columns: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        crate::physical::construction::OperatorBuild::build(child, |owner| {
            let schema = uqa_sql::semantics::alias_join_physical_schema_with_control(
                owner.row_schema(),
                alias,
                columns,
                control,
            )?;
            let (schema, slots) = relation_namespace(schema, control)?;
            let (compact_slots, slot_memory) = match slots {
                Some(slots) => {
                    let (slots, memory) = slots.into_parts();
                    (Some(slots), memory)
                }
                None => (None, control.empty_reservation()),
            };
            let mut ordering = ProductionVec::new(*control);
            for order in owner.output_ordering() {
                control.check()?;
                if order.position >= schema.len() {
                    break;
                }
                ordering
                    .push_produced(control.finish(order.clone(), control.empty_reservation())?)?;
            }
            let (ordering, ordering_memory) = ordering.finish()?.into_parts();
            let memory = control.combine(
                control.combine(ordering_memory, slot_memory),
                control.reserve(size_of::<Self>())?,
            );
            owner.wrap(|child| {
                Box::new(Self {
                    child,
                    schema,
                    ordering,
                    rebind_lock_qualifier: None,
                    discard_lock_origins: false,
                    compact_slots,
                    _memory: memory,
                })
            });
            Ok(())
        })
    }
}

type RelationProjection = (crate::RowSchema, Option<Produced<Vec<Option<usize>>>>);

/// Physical slots encode a relation's attribute order for qualified wildcards and whole-row values. A fresh namespace must establish that order before another JOIN can hide any of its attributes.
pub(super) fn relation_namespace(
    schema: crate::RowSchema,
    control: &ProductionControl<'_>,
) -> Result<RelationProjection, SQLError> {
    if schema
        .layout_slots()
        .iter()
        .enumerate()
        .all(|(position, slot)| position == *slot)
    {
        control.check()?;
        return Ok((schema, None));
    }
    let width = schema.physical_width();
    let (schema, sources) = schema.canonical_relation_projection_with_control(control)?;
    let mut slots = ProductionVec::new(*control);
    for source in &*sources {
        slots.push_copy((*source < width).then_some(*source))?;
    }
    Ok((schema, Some(slots.finish()?)))
}
