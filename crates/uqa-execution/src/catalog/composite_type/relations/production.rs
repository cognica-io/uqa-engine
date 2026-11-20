//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected relation descriptors admit borrowed table fields and view outputs before publication.

use super::{definitions, CatalogContext, CatalogReadView};
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    ast::ColumnDef,
    catalog::{relation_attributes::DroppedAttribute, CatalogWork},
    expr::composites::{
        CompositeDescriptorInput, CompositeTypeDescriptor, DroppedCompositeAttribute,
        OwnedCompositeTypeDescriptor,
    },
    ColumnType, SQLError,
};

pub fn descriptor_with_control(
    context: &CatalogContext<'_>,
    oid: u32,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedCompositeTypeDescriptor>, SQLError> {
    select(context, oid, control, std::convert::identity, |catalog| {
        crate::catalog::selection_work::inline(produce(context, &catalog, oid, control, &mut ()))
    })
}

/// Pin the original statement catalog before a cold descriptor reads any of its physical fields.
pub fn descriptor_input<'a>(
    context: CatalogContext<'a>,
    oid: u32,
    control: &ProductionControl<'_>,
) -> Result<CompositeDescriptorInput<'a>, SQLError> {
    select(
        &context,
        oid,
        control,
        CompositeDescriptorInput::ready,
        |catalog| {
            let owner = control.capture();
            CompositeDescriptorInput::from_catalog(control, move |mut work| async move {
                produce(&context, &catalog, oid, &owner.control(), &mut work).await
            })
        },
    )
}

fn select<T>(
    context: &CatalogContext<'_>,
    oid: u32,
    control: &ProductionControl<'_>,
    ready: impl FnOnce(Option<OwnedCompositeTypeDescriptor>) -> T,
    cold: impl FnOnce(CatalogReadView) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    control.check()?;
    let catalog = context.catalog_read_view();
    {
        let cached = catalog.relation_descriptors.entries.lock();
        if let Some(descriptor) = cached.known.get(&oid) {
            return Ok(ready(Some(OwnedCompositeTypeDescriptor::retained(
                descriptor.clone(),
            ))));
        }
        if cached.last_missing == Some(oid) {
            return Ok(ready(None));
        }
    }
    cold(catalog)
}

async fn produce<W: CatalogWork + Send>(
    context: &CatalogContext<'_>,
    catalog: &CatalogReadView,
    oid: u32,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Option<OwnedCompositeTypeDescriptor>, SQLError> {
    let mut selected = None;
    for (identity, oids, _) in definitions(catalog) {
        work.checkpoint().await?;
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
        CompositeTypeDescriptor::from_metadata_with_work(
            oid,
            oids.relation,
            columns.iter().enumerate().map(declared_field),
            dropped.iter().map(dropped_field),
            control,
            work,
        )
        .await?
    } else if let Some(view) = snapshot.definitions.views.get(identity) {
        let schema =
            context.stored_view_schema_with_catalog_with_control(view, catalog.clone(), control)?;
        CompositeTypeDescriptor::from_metadata_with_work(
            oid,
            oids.relation,
            (0..schema.columns().len()).map(|index| {
                Ok((
                    schema.columns()[index].as_str(),
                    schema
                        .column_type(index)
                        .unwrap_or(const { &ColumnType::Text }),
                    number(None, index)?,
                    schema.column_collation(index),
                ))
            }),
            std::iter::empty(),
            control,
            work,
        )
        .await?
    } else {
        return Ok(None);
    };
    OwnedCompositeTypeDescriptor::from_produced(descriptor, control).map(Some)
}

fn declared_field(
    (index, column): (usize, &ColumnDef),
) -> Result<(&str, &ColumnType, i16, Option<u32>), SQLError> {
    Ok((
        &column.name,
        &column.ty,
        number(column.attribute_number, index)?,
        column.collation_oid,
    ))
}

fn dropped_field(attribute: &DroppedAttribute) -> Result<DroppedCompositeAttribute, SQLError> {
    Ok(DroppedCompositeAttribute {
        number: attribute.number,
        length: attribute.type_length,
        alignment: attribute
            .alignment
            .as_bytes()
            .first()
            .copied()
            .ok_or_else(|| {
                SQLError::Internal("dropped attribute has no physical alignment".into())
            })?,
    })
}

fn number(number: Option<i16>, index: usize) -> Result<i16, SQLError> {
    number.map(Ok).unwrap_or_else(|| {
        i16::try_from(index + 1)
            .map_err(|_| SQLError::Internal("relation row type has too many attributes".into()))
    })
}
