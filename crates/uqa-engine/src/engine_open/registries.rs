//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyzer, FDW, view, index, path-index, and FTS registry restoration.

use super::{
    normalize_analyzer_phase, BTreeMap, CatalogFacade, Engine, IVFIndexParams, StorageBackendError,
    StorageBackendResult,
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
        self.restore_roles_from_metadata(catalog)?;
        self.restore_database_security_from_metadata(catalog)?;
        self.restore_sql_functions_from_metadata(catalog, mode)?;
        self.restore_generated_routine_identities(mode)?;
        self.restore_analyzers_from_catalog(catalog)?;
        self.restore_foreign_registries_from_catalog(catalog)?;
        // Stored view plans are rebound only after every row-producing
        // relation kind is present. Legacy unqualified sources may refer to a
        // foreign table and must not be classified as missing during reopen.
        self.restore_views_from_catalog(catalog, mode)?;
        // Triggers and rules may target views, so both event registries must be
        // restored only after the complete relation namespace is available.
        self.restore_triggers_from_metadata(catalog, mode)?;
        self.restore_rules_from_metadata(catalog, mode)?;
        self.restore_catalog_indexes_from_catalog(catalog)?;
        self.restore_path_indexes_from_catalog(catalog)?;
        Ok(())
    }

    fn restore_analyzers_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, config_json) in catalog.load_analyzers()? {
            super::parse_analyzer_config(&name, &config_json)
                .map_err(StorageBackendError::Other)?;
            self.durable
                .named_analyzers
                .write()
                .insert(name, config_json);
        }
        for (table, field, phase, analyzer_name) in catalog.load_table_field_analyzers()? {
            let t = self.try_table(&table)?.ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "table-field analyzer references missing table `{table}`"
                ))
            })?;
            Self::validate_table_analyzer_field(&table, &t, &field)
                .map_err(StorageBackendError::Other)?;
            let analyzer = self
                .resolve_analyzer(&analyzer_name)
                .map_err(StorageBackendError::Other)?;
            let (phase_name, normalized_phase) =
                normalize_analyzer_phase(&phase).map_err(StorageBackendError::Other)?;
            t.inverted_index
                .write()
                .set_field_analyzer(&field, analyzer, normalized_phase)
                .map_err(StorageBackendError::Other)?;
            self.durable
                .table_field_analyzers
                .write()
                .insert((table, field), (analyzer_name, phase_name));
        }
        Ok(())
    }

    fn restore_foreign_registries_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for (name, fdw_type, options_json) in catalog.load_foreign_servers()? {
            let options: BTreeMap<String, String> = serde_json::from_str(&options_json)?;
            self.durable.foreign_servers.write().insert(
                name.clone(),
                uqa_fdw::ForeignServer {
                    name,
                    fdw_type,
                    options,
                },
            );
        }
        for row in catalog.load_foreign_tables()? {
            let relation_name = row.relation.qualified_name();
            if !self
                .durable
                .foreign_servers
                .read()
                .contains_key(&row.server_name)
            {
                return Err(StorageBackendError::Other(format!(
                    "foreign table `{}` references missing server `{}`",
                    relation_name, row.server_name
                )));
            }
            let columns: Vec<uqa_sql::ast::ColumnDef> = serde_json::from_str(&row.columns_json)?;
            let options: BTreeMap<String, String> = serde_json::from_str(&row.options_json)?;
            let fdw_columns: Vec<uqa_fdw::ColumnDef> = columns
                .iter()
                .map(|c| uqa_fdw::ColumnDef {
                    name: c.name.clone(),
                    ty: crate::engine_fdw::sql_column_type_to_fdw(&c.ty),
                })
                .collect();
            if !self.durable.roles.read().contains_key(&row.role_owner) {
                return Err(StorageBackendError::Other(format!(
                    "foreign table `{relation_name}` references missing owner role `{}`",
                    row.role_owner
                )));
            }
            let security = crate::engine_state::TableSecurity {
                role_owner: row.role_owner,
                acl: row.acl,
                column_acls: row.column_acls,
            };
            let column_names = columns
                .iter()
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            crate::engine_table_security::validate_table_security_invariants(
                &security,
                Some(&column_names),
                &self.durable.roles.read(),
            )
            .map_err(|error| {
                StorageBackendError::Other(format!(
                    "foreign table `{relation_name}` has invalid security metadata: {error}"
                ))
            })?;
            let relation = row.relation;
            let mut tables = self.durable.foreign_tables.write();
            let mut securities = self.durable.foreign_table_security.write();
            tables.insert(
                relation.clone(),
                uqa_fdw::ForeignTable {
                    name: relation_name,
                    server_name: row.server_name,
                    columns: fdw_columns,
                    options,
                },
            );
            securities.insert(relation, security);
        }
        Ok(())
    }

    fn restore_catalog_indexes_from_catalog(
        &self,
        catalog: &dyn CatalogFacade,
    ) -> StorageBackendResult<()> {
        for row in catalog.load_catalog_indexes()? {
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
            let columns: Vec<String> = serde_json::from_str(&row.columns_json)?;
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

    /// Rebuild FTS postings once after `Catalog::open` had to replace an
    /// incompatible legacy storage shape. The catalog's reset marker is tied
    /// to that open operation and intentionally must not be consulted by
    /// runtime registry reloads, where rebuilding would turn reads and
    /// rollback cleanup into writes.
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
            let idx = uqa_graph::PathIndex::build(store, graph, &label_sequences)
                .map_err(|error| StorageBackendError::Other(error.to_string()))?;
            drop(graphs);
            self.durable.path_indexes.write().insert(key, idx);
        }
        Ok(())
    }
}
