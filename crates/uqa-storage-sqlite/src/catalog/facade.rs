//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Storage-backend facade delegation for the `SQLite` catalog.

use super::{
    Catalog, CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow,
    ForeignTableRow, GraphSnapshot, OptionalExtension, RelationIdentity, Result, SQLiteError,
    SequenceReservationResult, SequenceRow, SequenceSetValueResult, StorageBackendError,
    StorageBackendResult, TableSchema, ViewRow,
};

fn into_storage_result<T>(result: Result<T>) -> StorageBackendResult<T> {
    result.map_err(StorageBackendError::from)
}

impl CatalogFacade for Catalog {
    fn transaction_model(&self) -> uqa_storage::StorageTransactionModel {
        self.conn.transaction_model()
    }

    fn guard_graph_definition(&self, graph: Option<&str>) -> StorageBackendResult<()> {
        into_storage_result(
            self.conn
                .with_native_write(|snapshot, batch| {
                    snapshot.guard_graph_definition(batch, None, graph)
                })
                .map(|_| ()),
        )
    }
    fn transaction_affinity(&self) -> Option<uqa_storage::StorageSessionAffinity> {
        Some(self.conn.transaction_affinity())
    }

    fn clear_path_index_data(&self, index: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::clear_path_index_data(self, index))
    }

    fn save_path_index_pairs(
        &self,
        index: &str,
        sequence: &str,
        pairs: &[(u64, u64)],
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_path_index_pairs(self, index, sequence, pairs))
    }

    fn finish_path_index_data(
        &self,
        index: &str,
        graph: &str,
        definition: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::finish_path_index_data(
            self, index, graph, definition,
        ))
    }

    fn path_index_data_is_current(
        &self,
        index: &str,
        definition: &str,
    ) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::path_index_data_is_current(self, index, definition))
    }

    fn path_index_pairs(
        &self,
        index: &str,
        sequence: &str,
        after: Option<(u64, u64)>,
        limit: usize,
    ) -> StorageBackendResult<Vec<(u64, u64)>> {
        into_storage_result(Catalog::path_index_pairs(
            self, index, sequence, after, limit,
        ))
    }

    fn graph_vertex(&self, id: u64) -> StorageBackendResult<Option<uqa_storage::GraphVertexRow>> {
        into_storage_result(Catalog::graph_vertex(self, id))
    }

    fn graph_edge(&self, id: u64) -> StorageBackendResult<Option<EdgeRow>> {
        into_storage_result(Catalog::graph_edge(self, id))
    }

    fn graph_entity_ids(
        &self,
        filter: uqa_storage::GraphEntityFilter<'_>,
        after: Option<u64>,
        limit: usize,
    ) -> StorageBackendResult<Vec<u64>> {
        into_storage_result(Catalog::graph_entity_ids(self, filter, after, limit))
    }

    fn graph_entity_count(
        &self,
        filter: uqa_storage::GraphEntityFilter<'_>,
    ) -> StorageBackendResult<u64> {
        into_storage_result(Catalog::graph_entity_count(self, filter))
    }

    fn graph_entity_max_id(
        &self,
        kind: uqa_storage::GraphEntityKind,
    ) -> StorageBackendResult<Option<u64>> {
        into_storage_result(Catalog::graph_entity_max_id(self, kind))
    }

    fn graph_entity_memberships(
        &self,
        kind: uqa_storage::GraphEntityKind,
        id: u64,
    ) -> StorageBackendResult<Vec<String>> {
        into_storage_result(Catalog::graph_entity_memberships(self, kind, id))
    }

    fn graph_has_membership(
        &self,
        kind: uqa_storage::GraphEntityKind,
        id: u64,
        graph: &str,
    ) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::graph_has_membership(self, kind, id, graph))
    }

    fn initialize_storage(&self) -> StorageBackendResult<()> {
        into_storage_result(Catalog::initialize_storage(self))
    }

    fn cache_revisions(&self) -> StorageBackendResult<Option<uqa_storage::CatalogCacheRevisions>> {
        into_storage_result(Catalog::cache_revisions(self)).map(Some)
    }

    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::set_metadata(self, key, value))
    }

    fn delete_metadata(&self, key: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::delete_metadata(self, key))
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        into_storage_result(Catalog::get_metadata(self, key))
    }

    fn metadata_has_private_changes(&self, key: &str) -> StorageBackendResult<bool> {
        into_storage_result(self.native_metadata_has_private_changes(key))
    }
    fn metadata_with_prefix(&self, prefix: &str) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::metadata_with_prefix(self, prefix))
    }

    fn save_statistics_maintenance(
        &self,
        table: &str,
        state: &uqa_storage::statistics_maintenance::StatisticsMaintenance,
    ) -> StorageBackendResult<()> {
        use crate::mvcc::native::{NativeRecord, NativeRecordFamily, NativeRecordOwner};
        use rusqlite::types::ValueRef;
        use uqa_storage::mvcc::VersionError;
        let key = uqa_storage::statistics_maintenance::StatisticsMaintenance::key(table);
        let native = self.conn.with_native_write(|snapshot, batch| {
            let json = state
                .encode(&snapshot.control)
                .map_err(VersionError::into_storage_error)?;
            let record = NativeRecord::encode(
                NativeRecordFamily::Metadata,
                NativeRecordOwner::Database(snapshot.database),
                &[ValueRef::Text(key.as_bytes()), ValueRef::Text(&json)],
                &snapshot.control,
            )
            .map_err(VersionError::into_storage_error)?;
            batch
                .replace_statistics_maintenance(record.key(), record.row())
                .map_err(Into::into)
        });
        if into_storage_result(native)?.is_some() {
            return Ok(());
        }
        into_storage_result(self.set_metadata(&key, &serde_json::to_string(state)?))
    }

    fn migrate_relation_namespace(&self) -> StorageBackendResult<()> {
        if self
            .read_native(crate::mvcc::native::NativeSnapshot::validate_catalog_namespace)?
            .is_some()
        {
            return Ok(());
        }
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
                           UNION ALL
                           SELECT schema_name, relation_name, 'index' AS kind
                             FROM _catalog_indexes
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

    fn save_schema_row(
        &self,
        schema: &uqa_storage::catalog::SchemaRow,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_schema_row(self, schema))
    }

    fn drop_schema(&self, name: &str) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_schema(self, name))
    }

    fn load_schema_rows(&self) -> StorageBackendResult<Vec<uqa_storage::catalog::SchemaRow>> {
        into_storage_result(Catalog::load_schema_rows(self))
    }

    fn schema_has_private_changes(&self, name: &str) -> StorageBackendResult<bool> {
        into_storage_result(self.native_named_record_has_private_changes(
            crate::mvcc::native::NativeRecordFamily::Schemas,
            name,
        ))
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

    fn rename_sequence_row(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::rename_sequence_row(self, from, to))
    }

    fn drop_sequence_row(&self, name: &str) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::drop_sequence_row(self, name))
    }

    fn load_sequence_rows(&self) -> StorageBackendResult<Vec<SequenceRow>> {
        into_storage_result(Catalog::load_sequence_rows(self))
    }

    fn sequence_has_private_changes(
        &self,
        _relation: &RelationIdentity,
        object_id: [u8; 16],
    ) -> StorageBackendResult<bool> {
        into_storage_result(self.native_sequence_has_private_changes(object_id))
    }

    fn reserve_sequence_values(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
    ) -> StorageBackendResult<SequenceReservationResult> {
        into_storage_result(Catalog::reserve_sequence_values(
            self,
            name,
            object_id,
            definition_generation,
        ))
    }

    fn set_sequence_value(
        &self,
        name: &str,
        object_id: [u8; 16],
        definition_generation: [u8; 16],
        value: i64,
        called: bool,
        log_count: i64,
    ) -> StorageBackendResult<SequenceSetValueResult> {
        into_storage_result(Catalog::set_sequence_value(
            self,
            name,
            object_id,
            definition_generation,
            value,
            called,
            log_count,
        ))
    }

    fn save_view(&self, view: &ViewRow) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_view(self, view))
    }

    fn rename_view(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::rename_view(self, from, to))
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
    fn named_graph_exists(&self, name: &str) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::named_graph_exists(self, name))
    }

    fn load_named_graph_snapshot(&self, name: &str) -> StorageBackendResult<Option<GraphSnapshot>> {
        into_storage_result(Catalog::load_named_graph_snapshot(self, name))
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

    fn save_analyzer_revision(
        &self,
        name: &str,
        config_json: &str,
        descriptor_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_analyzer_revision(
            self,
            name,
            config_json,
            descriptor_json,
        ))
    }

    fn load_analyzer_descriptors(&self) -> StorageBackendResult<Vec<(String, String)>> {
        into_storage_result(Catalog::load_analyzer_descriptors(self))
    }

    fn replace_table_field_analyzer_binding(
        &self,
        table: &str,
        field: &str,
        phase: &str,
        name: &str,
        binding_json: &str,
    ) -> StorageBackendResult<()> {
        into_storage_result(Catalog::replace_table_field_analyzer_binding(
            self,
            table,
            field,
            phase,
            name,
            binding_json,
        ))
    }

    fn load_table_field_analyzer_bindings(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String)>> {
        into_storage_result(Catalog::load_table_field_analyzer_bindings(self))
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

    fn save_foreign_table(&self, row: &ForeignTableRow) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_foreign_table(self, row))
    }

    fn rename_foreign_table(
        &self,
        from: &RelationIdentity,
        to: &RelationIdentity,
    ) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::rename_foreign_table(self, from, to))
    }

    fn update_foreign_table_security(
        &self,
        relation: &RelationIdentity,
        security: &uqa_storage::RelationSecurityRow,
    ) -> StorageBackendResult<bool> {
        into_storage_result(Catalog::update_foreign_table_security(
            self, relation, security,
        ))
    }

    fn drop_foreign_table(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_foreign_table(self, relation))
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        into_storage_result(Catalog::load_foreign_tables(self))
    }

    fn save_catalog_index_row(&self, index: &CatalogIndexRow) -> StorageBackendResult<()> {
        into_storage_result(Catalog::save_catalog_index_row(self, index))
    }

    fn drop_catalog_index(&self, relation: &RelationIdentity) -> StorageBackendResult<()> {
        into_storage_result(Catalog::drop_catalog_index(self, relation))
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
