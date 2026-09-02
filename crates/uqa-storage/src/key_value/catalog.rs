//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog facade implementation for key/value-backed persistence.

use std::collections::BTreeSet;
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    GraphSnapshot, RelationIdentity, RelationKind, SchemaRow, SequenceOptions,
    SequenceReservationResult, SequenceRow, TableSchema, ViewRow,
};
use crate::{StorageBackendError, StorageBackendResult};

use super::codec::{
    decode_document_value, decode_string, decode_value, doc_length_key, doc_length_key_prefix,
    document_key_prefix, encode_document_value, encode_value, field_stats_key,
    field_stats_key_prefix, key_with_tag, posting_cluster_positions_field_prefix,
    posting_cluster_positions_key_prefix, posting_cluster_score_field_prefix,
    posting_cluster_score_key_prefix, posting_document_key, posting_document_key_prefix,
    posting_field_prefix, posting_key_prefix, push_str, push_u64, read_str, read_u64,
    reverse_posting_key, reverse_posting_key_prefix, single_str_key, string_value,
    vector_field_prefix, vector_key_prefix,
};
use super::{
    KeyValueBatch, KeyValueStore, TAG_ANALYZER, TAG_CATALOG_INDEX, TAG_COLUMN_STATS, TAG_EDGE,
    TAG_FOREIGN_SERVER, TAG_FOREIGN_TABLE, TAG_GRAPH_MEMBERSHIP, TAG_METADATA, TAG_MODEL,
    TAG_NAMED_GRAPH, TAG_PATH_INDEX, TAG_RELATION, TAG_SCHEMA, TAG_SCORING_PARAMS, TAG_SEQUENCE,
    TAG_TABLE, TAG_TABLE_FIELD_ANALYZER, TAG_VERTEX, TAG_VIEW,
};

mod analyzers;
mod foreign;
mod graphs;
mod indexes;
mod keys;
mod migration;
mod models;
mod physical_indexes;
mod records;
mod relations;
mod schema_table;
mod sequences;
mod views;

use keys::{
    batch_put_or_keep_existing, batch_rekey_prefix, batch_rekey_prefix_or_keep_existing,
    catalog_index_references_column, catalog_index_rename_column, column_stats_key,
    column_stats_prefix, decode_catalog_relation_key, decode_relation_key, edge_key,
    ensure_prefix_absent, graph_membership_graph_prefix, graph_membership_key,
    graph_membership_prefix, load_single_keys, load_single_string_rows,
    register_migration_relation, relation_key, table_field_analyzer_field_prefix,
    table_field_analyzer_key, table_field_analyzer_prefix, vertex_key,
};
use migration::{
    apply_relation_migrations, collect_relation_migrations, validate_relation_parents,
};
use records::{
    StoredCatalogIndex, StoredColumnStats, StoredEdge, StoredForeignServer, StoredForeignTable,
    StoredRelation, StoredSequence, StoredVertex, StoredView,
};

#[derive(Clone)]
pub struct KeyValueCatalog {
    store: Arc<dyn KeyValueStore>,
    sequence_lock: Arc<Mutex<()>>,
}

