//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    copy, inline, optional_type, OutputAlias, Produced, ProductionControl, ProductionVec,
    RowSchema, SQLError, SchemaBuildMetadata, SchemaMap, SchemaMemory, SchemaSet, ScoreSource,
};
use crate::schema::collations::Collations;

pub(super) fn produce(
    input: &RowSchema,
    aliases: &[OutputAlias<'_>],
    collations: Collations,
    wildcard_hidden: SchemaSet<usize>,
    keep_input_aliases: bool,
    rebound_score: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<SchemaBuildMetadata, SQLError> {
    let cold = &input.index.cold;
    let (score_sources, memory) = scores(input, rebound_score, control)?.into_parts();
    let mut metadata = SchemaBuildMetadata {
        collations,
        record_fields: SchemaMap::from_produced(
            cold.record_fields.iter().map(|(slot, record)| {
                Ok((inline(*slot, control)?, inline(record.clone(), control)?))
            }),
            control,
        )?,
        internal: SchemaMap::from_produced(
            input
                .index
                .executor_attributes
                .iter()
                .map(|(column, slot)| Ok((inline(*column, control)?, inline(*slot, control)?))),
            control,
        )?,
        internal_types: SchemaMap::from_produced(
            cold.executor_attribute_types.iter().map(|(column, ty)| {
                Ok((
                    inline(*column, control)?,
                    optional_type(ty.as_ref(), control)?,
                ))
            }),
            control,
        )?,
        score_sources,
        wildcard_hidden,
        memory: SchemaMemory::new(memory),
        ..SchemaBuildMetadata::empty_with_control(control)
    };
    if keep_input_aliases {
        metadata.aliases = SchemaMap::from_produced(
            input
                .index
                .aliases
                .iter()
                .map(|(identity, slot)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        inline(*slot, control)?,
                    ))
                })
                .chain(aliases.iter().map(|(identity, slot, _)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        inline(*slot, control)?,
                    ))
                })),
            control,
        )?;
        metadata.alias_types = SchemaMap::from_produced(
            cold.aliases
                .iter()
                .map(|(identity, ty)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain(aliases.iter().map(|(identity, _, ty)| {
                    Ok((
                        identity.clone_with_control(control)?,
                        optional_type(*ty, control)?,
                    ))
                })),
            control,
        )?;
        metadata.binding_only = SchemaMap::from_produced(
            cold.binding_only.iter().map(|(identity, ty)| {
                Ok((
                    identity.clone_with_control(control)?,
                    optional_type(ty.as_ref(), control)?,
                ))
            }),
            control,
        )?;
        metadata.open_qualifiers = SchemaSet::from_produced(
            cold.open_qualifiers.iter().map(|name| {
                let (name, memory) = match name {
                    Some(name) => {
                        let (name, memory) =
                            super::super::lease::boxed_text(name, control)?.into_parts();
                        (Some(name), memory)
                    }
                    None => (None, control.empty_reservation()),
                };
                Ok(control.finish(name, memory)?)
            }),
            control,
        )?;
        metadata.row_types = cold.row_types.clone();
        metadata.row_presence = cold.row_presence.clone();
    }
    Ok(metadata)
}

fn scores(
    input: &RowSchema,
    rebound: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ScoreSource>>, SQLError> {
    let Some(qualifier) = rebound else {
        return copy::score_sources(input, control);
    };
    let mut scores = ProductionVec::new(*control);
    for source in &input.index.cold.score_sources {
        let (qualifier, memory) = if qualifier.is_empty() {
            (None, control.empty_reservation())
        } else {
            let (name, memory) = super::super::lease::boxed_text(qualifier, control)?.into_parts();
            (Some(name), memory)
        };
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
