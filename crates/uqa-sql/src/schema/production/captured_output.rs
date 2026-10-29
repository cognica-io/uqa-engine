//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Positional capture retains physical record and score metadata while admitting only executor-visible bindings.

use super::{copy, lease::SchemaMemory, optional_type, parts};
use crate::{
    ast::InternalRelationId,
    schema::{RowSchema, SchemaBuildMetadata, SchemaMap},
    SQLError,
};
use uqa_core::memory::ProductionControl;

impl RowSchema {
    pub fn capture_output_with_control(
        input: &Self,
        relation: InternalRelationId,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut collations = input.index.cold.collations.copy_with_control(control)?;
        collations.clear_columns();
        let mut has_null = false;
        for position in 0..input.len() {
            control.check()?;
            has_null |= input.physical_slot(position).is_none();
            collations.internal_with_control(
                relation.column(position),
                input.column_collation(position),
                input.column_type(position),
                control,
            )?;
        }
        let internal = SchemaMap::from_produced(
            input
                .index
                .executor_attributes
                .iter()
                .map(|(column, slot)| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        control.finish(*slot, control.empty_reservation())?,
                    ))
                })
                .chain((0..input.len()).map(|position| {
                    Ok((
                        control.finish(relation.column(position), control.empty_reservation())?,
                        control.finish(
                            input
                                .physical_slot(position)
                                .unwrap_or(input.physical_width()),
                            control.empty_reservation(),
                        )?,
                    ))
                })),
            control,
        )?;
        let internal_types = SchemaMap::from_produced(
            input
                .index
                .cold
                .executor_attribute_types
                .iter()
                .map(|(column, ty)| {
                    Ok((
                        control.finish(*column, control.empty_reservation())?,
                        optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain((0..input.len()).map(|position| {
                    Ok((
                        control.finish(relation.column(position), control.empty_reservation())?,
                        optional_type(input.column_type(position), control)?,
                    ))
                })),
            control,
        )?;
        let record_fields = SchemaMap::from_produced(
            input.index.cold.record_fields.iter().map(|(slot, record)| {
                Ok((
                    control.finish(*slot, control.empty_reservation())?,
                    control.finish(record.clone(), control.empty_reservation())?,
                ))
            }),
            control,
        )?;
        let (score_sources, score_memory) = copy::score_sources(input, control)?.into_parts();
        let metadata = SchemaBuildMetadata {
            collations,
            internal,
            internal_types,
            record_fields,
            score_sources,
            memory: SchemaMemory::new(score_memory),
            ..SchemaBuildMetadata::empty_with_control(control)
        };
        parts::produce(
            control.finish(Vec::new(), control.empty_reservation())?,
            control.finish(Vec::new(), control.empty_reservation())?,
            control.finish(Vec::new(), control.empty_reservation())?,
            control.finish(Vec::new(), control.empty_reservation())?,
            input.physical_width() + usize::from(has_null),
            metadata,
            control,
        )
    }
}
