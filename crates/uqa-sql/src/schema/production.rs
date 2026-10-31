//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Schema constructors own admitted names, type payloads, lookup indexes and layout buffers through the final shared row generation.

use super::{ColumnIdentity, ColumnType, RowSchema, SchemaBuildMetadata};
use crate::{ast::InternalRelationId, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

mod canonical;
mod captured_aliases;
mod captured_output;
mod composition;
pub(super) mod copy;
mod identity_aliases;
mod indexes;
mod internal_aliases;
pub(super) mod lease;
mod parts;
mod projection;
mod rebinding;
pub(super) mod remapping;
mod renaming;
mod virtual_names;
mod wildcards;

impl SchemaBuildMetadata {
    pub(in crate::schema) fn empty_with_control(control: &ProductionControl<'_>) -> Self {
        Self {
            memory: lease::SchemaMemory::new(control.empty_reservation()),
            ..Self::default()
        }
    }
}

impl RowSchema {
    /// Construct an empty immutable layout with its index and shared headers admitted before allocation.
    pub fn empty_with_control(control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        Self::new_with_control(
            control.finish(Vec::new(), control.empty_reservation())?,
            control,
        )
    }

    /// Consume already admitted column names and construct the immutable positional layout under the same allowance.
    pub fn new_with_control(
        columns: Produced<Vec<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut identities = ProductionVec::new(*control);
        let mut types = ProductionVec::new(*control);
        let mut slots = ProductionVec::new(*control);
        for (position, column) in columns.iter().enumerate() {
            identities.push_produced(ColumnIdentity::from_parts_with_control(
                None, column, control,
            )?)?;
            types.push_produced(control.finish(None, control.empty_reservation())?)?;
            slots.push_copy(position)?;
        }
        let width = columns.len();
        parts::produce(
            columns,
            identities.finish()?,
            types.finish()?,
            slots.finish()?,
            width,
            SchemaBuildMetadata::empty_with_control(control),
            control,
        )
    }

    /// Select borrowed declarations without constructing ordinary names, identities, types or collation maps first.
    pub fn with_column_metadata_with_control<'a>(
        qualifier: Option<&str>,
        columns: impl IntoIterator<Item = (&'a str, Option<&'a ColumnType>, Option<u32>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        columns_with_control(
            qualifier,
            None,
            columns
                .into_iter()
                .map(|(name, ty, oid)| (name, ty, oid, None)),
            false,
            control,
        )
    }

    /// Publish complete typed output metadata once, before sharing the schema index. Record handles already own their immutable field/descriptor payloads.
    pub(crate) fn with_output_metadata_with_control<'a>(
        qualifier: Option<&str>,
        columns: impl IntoIterator<
            Item = (
                &'a str,
                Option<&'a ColumnType>,
                Option<u32>,
                Option<super::RecordMetadata>,
            ),
        >,
        open: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        columns_with_control(qualifier, None, columns, open, control)
    }

    /// Produce the visible routine parameter namespace and its matching executor-only identities in one layout. Both refer to the same physical positions and retain the same selected types.
    pub fn with_parameter_metadata_with_control<'a>(
        qualifier: &str,
        relation: InternalRelationId,
        columns: impl IntoIterator<Item = (&'a str, Option<&'a ColumnType>)>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        columns_with_control(
            Some(qualifier),
            Some(relation),
            columns.into_iter().map(|(name, ty)| (name, ty, None, None)),
            false,
            control,
        )
    }
}

fn columns_with_control<'a>(
    qualifier: Option<&str>,
    internal_relation: Option<InternalRelationId>,
    columns: impl IntoIterator<
        Item = (
            &'a str,
            Option<&'a ColumnType>,
            Option<u32>,
            Option<super::RecordMetadata>,
        ),
    >,
    open: bool,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    columns_with_metadata(
        qualifier,
        internal_relation,
        columns,
        open,
        SchemaBuildMetadata::empty_with_control(control),
        control,
    )
}

fn columns_with_metadata<'a>(
    qualifier: Option<&str>,
    internal_relation: Option<InternalRelationId>,
    columns: impl IntoIterator<
        Item = (
            &'a str,
            Option<&'a ColumnType>,
            Option<u32>,
            Option<super::RecordMetadata>,
        ),
    >,
    open: bool,
    mut metadata: SchemaBuildMetadata,
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    control.check()?;
    let mut names = ProductionVec::new(*control);
    let mut identities = ProductionVec::new(*control);
    let mut types = ProductionVec::new(*control);
    let mut slots = ProductionVec::new(*control);
    let mut records = ProductionVec::new(*control);
    for (name, ty, oid, record) in columns {
        control.check()?;
        assert!(
            oid.is_none_or(|oid| oid == 0 || ty.is_some_and(super::composites::type_is_collatable)),
            "collation requires a collatable column type"
        );
        let position = names.len();
        names.push_produced(control.copy_text(name)?)?;
        identities.push_produced(ColumnIdentity::from_parts_with_control(
            qualifier, name, control,
        )?)?;
        types.push_produced(optional_type(ty, control)?)?;
        slots.push_copy(position)?;
        if let Some(record) = record {
            records
                .push_produced(control.finish((position, record), control.empty_reservation())?)?;
        }
        metadata
            .collations
            .column_with_control(position, oid, ty, control)?;
    }
    metadata.record_fields = super::SchemaMap::from_produced(
        records.iter().map(|(position, record)| {
            Ok((
                control.finish(*position, control.empty_reservation())?,
                control.finish(record.clone(), control.empty_reservation())?,
            ))
        }),
        control,
    )?;
    drop(records);
    if open {
        metadata.open_qualifiers = super::SchemaSet::from_produced(
            std::iter::once_with(|| {
                let (qualifier, memory) = match qualifier {
                    Some(name) => {
                        let (name, memory) = lease::boxed_text(name, control)?.into_parts();
                        (Some(name), memory)
                    }
                    None => (None, control.empty_reservation()),
                };
                Ok(control.finish(qualifier, memory)?)
            }),
            control,
        )?;
    }
    let width = names.len();
    if let Some(qualifier) = qualifier.filter(|_| width == 0) {
        metadata.row_types =
            super::relation_types::RelationRowTypes::anonymous_with_control(qualifier, control)?;
    }
    if let Some(relation) = internal_relation {
        metadata.internal = super::SchemaMap::from_produced(
            (0..width).map(|position| {
                Ok((
                    control.finish(relation.column(position), control.empty_reservation())?,
                    control.finish(position, control.empty_reservation())?,
                ))
            }),
            control,
        )?;
        metadata.internal_types = super::SchemaMap::from_produced(
            types.iter().enumerate().map(|(position, ty)| {
                Ok((
                    control.finish(relation.column(position), control.empty_reservation())?,
                    optional_type(ty.as_ref(), control)?,
                ))
            }),
            control,
        )?;
    }
    parts::produce(
        names.finish()?,
        identities.finish()?,
        types.finish()?,
        slots.finish()?,
        width,
        metadata,
        control,
    )
}

pub(super) fn optional_type(
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<ColumnType>>, SQLError> {
    match ty {
        Some(ty) => {
            let (ty, memory) = ty.clone_with_control(control)?.into_parts();
            Ok(control.finish(Some(ty), memory)?)
        }
        None => Ok(control.finish(None, control.empty_reservation())?),
    }
}

#[cfg(test)]
mod tests;
