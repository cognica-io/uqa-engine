//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata, schema, table, column, and owned-data lifecycle.

use super::analyzers::field_binding_key;
use super::occurrence_lifecycle::{drop_occurrence_field, rename_occurrence_field};
use super::physical_indexes::{drop_field_indexes, rename_field_indexes};
use super::{
    apply_relation_migrations, batch_put_or_keep_existing, batch_rekey_prefix_or_keep_existing,
    catalog_index_references_column, catalog_index_rename_column, collect_relation_migrations,
    column_stats_key, decode_relation_key, decode_stored_document_value, decode_string,
    decode_value, doc_length_key, doc_length_key_prefix, document_key_prefix,
    encode_stored_document_value, encode_value, field_stats_key, key_with_tag,
    posting_cluster_positions_field_prefix, posting_cluster_score_field_prefix,
    posting_document_key, posting_document_key_prefix, posting_field_prefix, read_str, read_u64,
    relation_key, reverse_posting_key, reverse_posting_key_prefix, single_str_key, string_value,
    table_field_analyzer_field_prefix, validate_relation_parents, vector_field_prefix,
    CatalogFacade, KeyValueBatch, KeyValueCatalog, RelationKind, StorageBackendError,
    StorageBackendResult, StoredCatalogIndex, TAG_CATALOG_INDEX, TAG_METADATA, TAG_RELATION,
    TAG_SCHEMA,
};

fn rename_document_scoped_fts_fields(
    catalog: &KeyValueCatalog,
    batch: &mut dyn KeyValueBatch,
    table_name: &str,
    from: &str,
    to: &str,
) -> StorageBackendResult<()> {
    for (key, value) in catalog
        .store
        .scan_prefix(&doc_length_key_prefix(table_name)?)?
    {
        let mut offset = 1;
        let _table = read_str(&key, &mut offset)?;
        let doc_id = read_u64(&key, &mut offset)?;
        let field = read_str(&key, &mut offset)?;
        if field.eq_ignore_ascii_case(from) {
            batch_put_or_keep_existing(
                catalog.store.as_ref(),
                batch,
                &doc_length_key(table_name, doc_id, to)?,
                &value,
            )?;
            batch.delete(&key)?;
        }
    }
    for (key, value) in catalog
        .store
        .scan_prefix(&reverse_posting_key_prefix(table_name)?)?
    {
        let mut offset = 1;
        let _table = read_str(&key, &mut offset)?;
        let doc_id = read_u64(&key, &mut offset)?;
        let field = read_str(&key, &mut offset)?;
        let term = read_str(&key, &mut offset)?;
        if field.eq_ignore_ascii_case(from) {
            batch_put_or_keep_existing(
                catalog.store.as_ref(),
                batch,
                &reverse_posting_key(table_name, doc_id, to, &term)?,
                &value,
            )?;
            batch.delete(&key)?;
        }
    }
    for (key, value) in catalog
        .store
        .scan_prefix(&posting_document_key_prefix(table_name)?)?
    {
        let mut offset = 1;
        let _table = read_str(&key, &mut offset)?;
        let doc_id = read_u64(&key, &mut offset)?;
        let field = read_str(&key, &mut offset)?;
        if field.eq_ignore_ascii_case(from) {
            batch_put_or_keep_existing(
                catalog.store.as_ref(),
                batch,
                &posting_document_key(table_name, doc_id, to)?,
                &value,
            )?;
            batch.delete(&key)?;
        }
    }
    Ok(())
}

