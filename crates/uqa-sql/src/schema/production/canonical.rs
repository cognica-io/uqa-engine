//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Canonical row boundaries admit lookup ordering and physical-slot remapping before publishing their immutable layout.

use super::{
    copy,
    lease::{boxed_text, SchemaMemory},
    optional_type, parts,
};
use crate::{
    schema::{ColumnIdentity, RowSchema, SchemaBuildMetadata, SchemaMap, SchemaSet, NULL_SLOT},
    SQLError,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

#[cfg(test)]
mod tests;

impl RowSchema {
    pub fn canonical_projection_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, Produced<Vec<usize>>), SQLError> {
        self.project_canonical_with_control(false, control)
    }

    /// Establish a new relation's attribute order independently of its input's physical slots. Every visible attribute gets its own position, including repeated inputs and NULL slots; hidden executor attributes retain their selected values.
    pub fn canonical_relation_projection_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, Produced<Vec<usize>>), SQLError> {
        self.project_canonical_with_control(true, control)
    }

    fn project_canonical_with_control(
        &self,
        distinct_columns: bool,
        control: &ProductionControl<'_>,
    ) -> Result<(Self, Produced<Vec<usize>>), SQLError> {
        control.check()?;
        let mut source_slots = ProductionVec::new(*control);
        let mut positions = ProductionMap::new(*control);
        let mut slots = ProductionVec::new(*control);
        for slot in &self.index.slots {
            let position = if distinct_columns {
                let position = source_slots.len();
                source_slots.push_copy(*slot)?;
                if *slot != NULL_SLOT && positions.get(slot).is_none() {
                    positions.insert_copy(*slot, position)?;
                }
                position
            } else {
                remap(*slot, &mut source_slots, &mut positions)?
            };
            slots.push_copy(position)?;
        }
        let mut source_aliases = ProductionVec::new(*control);
        for (identity, slot) in &self.index.aliases {
            source_aliases.push_copy((identity, *slot))?;
        }
        source_aliases
            .as_mut_slice()
            .sort_unstable_by_key(|(identity, _)| *identity);
        let aliases = SchemaMap::from_produced(
            source_aliases.iter().map(|(identity, slot)| {
                Ok((
                    identity.clone_with_control(control)?,
                    inline(remap(*slot, &mut source_slots, &mut positions)?, control)?,
                ))
            }),
            control,
        )?;
        let mut source_internal = ProductionVec::new(*control);
        for (column, slot) in &self.index.executor_attributes {
            source_internal.push_copy((*column, *slot))?;
        }
        source_internal
            .as_mut_slice()
            .sort_unstable_by_key(|(column, _)| *column);
        let internal = SchemaMap::from_produced(
            source_internal.iter().map(|(column, slot)| {
                Ok((
                    inline(*column, control)?,
                    inline(remap(*slot, &mut source_slots, &mut positions)?, control)?,
                ))
            }),
            control,
        )?;
        let metadata = self.canonical_metadata(aliases, internal, &source_slots, control)?;
        let mut columns = ProductionVec::new(*control);
        let mut identities = ProductionVec::new(*control);
        let mut types = ProductionVec::new(*control);
        for name in self.columns() {
            columns.push_produced(control.copy_text(name)?)?;
        }
        for identity in self.identities() {
            identities.push_produced(identity.clone_with_control(control)?)?;
        }
        for ty in self.column_types() {
            types.push_produced(optional_type(ty.as_ref(), control)?)?;
        }
        let output = parts::produce(
            columns.finish()?,
            identities.finish()?,
            types.finish()?,
            slots.finish()?,
            source_slots.len(),
            metadata,
            control,
        )?;
        Ok((output, source_slots.finish()?))
    }
    fn canonical_metadata(
        &self,
        aliases: SchemaMap<ColumnIdentity, usize>,
        internal: SchemaMap<crate::ast::InternalColumnRef, usize>,
        source_slots: &[usize],
        control: &ProductionControl<'_>,
    ) -> Result<SchemaBuildMetadata, SQLError> {
        let cold = &self.index.cold;
        let scores = copy::score_sources(self, control)?;
        let (score_sources, memory) = scores.into_parts();
        Ok(SchemaBuildMetadata {
            collations: cold.collations.copy_with_control(control)?,
            record_fields: SchemaMap::from_produced(
                source_slots
                    .iter()
                    .enumerate()
                    .filter_map(|(position, slot)| {
                        cold.record_fields.get(slot).map(|fields| {
                            Ok((inline(position, control)?, inline(fields.clone(), control)?))
                        })
                    }),
                control,
            )?,
            aliases,
            internal,
            alias_types: identity_types(&cold.aliases, control)?,
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
            wildcard_hidden: SchemaSet::from_produced(
                cold.wildcard_hidden
                    .iter()
                    .map(|position| inline(*position, control)),
                control,
            )?,
            binding_only: identity_types(&cold.binding_only, control)?,
            row_types: cold.row_types.clone(),
            row_presence: cold.row_presence.clone(),
            open_qualifiers: SchemaSet::from_produced(
                cold.open_qualifiers.iter().map(|name| {
                    let (name, memory) = match name {
                        Some(name) => {
                            let (name, memory) = boxed_text(name, control)?.into_parts();
                            (Some(name), memory)
                        }
                        None => (None, control.empty_reservation()),
                    };
                    Ok(control.finish(name, memory)?)
                }),
                control,
            )?,
            memory: SchemaMemory::new(memory),
            ..SchemaBuildMetadata::empty_with_control(control)
        })
    }
}

fn remap(
    slot: usize,
    sources: &mut ProductionVec<'_, usize>,
    positions: &mut ProductionMap<'_, usize, usize>,
) -> Result<usize, SQLError> {
    if slot == NULL_SLOT {
        return Ok(NULL_SLOT);
    }
    if let Some(position) = positions.get(&slot) {
        return Ok(*position);
    }
    let position = sources.len();
    sources.push_copy(slot)?;
    positions.insert_copy(slot, position)?;
    Ok(position)
}

fn inline<T>(value: T, control: &ProductionControl<'_>) -> Result<Produced<T>, SQLError> {
    Ok(control.finish(value, control.empty_reservation())?)
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
