//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate the complete stored index set before initial-only address conversion writes.

use crate::catalog::{CatalogReadView, RelationNameResolution};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{catalog_identity::CatalogObjectIdentity, RelationIdentity};
use uqa_sql::catalog::index::IndexCatalogIdentity;
use uqa_storage::{CatalogFacade, CatalogIndexRow, StorageBackendError, StorageBackendResult};

const VERSION: &str = "sql_index_catalog_identity_version";

fn invalid(message: impl ToString) -> StorageBackendError {
    StorageBackendError::Other(message.to_string())
}

fn restore_addresses(
    storage: &dyn CatalogFacade,
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    allow_migration: bool,
) -> StorageBackendResult<Vec<CatalogIndexRow>> {
    let current = match storage.get_metadata(VERSION)?.as_deref() {
        Some("1") => true,
        None if allow_migration => false,
        _ => {
            return Err(invalid(
                "index catalog identity migration is incomplete or unsupported",
            ))
        }
    };
    let mut rows = storage.load_catalog_indexes()?;
    let (candidate, names) = candidate_catalog(catalog, &rows)?;
    let claims = if storage.get_metadata(registry::REGISTRY_VERSION)?.is_some() {
        crate::catalog::projection::relation_claims(&candidate, resolution)
    } else {
        crate::catalog::projection::legacy_relation_claims(&candidate, resolution)
    }
    .map_err(invalid)?;
    let mut occupied = claims
        .iter()
        .filter(|claim| !names.contains(&claim.relation))
        .map(|claim| claim.oid)
        .collect::<BTreeSet<_>>();
    let mut objects = claims
        .iter()
        .filter(|claim| !names.contains(&claim.relation))
        .filter_map(|claim| claim.object_id)
        .collect::<BTreeSet<_>>();
    let mut physical = BTreeSet::new();
    let mut definitions = rows
        .iter()
        .map(crate::catalog::index::index_definition)
        .collect::<StorageBackendResult<Vec<_>>>()?;
    // Current addresses are authoritative. Only new legacy conversions may choose another OID after a collision.
    for (row, definition) in rows.iter().zip(&definitions) {
        if let Some(identity) = &definition.catalog {
            let table = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
            identity
                .validate(candidate.snapshot().tables[&table].object_id)
                .map_err(invalid)?;
            if !occupied.insert(identity.identity.oid)
                || !objects.insert(identity.identity.object_id)
                || !physical.insert((identity.table_object_id, identity.physical_key.clone()))
            {
                return Err(invalid(
                    "duplicate index catalog identity or physical namespace",
                ));
            }
        } else if current {
            return Err(invalid(format!(
                "index `{}` has no catalog identity",
                row.relation.qualified_name()
            )));
        }
    }
    for (row, definition) in rows.iter_mut().zip(&mut definitions) {
        if definition.catalog.is_some() {
            continue;
        }
        let table = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
        let object_id = loop {
            let object_id =
                crate::catalog::identity::allocate_catalog_object_id("index").map_err(invalid)?;
            if objects.insert(object_id) {
                break object_id;
            }
        };
        let mut oid = claims
            .iter()
            .find(|claim| claim.relation == row.relation)
            .ok_or_else(|| invalid("stored index has no legacy catalog address"))?
            .oid;
        while !occupied.insert(oid) {
            oid = crate::catalog::identity::allocate_catalog_oid("relation").map_err(invalid)?;
        }
        let physical_key = row.relation.qualified_name();
        if !physical.insert((
            candidate.snapshot().tables[&table].object_id,
            physical_key.clone(),
        )) {
            return Err(invalid("duplicate legacy physical index namespace"));
        }
        definition.catalog = Some(IndexCatalogIdentity {
            identity: CatalogObjectIdentity { object_id, oid },
            table_object_id: candidate.snapshot().tables[&table].object_id,
            physical_key,
        });
        row.definition_json = Some(serde_json::to_string(definition)?);
    }
    Ok(rows)
}

fn candidate_catalog(
    catalog: &CatalogReadView,
    rows: &[CatalogIndexRow],
) -> StorageBackendResult<(CatalogReadView, BTreeSet<RelationIdentity>)> {
    let mut snapshot = catalog.snapshot().clone();
    let mut names = BTreeSet::new();
    for row in rows {
        let definition = crate::catalog::index::index_definition(row)?;
        if !names.insert(row.relation.clone()) {
            return Err(invalid("duplicate stored index name"));
        }
        let table = RelationIdentity::from_legacy_name(&row.table_name).map_err(invalid)?;
        if row.relation.schema != table.schema || !snapshot.tables.contains_key(&table) {
            return Err(invalid(format!(
                "index `{}` has an invalid indexed table `{}`",
                row.relation.qualified_name(),
                table.qualified_name()
            )));
        }
        if snapshot.tables.contains_key(&row.relation)
            || snapshot.definitions.views.contains_key(&row.relation)
            || snapshot
                .definitions
                .foreign_tables
                .contains_key(&row.relation)
            || snapshot.definitions.sequences.contains_key(&row.relation)
            || snapshot.tables.iter().any(|(table, state)| {
                table.schema == row.relation.schema
                    && state.keys.iter().any(|key| {
                        key.name.as_ref() == Some(&row.relation.name)
                            && (definition.relationships.owning_constraint.is_none()
                                || table.qualified_name() != row.table_name
                                || key.catalog_identity.map(|id| id.object_id)
                                    != definition.relationships.owning_constraint)
                    })
            })
        {
            return Err(invalid(format!(
                "index `{}` conflicts with another relation",
                row.relation.qualified_name()
            )));
        }
    }
    snapshot.definitions.catalog_indexes = rows
        .iter()
        .map(|row| (row.relation.clone(), row.clone()))
        .collect::<BTreeMap<_, _>>()
        .into();
    Ok((CatalogReadView::new(snapshot), names))
}

#[cfg(test)]
mod tests;

mod legacy;
mod registry;
mod tables;
pub use registry::{restore, RestoredIndexCatalog};
