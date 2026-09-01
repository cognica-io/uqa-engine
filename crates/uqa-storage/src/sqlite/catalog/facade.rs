//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage-backend facade delegation for the `SQLite` catalog.

use super::{
    Catalog, CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow,
    ForeignTableRow, GraphSnapshot, OptionalExtension, RelationIdentity, Result, SQLiteError,
    SequenceRow, StorageBackendError, StorageBackendResult, TableSchema, ViewRow,
};

fn into_storage_result<T>(result: Result<T>) -> StorageBackendResult<T> {
    result.map_err(StorageBackendError::from)
}

impl CatalogFacade for Catalog {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::set_metadata(self, key, value))
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::get_metadata(self, key))
    }

    fn fts_storage_was_reset(&self) -> bool {
        self.fts_storage_was_reset
    }

    fn migrate_relation_namespace(&self) -> StorageBackendResult<()> {
        into_storage_result(self.conn.with(|connection| {
            let foreign_key_violation = connection
                .query_row("PRAGMA foreign_key_check", [], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .optional()?;
            if let Some((table, row_id)) = foreign_key_violation {
                return Err(SQLiteError::StorageBackend(format!(
                    "relation catalog foreign-key violation in `{table}` row {row_id}"
                )));
            }
            let orphan = connection
                .query_row(
                    "SELECT r.schema_name, r.relation_name, r.kind
                       FROM _relations AS r
                       LEFT JOIN (
                           SELECT schema_name, relation_name, 'table' AS kind FROM _tables
                           UNION ALL
                           SELECT schema_name, relation_name, 'view' AS kind FROM _views
                           UNION ALL
                           SELECT schema_name, relation_name, 'sequence' AS kind FROM _sequences
                           UNION ALL
                           SELECT schema_name, relation_name, 'foreign_table' AS kind
                             FROM _foreign_tables
                       ) AS child
                         ON child.schema_name = r.schema_name
                        AND child.relation_name = r.relation_name
                        AND child.kind = r.kind
                      WHERE child.relation_name IS NULL
                      LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((schema, name, kind)) = orphan {
                return Err(SQLiteError::StorageBackend(format!(
                    "catalog relation `{schema}.{name}` has no {kind} child"
                )));
            }
            Ok(())
        }))
    }

    fn save_schema(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_schema(self, name))
    }

    fn drop_schema(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_schema(self, name))
    }

    fn load_schemas(&self) -> StorageBackendResult<Vec<String>> {
        into_storage_result(Catalog::load_schemas(self))
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_table(self, schema))
    }

    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>> {
        into_storage_result(Catalog::load_tables(self))
    }

    fn drop_table(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table(self, name))
    }

    fn drop_table_and_data(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table_and_data(self, name))
    }

    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::purge_table_data(self, name))
    }

    fn rename_table_data(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::rename_table_data(self, from, to))
    }

    fn drop_column_data(&self, table_name: &str, column_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_column_data(self, table_name, column_name))
    }

    fn rename_column_data(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::rename_column_data(self, table_name, from, to))
    }

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_model(self, name, json))
    }

    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_models(self))
    }

    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::load_model(self, name))
    }

    fn drop_model(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_model(self, name))
    }

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_scoring_params(self, name, params_json))
    }

    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::load_scoring_params(self, name))
    }

    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_all_scoring_params(self))
    }

    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_scoring_params(self, name))
    }

    fn create_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::create_sequence_row(self, sequence))
    }

    fn replace_sequence_row(&self, sequence: &SequenceRow) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::replace_sequence_row(self, sequence))
    }

    fn drop_sequence_row(&self, name: &str) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::drop_sequence_row(self, name))
    }

    fn load_sequence_rows(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        into_storage_result(Catalog::load_sequence_rows(self))
    }

    fn next_sequence_value(&self, name: &str) -> StorageBackendResult<Option<i64>> {
        into_storage_result(Catalog::next_sequence_value(self, name))
    }

    fn set_sequence_value(
        &self,
        name: &str,
        value: i64,
        called: bool,
    ) -> StorageBackendResult<Option<i64>> {
        into_storage_result(Catalog::set_sequence_value(self, name, value, called))
    }

    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_view(self, view))
    }

    fn drop_view(&self, relation: &RelationIdentity) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::drop_view(self, relation))
    }

    fn load_views(&self) -> StorageBackendResult<Vec<ViewRow>> {
        into_storage_result(Catalog::load_views(self))
    }

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_named_graph(self, name))
    }

    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_named_graph(self, name))
    }

    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>> {
        into_storage_result(Catalog::load_named_graphs(self))
    }

    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_vertex(
            self,
            vertex_id,
            label,
            properties_json,
        ))
    }

    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_vertex(self, vertex_id))
    }

    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        into_storage_result(Catalog::load_vertices(self))
    }

    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_edge(
            self,
            edge_id,
            source_id,
            target_id,
            label,
            properties_json,
        ))
    }

    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_edge(self, edge_id))
    }

    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        into_storage_result(Catalog::load_edges(self))
    }

    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_graph_membership(
            self,
            entity_type,
            entity_id,
            graph_name,
        ))
    }

    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_graph_membership(
            self,
            entity_type,
            entity_id,
            graph_name,
        ))
    }

    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_graph_membership_for_graph(self, graph_name))
    }

    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>> {
        into_storage_result(Catalog::load_graph_memberships(self))
    }

    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()> {
        into_storage_result(Catalog::purge_orphan_graph_entities(self))
    }

    fn replace_named_graph(
        &self,
        graph_name: &str,
        snapshot: &GraphSnapshot,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::replace_named_graph(self, graph_name, snapshot))
    }

    fn drop_named_graph_data(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_named_graph_data(self, graph_name))
    }

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_analyzer(self, name, config_json))
    }

    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_analyzer(self, name))
    }

    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_analyzers(self))
    }

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_table_field_analyzer(
            self,
            table_name,
            field,
            phase,
            analyzer_name,
        ))
    }

    fn replace_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::replace_table_field_analyzer(
            self,
            table_name,
            field,
            phase,
            analyzer_name,
        ))
    }

    fn drop_table_field_analyzer_field(
        &self,
        table_name: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table_field_analyzer_field(
            self, table_name, field,
        ))
    }

    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_table_field_analyzers(self, table_name))
    }

    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>> {
        into_storage_result(Catalog::load_table_field_analyzers(self))
    }

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_foreign_server(
            self,
            name,
            fdw_type,
            options_json,
        ))
    }

    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_foreign_server(self, name))
    }

    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>> {
        into_storage_result(Catalog::load_foreign_servers(self))
    }

    fn save_foreign_table(
        &self,
        relation: &RelationIdentity,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_foreign_table(
            self,
            relation,
            server_name,
            columns_json,
            options_json,
        ))
    }

    fn drop_foreign_table(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_foreign_table(self, relation))
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        into_storage_result(Catalog::load_foreign_tables(self))
    }

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_catalog_index(
            self,
            name,
            index_type,
            table_name,
            columns_json,
            parameters_json,
        ))
    }

    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_catalog_index(self, name))
    }

    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_catalog_indexes_for_table(self, table_name))
    }

    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        into_storage_result(Catalog::load_catalog_indexes(self))
    }

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_path_index(
            self,
            graph_name,
            label_sequences_json,
        ))
    }

    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_path_index(self, graph_name))
    }

    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_path_indexes(self))
    }

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_column_stats(self, stats))
    }

    fn replace_column_stats(
        &self,
        table_name: &str,
        stats: &[ColumnStatsInput<'_>],
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::replace_column_stats(self, table_name, stats))
    }

    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        into_storage_result(Catalog::load_column_stats(self, table_name))
    }

    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_column_stats(self, table_name))
    }
}
