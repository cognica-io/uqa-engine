//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical output publication keeps selected record descriptors shared and admits new labels, types, identities and hidden names.

use super::{optional_type, parts};
use crate::{
    schema::{ColumnIdentity, RowSchema, SchemaBuildMetadata, SchemaMap},
    SQLError,
};
use uqa_core::memory::{ProductionControl, ProductionMap, ProductionVec};

impl RowSchema {
    /// Publish fixed public names over the same logical output metadata. This canonicalizes the physical positions exactly as an ordinary named view or result schema does.
    pub fn with_output_names_with_control<'a>(
        input: &Self,
        names: impl IntoIterator<Item = &'a str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        output(
            input,
            names,
            SchemaBuildMetadata::empty_with_control(control),
            control,
        )
    }

    /// Recursive SEARCH/CYCLE columns remain explicitly addressable while the original public prefix owns wildcard expansion.
    pub fn with_recursive_input_with_control(
        input: &Self,
        visible: usize,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let visible = visible.min(input.len());
        let mut first = ProductionMap::new(*control);
        for (position, name) in input.columns().iter().enumerate() {
            control.check()?;
            if first.get(&name.as_str()).is_none() {
                first.insert_copy(name.as_str(), position)?;
            }
        }
        let binding_only = SchemaMap::from_produced(
            first
                .iter()
                .filter(|(_, position)| **position >= visible)
                .map(|(name, position)| {
                    Ok((
                        ColumnIdentity::from_parts_with_control(None, name, control)?,
                        optional_type(input.column_type(*position), control)?,
                    ))
                }),
            control,
        )?;
        drop(first);
        output(
            input,
            input.columns()[..visible].iter().map(String::as_str),
            SchemaBuildMetadata {
                binding_only,
                ..SchemaBuildMetadata::empty_with_control(control)
            },
            control,
        )
    }
}

fn output<'a>(
    input: &RowSchema,
    names: impl IntoIterator<Item = &'a str>,
    mut metadata: SchemaBuildMetadata,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    let mut columns = ProductionVec::new(*control);
    let mut identities = ProductionVec::new(*control);
    let mut types = ProductionVec::new(*control);
    let mut slots = ProductionVec::new(*control);
    for (position, name) in names.into_iter().enumerate() {
        control.check()?;
        let ty = input.column_type(position);
        columns.push_produced(control.copy_text(name)?)?;
        identities.push_produced(ColumnIdentity::from_parts_with_control(
            None, name, control,
        )?)?;
        types.push_produced(optional_type(ty, control)?)?;
        slots.push_copy(position)?;
        metadata.collations.column_with_control(
            position,
            input.column_collation(position),
            ty,
            control,
        )?;
    }
    let width = columns.len();
    metadata.record_fields = SchemaMap::from_produced(
        (0..width.min(input.len()))
            .filter_map(|position| {
                input
                    .record_metadata(position)
                    .map(|record| (position, record))
            })
            .map(|(position, record)| {
                Ok((
                    control.finish(position, control.empty_reservation())?,
                    control.finish(record.clone(), control.empty_reservation())?,
                ))
            }),
        control,
    )?;
    parts::produce(
        columns.finish()?,
        identities.finish()?,
        types.finish()?,
        slots.finish()?,
        width,
        metadata,
        control,
    )
}
