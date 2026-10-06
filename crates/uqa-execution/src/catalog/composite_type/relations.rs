//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation row types share their owning relation's catalog identity and live columns.

use crate::catalog::{context::CatalogContext, CatalogReadView};
use std::sync::Arc;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{ColumnType, CompositeTypeReference},
    catalog::{array_type_names::array_type_name, relation_oids::RelationCatalogOids},
    expr::composites::{CompositeAttribute, CompositeTypeDescriptor},
    SQLError,
};

fn definitions(
    catalog: &CatalogReadView,
) -> impl Iterator<Item = (&RelationIdentity, RelationCatalogOids, Option<&str>)> {
    let snapshot = catalog.snapshot();
    snapshot
        .tables
        .iter()
        .map(|(identity, table)| {
            (
                identity,
                table.catalog_oids,
                table.row_type_array_name.as_deref(),
            )
        })
        .chain(snapshot.definitions.views.iter().map(|(identity, view)| {
            (
                identity,
                view.relation_oids(),
                view.definition.row_type_array_name.as_deref(),
            )
        }))
        .chain(
            snapshot
                .definitions
                .foreign_tables
                .iter()
                .map(|(identity, table)| {
                    (
                        identity,
                        table.relation_oids(),
                        table.row_type_array_name.as_deref(),
                    )
                }),
        )
}

fn column_type(identity: &RelationIdentity, oids: RelationCatalogOids) -> Option<ColumnType> {
    Some(ColumnType::Composite(CompositeTypeReference {
        schema: identity.schema.clone(),
        name: identity.name.clone(),
        oid: oids.row_type?,
        array_oid: oids.array_type.unwrap_or(0),
        relation_oid: oids.relation,
    }))
}

pub(crate) fn by_name(catalog: &CatalogReadView, schema: &str, name: &str) -> Option<ColumnType> {
    definitions(catalog)
        .filter(|(identity, _, _)| identity.schema == schema)
        .find_map(|(identity, oids, array)| {
            if identity.name == name {
                column_type(identity, oids)
            } else if oids.array_type.is_some()
                && array.map_or_else(
                    || array_type_name(&identity.name, 0) == name,
                    |array| array == name,
                )
            {
                column_type(identity, oids).map(|ty| ColumnType::Array(Box::new(ty)))
            } else {
                None
            }
        })
}

pub(crate) fn by_oid(catalog: &CatalogReadView, oid: u32) -> Option<ColumnType> {
    definitions(catalog).find_map(|(identity, oids, _)| {
        if oids.row_type == Some(oid) {
            column_type(identity, oids)
        } else if oids.array_type == Some(oid) {
            column_type(identity, oids).map(|ty| ColumnType::Array(Box::new(ty)))
        } else {
            None
        }
    })
}

pub fn descriptor(
    context: &CatalogContext<'_>,
    oid: u32,
) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
    let catalog = context.catalog_read_view();
    let Some(ColumnType::Composite(reference)) = by_oid(&catalog, oid) else {
        return Ok(None);
    };
    let identity = RelationIdentity::new(reference.schema, reference.name);
    let snapshot = catalog.snapshot();
    let columns = if let Some(table) = snapshot.tables.get(&identity) {
        table
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.clone(),
                    column.ty.clone(),
                    column.attribute_number,
                )
            })
            .collect::<Vec<_>>()
    } else if let Some(table) = snapshot.definitions.foreign_tables.get(&identity) {
        table
            .columns
            .iter()
            .map(|column| {
                (
                    column.name.clone(),
                    column.ty.clone(),
                    column.attribute_number,
                )
            })
            .collect()
    } else if let Some(view) = snapshot.definitions.views.get(&identity) {
        let schema = context.stored_view_schema_with_catalog(
            view,
            catalog.clone(),
            context.session.relation_name_resolution(),
        )?;
        schema
            .columns()
            .iter()
            .cloned()
            .zip(
                schema
                    .column_types()
                    .iter()
                    .map(|ty| ty.clone().unwrap_or(ColumnType::Text)),
            )
            .map(|(name, ty)| (name, ty, None))
            .collect()
    } else {
        return Ok(None);
    };
    let attributes = columns
        .into_iter()
        .enumerate()
        .map(|(index, (name, ty, number))| {
            Ok(CompositeAttribute {
                name,
                ty,
                number: if let Some(number) = number {
                    number
                } else {
                    i16::try_from(index + 1).map_err(|_| {
                        SQLError::Internal("relation row type has too many attributes".into())
                    })?
                },
            })
        })
        .collect::<Result<_, SQLError>>()?;
    Ok(Some(Arc::new(CompositeTypeDescriptor {
        type_oid: oid,
        relation_oid: reference.relation_oid,
        attributes,
    })))
}
