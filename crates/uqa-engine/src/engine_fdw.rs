//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::Engine;

impl Engine {
    #[allow(clippy::needless_pass_by_value)]
    pub fn register_foreign_server(
        &self,
        name: String,
        fdw_type: String,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        let mut servers = self.foreign_servers.write();
        if servers.contains_key(&name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign server `{name}` already exists"));
        }
        if !matches!(fdw_type.as_str(), "duckdb_fdw" | "arrow_fdw" | "memory_fdw") {
            return Err(format!("Unsupported FDW type: `{fdw_type}`"));
        }
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        servers.insert(
            name.clone(),
            uqa_fdw::ForeignServer {
                name: name.clone(),
                fdw_type: fdw_type.clone(),
                options: opt_map.clone(),
            },
        );
        drop(servers);
        if let Some(catalog) = self.catalog.as_ref() {
            let options_json = serde_json::to_string(&opt_map).unwrap_or_else(|_| "{}".into());
            let _ = catalog.save_foreign_server(&name, &fdw_type, &options_json);
        }
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    pub fn register_foreign_table(
        &self,
        name: String,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        let name = self.relation_name_for_create(&name);
        if self.has_table(&name) {
            return Err(format!("Table `{name}` already exists"));
        }
        let mut tables = self.foreign_tables.write();
        if tables.contains_key(&name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign table `{name}` already exists"));
        }
        if !self.foreign_servers.read().contains_key(&server_name) {
            return Err(format!("Foreign server `{server_name}` does not exist"));
        }
        let fdw_columns: Vec<uqa_fdw::ColumnDef> = columns
            .iter()
            .map(|c| uqa_fdw::ColumnDef {
                name: c.name.clone(),
                ty: match &c.ty {
                    uqa_sql::ast::ColumnType::Integer => uqa_fdw::ColumnType::Integer,
                    uqa_sql::ast::ColumnType::Real | uqa_sql::ast::ColumnType::Numeric { .. } => {
                        uqa_fdw::ColumnType::Real
                    }
                    uqa_sql::ast::ColumnType::Text
                    | uqa_sql::ast::ColumnType::Json
                    | uqa_sql::ast::ColumnType::JsonB
                    | uqa_sql::ast::ColumnType::Date
                    | uqa_sql::ast::ColumnType::Time
                    | uqa_sql::ast::ColumnType::TimeTz
                    | uqa_sql::ast::ColumnType::Timestamp
                    | uqa_sql::ast::ColumnType::TimestampTz => uqa_fdw::ColumnType::Text,
                    uqa_sql::ast::ColumnType::Bytea
                    | uqa_sql::ast::ColumnType::Vector(_)
                    | uqa_sql::ast::ColumnType::Tensor(_) => uqa_fdw::ColumnType::Bytes,
                },
            })
            .collect();
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        tables.insert(
            name.clone(),
            uqa_fdw::ForeignTable {
                name: name.clone(),
                server_name: server_name.clone(),
                columns: fdw_columns,
                options: opt_map.clone(),
            },
        );
        drop(tables);
        if let Some(catalog) = self.catalog.as_ref() {
            let columns_json = serde_json::to_string(&columns).unwrap_or_else(|_| "[]".into());
            let options_json = serde_json::to_string(&opt_map).unwrap_or_else(|_| "{}".into());
            let _ = catalog.save_foreign_table(&name, &server_name, &columns_json, &options_json);
        }
        Ok(())
    }

    pub fn drop_foreign_server(&self, name: &str) -> bool {
        // Reject when any foreign table references this server.
        let referenced = self
            .foreign_tables
            .read()
            .values()
            .any(|t| t.server_name == name);
        if referenced {
            return false;
        }
        let removed = self.foreign_servers.write().remove(name).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_foreign_server(name);
            }
        }
        removed
    }

    pub fn drop_foreign_table(&self, name: &str) -> bool {
        let Some(name) = self.resolve_foreign_table_name(name) else {
            return false;
        };
        let removed = self.foreign_tables.write().remove(&name).is_some();
        if removed {
            if let Some(catalog) = self.catalog.as_ref() {
                let _ = catalog.drop_foreign_table(&name);
            }
        }
        removed
    }

    pub fn foreign_server(&self, name: &str) -> Option<uqa_fdw::ForeignServer> {
        self.foreign_servers.read().get(name).cloned()
    }

    pub fn foreign_table(&self, name: &str) -> Option<uqa_fdw::ForeignTable> {
        let resolved = self.resolve_foreign_table_name(name)?;
        self.foreign_tables.read().get(&resolved).cloned()
    }

    pub fn list_foreign_servers(&self) -> Vec<String> {
        let mut out: Vec<String> = self.foreign_servers.read().keys().cloned().collect();
        out.sort();
        out
    }

    pub fn list_foreign_tables(&self) -> Vec<String> {
        let mut out: Vec<String> = self.foreign_tables.read().keys().cloned().collect();
        out.sort();
        out
    }

    pub fn foreign_table_columns(&self, table: &str) -> Vec<String> {
        self.foreign_table(table)
            .map(|t| t.columns.iter().map(|c| c.name.clone()).collect())
            .unwrap_or_default()
    }

    pub fn load_memory_foreign_table(
        &self,
        table_name: impl Into<String>,
        rows: Vec<uqa_fdw::Row>,
    ) -> std::result::Result<(), String> {
        let table_name = table_name.into();
        let table_name = self
            .resolve_foreign_table_name(&table_name)
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let table = self
            .foreign_table(&table_name)
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;
        if server.fdw_type != "memory_fdw" {
            return Err(format!(
                "Foreign table `{table_name}` is backed by `{}` not `memory_fdw`",
                server.fdw_type
            ));
        }
        self.foreign_memory_tables.write().insert(table_name, rows);
        Ok(())
    }

    pub(crate) fn scan_foreign_table(
        &self,
        table_name: &str,
        columns: Option<&[String]>,
        predicates: &[uqa_fdw::FDWPredicate],
        limit: Option<u64>,
    ) -> std::result::Result<Vec<uqa_fdw::Row>, String> {
        use uqa_fdw::FDWHandler as _;

        let table = self
            .foreign_table(table_name)
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;

        let rows = match server.fdw_type.as_str() {
            "memory_fdw" => {
                let mut handler = uqa_fdw::MemoryHandler::new();
                let rows = self
                    .foreign_memory_tables
                    .read()
                    .get(table_name)
                    .cloned()
                    .unwrap_or_default();
                handler.load(table_name, rows);
                handler.scan(&table, columns, predicates, limit)
            }
            #[cfg(not(target_os = "emscripten"))]
            "duckdb_fdw" => {
                let handler = uqa_fdw::DuckDBHandler::new(server);
                handler.scan(&table, columns, predicates, limit)
            }
            #[cfg(not(target_os = "emscripten"))]
            "arrow_fdw" => {
                let handler = uqa_fdw::ArrowIpcHandler::new(server);
                handler.scan(&table, columns, predicates, limit)
            }
            other => return Err(format!("FDW type `{other}` is not available in this build")),
        };
        rows.map_err(|err| err.to_string())
    }
}
