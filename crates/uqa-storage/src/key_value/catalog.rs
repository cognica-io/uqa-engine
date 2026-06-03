//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog facade implementation for key/value-backed persistence.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::catalog::{
    CatalogFacade, CatalogIndexRow, ColumnStatsInput, ColumnStatsRow, EdgeRow, ForeignTableRow,
    TableSchema,
};
use crate::document_store::Document;
use crate::StorageBackendResult;

use super::{
    decode_string, decode_value, doc_length_key, doc_length_key_prefix, document_key_prefix,
    encode_value, field_stats_key, field_stats_key_prefix, key_with_tag, posting_field_prefix,
    posting_key_prefix, push_str, push_u64, read_str, read_u64, reverse_posting_key,
    reverse_posting_key_prefix, single_str_key, string_value, vector_field_prefix,
    vector_key_prefix, KeyValueBatch, KeyValueStore, TAG_ANALYZER, TAG_CATALOG_INDEX,
    TAG_COLUMN_STATS, TAG_EDGE, TAG_FOREIGN_SERVER, TAG_FOREIGN_TABLE, TAG_GRAPH_MEMBERSHIP,
    TAG_METADATA, TAG_MODEL, TAG_NAMED_GRAPH, TAG_PATH_INDEX, TAG_SCORING_PARAMS, TAG_TABLE,
    TAG_TABLE_FIELD_ANALYZER, TAG_VERTEX,
};

