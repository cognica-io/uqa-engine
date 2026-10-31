//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical projection layouts admit only their surviving columns and preserve independent hidden metadata.

use super::{copy, lease::SchemaMemory, optional_type, parts};
use crate::{
    ast::InternalColumnRef,
    production::Buffer,
    schema::{
        ColumnIdentity, ColumnType, ProjectedSlot, RowSchema, SchemaBuildMetadata, SchemaMap,
        SchemaSet, NULL_SLOT,
    },
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

struct Column<'a> {
    name: &'a str,
    ty: Option<&'a ColumnType>,
    identity: Option<&'a ColumnIdentity>,
    source: ProjectedSlot,
    replaced: bool,
}

impl RowSchema {
    /// Build a physical output from borrowed selected fields. Replacement releases no unrelated metadata and copies no discarded column payload.
    pub fn project_with_sources_with_control<'a>(
        input: &'a Self,
        projected: impl IntoIterator<Item = (&'a str, Option<&'a ColumnType>, ProjectedSlot)>,
        projected_internal: &[(InternalColumnRef, Option<&ColumnType>, ProjectedSlot)],
        computed_count: usize,
        pass_through: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut columns = Buffer::new(control);
        if pass_through {
            for (position, name) in input.columns().iter().enumerate() {
                columns.push(
                    Column {
                        name,
                        ty: input.column_type(position),
                        identity: input.identity(position),
                        source: ProjectedSlot::Input(input.physical_slot(position)),
                        replaced: false,
                    },
                    control,
                )?;
            }
        }
        let mut collations = input.index.cold.collations.copy_with_control(control)?;
        for (column, ty, source) in projected_internal {
            collations.internal_with_control(
                *column,
                source_collation(input, *source),
                *ty,
                control,
            )?;
        }
        if !pass_through {
            collations.clear_columns();
        }
        for (name, ty, source) in projected {
            control.check()?;
            let position = if pass_through {
                columns.iter().position(|column| column.name == name)
            } else {
                None
            };
            let column = Column {
                name,
                ty,
                identity: None,
                source,
                replaced: true,
            };
            let position = if let Some(position) = position {
                columns[position] = column;
                position
            } else {
                let position = columns.len();
                columns.push(column, control)?;
                position
            };
            collations.column_with_control(
                position,
                source_collation(input, source),
                ty,
                control,
            )?;
        }
        let mut names = ProductionVec::new(*control);
        let mut identities = ProductionVec::new(*control);
        let mut types = ProductionVec::new(*control);
        let mut slots = ProductionVec::new(*control);
        for column in columns.iter() {
            names.push_produced(control.copy_text(column.name)?)?;
            identities.push_produced(match column.identity {
                Some(identity) => identity.clone_with_control(control)?,
                None => ColumnIdentity::from_parts_with_control(None, column.name, control)?,
            })?;
            types.push_produced(optional_type(column.ty, control)?)?;
            slots.push_copy(resolve_slot(input, column.source))?;
        }
        let metadata = projection_metadata(
            input,
            projected_internal,
            collations,
            pass_through,
            &columns,
            control,
        )?;
        parts::produce(
            names.finish()?,
            identities.finish()?,
            types.finish()?,
            slots.finish()?,
            input.physical_width() + computed_count,
            metadata,
            control,
        )
    }
}

