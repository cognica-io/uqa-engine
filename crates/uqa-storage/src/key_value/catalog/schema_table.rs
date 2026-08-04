//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Metadata, schema, table, column, and owned-data lifecycle.

use super::physical_indexes::{
    drop_field_indexes, drop_table_indexes, rename_field_indexes, rename_table_indexes,
};
use super::{
    apply_relation_migrations, batch_put_or_keep_existing, batch_rekey_prefix,
    batch_rekey_prefix_or_keep_existing, catalog_index_references_column,
    catalog_index_rename_column, collect_relation_migrations, column_stats_key,
    column_stats_prefix, decode_document_value, decode_relation_key, decode_string, decode_value,
    doc_length_key, doc_length_key_prefix, document_key_prefix, encode_document_value,
    encode_value, field_stats_key, field_stats_key_prefix, key_with_tag, posting_field_prefix,
    posting_key_prefix, read_str, read_u64, relation_key, reverse_posting_key,
    reverse_posting_key_prefix, single_str_key, string_value, table_field_analyzer_field_prefix,
    table_field_analyzer_prefix, validate_relation_parents, vector_field_prefix, vector_key_prefix,
    CatalogFacade, KeyValueCatalog, RelationIdentity, RelationKind, StorageBackendError,
    StorageBackendResult, StoredCatalogIndex, StoredRelation, TableSchema, TAG_CATALOG_INDEX,
    TAG_METADATA, TAG_RELATION, TAG_SCHEMA, TAG_TABLE,
};

