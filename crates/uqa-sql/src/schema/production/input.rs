//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Parameter and captured event rows publish their immutable names, types and physical indexes within caller work windows.

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
        qualified_columns(
            Columns::Parameters {
                qualifier,
                relation,
                columns,
            },
            control,
            input,
        )
        .await
    }

    /// Produce both visible row sides and their stored attribute identities in a single immutable layout.
    pub(crate) async fn with_captured_row_images<W: crate::catalog::CatalogWork + Send>(
        &self,
        sides: [(&str, Option<InternalRelationId>); 2],
        positions: &[Option<usize>],
        control: &ProductionControl<'_>,
        input: &mut W,
    ) -> Result<Self, SQLError> {
        assert_eq!(
            positions.len(),
            self.len(),
            "one captured position per visible column"
        );
        let sides = sides.map(|(name, id)| id.map(|id| (name, id)));
        qualified_columns(
            Columns::RowImages {
                row: self,
                sides,
                positions,
            },
            *control,
            input,
        )
        .await
    }
}

enum Columns<'a> {
    Parameters {
        qualifier: &'a str,
        relation: InternalRelationId,
        columns: &'a [(&'a str, Option<&'a ColumnType>)],
    },
    RowImages {
        row: &'a RowSchema,
        sides: [Option<(&'a str, InternalRelationId)>; 2],
        positions: &'a [Option<usize>],
    },
}

impl<'a> Columns<'a> {
    fn width(&self) -> Result<usize, SQLError> {
        match self {
            Self::Parameters { columns, .. } => Ok(columns.len()),
            Self::RowImages { row, sides, .. } => Ok(row
                .len()
                .checked_mul(sides.iter().flatten().count())
                .ok_or(uqa_core::memory::MemoryError::SizeOverflow)?),
        }
    }

    fn at(&self, slot: usize) -> Column<'a> {
        match self {
            Self::Parameters {
                qualifier,
                relation,
                columns,
            } => {
                let (name, ty) = columns[slot];
                Column {
                    qualifier,
                    name,
                    ty,
                    collation: None,
                    captured: Some(relation.column(slot)),
                }
            }
            Self::RowImages {
                row,
                sides,
                positions,
            } => {
                let (qualifier, relation) = sides
                    .iter()
                    .flatten()
                    .nth(slot / row.len())
                    .expect("selected row side");
                let source = slot % row.len();
                Column {
                    qualifier,
                    name: row.public_name(source).unwrap_or(&row.columns()[source]),
                    ty: row.column_type(source),
                    collation: row.column_collation(source),
                    captured: positions[source].map(|position| relation.column(position)),
                }
            }
        }
    }

    fn empty_names(&self) -> ([Option<&'a str>; 2], Option<&'a str>) {
        match self {
            Self::Parameters { qualifier, .. } => ([Some(*qualifier), None], Some(*qualifier)),
            Self::RowImages { sides, .. } => (sides.map(|side| side.map(|(name, _)| name)), None),
        }
    }
}

struct Column<'a> {
    qualifier: &'a str,
    name: &'a str,
    ty: Option<&'a ColumnType>,
    collation: Option<u32>,
    captured: Option<crate::ast::InternalColumnRef>,
}

async fn qualified_columns<W: crate::catalog::CatalogWork + Send>(
    columns: Columns<'_>,
    control: ProductionControl<'_>,
    input: &mut W,
) -> Result<RowSchema, SQLError> {
    let mut work = Work { control, input };
    let width = columns.width()?;
    work.checkpoint().await?;
    let mut names = ProductionVec::new(control);
    let mut identities = ProductionVec::new(control);
    let mut types = ProductionVec::new(control);
    let mut slots = ProductionVec::new(control);
    names.reserve(width)?;
    identities.reserve(width)?;
    types.reserve(width)?;
    slots.reserve(width)?;
    let mut lookups = indexes::Names::default();
    let mut internal = OwnedMap::new();
    let mut internal_types = OwnedMap::new();
    let mut metadata = SchemaBuildMetadata::empty_with_control(&control);
    for position in 0..width {
        let Column {
            qualifier,
            name,
            ty,
            collation,
            captured,
        } = columns.at(position);
        work.checkpoint().await?;
        names.push_produced(work.text(name).await?)?;
        identities.push_produced(work.identity(qualifier, name).await?)?;
        types.push_produced(optional_type(ty, &mut work).await?)?;
        slots.push_copy(position)?;

        lookups.insert(qualifier, name, position, &mut work).await?;
        metadata
            .collations
            .column_with_control(position, collation, ty, &control)?;
        if let Some(captured) = captured {
            work.insert(
                &mut internal,
                control.finish(captured, control.empty_reservation())?,
                control.finish(position, control.empty_reservation())?,
            )
            .await?;
            let declared = optional_type(ty, &mut work).await?;
            work.insert(
                &mut internal_types,
                control.finish(captured, control.empty_reservation())?,
                declared,
            )
            .await?;
            metadata
                .collations
                .internal_with_control(captured, collation, ty, &control)?;
        }
    }
    metadata.internal = SchemaMap::from_ordered(internal, &control, work.input).await?;
    metadata.internal_types = SchemaMap::from_ordered(internal_types, &control, work.input).await?;
    if width == 0 {
        let (empty_sides, source) = columns.empty_names();
        let mut rows = OwnedMap::new();
        for qualifier in empty_sides.into_iter().flatten() {
            let key = work.boxed_text(qualifier).await?;
            work.insert(
                &mut rows,
                key,
                control.finish(None, control.empty_reservation())?,
            )
            .await?;
        }
        let source = match source {
            Some(qualifier) => Some(work.boxed_text(qualifier).await?),
            None => None,
        };
        metadata.row_types = RelationRowTypes::from_admitted_names(
            SchemaMap::from_ordered(rows, &control, work.input).await?,
            source,
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
            physical_width: width,
            metadata,
            indexes,
            identity_layout: true,
        },
        &control,
    )
}

async fn optional_type<W: crate::catalog::CatalogWork + Send>(
    ty: Option<&ColumnType>,
    work: &mut Work<'_, '_, W>,
) -> Result<Produced<Option<ColumnType>>, SQLError> {
    match ty {
        Some(ty) => {
            let (ty, memory) = ty
                .clone_with_input_work(
                    &work.control,
                    &mut crate::catalog::work::ReferenceWork {
                        control: &work.control,
                        work: work.input,
                    },
                )
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
