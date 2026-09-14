//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named and table-field analyzer persistence.

use super::{
    decode_string, key_with_tag, load_single_string_rows, push_str, read_str, single_str_key,
    string_value, table_field_analyzer_field_prefix, table_field_analyzer_key,
    table_field_analyzer_prefix, KeyValueCatalog, StorageBackendResult, TAG_ANALYZER,
    TAG_ANALYZER_DESCRIPTOR, TAG_FIELD_ANALYZER_BINDING, TAG_TABLE_FIELD_ANALYZER,
};

impl KeyValueCatalog {
    pub(super) fn save_analyzer_impl(
        &self,
        name: &str,
        config_json: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.put(
            &single_str_key(TAG_ANALYZER, name)?,
            &string_value(config_json),
        )?;
        batch.delete(&single_str_key(TAG_ANALYZER_DESCRIPTOR, name)?)?;
        batch.commit()
    }

    pub(super) fn drop_analyzer_impl(&self, name: &str) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&single_str_key(TAG_ANALYZER, name)?)?;
        batch.delete(&single_str_key(TAG_ANALYZER_DESCRIPTOR, name)?)?;
        batch.commit()
    }

    pub(super) fn load_analyzers_impl(&self) -> StorageBackendResult<Vec<(String, String)>> {
        load_single_string_rows(self.store.as_ref(), TAG_ANALYZER)
    }

    pub(super) fn save_table_field_analyzer_impl(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete(&field_binding_key(table_name, field)?)?;
        batch.put(
            &table_field_analyzer_key(table_name, field, phase)?,
            &string_value(analyzer_name),
        )?;
        batch.commit()
    }

    pub(super) fn replace_table_field_analyzer_impl(
        &self,
        table_name: &str,
        field: &str,
        phase: &str,
        analyzer_name: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&table_field_analyzer_field_prefix(table_name, field)?)?;
        batch.delete(&field_binding_key(table_name, field)?)?;
        batch.put(
            &table_field_analyzer_key(table_name, field, phase)?,
            &string_value(analyzer_name),
        )?;
        batch.commit()
    }

    pub(super) fn drop_table_field_analyzer_field_impl(
        &self,
        table_name: &str,
        field: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&table_field_analyzer_field_prefix(table_name, field)?)?;
        batch.delete(&field_binding_key(table_name, field)?)?;
        batch.commit()
    }

    pub(super) fn drop_table_field_analyzers_impl(
        &self,
        table_name: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&table_field_analyzer_prefix(table_name)?)?;
        batch.delete_prefix(&field_binding_prefix(table_name)?)?;
        batch.commit()
    }

    pub(super) fn load_table_field_analyzers_impl(
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
}

pub(super) fn field_binding_prefix(table: &str) -> StorageBackendResult<Vec<u8>> {
    single_str_key(TAG_FIELD_ANALYZER_BINDING, table)
}

pub(super) fn field_binding_key(table: &str, field: &str) -> StorageBackendResult<Vec<u8>> {
    let mut key = field_binding_prefix(table)?;
    push_str(&mut key, field)?;
    Ok(key)
}

impl KeyValueCatalog {
    pub(super) fn save_analyzer_revision_impl(
        &self,
        name: &str,
        config_json: &str,
        descriptor_json: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.put(
            &single_str_key(TAG_ANALYZER, name)?,
            &string_value(config_json),
        )?;
        batch.put(
            &single_str_key(TAG_ANALYZER_DESCRIPTOR, name)?,
            &string_value(descriptor_json),
        )?;
        batch.commit()
    }

    pub(super) fn replace_table_field_analyzer_binding_impl(
        &self,
        table: &str,
        field: &str,
        phase: &str,
        name: &str,
        binding_json: &str,
    ) -> StorageBackendResult<()> {
        let mut batch = self.store.batch();
        batch.delete_prefix(&table_field_analyzer_field_prefix(table, field)?)?;
        batch.put(
            &table_field_analyzer_key(table, field, phase)?,
            &string_value(name),
        )?;
        batch.put(
            &field_binding_key(table, field)?,
            &string_value(binding_json),
        )?;
        batch.commit()
    }

    pub(super) fn load_table_field_analyzer_bindings_impl(
        &self,
    ) -> StorageBackendResult<Vec<(String, String, String)>> {
        let mut rows = Vec::new();
        for (key, value) in self
            .store
            .scan_prefix(&key_with_tag(TAG_FIELD_ANALYZER_BINDING))?
        {
            let mut offset = 1;
            let table = read_str(&key, &mut offset)?;
            let field = read_str(&key, &mut offset)?;
            if offset != key.len() {
                return Err(crate::StorageBackendError::Other(
                    "invalid analyzer binding key".into(),
                ));
            }
            rows.push((table, field, decode_string(value)?));
        }
        rows.sort();
        Ok(rows)
    }
}
