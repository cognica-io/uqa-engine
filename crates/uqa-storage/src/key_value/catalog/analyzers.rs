//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Named and table-field analyzer persistence.

use super::{
    decode_string, key_with_tag, load_single_string_rows, read_str, single_str_key, string_value,
    table_field_analyzer_field_prefix, table_field_analyzer_key, table_field_analyzer_prefix,
    KeyValueCatalog, StorageBackendResult, TAG_ANALYZER, TAG_TABLE_FIELD_ANALYZER,
};

impl KeyValueCatalog {
    pub(super) fn save_analyzer_impl(
        &self,
        name: &str,
        config_json: &str,
    ) -> StorageBackendResult<()> {
        self.store.put(
            &single_str_key(TAG_ANALYZER, name)?,
            &string_value(config_json),
        )
    }

    pub(super) fn drop_analyzer_impl(&self, name: &str) -> StorageBackendResult<()> {
        self.store.delete(&single_str_key(TAG_ANALYZER, name)?)
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
        self.store.put(
            &table_field_analyzer_key(table_name, field, phase)?,
            &string_value(analyzer_name),
        )
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
        self.store
            .delete_prefix(&table_field_analyzer_field_prefix(table_name, field)?)?;
        Ok(())
    }

    pub(super) fn drop_table_field_analyzers_impl(
        &self,
        table_name: &str,
    ) -> StorageBackendResult<()> {
        self.store
            .delete_prefix(&table_field_analyzer_prefix(table_name)?)?;
        Ok(())
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
