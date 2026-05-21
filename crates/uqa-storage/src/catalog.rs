//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Backend-neutral persistent catalog facade.
//!
//! The engine depends on this trait for table metadata, analyzers, models,
//! graph registries, and planner statistics. Concrete storage layers such as
//! `SQLite` or a future RocksDB-backed catalog implement it behind the same
//! object-safe boundary.

use serde::{Deserialize, Serialize};

use crate::backend::StorageBackendResult;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableSchema {
    pub name: String,
    pub analyzer_json: String,
    pub fts_fields: Vec<String>,
    pub vector_fields: Vec<VectorFieldSchema>,
    /// Serialized `Vec<uqa_sql::ast::ColumnDef>` capturing the schema
    /// columns (name, type, `auto_increment`, flags). Empty for
    /// tables created by the legacy code path before column tracking
    /// existed.
    #[serde(default)]
    pub columns_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VectorFieldSchema {
    pub field: String,
    pub dimensions: u32,
}

/// One row from graph edge persistence. Mirrors the canonical UQA implementation's
/// `(edge_id, source_id, target_id, label, properties_json)` tuple
/// but as a typed struct so the catalog API stays clippy-clean.
#[derive(Debug, Clone)]
pub struct EdgeRow {
    pub edge_id: u64,
    pub source_id: u64,
    pub target_id: u64,
    pub label: String,
    pub properties_json: String,
}

/// One row from the foreign-table registry.
#[derive(Debug, Clone)]
pub struct ForeignTableRow {
    pub name: String,
    pub server_name: String,
    pub columns_json: String,
    pub options_json: String,
}

/// One row from the secondary-index registry.
#[derive(Debug, Clone)]
pub struct CatalogIndexRow {
    pub name: String,
    pub index_type: String,
    pub table_name: String,
    pub columns_json: String,
    pub parameters_json: String,
}

/// Values persisted into one column-statistics row.
#[derive(Debug, Clone, Copy)]
pub struct ColumnStatsInput<'a> {
    pub table_name: &'a str,
    pub column_name: &'a str,
    pub distinct_count: i64,
    pub null_count: i64,
    pub min_value: Option<&'a str>,
    pub max_value: Option<&'a str>,
    pub row_count: i64,
    pub histogram_json: &'a str,
    pub mcv_values_json: &'a str,
    pub mcv_frequencies_json: &'a str,
}

impl<'a> ColumnStatsInput<'a> {
    pub fn basic(
        table_name: &'a str,
        column_name: &'a str,
        distinct_count: i64,
        null_count: i64,
        min_value: Option<&'a str>,
        max_value: Option<&'a str>,
        row_count: i64,
    ) -> Self {
        Self {
            table_name,
            column_name,
            distinct_count,
            null_count,
            min_value,
            max_value,
            row_count,
            histogram_json: "[]",
            mcv_values_json: "[]",
            mcv_frequencies_json: "[]",
        }
    }
}

/// One row from persisted column statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnStatsRow {
    pub column_name: String,
    pub distinct_count: i64,
    pub null_count: i64,
    pub min_value: Option<String>,
    pub max_value: Option<String>,
    pub row_count: i64,
    pub histogram_json: String,
    pub mcv_values_json: String,
    pub mcv_frequencies_json: String,
}

/// Engine-facing catalog facade for persistent metadata.
pub trait CatalogFacade: Send + Sync {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()>;
    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>>;
    fn fts_storage_was_reset(&self) -> bool {
        false
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()>;
    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>>;
    fn drop_table(&self, name: &str) -> StorageBackendResult<()>;
    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()>;

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()>;
    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>>;
    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>>;
    fn drop_model(&self, name: &str) -> StorageBackendResult<()>;

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()>;
    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>>;
    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>>;
    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()>;

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()>;
    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()>;
    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>>;
    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()>;
    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()>;
    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>>;
    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()>;
    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()>;
    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>>;
    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()>;
    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()>;
    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()>;
    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>>;
    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()>;

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()>;
    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()>;
    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>>;

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()>;
    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()>;
    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>>;

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()>;
    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>>;

    fn save_foreign_table(
        &self,
        name: &str,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_foreign_table(&self, name: &str) -> StorageBackendResult<()>;
    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>>;

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()>;
    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()>;
    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>>;

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()>;
    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()>;
    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>>;

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()>;
    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>>;
    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()>;
}
