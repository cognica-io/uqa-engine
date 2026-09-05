//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored index semantics and the common key-enforcement boundary.

use crate::engine_capabilities::RelationLookupMode;
use crate::{CatalogIndexRow, Engine, StorageBackendError, StorageBackendResult};
use uqa_sql::ast::{Expr, IndexKey, TableKeyConstraint, TableKeyConstraintKind};

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IndexDefinition {
    #[serde(default)]
    pub(crate) included_columns: Vec<String>,
    #[serde(default)]
    pub(crate) column_order: Vec<uqa_sql::ast::IndexColumnOrder>,
    #[serde(default)]
    pub(crate) key_names: Vec<String>,
    #[serde(default)]
    pub(crate) key_types: Vec<uqa_sql::ast::ColumnType>,
    #[serde(default)]
    pub(crate) predicate: Option<Box<Expr>>,
    pub(crate) unique: bool,
    pub(crate) nulls_not_distinct: bool,
}

pub(crate) fn index_definition(index: &CatalogIndexRow) -> StorageBackendResult<IndexDefinition> {
    index.definition_json.as_deref().map_or_else(
        || Ok(IndexDefinition::default()),
        |definition| serde_json::from_str(definition).map_err(StorageBackendError::from),
    )
}

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

/// Runtime key enforcement keeps index predicates separate from SQL constraints.
#[derive(Debug, Clone)]
pub(crate) struct EnforcedKey {
    pub(crate) constraint: TableKeyConstraint,
    pub(crate) keys: Vec<IndexKey>,
    pub(crate) index: Option<crate::RelationIdentity>,
    pub(crate) predicate: Option<Box<Expr>>,
    pub(crate) constraint_owned: bool,
}

impl std::ops::Deref for EnforcedKey {
    type Target = TableKeyConstraint;

    fn deref(&self) -> &Self::Target {
        &self.constraint
    }
}

impl From<TableKeyConstraint> for EnforcedKey {
    fn from(constraint: TableKeyConstraint) -> Self {
        Self {
            keys: constraint
                .columns
                .iter()
                .cloned()
                .map(IndexKey::Column)
                .collect(),
            index: None,
            constraint,
            predicate: None,
            constraint_owned: true,
        }
    }
}

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