fn projection_metadata(
    input: &RowSchema,
    projected_internal: &[(InternalColumnRef, Option<&ColumnType>, ProjectedSlot)],
    collations: crate::schema::Collations,
    pass_through: bool,
    columns: &[Column<'_>],
    control: &ProductionControl<'_>,
) -> Result<SchemaBuildMetadata, SQLError> {
    let (scores, score_memory) = copy::score_sources(input, control)?.into_parts();
    let cold = &input.index.cold;
    let inherited = if pass_through {
        inherited_metadata(input, columns, control)?
    } else {
        SchemaBuildMetadata::empty_with_control(control)
    };
    Ok(SchemaBuildMetadata {
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
                .map(|(column, slot)| Ok((inline(*column, control)?, inline(*slot, control)?)))
                .chain(projected_internal.iter().map(|(column, _, source)| {
                    Ok((
                        inline(*column, control)?,
                        inline(resolve_slot(input, *source), control)?,
                    ))
                })),
            control,
        )?,
        internal_types: SchemaMap::from_produced(
            cold.executor_attribute_types
                .iter()
                .map(|(column, ty)| {
                    Ok((
                        inline(*column, control)?,
                        optional_type(ty.as_ref(), control)?,
                    ))
                })
                .chain(projected_internal.iter().map(|(column, ty, _)| {
                    Ok((inline(*column, control)?, optional_type(*ty, control)?))
                })),
            control,
        )?,
        score_sources: scores,
        memory: SchemaMemory::new(score_memory),
        ..inherited
    })
}

fn inherited_metadata(
    input: &RowSchema,
    columns: &[Column<'_>],
    control: &ProductionControl<'_>,
) -> Result<SchemaBuildMetadata, SQLError> {
    let cold = &input.index.cold;
    Ok(SchemaBuildMetadata {
        aliases: SchemaMap::from_produced(
            input.index.aliases.iter().map(|(identity, slot)| {
                Ok((
                    identity.clone_with_control(control)?,
                    inline(*slot, control)?,
                ))
            }),
            control,
        )?,
        alias_types: identity_types(
            cold.aliases
                .iter()
                .map(|(identity, ty)| (identity, ty.as_ref())),
            control,
        )?,
        binding_only: identity_types(
            cold.binding_only
                .iter()
                .map(|(identity, ty)| (identity, ty.as_ref())),
            control,
        )?,
        wildcard_hidden: SchemaSet::from_produced(
            cold.wildcard_hidden
                .iter()
                .filter(|position| !columns[**position].replaced)
                .map(|position| inline(*position, control)),
            control,
        )?,
        open_qualifiers: SchemaSet::from_produced(
            cold.open_qualifiers.iter().map(|name| {
                let (name, memory) = match name.as_deref() {
                    Some(name) => {
                        let (name, memory) = super::lease::boxed_text(name, control)?.into_parts();
                        (Some(name), memory)
                    }
                    None => (None, control.empty_reservation()),
                };
                Ok(control.finish(name, memory)?)
            }),
            control,
        )?,
        row_types: cold.row_types.clone(),
        row_presence: cold.row_presence.clone(),
        ..SchemaBuildMetadata::empty_with_control(control)
    })
}

fn resolve_slot(input: &RowSchema, source: ProjectedSlot) -> usize {
    match source {
        ProjectedSlot::Input(slot) | ProjectedSlot::BoundInput { physical: slot, .. } => {
            slot.unwrap_or(NULL_SLOT)
        }
        ProjectedSlot::Computed { position, .. } => input.physical_width() + position,
    }
}

fn source_collation(input: &RowSchema, source: ProjectedSlot) -> Option<u32> {
    match source {
        ProjectedSlot::Input(Some(slot)) => input.inherited_physical_collation(slot),
        ProjectedSlot::BoundInput { collation, .. } | ProjectedSlot::Computed { collation, .. } => {
            collation
        }
        ProjectedSlot::Input(None) => None,
    }
}

fn inline<T>(value: T, control: &ProductionControl<'_>) -> Result<Produced<T>, SQLError> {
    Ok(control.finish(value, control.empty_reservation())?)
}

fn identity_types<'a>(
    entries: impl IntoIterator<Item = (&'a ColumnIdentity, Option<&'a ColumnType>)>,
    control: &ProductionControl<'_>,
) -> Result<SchemaMap<ColumnIdentity, Option<ColumnType>>, SQLError> {
    SchemaMap::from_produced(
        entries.into_iter().map(|(identity, ty)| {
            Ok((
                identity.clone_with_control(control)?,
                optional_type(ty, control)?,
            ))
        }),
        control,
    )
}

#[cfg(test)]
mod tests;
