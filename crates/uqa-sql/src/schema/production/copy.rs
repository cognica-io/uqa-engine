//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Mutable binding transforms publish complete independent layout and lookup owners while sharing immutable descriptor generations.

use super::{
    lease::{boxed_text, SchemaMemory},
    optional_type,
};
use crate::{
    schema::{ColumnIdentity, RowSchema, SchemaBuildMetadata, SchemaMap, SchemaSet, ScoreSource},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::schema) fn metadata(
    input: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<SchemaBuildMetadata, SQLError> {
    control.check()?;
    let index = &input.index;
    let cold = &index.cold;
    let score_sources = score_sources(input, control)?;
    let mut output = SchemaBuildMetadata {
        collations: cold.collations.copy_with_control(control)?,
        row_types: cold.row_types.clone(),
        row_presence: cold.row_presence.clone(),
        record_fields: SchemaMap::from_produced(
            cold.record_fields.iter().map(|(slot, record)| {
                Ok((inline(*slot, control)?, inline(record.clone(), control)?))
            }),
            control,
        )?,
        aliases: SchemaMap::from_produced(
            index.aliases.iter().map(|(identity, slot)| {
                Ok((
                    identity.clone_with_control(control)?,
                    inline(*slot, control)?,
                ))
            }),
            control,
        )?,
        alias_types: identity_types(&cold.aliases, control)?,
        internal: SchemaMap::from_produced(
            index
                .executor_attributes
                .iter()
                .map(|(identity, slot)| Ok((inline(*identity, control)?, inline(*slot, control)?))),
            control,
        )?,
        internal_types: SchemaMap::from_produced(
            cold.executor_attribute_types.iter().map(|(identity, ty)| {
                Ok((
                    inline(*identity, control)?,
                    optional_type(ty.as_ref(), control)?,
                ))
            }),
            control,
        )?,
        wildcard_hidden: SchemaSet::from_produced(
            cold.wildcard_hidden
                .iter()
                .map(|position| inline(*position, control)),
            control,
        )?,
        binding_only: identity_types(&cold.binding_only, control)?,
        open_qualifiers: SchemaSet::from_produced(
            cold.open_qualifiers
                .iter()
                .map(|name| optional_text(name.as_deref(), control)),
            control,
        )?,
        extra_ambiguous_unqualified: SchemaSet::from_produced(
            index
                .ambiguous_unqualified
                .iter()
                .map(|name| boxed_text(name, control)),
            control,
        )?,
        extra_ambiguous_qualified: SchemaSet::from_produced(
            index
                .ambiguous_qualified
                .iter()
                .map(|identity| identity.clone_with_control(control).map_err(Into::into)),
            control,
        )?,
        local_width: cold.local_width,
        lateral_restrictions: cold.lateral_restrictions.clone(),
        ..SchemaBuildMetadata::empty_with_control(control)
    };
    let (score_sources, memory) = score_sources.into_parts();
    output.score_sources = score_sources;
    output.memory = SchemaMemory::new(memory);
    Ok(output)
}

pub(super) fn score_sources(
    input: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ScoreSource>>, SQLError> {
    let mut scores = ProductionVec::new(*control);
    for source in &input.index.cold.score_sources {
        let qualifier = optional_text(source.qualifier.as_deref(), control)?;
        let (qualifier, memory) = qualifier.into_parts();
        scores.push_produced(control.finish(
            ScoreSource {
                qualifier,
                column: source.column,
            },
            memory,
        )?)?;
    }
    Ok(scores.finish()?)
}

fn identity_types(
    input: &SchemaMap<ColumnIdentity, Option<crate::ColumnType>>,
    control: &ProductionControl<'_>,
) -> Result<SchemaMap<ColumnIdentity, Option<crate::ColumnType>>, SQLError> {
    SchemaMap::from_produced(
        input.iter().map(|(identity, ty)| {
            Ok((
                identity.clone_with_control(control)?,
                optional_type(ty.as_ref(), control)?,
            ))
        }),
        control,
    )
}

fn inline<T>(value: T, control: &ProductionControl<'_>) -> Result<Produced<T>, SQLError> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn optional_text(
    value: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<Box<str>>>, SQLError> {
    let (value, memory) = match value {
        Some(value) => {
            let (value, memory) = boxed_text(value, control)?.into_parts();
            (Some(value), memory)
        }
        None => (None, control.empty_reservation()),
    };
    Ok(control.finish(value, memory)?)
}

pub(in crate::schema) fn with_metadata(
    input: &RowSchema,
    metadata: SchemaBuildMetadata,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let mut columns = ProductionVec::new(*control);
    let mut identities = ProductionVec::new(*control);
    let mut types = ProductionVec::new(*control);
    let mut slots = ProductionVec::new(*control);
    for (position, name) in input.columns().iter().enumerate() {
        columns.push_produced(control.copy_text(name)?)?;
        identities.push_produced(input.identities()[position].clone_with_control(control)?)?;
        types.push_produced(optional_type(input.column_type(position), control)?)?;
        slots.push_copy(input.index.slots[position])?;
    }
    super::parts::produce(
        columns.finish()?,
        identities.finish()?,
        types.finish()?,
        slots.finish()?,
        input.physical_width(),
        metadata,
        control,
    )
}