#[derive(Debug, Serialize, Deserialize)]
struct StoredVertex {
    label: String,
    properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredEdge {
    source_id: u64,
    target_id: u64,
    label: String,
    properties_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredForeignServer {
    fdw_type: String,
    options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredForeignTable {
    server_name: String,
    columns_json: String,
    options_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredCatalogIndex {
    index_type: String,
    table_name: String,
    columns_json: String,
    parameters_json: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StoredColumnStats {
    distinct_count: i64,
    null_count: i64,
    min_value: Option<String>,
    max_value: Option<String>,
    row_count: i64,
    histogram_json: String,
    mcv_values_json: String,
    mcv_frequencies_json: String,
}

fn graph_membership_prefix() -> Vec<u8> {
    key_with_tag(TAG_GRAPH_MEMBERSHIP)
}

fn graph_membership_graph_prefix(graph_name: &str) -> Vec<u8> {
    let mut key = graph_membership_prefix();
    push_str(&mut key, graph_name);
    key
}

fn graph_membership_key(entity_type: &str, entity_id: u64, graph_name: &str) -> Vec<u8> {
    let mut key = graph_membership_graph_prefix(graph_name);
    push_str(&mut key, entity_type);
    push_u64(&mut key, entity_id);
    key
}

fn table_field_analyzer_prefix(table_name: &str) -> Vec<u8> {
    let mut key = key_with_tag(TAG_TABLE_FIELD_ANALYZER);
    push_str(&mut key, table_name);
    key
}

fn table_field_analyzer_key(table_name: &str, field: &str, phase: &str) -> Vec<u8> {
    let mut key = table_field_analyzer_field_prefix(table_name, field);
    push_str(&mut key, phase);
    key
}

fn table_field_analyzer_field_prefix(table_name: &str, field: &str) -> Vec<u8> {
    let mut key = table_field_analyzer_prefix(table_name);
    push_str(&mut key, field);
    key
}

fn column_stats_prefix(table_name: &str) -> Vec<u8> {
    let mut key = key_with_tag(TAG_COLUMN_STATS);
    push_str(&mut key, table_name);
    key
}

fn column_stats_key(table_name: &str, column_name: &str) -> Vec<u8> {
    let mut key = column_stats_prefix(table_name);
    push_str(&mut key, column_name);
    key
}

/// Catalog facade implemented over [`KeyValueStore`].
#[derive(Clone)]
pub struct KeyValueCatalog {
    store: Arc<dyn KeyValueStore>,
}

fn batch_rekey_prefix(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    old_prefix: &[u8],
    new_prefix: &[u8],
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(old_prefix)? {
        let mut new_key = new_prefix.to_vec();
        new_key.extend_from_slice(&key[old_prefix.len()..]);
        batch.put(&new_key, &value)?;
        batch.delete(&key)?;
    }
    Ok(())
}

fn batch_rekey_prefix_or_keep_existing(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    old_prefix: &[u8],
    new_prefix: &[u8],
) -> StorageBackendResult<()> {
    for (key, value) in store.scan_prefix(old_prefix)? {
        let mut new_key = new_prefix.to_vec();
        new_key.extend_from_slice(&key[old_prefix.len()..]);
        if store.get(&new_key)?.is_none() {
            batch.put(&new_key, &value)?;
        }
        batch.delete(&key)?;
    }
    Ok(())
}

fn batch_put_or_keep_existing(
    store: &dyn KeyValueStore,
    batch: &mut dyn KeyValueBatch,
    key: &[u8],
    value: &[u8],
) -> StorageBackendResult<()> {
    if store.get(key)?.is_none() {
        batch.put(key, value)?;
    }
    Ok(())
}

fn catalog_index_references_column(row: &CatalogIndexRow, column_name: &str) -> bool {
    serde_json::from_str::<Vec<String>>(&row.columns_json).is_ok_and(|columns| {
        columns
            .iter()
            .any(|column| column.eq_ignore_ascii_case(column_name))
    })
}

fn catalog_index_rename_column(row: &CatalogIndexRow, from: &str, to: &str) -> Option<String> {
    let mut columns = serde_json::from_str::<Vec<String>>(&row.columns_json).ok()?;
    let mut changed = false;
    for column in &mut columns {
        if column.eq_ignore_ascii_case(from) {
            *column = to.to_string();
            changed = true;
        }
    }
    changed
        .then(|| serde_json::to_string(&columns).ok())
        .flatten()
}

impl KeyValueCatalog {
    pub fn new(store: Arc<dyn KeyValueStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> Arc<dyn KeyValueStore> {
        Arc::clone(&self.store)
    }
}

impl CatalogFacade for KeyValueCatalog {
    fn set_metadata(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_METADATA, key), &string_value(value))
    }

    fn get_metadata(&self, key: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_METADATA, key))?
            .map(decode_string)
            .transpose()
    }

    fn save_table(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_TABLE, &schema.name),
            &encode_value(schema)?,
        )
    }

