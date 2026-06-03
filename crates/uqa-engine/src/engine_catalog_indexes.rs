//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{CatalogIndexRow, Engine};

impl Engine {
    pub fn register_catalog_index(
        &self,
        name: &str,
        index_type: &str,
        table: &str,
        columns: &[String],
        options: &[(String, String)],
    ) {
        let table = self
            .resolve_table_name(table)
            .unwrap_or_else(|| table.to_string());
        let columns_json = serde_json::to_string(columns).unwrap_or_else(|_| "[]".into());
        let options_map: std::collections::BTreeMap<String, String> =
            options.iter().cloned().collect();
        let parameters_json = serde_json::to_string(&options_map).unwrap_or_else(|_| "{}".into());
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
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.save_catalog_index(
                name,
                index_type,
                &table,
                &columns_json,
                &parameters_json,
            );
        }
    }

    pub fn drop_catalog_index(&self, name: &str) {
        self.catalog_indexes.write().remove(name);
        if let Some(catalog) = self.catalog.as_ref() {
            let _ = catalog.drop_catalog_index(name);
        }
    }

    pub fn has_catalog_index(&self, name: &str) -> bool {
        self.catalog_indexes.read().contains_key(name)
    }
}
