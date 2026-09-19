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

/// Combine the caller-selected table constraints with unique indexes visible in the supplied catalog. Ordinary inheritance does not propagate indexes; partitioned index roots apply throughout their descendant tree.
pub fn enforced_keys(
    catalog: &CatalogReadView,
    resolution: &RelationNameResolution,
    table: &str,
    constraints: Vec<TableKeyConstraint>,
) -> StorageBackendResult<Vec<EnforcedKey>> {
    let mut keys: Vec<_> = constraints.into_iter().map(EnforcedKey::from).collect();
    for index in catalog.catalog_indexes() {
        let definition = index_definition(index)?;
        if !definition.unique {
            continue;
        }
        let applies = if index.table_name == table {
            true
        } else {
            let source = catalog
                .table_resolved(resolution, &index.table_name)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            source.is_some_and(|source| source.hierarchy.partition_spec.is_some())
                && catalog
                    .hierarchy_scan_tables(resolution, &index.table_name, true)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?
                    .iter()
                    .any(|candidate| candidate == table)
        };
        if applies {
            let index_keys: Vec<IndexKey> = serde_json::from_str(&index.columns_json)?;
            keys.push(EnforcedKey {
                index: Some(index.relation.clone()),
                index_catalog: definition.catalog,
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

#[cfg(test)]
mod tests;
