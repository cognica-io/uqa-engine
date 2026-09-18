//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzer, FDW, view, index, path-index, and FTS registry restoration.

use super::{
    BTreeMap, CatalogFacade, Engine, IVFIndexParams, StorageBackendError, StorageBackendResult,
};
use crate::{HNSWIndexParams, VectorIndexSpec};

impl Engine {
    /// Rehydrate analyzer, foreign-data, catalog-index, and path-index
    /// registries from the catalog. Apply registration side effects without
    /// writing them back, so loading remains idempotent.
    pub(super) fn restore_engine_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        mode: super::CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        self.restore_sequences_from_catalog(catalog)?;
        self.restore_domains_from_catalog(catalog)?;
        *self.durable.system_relation_security.write() =
            uqa_execution::catalog::security::system_relations::restore(
                catalog,
                &self.durable.roles.read(),
                mode.allows_migration(),
            )?;
        self.restore_database_security_from_metadata(catalog, mode.allows_migration())?;
        // Install definition-only routine placeholders before any stored expression is rebound. Final compilation waits until every row-producing relation registry is present, which also permits views and routines to bind each other without recursive catalog synchronization.
        let pending_sql_functions =
            self.install_sql_function_restore_placeholders(catalog, mode)?;
        self.restore_schema_routine_identities(mode)?;
        self.restore_analyzers_from_catalog(catalog, mode)?;
        self.restore_foreign_registries_from_catalog(catalog, mode)?;
        // Stored view plans are rebound only after every row-producing
        // relation kind is present. Legacy unqualified sources may refer to a
        // foreign table and must not be classified as missing during reopen.
        self.restore_views_from_catalog(catalog, mode)?;
        if let Some(pending) = pending_sql_functions {
            self.finalize_sql_function_restore(pending, mode)?;
        }
        // Triggers and rules may target views, so both event registries must be
        // restored only after the complete relation namespace is available.
        self.event_restore_context()
            .restore_triggers_from_metadata(catalog, mode.allows_migration())?;
        self.event_restore_context()
            .restore_rules_from_metadata(catalog, mode.allows_migration())?;
        self.restore_catalog_indexes_from_catalog(catalog)?;
        self.restore_path_indexes_from_catalog(catalog)?;
        Ok(())
    }

    fn restore_foreign_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
        mode: super::CatalogRestoreMode,
    ) -> StorageBackendResult<()> {
        let restored = uqa_execution::catalog::foreign::restoration::restore(
            &uqa_execution::catalog::foreign::restoration::ForeignRestoreContext {
                schema: self.foreign_schema_context(),
                sequences: self.sequence_owner_publication_context(),
                roles: self,
            },
            catalog,
            mode.allows_migration(),
        )?;
        *self.durable.foreign_servers.write() = restored.servers;
        *self.durable.foreign_tables.write() = restored.tables;
        *self.durable.foreign_table_security.write() = restored.security;
        Ok(())
    }

    fn restore_catalog_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for row in catalog.load_catalog_indexes()? {
            crate::catalog_indexes::index_definition(&row)?;
            let table = crate::RelationIdentity::from_legacy_name(&row.table_name)
                .map_err(StorageBackendError::Other)?;
            if row.relation.schema != table.schema {
                return Err(StorageBackendError::Other(format!(
                    "catalog index `{}` belongs to schema `{}` but references table `{}` in schema `{}`",
                    row.relation.qualified_name(),
                    row.relation.schema,
                    row.table_name,
                    table.schema
                )));
            }
            if !self.storage.tables.read().contains_key(&table) {
                return Err(StorageBackendError::Other(format!(
                    "catalog index `{}` references missing table `{}`",
                    row.relation.qualified_name(),
                    row.table_name
                )));
            }
            let conflicting_kind = if self.storage.tables.read().contains_key(&row.relation) {
                Some("table")
            } else if self.durable.views.read().contains_key(&row.relation) {
                Some("view")
            } else if self.durable.sequences.read().contains_key(&row.relation) {
                Some("sequence")
            } else if self
                .durable
                .foreign_tables
                .read()
                .contains_key(&row.relation)
            {
                Some("foreign table")
            } else {
                None
            };
            if let Some(kind) = conflicting_kind {
                return Err(StorageBackendError::Other(format!(
                    "catalog index `{}` conflicts with existing {kind}",
                    row.relation.qualified_name()
                )));
            }
            self.durable
                .catalog_indexes
                .write()
                .insert(row.relation.clone(), row.clone());
            let keys: Vec<uqa_sql::ast::IndexKey> = serde_json::from_str(&row.columns_json)?;
            let columns = keys
                .iter()
                .filter_map(uqa_sql::ast::IndexKey::column)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let parameters: BTreeMap<String, String> = serde_json::from_str(&row.parameters_json)?;
            if row.index_type.eq_ignore_ascii_case("gin") {
                let analyzer = parameters
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("analyzer"))
                    .map(|(_, v)| v.as_str());
                for col in &columns {
                    self.restore_fts_field_from_catalog(&row.table_name, col, analyzer)
                        .map_err(StorageBackendError::Other)?;
                }
            } else if row.index_type.eq_ignore_ascii_case("ivf")
                || row.index_type.eq_ignore_ascii_case("hnsw")
            {
                let spec = if row.index_type.eq_ignore_ascii_case("ivf") {
                    VectorIndexSpec::IVF(IVFIndexParams::from_catalog_map(&parameters)?)
                } else {
                    VectorIndexSpec::HNSW(HNSWIndexParams::from_catalog_map(&parameters)?)
                };
                for col in &columns {
                    let Some(
                        uqa_sql::ast::ColumnType::Vector(dim)
                        | uqa_sql::ast::ColumnType::Tensor(dim),
                    ) = self.column_type(&row.table_name, col)?
                    else {
                        return Err(StorageBackendError::Other(format!(
                            "vector index `{}` references missing or non-vector column `{}`.`{col}`",
                            row.relation.qualified_name(),
                            row.table_name
                        )));
                    };
                    if !self.restore_vector_field_index(&row.table_name, col, dim, spec)? {
                        return Err(StorageBackendError::Other(format!(
                            "failed to restore vector index `{}` for table `{}`",
                            row.relation.qualified_name(),
                            row.table_name
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Rebuild FTS postings after catalog initialization replaces an incompatible legacy storage shape, inside the initial restore transaction. Runtime registry reloads must not consult this marker, because rebuilding would turn reads and rollback cleanup into writes.
    pub(super) fn repair_reset_fts_storage(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        if !catalog.fts_storage_was_reset() {
            return Ok(());
        }
        let tables = self
            .durable
            .catalog_indexes
            .read()
            .values()
            .filter(|row| row.index_type.eq_ignore_ascii_case("gin"))
            .map(|row| row.table_name.clone())
            .collect::<std::collections::BTreeSet<_>>();
        for table_name in tables {
            let table = self.try_table(&table_name)?.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "GIN catalog repair references missing table `{table_name}`"
                ))
            })?;
            Self::rebuild_fts_index(&table).map_err(StorageBackendError::Other)?;
        }
        Ok(())
    }

    fn restore_path_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (key, seq_json) in catalog.load_path_indexes()? {
            let label_sequences: Vec<Vec<String>> = serde_json::from_str(&seq_json)?;
            let (graph, name) = key.split_once("::").ok_or_else(|| {
                StorageBackendError::Other(format!("invalid path-index key `{key}`"))
            })?;
            if graph.is_empty() || name.is_empty() {
                return Err(StorageBackendError::Other(format!(
                    "invalid path-index key `{key}`"
                )));
            }
            let graphs = self.durable.graphs.read();
            let store = graphs.get(graph).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "path index `{key}` references missing graph `{graph}`"
                ))
            })?;
            let _ = store;
            let idx = self.bind_path_index_definition(&key, graph, &label_sequences)?;
            drop(graphs);
            self.durable.path_indexes.write().insert(key, idx);
        }
        Ok(())
    }

    pub(super) fn refresh_changed_graph_path_indexes(
        &self,
        catalog: &dyn CatalogFacade,
        changed: &std::collections::BTreeSet<String>,
    ) -> StorageBackendResult<()> {
        if changed.is_empty() {
            return Ok(());
        }
        self.durable.path_indexes.write().retain(|key, _| {
            key.split_once("::")
                .is_none_or(|(graph, _)| !changed.contains(graph))
        });
        for (key, json) in catalog.load_path_indexes()? {
            let Some((graph, _)) = key.split_once("::") else {
                continue;
            };
            if !changed.contains(graph) {
                continue;
            }
            let sequences: Vec<Vec<String>> = serde_json::from_str(&json)?;
            let graphs = self.durable.graphs.read();
            let store = graphs.get(graph).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "path index `{key}` references missing graph `{graph}`"
                ))
            })?;
            let _ = store;
            let index = self.bind_path_index_definition(&key, graph, &sequences)?;
            self.durable.path_indexes.write().insert(key, index);
        }
        Ok(())
    }
}
