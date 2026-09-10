//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored index semantics and the common key-enforcement boundary.

use crate::capabilities::RelationLookupMode;
use crate::{Engine, StorageBackendError, StorageBackendResult};
use uqa_sql::ast::{IndexKey, TableKeyConstraint, TableKeyConstraintKind};

pub(crate) use uqa_sql::catalog::index::IndexDefinition;

pub(crate) use uqa_execution::catalog::index::index_definition;

impl Engine {
    /// Key descriptors used by row validation, key reservations, and conflict arbitration. Standalone unique indexes remain independent catalog objects and do not create SQL constraints.
    pub(crate) fn enforced_keys(&self, table: &str) -> StorageBackendResult<Vec<EnforcedKey>> {
        let mut keys = self
            .try_key_constraints(table)?
            .into_iter()
            .map(EnforcedKey::from)
            .collect::<Vec<_>>();
        let table = self
            .try_resolve_table_name(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let catalog = self.catalog_read_view();
        let mut resolution = self.session_execution_view().relation_name_resolution();
        resolution.set_lookup_mode(RelationLookupMode::Bound);
        for index in catalog.catalog_indexes() {
            let definition = index_definition(index)?;
            if !definition.unique {
                continue;
            }
            let applies = if index.table_name == table {
                true
            } else {
                let source = catalog
                    .table_resolved(&resolution, &index.table_name)
                    .map_err(|error| StorageBackendError::Other(error.to_string()))?;
                source.is_some_and(|source| source.hierarchy.partition_spec.is_some())
                    && catalog
                        .hierarchy_scan_tables(&resolution, &index.table_name, true)
                        .map_err(|error| StorageBackendError::Other(error.to_string()))?
                        .contains(&table)
            };
            if applies {
                let index_keys: Vec<IndexKey> = serde_json::from_str(&index.columns_json)?;
                keys.push(EnforcedKey {
                    index: Some(index.relation.clone()),
                    keys: index_keys.clone(),
                    predicate: definition.predicate,
                    constraint_owned: false,
                    constraint: TableKeyConstraint {
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
}

pub(crate) use uqa_sql::catalog::index::EnforcedKey;

impl Engine {
    pub(crate) fn referenceable_keys(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<TableKeyConstraint>> {
        Ok(self
            .enforced_keys(table)?
            .into_iter()
            .filter(|key| {
                key.predicate.is_none() && key.keys.iter().all(|key| key.column().is_some())
            })
            .map(|key| key.constraint)
            .collect())
    }
}
