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
    pub(crate) fn with_scan_projection_with_control(
        child: Box<dyn PhysicalOperator + 'a>,
        qualifier: &str,
        aliases: &[String],
        wanted: Option<&uqa_sql::plan::source_projection::SourceProjection>,
        rebind_locks: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        crate::physical::construction::OperatorBuild::build(child, |owner| {
            let mut selected = ProductionVec::new(*control);
            for (position, identity) in owner.row_schema().identities().iter().enumerate() {
                let name = aliases
                    .get(position)
                    .map_or(identity.column(), String::as_str);
                if wanted.is_none_or(|wanted| wanted.contains(name)) {
                    selected.push_copy((name, position))?;
                }
            }
            let schema = crate::RowSchema::with_scan_projection_with_control(
                owner.row_schema(),
                qualifier,
                selected.iter().copied(),
                control,
            )?;
            let mut ordering = ProductionVec::new(*control);
            for order in owner.output_ordering() {
                let Some(position) = selected
                    .iter()
                    .position(|(_, input)| *input == order.position)
                else {
                    break;
                };
                ordering.push_produced(control.finish(
                    crate::PhysicalOrder {
                        position,
                        ..order.clone()
                    },
                    control.empty_reservation(),
                )?)?;
            }
            let rebind_lock_qualifier = (rebind_locks && !qualifier.is_empty())
                .then(|| crate::RowLockName::copy_with_control(qualifier, control))
                .transpose()?;
            let (ordering, memory) = ordering.finish()?.into_parts();
            let memory = control.combine(memory, control.reserve(size_of::<Self>())?);
            owner.wrap(|child| {
                Box::new(Self {
                    child,
                    schema,
                    ordering,
                    rebind_lock_qualifier,
                    discard_lock_origins: false,
                    compact_slots: None,
                    _memory: memory,
                })
            });
            Ok(())
        })
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{memory::MemoryBudget, CancellationToken};

    #[test]
    fn scan_projection_admits_public_aliases_and_retains_the_last_schema_reader() {
        let schema = crate::RowSchema::with_column_metadata(
            Some("original"),
            [
                ("x".into(), Some(uqa_sql::ColumnType::Text), Some(950)),
                ("n".into(), Some(uqa_sql::ColumnType::Integer), None),
            ],
        );
        let aliases = vec!["a".repeat(4 << 20), "number".into()];
        let memory = MemoryBudget::new(64 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&memory, &original, &invoking);
        let build = || {
            ColumnSelection::with_scan_projection_with_control(
                Box::new(crate::TableScan::from_physical_rows(
                    schema.clone(),
                    Vec::new(),
                )),
                "q",
                &aliases,
                None,
                true,
                &control,
            )
        };
        let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
        let mut result = None;
        let allocations = allocation_counter::measure(|| result = Some(build()));
        assert_eq!(result.unwrap().err().unwrap().sqlstate(), Some("53200"));
        assert!(allocations.bytes_total < 256 << 10, "{allocations:?}");
        assert_eq!(memory.used(), occupied.bytes());
        drop(occupied);
        for token in [&original, &invoking] {
            token.cancel();
            assert_eq!(build().err().unwrap().sqlstate(), Some("57014"));
            assert_eq!(memory.used(), 0);
            token.reset();
        }
        let mut operator = build().unwrap();
        let retained = operator.row_schema().clone();
        assert_eq!(retained.columns(), aliases);
        assert_eq!(retained.qualified_slot("q", &aliases[0]), Some(0));
        assert_eq!(retained.qualified_slot("q", "number"), Some(1));
        assert_eq!(retained.column_collation(0), Some(950));
        assert!(memory.used() >= aliases[0].len());
        operator.close().unwrap();
        drop(operator);
        assert!(memory.used() >= aliases[0].len());
        drop(retained);
        assert_eq!(memory.used(), 0);
    }
}
