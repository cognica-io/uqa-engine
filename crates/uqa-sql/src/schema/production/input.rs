//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained parameter layouts publish complete immutable schemas after bounded name and type work.

mod indexes;
mod work;

use super::parts;
use crate::{
    ast::{ColumnType, InternalRelationId},
    schema::{relation_types::RelationRowTypes, RowSchema, SchemaBuildMetadata, SchemaMap},
    SQLError,
};
use uqa_core::{
    image_io::AsyncImageInput,
    memory::{MemoryBudget, OwnedMap, Produced, ProductionControl, ProductionVec},
    CancellationToken,
};
use work::Work;

impl RowSchema {
    /// Construct the same visible and executor-only parameter slots while retaining each name comparison, type copy and unfinished index under the original allowance.
    pub async fn with_parameter_metadata_from_input(
        qualifier: &str,
        relation: InternalRelationId,
        columns: &[(&str, Option<&ColumnType>)],
        memory: &MemoryBudget,
        original: &CancellationToken,
        input: &mut AsyncImageInput<'_>,
    ) -> Result<Self, SQLError> {
        let control = ProductionControl::new(memory, original, input.cancellation);
        let mut work = Work { control, input };
        work.checkpoint().await?;
        let mut names = ProductionVec::new(control);
        let mut identities = ProductionVec::new(control);
        let mut types = ProductionVec::new(control);
        let mut slots = ProductionVec::new(control);
        names.reserve(columns.len())?;
        identities.reserve(columns.len())?;
        types.reserve(columns.len())?;
        slots.reserve(columns.len())?;
        let mut lookups = indexes::Names::default();
        let mut internal = OwnedMap::new();
        let mut internal_types = OwnedMap::new();
        for (position, (name, ty)) in columns.iter().enumerate() {
            work.checkpoint().await?;
            names.push_produced(work.text(name).await?)?;
            identities.push_produced(work.identity(qualifier, name).await?)?;
            types.push_produced(optional_type(*ty, &mut work).await?)?;
            slots.push_copy(position)?;

            lookups.insert(qualifier, name, position, &mut work).await?;
            work.insert(
                &mut internal,
                control.finish(relation.column(position), control.empty_reservation())?,
                control.finish(position, control.empty_reservation())?,
            )
            .await?;
            let ty = optional_type(*ty, &mut work).await?;
            work.insert(
                &mut internal_types,
                control.finish(relation.column(position), control.empty_reservation())?,
                ty,
            )
            .await?;
        }
        let mut metadata = SchemaBuildMetadata::empty_with_control(&control);
        metadata.internal = SchemaMap::from_ordered(internal, &control, work.input).await?;
        metadata.internal_types =
            SchemaMap::from_ordered(internal_types, &control, work.input).await?;
        if columns.is_empty() {
            let mut rows = OwnedMap::new();
            let key = work.boxed_text(qualifier).await?;
            work.insert(
                &mut rows,
                key,
                control.finish(None, control.empty_reservation())?,
            )
            .await?;
            metadata.row_types = RelationRowTypes::from_admitted_anonymous(
                SchemaMap::from_ordered(rows, &control, work.input).await?,
                work.boxed_text(qualifier).await?,
                &control,
            )?;
        }
        let indexes = lookups.finish(&mut work).await?;
        work.checkpoint().await?;
        parts::publish(
            names.finish()?,
            identities.finish()?,
            types.finish()?,
            slots.finish()?,
            parts::Publication {
                physical_width: columns.len(),
                metadata,
                indexes,
                identity_layout: true,
            },
            &control,
        )
    }
}

async fn optional_type(
    ty: Option<&ColumnType>,
    work: &mut Work<'_, '_, '_>,
) -> Result<Produced<Option<ColumnType>>, SQLError> {
    match ty {
        Some(ty) => {
            let (ty, memory) = ty
                .clone_from_input(&work.control, work.input)
                .await?
                .into_parts();
            Ok(work.control.finish(Some(ty), memory)?)
        }
        None => Ok(work
            .control
            .finish(None, work.control.empty_reservation())?),
    }
}

#[cfg(test)]
mod tests;
