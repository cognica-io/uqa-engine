//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CatalogIndexRow, Engine, StorageBackendError, StorageBackendResult};

impl Engine {
    pub fn register_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) -> StorageBackendResult<()> {
        self.try_register_catalog_index(name, index_type, table, columns, options)
    }

    pub(crate) fn try_register_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) -> StorageBackendResult<()> {
        self.with_implicit_storage_transaction(|engine| {
            engine.try_register_catalog_index_inner(name, index_type, table, columns, options)
        })
    }

    fn try_register_catalog_index_inner(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) -> StorageBackendResult<()> {
        self.synchronize_catalog_registries()?;
        let table = self
            .try_resolve_table_name(table)?
            .unwrap_or_else(|| table.to_string());
        let columns_json = serde_json::to_string(columns).map_err(StorageBackendError::from)?;
        let options_map: std::collections::BTreeMap<String, String> =
            options.iter().cloned().collect();
        let parameters_json =
            serde_json::to_string(&options_map).map_err(StorageBackendError::from)?;
        let row = CatalogIndexRow {
            name: name.to_string(),
            index_type: index_type.to_string(),
            table_name: table.clone(),
            columns_json: columns_json.clone(),
            parameters_json: parameters_json.clone(),
        };
        let previous = self
            .durable
            .catalog_indexes
            .write()
            .insert(name.to_string(), row.clone());
        if let Err(err) = self.refresh_catalog_index_tables(&row, previous.as_ref()) {
            self.restore_catalog_index_entry(name, previous.as_ref());
            if let Err(cleanup) = self.restore_catalog_index_tables(&row, previous.as_ref()) {
                return Err(StorageBackendError::Other(format!(
                    "{err}; restoring value indexes after the index build failure also failed: {cleanup}"
                )));
            }
            return Err(err);
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            if let Err(err) = catalog.save_catalog_index(
                name,
                index_type,
                &table,
                &columns_json,
                &parameters_json,
            ) {
                self.restore_catalog_index_entry(name, previous.as_ref());
                if let Err(cleanup) = self.restore_catalog_index_tables(&row, previous.as_ref()) {
                    return Err(StorageBackendError::Other(format!(
                        "{err}; restoring value indexes after the catalog write failure also failed: {cleanup}"
                    )));
                }
                return Err(err);
            }
            self.note_table_catalog_changed();
        }
        self.note_catalog_registry_changed();
        Ok(())
    }

    fn restore_catalog_index_tables(
        &self,
        current: &CatalogIndexRow,
        previous: Option<&CatalogIndexRow>,
    ) -> StorageBackendResult<()> {
        let mut tables = std::collections::BTreeSet::new();
        for row in std::iter::once(current).chain(previous) {
            if row.index_type.eq_ignore_ascii_case("btree") {
                tables.insert(row.table_name.as_str());
            }
        }
        for table in tables {
            if self.try_resolve_table_name(table)?.is_some() {
                self.refresh_value_indexes_for_table(table)?;
            }
        }
        Ok(())
    }

    fn restore_catalog_index_entry(&self, name: &str, previous: Option<&CatalogIndexRow>) {
        let mut indexes = self.durable.catalog_indexes.write();
        indexes.remove(name);
        if let Some(previous) = previous {
            indexes.insert(name.to_string(), previous.clone());
        }
    }

    fn refresh_catalog_index_tables(
        &self,
        current: &CatalogIndexRow,
        previous: Option<&CatalogIndexRow>,
    ) -> StorageBackendResult<()> {
        let mut tables = std::collections::BTreeSet::new();
        for row in std::iter::once(current).chain(previous) {
            if row.index_type.eq_ignore_ascii_case("btree") {
                tables.insert(row.table_name.as_str());
            }
        }
        for table in tables {
            self.refresh_value_indexes_for_table(table)?;
        }
        Ok(())
    }

    pub fn drop_catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.try_drop_catalog_index(name)
    }

    pub(crate) fn try_drop_catalog_index(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.with_implicit_storage_transaction(|engine| engine.try_drop_catalog_index_inner(name))
    }

    fn try_drop_catalog_index_inner(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.synchronize_catalog_registries()?;
        let existing = self.durable.catalog_indexes.read().get(name).cloned();
        let Some(existing_row) = existing else {
            return Ok(None);
        };
        let removed = self.durable.catalog_indexes.write().remove(name);
        if existing_row.index_type.eq_ignore_ascii_case("btree") {
            if let Err(err) = self.refresh_value_indexes_for_table(&existing_row.table_name) {
                self.durable
                    .catalog_indexes
                    .write()
                    .insert(name.to_string(), existing_row.clone());
                if let Err(cleanup) = self.refresh_value_indexes_for_table(&existing_row.table_name)
                {
                    return Err(StorageBackendError::Other(format!(
                        "{err}; restoring value indexes after the index drop failure also failed: {cleanup}"
                    )));
                }
                return Err(err);
            }
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            if let Err(err) = catalog.drop_catalog_index(name) {
                self.durable
                    .catalog_indexes
                    .write()
                    .insert(name.to_string(), existing_row.clone());
                if existing_row.index_type.eq_ignore_ascii_case("btree") {
                    if let Err(cleanup) =
                        self.refresh_value_indexes_for_table(&existing_row.table_name)
                    {
                        return Err(StorageBackendError::Other(format!(
                            "{err}; restoring value indexes after the catalog delete failure also failed: {cleanup}"
                        )));
                    }
                }
                return Err(err);
            }
            self.note_table_catalog_changed();
        }
        self.note_catalog_registry_changed();
        Ok(removed)
    }

    pub fn catalog_index(&self, name: &str) -> StorageBackendResult<Option<CatalogIndexRow>> {
        self.synchronize_catalog_registries()?;
        Ok(self.durable.catalog_indexes.read().get(name).cloned())
    }

    pub fn has_catalog_index(&self, name: &str) -> StorageBackendResult<bool> {
        self.synchronize_catalog_registries()?;
        Ok(self.durable.catalog_indexes.read().contains_key(name))
    }
}