impl KeyValueCatalog {
    pub(super) fn metadata_with_prefix_impl(
        &self,
        prefix: &str,
    ) -> StorageBackendResult<Vec<(String, String)>> {
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            let mut entries = Vec::new();
            read.visit_prefix(&[TAG_METADATA], &mut |key, value| {
                let name = read_str(key, &mut 1)?;
                if name.starts_with(prefix) {
                    entries.push((
                        name,
                        std::str::from_utf8(value)
                            .map_err(|error| {
                                StorageBackendError::Other(format!(
                                    "invalid UTF-8 metadata value: {error}"
                                ))
                            })?
                            .to_owned(),
                    ));
                }
                Ok(())
            })?;
            Ok(entries)
        })
    }
    pub(super) fn set_metadata_impl(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        let identifiers = self.store.identifier_allocator().is_some();
        if let Some(graph) = key.strip_prefix("graph_label_registry::") {
            return self.store.with_mutation(&mut |read, batch| {
                if identifiers {
                    let view = super::graph_view::GraphRead { read, identifiers };
                    view.fence_definition(batch, graph)?;
                    view.observe_registry(batch, graph, value)?;
                }
                Self::invalidate_graph_path_data(read, batch, graph)?;
                batch.put(&single_str_key(TAG_METADATA, key)?, &string_value(value))
            });
        }
        if key == "graph_identifier_generation" && identifiers {
            return self.store.with_mutation(&mut |_, batch| {
                batch.fence_record(&single_str_key(
                    TAG_METADATA,
                    "graph_identifier_data_revision",
                )?)?;
                batch.put(&single_str_key(TAG_METADATA, key)?, &string_value(value))
            });
        }
        self.store
            .put(&single_str_key(TAG_METADATA, key)?, &string_value(value))
    }

    pub(super) fn get_metadata_impl(&self, key: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_METADATA, key)?)?
            .map(decode_string)
            .transpose()
    }

    pub(super) fn metadata_has_private_changes_impl(
        &self,
        name: &str,
    ) -> StorageBackendResult<bool> {
        if !self.store.transaction_model().is_versioned() {
            return Ok(false);
        }
        let key = single_str_key(TAG_METADATA, name)?;
        crate::key_value::index_view::read_view(self.store.as_ref(), |read| {
            Ok(read.revision(&[&key])?.has_private_changes())
        })
    }

    pub(super) fn migrate_relation_namespace_impl(&self) -> StorageBackendResult<()> {
        let migrations = collect_relation_migrations(self)?;
        validate_relation_parents(self.store.as_ref(), &migrations.seen)?;
        apply_relation_migrations(self, migrations)
    }

    pub(super) fn save_schema_row_impl(
        &self,
        schema: &crate::catalog::SchemaRow,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_SCHEMA, &schema.name)?,
            &encode_value(schema)?,
        )
    }

    pub(super) fn drop_schema_impl(&self, name: &str) -> StorageBackendResult<()> {
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_RELATION))? {
            if decode_relation_key(&key)?.schema == name {
                return Err(StorageBackendError::Other(format!(
                    "schema `{name}` still owns catalog relations"
                )));
            }
        }
        self.store.delete(&single_str_key(TAG_SCHEMA, name)?)
    }

    pub(super) fn load_schema_rows_impl(
        &self,
    ) -> StorageBackendResult<Vec<crate::catalog::SchemaRow>> {
        let mut rows = Vec::new();
        for (key, value) in self.store.scan_prefix(&key_with_tag(TAG_SCHEMA))? {
            let mut offset = 1;
            let name = read_str(&key, &mut offset)?;
            let schema = decode_value::<crate::catalog::SchemaRow>(&value)
                .or_else(|_| decode_string(value).map(crate::catalog::SchemaRow::legacy))?;
            if schema.name != name {
                return Err(StorageBackendError::Other(format!(
                    "schema catalog key `{name}` disagrees with stored name `{}`",
                    schema.name
                )));
            }
            rows.push(schema);
        }
        rows.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(rows)
    }

    pub(super) fn drop_column_data_impl(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for (key, value) in self.store.scan_prefix(&document_key_prefix(table_name)?)? {
            let mut document = decode_stored_document_value(&value)?;
            if document.fields_mut().remove(column_name).is_some() {
                batch.put(&key, &encode_stored_document_value(&document)?)?;
            }
        }
        drop_occurrence_field(self.store.as_ref(), batch.as_mut(), table_name, column_name)?;
        batch.delete_prefix(&posting_field_prefix(table_name, column_name)?)?;
        batch.delete_prefix(&posting_cluster_score_field_prefix(
            table_name,
            column_name,
        )?)?;
        batch.delete_prefix(&posting_cluster_positions_field_prefix(
            table_name,
            column_name,
        )?)?;
        batch.delete_prefix(&field_stats_key(table_name, column_name)?)?;
        batch.delete_prefix(&vector_field_prefix(table_name, column_name)?)?;
        drop_field_indexes(batch.as_mut(), table_name, column_name)?;
        batch.delete_prefix(&table_field_analyzer_field_prefix(table_name, column_name)?)?;
        batch.delete(&field_binding_key(table_name, column_name)?)?;
        batch.delete(&column_stats_key(table_name, column_name)?)?;
        for (key, _) in self
            .store
            .scan_prefix(&doc_length_key_prefix(table_name)?)?
        {
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
            .scan_prefix(&reverse_posting_key_prefix(table_name)?)?
        {
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
            .scan_prefix(&posting_document_key_prefix(table_name)?)?
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
            if row.table_name == table_name && catalog_index_references_column(&row, column_name)? {
                batch.delete(&relation_key(TAG_CATALOG_INDEX, &row.relation)?)?;
                self.release_relation(batch.as_mut(), &row.relation, RelationKind::Index)?;
            }
        }
        batch.commit()
    }

    pub(super) fn rename_column_data_impl(
        &self,
        table_name: &str,
        from: &str,
        to: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for (key, value) in self.store.scan_prefix(&document_key_prefix(table_name)?)? {
            let mut document = decode_stored_document_value(&value)?;
            if let Some(value) = document.fields_mut().remove(from) {
                document.fields_mut().insert(to.to_string(), value);
                batch.put(&key, &encode_stored_document_value(&document)?)?;
            }
        }
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &posting_field_prefix(table_name, from)?,
            &posting_field_prefix(table_name, to)?,
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &posting_cluster_score_field_prefix(table_name, from)?,
            &posting_cluster_score_field_prefix(table_name, to)?,
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &posting_cluster_positions_field_prefix(table_name, from)?,
            &posting_cluster_positions_field_prefix(table_name, to)?,
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &field_stats_key(table_name, from)?,
            &field_stats_key(table_name, to)?,
        )?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &vector_field_prefix(table_name, from)?,
            &vector_field_prefix(table_name, to)?,
        )?;
        rename_field_indexes(self.store.as_ref(), batch.as_mut(), table_name, from, to)?;
        batch_rekey_prefix_or_keep_existing(
            self.store.as_ref(),
            batch.as_mut(),
            &table_field_analyzer_field_prefix(table_name, from)?,
            &table_field_analyzer_field_prefix(table_name, to)?,
        )?;
        if let Some(value) = self.store.get(&field_binding_key(table_name, from)?)? {
            batch_put_or_keep_existing(
                self.store.as_ref(),
                batch.as_mut(),
                &field_binding_key(table_name, to)?,
                &value,
            )?;
            batch.delete(&field_binding_key(table_name, from)?)?;
        }
        if let Some(value) = self.store.get(&column_stats_key(table_name, from)?)? {
            batch_put_or_keep_existing(
                self.store.as_ref(),
                batch.as_mut(),
                &column_stats_key(table_name, to)?,
                &value,
            )?;
            batch.delete(&column_stats_key(table_name, from)?)?;
        }
        rename_occurrence_field(self.store.as_ref(), batch.as_mut(), table_name, from, to)?;
        rename_document_scoped_fts_fields(self, batch.as_mut(), table_name, from, to)?;
        for row in self.load_catalog_indexes()? {
            if row.table_name != table_name {
                continue;
            }
            if let Some(columns_json) = catalog_index_rename_column(&row, from, to)? {
                batch.put(
                    &relation_key(TAG_CATALOG_INDEX, &row.relation)?,
                    &encode_value(&StoredCatalogIndex {
                        index_type: row.index_type,
                        table_name: row.table_name,
                        columns_json,
                        parameters_json: row.parameters_json,
                        definition_json: row.definition_json,
                    })?,
                )?;
            }
        }
        batch.commit()
    }
}
