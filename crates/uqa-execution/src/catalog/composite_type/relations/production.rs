//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected relation descriptors admit borrowed table fields and view outputs before publication.

use super::{definitions, CatalogContext};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    expr::composites::{
        CompositeTypeDescriptor, DroppedCompositeAttribute, OwnedCompositeTypeDescriptor,
    },
    ColumnType, SQLError,
};

pub fn descriptor_with_control(
    context: &CatalogContext<'_>,
    oid: u32,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedCompositeTypeDescriptor>, SQLError> {
    control.check()?;
    let catalog = context.catalog_read_view();
    {
        let cached = catalog.relation_descriptors.entries.lock();
        if let Some(descriptor) = cached.known.get(&oid) {
            return Ok(Some(OwnedCompositeTypeDescriptor::retained(
                descriptor.clone(),
            )));
        }
        if cached.last_missing == Some(oid) {
            return Ok(None);
        }
    }
    let mut selected = None;
    for (identity, oids, _) in definitions(&catalog) {
        control.check()?;
        if oids.row_type == Some(oid) {
            selected = Some((identity, oids));
            break;
        }
    }
    let Some((identity, oids)) = selected else {
        return Ok(None);
    };
    let snapshot = catalog.snapshot();
    let declared = snapshot
        .tables
        .get(identity)
        .map(|table| {
            (
                table.columns.as_slice(),
                table.dropped_attributes.as_slice(),
            )
        })
        .or_else(|| {
            snapshot
                .definitions
                .foreign_tables
                .get(identity)
                .map(|table| {
                    (
                        table.columns.as_slice(),
                        table.dropped_attributes.as_slice(),
                    )
                })
        });
    let descriptor = if let Some((columns, dropped)) = declared {
        CompositeTypeDescriptor::from_metadata_with_control(
            oid,
            oids.relation,
            columns.iter().enumerate().map(|(index, column)| {
                Ok((
                    column.name.as_str(),
                    &column.ty,
                    number(column.attribute_number, index)?,
                    column.collation_oid,
                ))
            }),
            dropped.iter().map(|attribute| {
                Ok(DroppedCompositeAttribute {
                    number: attribute.number,
                    length: attribute.type_length,
                    alignment: attribute.alignment.as_bytes().first().copied().ok_or_else(
                        || SQLError::Internal("dropped attribute has no physical alignment".into()),
                    )?,
                })
            }),
            control,
        )?
    } else if let Some(view) = snapshot.definitions.views.get(identity) {
        let schema =
            context.stored_view_schema_with_catalog_with_control(view, catalog.clone(), control)?;
        CompositeTypeDescriptor::from_metadata_with_control(
            oid,
            oids.relation,
            schema.columns().iter().enumerate().map(|(index, name)| {
                Ok((
                    name.as_str(),
                    schema.column_type(index).unwrap_or(&ColumnType::Text),
                    number(None, index)?,
                    schema.column_collation(index),
                ))
            }),
            std::iter::empty(),
            control,
        )?
    } else {
        return Ok(None);
    };
    OwnedCompositeTypeDescriptor::from_produced(descriptor, control).map(Some)
}

fn number(number: Option<i16>, index: usize) -> Result<i16, SQLError> {
    number.map(Ok).unwrap_or_else(|| {
        i16::try_from(index + 1)
            .map_err(|_| SQLError::Internal("relation row type has too many attributes".into()))
    })
}