impl KeyValueCatalog {
    pub(super) fn set_metadata_impl(&self, key: &str, value: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_METADATA, key)?, &string_value(value))
    }

    pub(super) fn get_metadata_impl(&self, key: &str) -> StorageBackendResult<Option<String>> {
        self.store
            .get(&single_str_key(TAG_METADATA, key)?)?
            .map(decode_string)
            .transpose()
    }

    pub(super) fn migrate_relation_namespace_impl(&self) -> StorageBackendResult<()> {
        let migrations = collect_relation_migrations(self)?;
        validate_relation_parents(self.store.as_ref(), &migrations.seen)?;
        apply_relation_migrations(self, migrations)
    }

    pub(super) fn save_schema_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.store
            .put(&single_str_key(TAG_SCHEMA, name)?, &string_value(name))
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

    pub(super) fn load_schemas_impl(&self) -> StorageBackendResult<Vec<String>> {
        let mut rows = Vec::new();
        for (key, _) in self.store.scan_prefix(&key_with_tag(TAG_SCHEMA))? {
            let mut offset = 1;
            rows.push(read_str(&key, &mut offset)?);
        }
        rows.sort();
        Ok(rows)
    }

    pub(super) fn save_table_impl(&self, schema: &TableSchema) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        self.claim_relation(batch.as_mut(), &schema.relation, RelationKind::Table)?;
        batch.put(
            &relation_key(TAG_TABLE, &schema.relation)?,
            &encode_value(schema)?,
        )?;
        batch.commit()
    }

    pub(super) fn load_tables_impl(&self) -> StorageBackendResult<Vec<TableSchema>> {
        let mut rows = self
            .store
            .scan_prefix(&key_with_tag(TAG_TABLE))?
            .into_iter()
            .map(|(key, value)| {
                let relation = decode_relation_key(&key)?;
                let schema = decode_value::<TableSchema>(&value)?;
                if schema.relation != relation {
                    return Err(StorageBackendError::Other(format!(
                        "table catalog key `{}` disagrees with stored relation `{}`",
                        relation.qualified_name(),
                        schema.relation.qualified_name()
                    )));
                }
                Ok(schema)
            })
            .collect::<StorageBackendResult<Vec<_>>>()?;
        rows.sort_by(|a, b| a.relation.cmp(&b.relation));
        Ok(rows)
    }

    pub(super) fn drop_table_impl(&self, name: &str) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let mut batch = self.store.batch();
        batch.delete(&relation_key(TAG_TABLE, &relation)?)?;
        self.release_relation(batch.as_mut(), &relation, RelationKind::Table)?;
        batch.commit()
    }

    pub(super) fn drop_table_and_data_impl(&self, name: &str) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let storage_names = relation.canonical_and_legacy_public_names();
        let mut batch = self.store.batch();
        batch.delete(&relation_key(TAG_TABLE, &relation)?)?;
        self.release_relation(batch.as_mut(), &relation, RelationKind::Table)?;
        for storage_name in &storage_names {
            batch.delete_prefix(&document_key_prefix(storage_name)?)?;
            batch.delete_prefix(&posting_key_prefix(storage_name)?)?;
            batch.delete_prefix(&doc_length_key_prefix(storage_name)?)?;
            batch.delete_prefix(&field_stats_key_prefix(storage_name)?)?;
            batch.delete_prefix(&reverse_posting_key_prefix(storage_name)?)?;
            batch.delete_prefix(&vector_key_prefix(storage_name)?)?;
            batch.delete_prefix(&column_stats_prefix(storage_name)?)?;
            batch.delete_prefix(&table_field_analyzer_prefix(storage_name)?)?;
            drop_table_indexes(batch.as_mut(), storage_name)?;
        }
        for row in self.load_catalog_indexes()? {
            if storage_names.contains(&row.table_name) {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name)?)?;
            }
        }
        batch.commit()
    }

    pub(super) fn purge_table_data_impl(&self, name: &str) -> StorageBackendResult<()> {
        let relation =
            RelationIdentity::from_legacy_name(name).map_err(StorageBackendError::Other)?;
        let mut batch = self.store.batch();
        for storage_name in relation.canonical_and_legacy_public_names() {
            batch.delete_prefix(&document_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&posting_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&doc_length_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&field_stats_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&reverse_posting_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&vector_key_prefix(&storage_name)?)?;
            batch.delete_prefix(&column_stats_prefix(&storage_name)?)?;
            drop_table_indexes(batch.as_mut(), &storage_name)?;
        }
        batch.commit()
    }

    pub(super) fn rename_table_data_impl(&self, from: &str, to: &str) -> StorageBackendResult<()> {
        let from_relation =
            RelationIdentity::from_legacy_name(from).map_err(StorageBackendError::Other)?;
        let to_relation =
            RelationIdentity::from_legacy_name(to).map_err(StorageBackendError::Other)?;
        if from_relation == to_relation {
            return Ok(());
        }
        self.ensure_schema_exists(&to_relation)?;
        let from_key = relation_key(TAG_TABLE, &from_relation)?;
        let to_key = relation_key(TAG_TABLE, &to_relation)?;
        if self.store.get(&to_key)?.is_some()
            || self
                .store
                .get(&relation_key(TAG_RELATION, &to_relation)?)?
                .is_some()
        {
            return Err(StorageBackendError::Other(format!(
                "relation `{}` already exists",
                to_relation.qualified_name()
            )));
        }
        let value = self
            .store
            .get(&from_key)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{from}` does not exist")))?;
        let mut batch = self.store.batch();
        let mut schema = decode_value::<TableSchema>(&value)?;
        schema.relation = to_relation.clone();
        batch.put(&to_key, &encode_value(&schema)?)?;
        batch.delete(&from_key)?;
        self.release_relation(batch.as_mut(), &from_relation, RelationKind::Table)?;
        batch.put(
            &relation_key(TAG_RELATION, &to_relation)?,
            &encode_value(&StoredRelation {
                kind: RelationKind::Table,
            })?,
        )?;
        for (old_prefix, new_prefix) in [
            (document_key_prefix(from)?, document_key_prefix(to)?),
            (posting_key_prefix(from)?, posting_key_prefix(to)?),
            (doc_length_key_prefix(from)?, doc_length_key_prefix(to)?),
            (field_stats_key_prefix(from)?, field_stats_key_prefix(to)?),
            (
                reverse_posting_key_prefix(from)?,
                reverse_posting_key_prefix(to)?,
            ),
            (vector_key_prefix(from)?, vector_key_prefix(to)?),
            (column_stats_prefix(from)?, column_stats_prefix(to)?),
            (
                table_field_analyzer_prefix(from)?,
                table_field_analyzer_prefix(to)?,
            ),
        ] {
            batch_rekey_prefix(
                self.store.as_ref(),
                batch.as_mut(),
                &old_prefix,
                &new_prefix,
            )?;
        }
        rename_table_indexes(self.store.as_ref(), batch.as_mut(), from, to)?;
        for row in self.load_catalog_indexes()? {
            if row.table_name == from {
                batch.put(
                    &single_str_key(TAG_CATALOG_INDEX, &row.name)?,
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

    pub(super) fn drop_column_data_impl(
        &self,
        table_name: &str,
        column_name: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        for (key, value) in self.store.scan_prefix(&document_key_prefix(table_name)?)? {
            let mut document = decode_document_value(&value)?;
            if document.remove(column_name).is_some() {
                batch.put(&key, &encode_document_value(&document)?)?;
            }
        }
        batch.delete_prefix(&posting_field_prefix(table_name, column_name)?)?;
        batch.delete_prefix(&field_stats_key(table_name, column_name)?)?;
        batch.delete_prefix(&vector_field_prefix(table_name, column_name)?)?;
        drop_field_indexes(batch.as_mut(), table_name, column_name)?;
        batch.delete_prefix(&table_field_analyzer_field_prefix(table_name, column_name)?)?;
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
        for row in self.load_catalog_indexes()? {
            if row.table_name == table_name && catalog_index_references_column(&row, column_name)? {
                batch.delete(&single_str_key(TAG_CATALOG_INDEX, &row.name)?)?;
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
            let mut document = decode_document_value(&value)?;
            if let Some(value) = document.remove(from) {
                document.insert(to.to_string(), value);
                batch.put(&key, &encode_document_value(&document)?)?;
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
        if let Some(value) = self.store.get(&column_stats_key(table_name, from)?)? {
            batch_put_or_keep_existing(
                self.store.as_ref(),
                batch.as_mut(),
                &column_stats_key(table_name, to)?,
                &value,
            )?;
            batch.delete(&column_stats_key(table_name, from)?)?;
        }
        for (key, value) in self
            .store
            .scan_prefix(&doc_length_key_prefix(table_name)?)?
        {
            let mut offset = 1;
            let _table = read_str(&key, &mut offset)?;
            let doc_id = read_u64(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if field.eq_ignore_ascii_case(from) {
                batch_put_or_keep_existing(
                    self.store.as_ref(),
                    batch.as_mut(),
                    &doc_length_key(table_name, doc_id, to)?,
                    &value,
                )?;
                batch.delete(&key)?;
            }
        }
        for (key, value) in self
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
                    self.store.as_ref(),
                    batch.as_mut(),
                    &reverse_posting_key(table_name, doc_id, to, &term)?,
                    &value,
                )?;
                batch.delete(&key)?;
            }
        }
        for row in self.load_catalog_indexes()? {
            if row.table_name != table_name {
                continue;
            }
            if let Some(columns_json) = catalog_index_rename_column(&row, from, to)? {
                batch.put(
                    &single_str_key(TAG_CATALOG_INDEX, &row.name)?,
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
}
