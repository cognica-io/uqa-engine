//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select standalone and partition index keys from the caller's retained catalog view.

use super::index_definition;
use crate::catalog::{CatalogReadView, RelationNameResolution};
use uqa_sql::{
    ast::{IndexKey, TableKeyConstraint, TableKeyConstraintKind},
    catalog::index::EnforcedKey,
};
use uqa_storage::{StorageBackendError, StorageBackendResult};

/// Combine local declared keys and local unique-index rows. Resolve only a selected partition index's ancestor chain for parent conflict arbiters.
pub fn enforced_keys(
    catalog: &CatalogReadView,
    _resolution: &RelationNameResolution,
    table: &str,
    constraints: Vec<TableKeyConstraint>,
) -> StorageBackendResult<Vec<EnforcedKey>> {
    let mut keys: Vec<_> = constraints.into_iter().map(EnforcedKey::from).collect();
    for index in catalog.catalog_indexes() {
        if index.table_name != table {
            continue;
        }
        let definition = index_definition(index)?;
        if !definition.unique {
            continue;
        }
        if let Some(owner) = definition.relationships.owning_constraint {
            let key = keys
                .iter_mut()
                .find(|key| {
                    key.constraint_owned
                        && key
                            .constraint
                            .catalog_identity
                            .is_some_and(|identity| identity.object_id == owner)
                })
                .ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "index `{}` has no declared owning constraint",
                        index.relation.qualified_name()
                    ))
                })?;
            if key.index.is_some() {
                return Err(StorageBackendError::Other(
                    "constraint owns more than one index".into(),
                ));
            }
            key.index = Some(index.relation.clone());
            key.index_ancestors = ancestors(catalog, table, definition.relationships.parent_index)?;
            key.index_catalog = definition.catalog;
            continue;
        }
        {
            let index_keys: Vec<IndexKey> = serde_json::from_str(&index.columns_json)?;
            keys.push(EnforcedKey {
                index: Some(index.relation.clone()),
                index_catalog: definition.catalog,
                index_ancestors: ancestors(catalog, table, definition.relationships.parent_index)?,
                keys: index_keys.clone(),
                predicate: definition.predicate,
                constraint_owned: false,
                constraint: TableKeyConstraint {
                    catalog_identity: None,
                    name: Some(index.relation.name.clone()),
                    kind: TableKeyConstraintKind::Unique,
                    columns: index_keys
                        .iter()
                        .filter_map(IndexKey::column)
                        .map(str::to_owned)
                        .collect(),
                    nulls_not_distinct: definition.nulls_not_distinct,
                    without_overlaps: false,
                },
            });
        }
    }
    Ok(keys)
}

fn ancestors<'a>(
    catalog: &'a CatalogReadView,
    table: &'a str,
    mut parent: Option<[u8; 16]>,
) -> StorageBackendResult<Vec<[u8; 16]>> {
    let mut result = Vec::new();
    let mut table = table;
    while let Some(id) = parent {
        if result.contains(&id) {
            return Err(StorageBackendError::Other("cyclic index ancestry".into()));
        }
        result.push(id);
        let relation = uqa_core::RelationIdentity::from_legacy_name(table)
            .map_err(StorageBackendError::Other)?;
        let parent_table = catalog
            .snapshot()
            .tables
            .get(&relation)
            .filter(|table| table.hierarchy.is_partition())
            .and_then(|table| table.hierarchy.parents.first())
            .ok_or_else(|| StorageBackendError::Other("missing index parent table".into()))?;
        let mut found = false;
        for row in catalog
            .catalog_indexes()
            .filter(|row| row.table_name == *parent_table)
        {
            let definition = index_definition(row)?;
            if definition
                .catalog
                .as_ref()
                .is_some_and(|identity| identity.identity.object_id == id)
            {
                parent = definition.relationships.parent_index;
                table = &row.table_name;
                found = true;
                break;
            }
        }
        if !found {
            return Err(StorageBackendError::Other("missing index ancestor".into()));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