impl KeyValueCatalog {
    pub fn new(store: Arc<dyn KeyValueStore>) -> Self {
        Self {
            store,
            sequence_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn store(&self) -> Arc<dyn KeyValueStore> {
        Arc::clone(&self.store)
    }
}

impl CatalogFacade for KeyValueCatalog {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        self.set_metadata_impl(key, value)
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        self.get_metadata_impl(key)
    }

    fn migrate_relation_namespace(&self) -> StorageBackendResult<()> {
        self.migrate_relation_namespace_impl()
    }

    fn save_schema_row(&self, schema: &SchemaRow) -> StorageBackendResult<()> {
        self.save_schema_row_impl(schema)
    }

    fn drop_schema(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_schema_impl(name)
    }

    fn load_schema_rows(&self) -> StorageBackendResult<Vec<SchemaRow>> {
        self.load_schema_rows_impl()
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        self.save_table_impl(schema)
    }

    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>> {
        self.load_tables_impl()
    }

    fn drop_table(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_table_impl(name)
    }

    fn drop_table_and_data(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_table_and_data_impl(name)
    }

    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()> {
        self.purge_table_data_impl(name)
    }

    fn rename_table_data(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        self.rename_table_data_impl(from, to)
    }

    fn drop_column_data(&self, table_name: &str, column_name: &str) -> StorageBackendResult<()> {
        self.drop_column_data_impl(table_name, column_name)
    }

    fn rename_column_data(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        self.rename_column_data_impl(table_name, from, to)
    }

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        self.save_model_impl(name, json)
    }

    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>> {
        self.load_models_impl()
    }

    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.load_model_impl(name)
    }

    fn drop_model(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_model_impl(name)
    }

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()> {
        self.save_scoring_params_impl(name, params_json)
    }

    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.load_scoring_params_impl(name)
    }

    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>> {
        self.load_all_scoring_params_impl()
    }

    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_scoring_params_impl(name)
    }

    fn create_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        self.create_sequence_row_impl(sequence)
    }

    fn replace_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        self.replace_sequence_row_impl(sequence)
    }

    fn rename_sequence_row(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.rename_sequence_row_impl(from, to)
    }

    fn drop_sequence_row(&self, name: &str) -> StorageBackendResult<bool> {
        self.drop_sequence_row_impl(name)
    }

    fn load_sequence_rows(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        self.load_sequence_rows_impl()
    }

    fn reserve_sequence_values(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> StorageBackendResult<SequenceReservationResult> {
        self.reserve_sequence_values_impl(name, object_id, definition_generation)
    }

    fn set_sequence_value(
        &self,
        name: &str,
        object_id: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> StorageBackendResult<Option<i64>> {
        self.set_sequence_value_impl(name, object_id, value, called, log_count)
    }

    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()> {
        self.save_view_impl(view)
    }

    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<bool> {
        self.drop_view_impl(relation)
    }

    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>> {
        self.load_views_impl()
    }

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        self.save_named_graph_impl(name)
    }

    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_named_graph_impl(name)
    }

    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>> {
        self.load_named_graphs_impl()
    }

    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        self.save_vertex_impl(vertex_id, label, properties_json)
    }

    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()> {
        self.delete_vertex_impl(vertex_id)
    }

    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        self.load_vertices_impl()
    }

    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        self.save_edge_impl(edge_id, source_id, target_id, label, properties_json)
    }

    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()> {
        self.delete_edge_impl(edge_id)
    }

    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        self.load_edges_impl()
    }

    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.save_graph_membership_impl(entity_type, entity_id, graph_name)
    }

    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.delete_graph_membership_impl(entity_type, entity_id, graph_name)
    }

    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.delete_graph_membership_for_graph_impl(graph_name)
    }

    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>> {
        self.load_graph_memberships_impl()
    }

    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()> {
        self.purge_orphan_graph_entities_impl()
    }

    fn replace_named_graph(
        &self,
        graph_name: &str,
        snapshot: &GraphSnapshot,
    ) -> StorageBackendResult<()> {
        self.replace_named_graph_impl(graph_name, snapshot)
    }

    fn drop_named_graph_data(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.drop_named_graph_data_impl(graph_name)
    }

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()> {
        self.save_analyzer_impl(name, config_json)
    }

    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_analyzer_impl(name)
    }

    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>> {
        self.load_analyzers_impl()
    }

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        self.save_table_field_analyzer_impl(table_name, field, phase, analyzer_name)
    }

    fn replace_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        self.replace_table_field_analyzer_impl(table_name, field, phase, analyzer_name)
    }

    fn drop_table_field_analyzer_field(
        &self,
        table_name: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        self.drop_table_field_analyzer_field_impl(table_name, field)
    }

    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()> {
        self.drop_table_field_analyzers_impl(table_name)
    }

    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>> {
        self.load_table_field_analyzers_impl()
    }

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.save_foreign_server_impl(name, fdw_type, options_json)
    }

    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()> {
        self.drop_foreign_server_impl(name)
    }

    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>> {
        self.load_foreign_servers_impl()
    }

    fn save_foreign_table(
        &self,
        relation: &RelationIdentity,
        role_owner: &str,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.save_foreign_table_impl(
            relation,
            role_owner,
            server_name,
            columns_json,
            options_json,
        )
    }

    fn update_foreign_table_role_owner(
        &self,
        relation: &RelationIdentity,
        role_owner: &str,
    ) -> StorageBackendResult<bool> {
        self.update_foreign_table_role_owner_impl(relation, role_owner)
    }

    fn drop_foreign_table(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        self.drop_foreign_table_impl(relation)
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        self.load_foreign_tables_impl()
    }

    fn save_catalog_index(
        &self,
        relation: &RelationIdentity,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        self.save_catalog_index_impl(
            relation,
            index_type,
            table_name,
            columns_json,
            parameters_json,
        )
    }

    fn drop_catalog_index(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        self.drop_catalog_index_impl(relation)
    }

    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()> {
        self.drop_catalog_indexes_for_table_impl(table_name)
    }

    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        self.load_catalog_indexes_impl()
    }

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        self.save_path_index_impl(graph_name, label_sequences_json)
    }

    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.drop_path_index_impl(graph_name)
    }

    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>> {
        self.load_path_indexes_impl()
    }

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()> {
        self.save_column_stats_impl(stats)
    }

    fn replace_column_stats(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> StorageBackendResult<()> {
        self.replace_column_stats_impl(table_name, stats)
    }

    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        self.load_column_stats_impl(table_name)
    }

    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()> {
        self.delete_column_stats_impl(table_name)
    }
}

#[cfg(test)]
mod tests;
