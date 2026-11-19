//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selection and relation iteration from 15b534ad17f241009bcce85d1f0b0d12a97eec75; only entry-point visibility, imports and the local relation call differ.

use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{Produced, ProductionControl},
    RelationIdentity,
};
use uqa_sql::{catalog::relation_oids::RelationCatalogOids, ColumnType, SQLError};

pub(super) fn select(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<ColumnType>>, SQLError> {
    for definition in catalog.ranges() {
        control.check()?;
        for (identity, array, multi) in [
            (&definition.identity, &definition.array_name, false),
            (
                &definition.multirange.identity,
                &definition.multirange.array_name,
                true,
            ),
        ] {
            if identity.schema != schema || (identity.name != name && array != name) {
                continue;
            }
            let ty = if multi {
                definition.multirange_type_with_control(control)?
            } else {
                definition.column_type_with_control(control)?
            };
            return array_type(ty, array == name, control).map(Some);
        }
    }
    for definition in catalog.domains() {
        control.check()?;
        if definition.identity.schema != schema {
            continue;
        }
        let array = definition.array_name.as_deref().map_or_else(
            || {
                uqa_sql::catalog::array_type_names::default_array_type_name_matches(
                    &definition.identity.name,
                    name,
                )
            },
            |array| array == name,
        );
        if definition.identity.name == name || array {
            return array_type(
                definition.column_type_with_control(control)?,
                definition.identity.name != name,
                control,
            )
            .map(Some);
        }
    }
    for definition in catalog.enums() {
        control.check()?;
        if definition.identity.schema == schema
            && (definition.identity.name == name || definition.array_name == name)
        {
            return array_type(
                definition.column_type_with_control(control)?,
                definition.identity.name != name,
                control,
            )
            .map(Some);
        }
    }
    for definition in catalog.composites() {
        control.check()?;
        if definition.identity.schema == schema
            && (definition.identity.name == name || definition.array_name == name)
        {
            return array_type(
                definition.column_type_with_control(control)?,
                definition.identity.name != name,
                control,
            )
            .map(Some);
        }
    }
    by_name_with_control(catalog, schema, name, control)
}

fn array_type(
    ty: Produced<ColumnType>,
    array: bool,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    if array {
        ColumnType::array_with_control(ty, control).map_err(Into::into)
    } else {
        Ok(ty)
    }
}

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

fn by_name_with_control(
    catalog: &CatalogReadView,
    schema: &str,
    name: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Option<uqa_core::memory::Produced<ColumnType>>, SQLError> {
    for (identity, oids, array) in definitions(catalog) {
        control.check()?;
        if identity.schema != schema {
            continue;
        }
        let array = if identity.name == name {
            false
        } else if oids.array_type.is_some()
            && array.map_or_else(
                || {
                    uqa_sql::catalog::array_type_names::default_array_type_name_matches(
                        &identity.name,
                        name,
                    )
                },
                |array| array == name,
            )
        {
            true
        } else {
            continue;
        };
        let Some(reference) = oids.row_type_reference_with_control(identity, control)? else {
            return Ok(None);
        };
        let (reference, memory) = reference.into_parts();
        let ty = control.finish(ColumnType::Composite(reference), memory)?;
        return if array {
            ColumnType::array_with_control(ty, control)
                .map(Some)
                .map_err(Into::into)
        } else {
            Ok(Some(ty))
        };
    }
    Ok(None)
}
