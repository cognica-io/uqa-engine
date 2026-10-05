//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Complete legacy row-array metadata in the caller's initial catalog transaction.

use std::collections::{BTreeMap, BTreeSet};

use crate::catalog::{projection, CatalogReadView, RelationNameResolution};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::TableConstraintSet,
    catalog::{array_type_names::choose_array_type_name, relation_oids::RelationCatalogOids},
    schema::constraint_metadata::CatalogOidClass,
    SQLError,
};
use uqa_storage::{CatalogFacade, StorageBackendError, StorageBackendResult};

type RowArrays = BTreeMap<RelationIdentity, (RelationCatalogOids, String)>;

/// The caller owns the initial catalog transaction and reloads definitions after a successful upgrade. Existing relation, scalar, array and rule identities are preserved; only a missing array identity is allocated from the database's existing counter. Ordinary snapshot refreshes never call this migration.
pub fn upgrade(
    catalog: &dyn CatalogFacade,
    snapshot: &CatalogReadView,
    resolution: &RelationNameResolution,
    next_oid: &mut dyn FnMut() -> Result<u32, SQLError>,
) -> StorageBackendResult<bool> {
    let changes = prepare(snapshot, resolution, next_oid)
        .map_err(|error| StorageBackendError::backend("restore relation arrays", error))?;
    if changes.is_empty() {
        return Ok(false);
    }
    for mut schema in catalog.load_tables()? {
        let Some((oids, name)) = changes.get(&schema.relation) else {
            continue;
        };
        let mut constraints: TableConstraintSet = if schema.constraints_json.is_empty() {
            TableConstraintSet::default()
        } else {
            serde_json::from_str(&schema.constraints_json)?
        };
        constraints.catalog_oids = Some(*oids);
        constraints.row_type_array_name = Some(name.clone());
        schema.constraints_json = serde_json::to_string(&constraints)?;
        catalog.save_table(&schema)?;
    }
    for (relation, view) in snapshot.snapshot().definitions.views.iter() {
        let Some((oids, name)) = changes.get(relation) else {
            continue;
        };
        let mut view = view.clone();
        view.catalog_oids = Some(*oids);
        view.row_type_array_name = Some(name.clone());
        catalog.save_view(&crate::catalog::view::catalog_view_row(relation, &view)?)?;
    }
    for (relation, table) in snapshot.snapshot().definitions.foreign_tables.iter() {
        let Some((oids, name)) = changes.get(relation) else {
            continue;
        };
        let mut table = table.clone();
        table.catalog_oids = Some(*oids);
        table.row_type_array_name = Some(name.clone());
        let security = snapshot
            .snapshot()
            .definitions
            .foreign_table_security
            .get(relation)
            .ok_or_else(|| {
                StorageBackendError::Other("restored foreign table has no security metadata".into())
            })?;
        catalog.save_foreign_table(&table.catalog_row(relation, security)?)?;
    }
    Ok(true)
}

type RelationArray<'a> = (&'a RelationIdentity, RelationCatalogOids, Option<&'a str>);

fn relations(snapshot: &CatalogReadView) -> Vec<RelationArray<'_>> {
    let definitions = snapshot.snapshot();
    definitions
        .tables
        .iter()
        .map(|(name, table)| {
            (
                name,
                table.catalog_oids,
                table.row_type_array_name.as_deref(),
            )
        })
        .chain(definitions.definitions.views.iter().map(|(name, view)| {
            (
                name,
                view.relation_oids(),
                view.row_type_array_name.as_deref(),
            )
        }))
        .chain(
            definitions
                .definitions
                .foreign_tables
                .iter()
                .map(|(name, table)| {
                    (
                        name,
                        table.relation_oids(),
                        table.row_type_array_name.as_deref(),
                    )
                }),
        )
        .collect()
}

fn recorded_names(
    snapshot: &CatalogReadView,
    relations: &[RelationArray<'_>],
) -> Result<BTreeSet<RelationIdentity>, SQLError> {
    let mut names = relations
        .iter()
        .map(|(relation, _, _)| (*relation).clone())
        .collect::<BTreeSet<_>>();
    for (relation, oids, array_name) in relations {
        let Some(array_name) = array_name else {
            continue;
        };
        let identity = RelationIdentity::new(&relation.schema, *array_name);
        if oids.array_type.is_none()
            || !uqa_sql::catalog::array_type_names::valid_type_name(array_name)
            || !names.insert(identity.clone())
            || projection::named_type_exists(
                snapshot.domains(),
                snapshot.enums(),
                snapshot.composites(),
                &identity,
            )
        {
            return Err(SQLError::Internal(format!(
                "invalid or occupied recorded array type name for relation `{}`",
                relation.qualified_name()
            )));
        }
    }
    Ok(names)
}

/// Validate persisted names during both initial restoration and later read-only reloads, before any migration writes.
pub fn validate(snapshot: &CatalogReadView) -> StorageBackendResult<()> {
    recorded_names(snapshot, &relations(snapshot))
        .map(|_| ())
        .map_err(|error| StorageBackendError::backend("restore relation arrays", error))
}

fn prepare(
    snapshot: &CatalogReadView,
    resolution: &RelationNameResolution,
    next_oid: &mut dyn FnMut() -> Result<u32, SQLError>,
) -> Result<RowArrays, SQLError> {
    let relations = relations(snapshot);
    let mut names = recorded_names(snapshot, &relations)?;
    let mut assigned = BTreeSet::new();
    let mut changes = BTreeMap::new();
    for (relation, mut oids, recorded) in relations {
        if recorded.is_some() && oids.array_type.is_some() {
            continue;
        }
        let name = recorded.map(str::to_owned).unwrap_or_else(|| {
            choose_array_type_name(&relation.name, |name| {
                let identity = RelationIdentity::new(&relation.schema, name);
                names.contains(&identity)
                    || projection::named_type_exists(
                        snapshot.domains(),
                        snapshot.enums(),
                        snapshot.composites(),
                        &identity,
                    )
            })
        });
        names.insert(RelationIdentity::new(&relation.schema, &name));
        if oids.array_type.is_none() {
            loop {
                let oid = next_oid()?;
                if oid < crate::catalog::identity::FIRST_NORMAL_OBJECT_ID {
                    return Err(SQLError::Internal(
                        "invalid restored array type OID allocation".into(),
                    ));
                }
                if assigned.contains(&oid)
                    || projection::catalog_oid_in_use(
                        snapshot,
                        resolution,
                        CatalogOidClass::Type,
                        i64::from(oid),
                    )?
                {
                    continue;
                }
                assigned.insert(oid);
                oids.array_type = Some(oid);
                break;
            }
        }
        changes.insert(relation.clone(), (oids, name));
    }
    Ok(changes)
}

#[cfg(test)]
mod tests;