    fn load_tables(&self) -> StorageBackendResult<Vec<TableSchema>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_TABLE))?
            .into_iter()
            .map(|(_, value)| decode_value::<TableSchema>(&value))
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    fn drop_table(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_TABLE, name))
    }

    fn purge_table_data(&self, name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&document_key_prefix(name))?;
        batch.delete_prefix(&posting_key_prefix(name))?;
        batch.delete_prefix(&doc_length_key_prefix(name))?;
        batch.delete_prefix(&field_stats_key_prefix(name))?;
        batch.delete_prefix(&reverse_posting_key_prefix(name))?;
        batch.delete_prefix(&vector_key_prefix(name))?;
        batch.delete_prefix(&column_stats_prefix(name))?;
        batch.commit()
    }

    fn rename_table_data(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        if let Some(value) = self.store.get(&single_str_key(TAG_TABLE, from))? {
            let mut schema = decode_value::<TableSchema>(&value)?;
            schema.name = to.to_string();
            batch.put(&single_str_key(TAG_TABLE, to), &encode_value(&schema)?)?;
            batch.delete(&single_str_key(TAG_TABLE, from))?;
        }
        for (old_prefix, new_prefix) in [
            (document_key_prefix(from), document_key_prefix(to)),
            (posting_key_prefix(from), posting_key_prefix(to)),
            (doc_length_key_prefix(from), doc_length_key_prefix(to)),
            (field_stats_key_prefix(from), field_stats_key_prefix(to)),
            (
                reverse_posting_key_prefix(from),
                reverse_posting_key_prefix(to),
            ),
            (vector_key_prefix(from), vector_key_prefix(to)),
            (column_stats_prefix(from), column_stats_prefix(to)),
            (
                table_field_analyzer_prefix(from),
                table_field_analyzer_prefix(to),
            ),
        ] {
            batch_rekey_prefix(
                self.store.as_ref(),
                batch.as_mut(),
                &old_prefix,
                &new_prefix,
            )?;
        }
        for row in self.load_catalog_indexes()? {
            if row.table_name == from {
                batch.put(
                    &single_str_key(TAG_CATALOG_INDEX, &row.name),
                    &encode_value(&StoredCatalogIndex {
                        index_type: row.index_type,
                        table_name: to.to_string(),
                        columns_json: row.columns_json,
                        parameters_json: row.parameters_json,
                    })?,
                )?;
            }
        }
        batch.commit()
    }

    fn drop_column_data(&self, table_name: &str, column_name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for (key, value) in self.store.scan_prefix(&document_key_prefix(table_name))? {
            let mut document = decode_value::<Document>(&value)?;
            if document.remove(column_name).is_some() {
                batch.put(&key, &encode_value(&document)?)?;
            }
        }
        batch.delete_prefix(&posting_field_prefix(table_name, column_name))?;
        batch.delete_prefix(&field_stats_key(table_name, column_name))?;
        batch.delete_prefix(&vector_field_prefix(table_name, column_name))?;
        batch.delete_prefix(&table_field_analyzer_field_prefix(table_name, column_name))?;
        batch.delete(&column_stats_key(table_name, column_name))?;
        for (key, _) in self.store.scan_prefix(&doc_length_key_prefix(table_name))? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(column_name) {
                batch.delete(&key)?;
            }
        }
        for (key, _) in self
            .store
            .scan_prefix(&reverse_posting_key_prefix(table_name))?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let _doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(column_name) {
                batch.delete(&key)?;
            }
        }
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name && catalog_index_references_column(&row, column_name) {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name))?;
            }
        }
        batch.commit()
    }

    fn rename_column_data(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for (key, value) in self.store.scan_prefix(&document_key_prefix(table_name))? {
            let mut document = decode_value::<Document>(&value)?;
            if let Some(value) = document.remove(from) {
                document.insert(to.to_string(), value);
                batch.put(&key, &encode_value(&document)?)?;
            }
        }
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &posting_field_prefix(table_name, from),
            &posting_field_prefix(table_name, to),
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &field_stats_key(table_name, from),
            &field_stats_key(table_name, to),
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &vector_field_prefix(table_name, from),
            &vector_field_prefix(table_name, to),
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &table_field_analyzer_field_prefix(table_name, from),
            &table_field_analyzer_field_prefix(table_name, to),
        )?;
        if let Some(value) = self.store.get(&column_stats_key(table_name, from))? {
            batch_put_or_keep_existing(
                self.store.as_ref(),
                batch.as_mut(),
                &column_stats_key(table_name, to),
                &value,
            )?;
            batch.delete(&column_stats_key(table_name, from))?;
        }
        for (key, value) in self.store.scan_prefix(&doc_length_key_prefix(table_name))? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(from) {
                batch_put_or_keep_existing(
                    self.store.as_ref(),
                    batch.as_mut(),
                    &doc_length_key(table_name, doc_id, to),
                    &value,
                )?;
                batch.delete(&key)?;
            }
        }
        for (key, value) in self
            .store
            .scan_prefix(&reverse_posting_key_prefix(table_name))?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let term = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(from) {
                batch_put_or_keep_existing(
                    self.store.as_ref(),
                    batch.as_mut(),
                    &reverse_posting_key(table_name, doc_id, to, &term),
                    &value,
                )?;
                batch.delete(&key)?;
            }
        }
        for row in self.load_catalog_indexes()? {
            if row.table_name != table_name {
                continue;
            }
            if let Some(columns_json) = catalog_index_rename_column(&row, from, to) {
                batch.put(
                    &single_str_key(TAG_CATALOG_INDEX, &row.name),
                    &encode_value(&StoredCatalogIndex {
                        index_type: row.index_type,
                        table_name: row.table_name,
                        columns_json,
                        parameters_json: row.parameters_json,
                    })?,
                )?;
            }
        }
        batch.commit()
    }

    fn save_model(&self, name: &str, json: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_MODEL, name), &string_value(json))
    }

    fn load_models(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_MODEL)
    }

    fn load_model(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_MODEL, name))?
            .map(decode_string)
            .transpose()
    }

    fn drop_model(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_MODEL, name))
    }

    fn save_scoring_params(&self, name: &str, params_json: &str) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_SCORING_PARAMS, name),
            &string_value(params_json),
        )
    }

    fn load_scoring_params(&self, name: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_SCORING_PARAMS, name))?
            .map(decode_string)
            .transpose()
    }

    fn load_all_scoring_params(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_SCORING_PARAMS)
    }

    fn drop_scoring_params(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_SCORING_PARAMS, name))
    }

    fn save_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        self.store.put(&single_str_key(TAG_NAMED_GRAPH, name), &[])
    }

    fn drop_named_graph(&self, name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&single_str_key(TAG_NAMED_GRAPH, name))?;
        batch.delete_prefix(&graph_membership_graph_prefix(name))?;
        batch.commit()
    }

    fn load_named_graphs(&self) -> StorageBackendResult<Vec<String>> {
        load_single_keys(self.store.as_ref(), TAG_NAMED_GRAPH)
    }

    fn save_vertex(
        &self,
        vertex_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_VERTEX);
        push_u64(&mut key, vertex_id);
        self.store.put(
            &key,
            &encode_value(&StoredVertex {
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    fn delete_vertex(&self, vertex_id: u64) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_VERTEX);
        push_u64(&mut key, vertex_id);
        self.store.delete(&key)
    }

    fn load_vertices(&self) -> StorageBackendResult<Vec<(u64, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_VERTEX))? {
            let mut offset = 1;
            let vertex_id = read_u64(&key, &mut offset)?;
            let stored: StoredVertex = decode_value(&value)?;
            rows.push((vertex_id, stored.label, stored.properties_json));
        }
        Ok(rows)
    }

    fn save_edge(
        &self,
        edge_id: u64,
        source_id: u64,
        target_id: u64,
        label: &str,
        properties_json: &str,
    ) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_EDGE);
        push_u64(&mut key, edge_id);
        self.store.put(
            &key,
            &encode_value(&StoredEdge {
                source_id,
                target_id,
                label: label.to_string(),
                properties_json: properties_json.to_string(),
            })?,
        )
    }

    fn delete_edge(&self, edge_id: u64) -> StorageBackendResult<()> {
        let mut key = key_with_tag(TAG_EDGE);
        push_u64(&mut key, edge_id);
        self.store.delete(&key)
    }

    fn load_edges(&self) -> StorageBackendResult<Vec<EdgeRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_EDGE))? {
            let mut offset = 1;
            let edge_id = read_u64(&key, &mut offset)?;
            let stored: StoredEdge = decode_value(&value)?;
            rows.push(EdgeRow {
                edge_id,
                source_id: stored.source_id,
                target_id: stored.target_id,
                label: stored.label,
                properties_json: stored.properties_json,
            });
        }
        Ok(rows)
    }

    fn save_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &graph_membership_key(entity_type, entity_id, graph_name),
            &[],
        )
    }

    fn delete_graph_membership(
        &self,
        entity_type: &str,
        entity_id: u64,
        graph_name: &str,
    ) -> StorageBackendResult<()> {
        self.store
            .delete(&graph_membership_key(entity_type, entity_id, graph_name))
    }

    fn delete_graph_membership_for_graph(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&graph_membership_graph_prefix(graph_name))?;
        Ok(())
    }

    fn load_graph_memberships(&self) -> StorageBackendResult<Vec<(String, u64, String)>> {
        let mut rows = Vec::new();
        for (key, _) in self.store.scan_prefix(&graph_membership_prefix())? {
            let mut offset = 1;
            let graph_name = read_str(&key, &mut offset)?;
            let entity_type = read_str(&key, &mut offset)?;
            let entity_id = read_u64(&key, &mut offset)?;
            rows.push((entity_type, entity_id, graph_name));
        }
        Ok(rows)
    }

    fn purge_orphan_graph_entities(&self) -> StorageBackendResult<()> {
        let memberships = self.load_graph_memberships()?;
        let vertex_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "vertex").then_some(*id))
            .collect::<BTreeSet<_>>();
        let edge_ids = memberships
            .iter()
            .filter_map(|(ty, id, _)| (ty == "edge").then_some(*id))
            .collect::<BTreeSet<_>>();
        let mut batch = self.store.batch();
        for (id, _, _) in self.load_vertices()? {
            if !vertex_ids.contains(&id) {
                let mut key = key_with_tag(TAG_VERTEX);
                push_u64(&mut key, id);
                batch.delete(&key)?;
            }
        }
        for edge in self.load_edges()? {
            if !edge_ids.contains(&edge.edge_id) {
                let mut key = key_with_tag(TAG_EDGE);
                push_u64(&mut key, edge.edge_id);
                batch.delete(&key)?;
            }
        }
        batch.commit()
    }

    fn save_analyzer(&self, name: &str, config_json: &str) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_ANALYZER, name),
            &string_value(config_json),
        )
    }

    fn drop_analyzer(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_ANALYZER, name))
    }

    fn load_analyzers(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_ANALYZER)
    }

    fn save_table_field_analyzer(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &table_field_analyzer_key(table_name, field, phase),
            &string_value(analyzer_name),
        )
    }

    fn drop_table_field_analyzers(&self, table_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&table_field_analyzer_prefix(table_name))?;
        Ok(())
    }

    fn load_table_field_analyzers(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self
            .store
            .scan_prefix(&key_with_tag(TAG_TABLE_FIELD_ANALYZER))?
        {
            let mut offset = 1;
            let table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            let phase = read_str(&key, &mut offset)?;
            rows.push((table, field, phase, decode_string(value)?));
        }
        rows.sort();
        Ok(rows)
    }

    fn save_foreign_server(
        &self,
        name: &str,
        fdw_type: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_FOREIGN_SERVER, name),
            &encode_value(&StoredForeignServer {
                fdw_type: fdw_type.to_string(),
                options_json: options_json.to_string(),
            })?,
        )
    }

    fn drop_foreign_server(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_FOREIGN_SERVER, name))
    }

    fn load_foreign_servers(&self) -> StorageBackendResult<Vec<(String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_SERVER))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredForeignServer = decode_value(&value)?;
            rows.push((name, stored.fdw_type, stored.options_json));
        }
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(rows)
    }

    fn save_foreign_table(
        &self,
        name: &str,
        server_name: &str,
        columns_json: &str,
        options_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_FOREIGN_TABLE, name),
            &encode_value(&StoredForeignTable {
                server_name: server_name.to_string(),
                columns_json: columns_json.to_string(),
                options_json: options_json.to_string(),
            })?,
        )
    }

    fn drop_foreign_table(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_FOREIGN_TABLE, name))
    }

    fn load_foreign_tables(&self) -> StorageBackendResult<Vec<ForeignTableRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_FOREIGN_TABLE))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredForeignTable = decode_value(&value)?;
            rows.push(ForeignTableRow {
                name,
                server_name: stored.server_name,
                columns_json: stored.columns_json,
                options_json: stored.options_json,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    fn save_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table_name: &str,
        columns_json: &str,
        parameters_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_CATALOG_INDEX, name),
            &encode_value(&StoredCatalogIndex {
                index_type: index_type.to_string(),
                table_name: table_name.to_string(),
                columns_json: columns_json.to_string(),
                parameters_json: parameters_json.to_string(),
            })?,
        )
    }

    fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_CATALOG_INDEX, name))
    }

    fn drop_catalog_indexes_for_table(&self, table_name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name))?;
            }
        }
        batch.commit()
    }

    fn load_catalog_indexes(&self) -> StorageBackendResult<Vec<CatalogIndexRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_CATALOG_INDEX))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let stored: StoredCatalogIndex = decode_value(&value)?;
            rows.push(CatalogIndexRow {
                name,
                index_type: stored.index_type,
                table_name: stored.table_name,
                columns_json: stored.columns_json,
                parameters_json: stored.parameters_json,
            });
        }
        rows.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(rows)
    }

    fn save_path_index(
        &self,
        graph_name: &str,
        label_sequences_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_PATH_INDEX, graph_name),
            &string_value(label_sequences_json),
        )
    }

    fn drop_path_index(&self, graph_name: &str) -> StorageBackendResult<()> {
        self.store
            .delete(&single_str_key(TAG_PATH_INDEX, graph_name))
    }

    fn load_path_indexes(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_PATH_INDEX)
    }

    fn save_column_stats(&self, stats: ColumnStatsInput<'_>) -> StorageBackendResult<()> {
        self.store.put(
            &column_stats_key(stats.table_name, stats.column_name),
            &encode_value(&StoredColumnStats {
                distinct_count: stats.distinct_count,
                null_count: stats.null_count,
                min_value: stats.min_value.map(str::to_string),
                max_value: stats.max_value.map(str::to_string),
                row_count: stats.row_count,
                histogram_json: stats.histogram_json.to_string(),
                mcv_values_json: stats.mcv_values_json.to_string(),
                mcv_frequencies_json: stats.mcv_frequencies_json.to_string(),
            })?,
        )
    }

    fn load_column_stats(&self, table_name: &str) -> StorageBackendResult<Vec<ColumnStatsRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&column_stats_prefix(table_name))? {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let column_name = read_str(&key, &mut offset)?;
            let stored: StoredColumnStats = decode_value(&value)?;
            rows.push(ColumnStatsRow {
                column_name,
                distinct_count: stored.distinct_count,
                null_count: stored.null_count,
                min_value: stored.min_value,
                max_value: stored.max_value,
                row_count: stored.row_count,
                histogram_json: stored.histogram_json,
                mcv_values_json: stored.mcv_values_json,
                mcv_frequencies_json: stored.mcv_frequencies_json,
            });
        }
        rows.sort_by(|a, b| a.column_name.cmp(&b.column_name));
        Ok(rows)
    }

    fn delete_column_stats(&self, table_name: &str) -> StorageBackendResult<()> {
        self.store.delete_prefix(&column_stats_prefix(table_name))?;
        Ok(())
    }
}

fn load_single_keys(store: &dyn KeyValueStore, tag: u8) -> StorageBackendResult<Vec<String>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, _)| {
            let mut offset = 1;
            read_str(&key, &mut offset)
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort();
    Ok(rows)
}

fn load_single_string_rows(
    store: &dyn KeyValueStore,
    tag: u8,
) -> StorageBackendResult<Vec<(String, String)>> {
    let mut rows = store
        .scan_prefix(&key_with_tag(tag))?
        .into_iter()
        .map(|(key, value)| {
            let mut offset = 1;
            Ok((read_str(&key, &mut offset)?, decode_string(value)?))
        })
        .collect::<StorageBackendResult<Vec<_>>>()?;
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
}
