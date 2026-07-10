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
    ) {
        let _ = self.try_register_catalog_index(name, index_type, table, columns, options);
    }

    pub(crate) fn try_register_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) -> StorageBackendResult<()> {
        let table = self
            .resolve_table_name(table)
            .unwrap_or_else(|| table.to_string());
        let columns_json = serde_json::to_string(columns).map_err(StorageBackendError::from)?;
        let options_map: std::collections::BTreeMap<String, String> =
            options.iter().cloned().collect();
        let parameters_json =
            serde_json::to_string(&options_map).map_err(StorageBackendError::from)?;
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.save_catalog_index(
                name,
                index_type,
                &table,
                &columns_json,
                &parameters_json,
            )?;
        }
        self.catalog_indexes.write().insert(
            name.to_string(),
            CatalogIndexRow {
                name: name.to_string(),
                index_type: index_type.to_string(),
                table_name: table.clone(),
                columns_json: columns_json.clone(),
                parameters_json: parameters_json.clone(),
            },
        );
        Ok(())
    }

    pub fn drop_catalog_index(&self, name: &str) -> Option<CatalogIndexRow> {
        self.try_drop_catalog_index(name).ok().flatten()
    }

    pub(crate) fn try_drop_catalog_index(
        &self,
        name: &str,
    ) -> StorageBackendResult<Option<CatalogIndexRow>> {
        let existing = self.catalog_indexes.read().get(name).cloned();
        let Some(existing_row) = existing else {
            return Ok(None);
        };
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.drop_catalog_index(name)?;
        }
        let removed = self.catalog_indexes.write().remove(name);
        // Built value indexes for the table are dropped so the lazy
        // rebuild re-derives them from the updated index policy.
        if let Some(t) = self.table(&existing_row.table_name) {
            Self::value_indexes_clear(&t);
        }
        Ok(removed)
    }

    pub fn catalog_index(&self, name: &str) -> Option<CatalogIndexRow> {
        self.catalog_indexes.read().get(name).cloned()
    }

    pub fn has_catalog_index(&self, name: &str) -> bool {
        self.catalog_indexes.read().contains_key(name)
    }
}
