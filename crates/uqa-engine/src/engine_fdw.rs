//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, RelationIdentity};

pub(crate) fn sql_column_type_to_fdw(
    column_type: &uqa_sql::ast::ColumnType,
) -> uqa_fdw::ColumnType {
    match column_type {
        uqa_sql::ast::ColumnType::Boolean => uqa_fdw::ColumnType::Bool,
        uqa_sql::ast::ColumnType::Integer => uqa_fdw::ColumnType::Integer,
        uqa_sql::ast::ColumnType::Real | uqa_sql::ast::ColumnType::Numeric { .. } => {
            uqa_fdw::ColumnType::Real
        }
        uqa_sql::ast::ColumnType::Text
        | uqa_sql::ast::ColumnType::Character(_)
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
        uqa_sql::ast::ColumnType::Array(element) => {
            uqa_fdw::ColumnType::Array(Box::new(sql_column_type_to_fdw(element)))
        }
    }
}

struct MemoryForeignRowStream<'a> {
    engine: &'a Engine,
    table_name: RelationIdentity,
    columns: Option<Vec<String>>,
    predicates: Vec<uqa_fdw::FDWPredicate>,
    limit: Option<u64>,
    index: usize,
    emitted: u64,
}

impl Iterator for MemoryForeignRowStream<'_> {
    type Item = std::result::Result<uqa_fdw::Row, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.limit.is_some_and(|limit| self.emitted >= limit) {
            return None;
        }
        loop {
            let row = {
                let tables = self.engine.extensions.foreign_memory_tables.read();
                let Some(rows) = tables.get(&self.table_name) else {
                    return Some(Err(format!(
                        "Foreign table `{}` lost its loaded memory data during the scan",
                        self.table_name.qualified_name()
                    )));
                };
                let row = rows.get(self.index)?;
                row.clone()
            };
            self.index = match self.index.checked_add(1) {
                Some(index) => index,
                None => return Some(Err("memory FDW cursor index overflow".into())),
            };
            match uqa_fdw::row_matches_predicates(&row, &self.predicates) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(error) => return Some(Err(error.to_string())),
            }
            self.emitted = match self.emitted.checked_add(1) {
                Some(emitted) => emitted,
                None => return Some(Err("memory FDW emitted-row count overflow".into())),
            };
            return Some(Ok(uqa_fdw::project_row(&row, self.columns.as_deref())));
        }
    }
}

impl Engine {
    pub fn register_foreign_server(
        &self,
        name: String,
        fdw_type: String,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.with_implicit_string_transaction(move |engine| {
            engine.register_foreign_server_inner(name, &fdw_type, options, if_not_exists)
        })
    }

    fn register_foreign_server_inner(
        &self,
        name: String,
        fdw_type: &str,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut servers = self.durable.foreign_servers.write();
        if servers.contains_key(&name) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign server `{name}` already exists"));
        }
        if !matches!(fdw_type, "duckdb_fdw" | "arrow_fdw" | "memory_fdw") {
            return Err(format!("Unsupported FDW type: `{fdw_type}`"));
        }
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        let server = uqa_fdw::ForeignServer {
            name: name.clone(),
            fdw_type: fdw_type.to_string(),
            options: opt_map.clone(),
        };
        if let Some(catalog) = self.storage.catalog.as_ref() {
            let options_json = serde_json::to_string(&opt_map)
                .map_err(|err| format!("serialize foreign server `{name}`: {err}"))?;
            catalog
                .save_foreign_server(&name, fdw_type, &options_json)
                .map_err(|err| format!("persist foreign server `{name}`: {err}"))?;
        }
        servers.insert(name, server);
        drop(servers);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub fn register_foreign_table(
        &self,
        name: String,
        server_name: String,
        columns: Vec<uqa_sql::ast::ColumnDef>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.with_implicit_string_transaction(move |engine| {
            engine.register_foreign_table_inner(
                &name,
                &server_name,
                &columns,
                options,
                if_not_exists,
            )
        })
    }

    fn register_foreign_table_inner(
        &self,
        name: &str,
        server_name: &str,
        columns: &[uqa_sql::ast::ColumnDef],
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> std::result::Result<(), String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let name = self.try_relation_name_for_create(name)?;
        let relation = RelationIdentity::from_legacy_name(&name)?;
        if let Some(kind) = self
            .relation_kind_at(&name)
            .map_err(|err| format!("resolve relation `{name}`: {err}"))?
        {
            if kind != "foreign table" {
                return Err(format!("Relation `{name}` already exists as {kind}"));
            }
        }
        let mut tables = self.durable.foreign_tables.write();
        if tables.contains_key(&relation) {
            if if_not_exists {
                return Ok(());
            }
            return Err(format!("Foreign table `{name}` already exists"));
        }
        if !self
            .durable
            .foreign_servers
            .read()
            .contains_key(server_name)
        {
            return Err(format!("Foreign server `{server_name}` does not exist"));
        }
        let fdw_columns: Vec<uqa_fdw::ColumnDef> = columns
            .iter()
            .map(|c| uqa_fdw::ColumnDef {
                name: c.name.clone(),
                ty: sql_column_type_to_fdw(&c.ty),
            })
            .collect();
        let mut opt_map: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for (k, v) in options {
            opt_map.insert(k, v);
        }
        let table = uqa_fdw::ForeignTable {
            name: name.clone(),
            server_name: server_name.to_string(),
            columns: fdw_columns,
            options: opt_map.clone(),
        };
        if let Some(catalog) = self.storage.catalog.as_ref() {
            let columns_json = serde_json::to_string(columns)
                .map_err(|err| format!("serialize foreign table `{name}` columns: {err}"))?;
            let options_json = serde_json::to_string(&opt_map)
                .map_err(|err| format!("serialize foreign table `{name}` options: {err}"))?;
            catalog
                .save_foreign_table(&relation, server_name, &columns_json, &options_json)
                .map_err(|err| format!("persist foreign table `{name}`: {err}"))?;
        }
        tables.insert(relation, table);
        drop(tables);
        self.note_catalog_registry_changed();
        Ok(())
    }

    pub fn drop_foreign_server(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| engine.drop_foreign_server_inner(name))
    }

    fn drop_foreign_server_inner(&self, name: &str) -> Result<bool, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        // Reject when any foreign table references this server.
        let referenced = self
            .durable
            .foreign_tables
            .read()
            .values()
            .any(|t| t.server_name == name);
        if referenced {
            return Err(format!(
                "foreign server `{name}` is referenced by a foreign table"
            ));
        }
        if !self.durable.foreign_servers.read().contains_key(name) {
            return Ok(false);
        }
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_foreign_server(name)
                .map_err(|err| format!("drop foreign server `{name}`: {err}"))?;
        }
        let removed = self.durable.foreign_servers.write().remove(name).is_some();
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub fn drop_foreign_table(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| engine.drop_foreign_table_inner(name))
    }

    fn drop_foreign_table_inner(&self, name: &str) -> Result<bool, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(name) = self
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(false);
        };
        let relation = RelationIdentity::from_legacy_name(&name)?;
        if let Some(catalog) = self.storage.catalog.as_ref() {
            catalog
                .drop_foreign_table(&relation)
                .map_err(|err| format!("drop foreign table `{name}`: {err}"))?;
        }
        self.extensions
            .foreign_memory_tables
            .write()
            .remove(&relation);
        let removed = self
            .durable
            .foreign_tables
            .write()
            .remove(&relation)
            .is_some();
        if removed {
            self.note_catalog_registry_changed();
        }
        Ok(removed)
    }

    pub fn foreign_server(&self, name: &str) -> Result<Option<uqa_fdw::ForeignServer>, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        Ok(self.durable.foreign_servers.read().get(name).cloned())
    }

    pub fn foreign_table(&self, name: &str) -> Result<Option<uqa_fdw::ForeignTable>, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let Some(resolved) = self
            .resolve_foreign_table_name(name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
        else {
            return Ok(None);
        };
        let relation = RelationIdentity::from_legacy_name(&resolved)?;
        Ok(self.durable.foreign_tables.read().get(&relation).cloned())
    }

    pub fn list_foreign_servers(&self) -> Result<Vec<String>, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut out: Vec<String> = self
            .durable
            .foreign_servers
            .read()
            .keys()
            .cloned()
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn list_foreign_tables(&self) -> Result<Vec<String>, String> {
        self.synchronize_catalog_registries()
            .map_err(|err| format!("refresh FDW catalog: {err}"))?;
        let mut out: Vec<String> = self
            .durable
            .foreign_tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect();
        out.sort();
        Ok(out)
    }

    pub fn foreign_table_columns(&self, table: &str) -> Result<Vec<String>, String> {
        let table = self
            .foreign_table(table)?
            .ok_or_else(|| format!("Foreign table `{table}` does not exist"))?;
        Ok(table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect())
    }

    pub fn load_memory_foreign_table(
        &self,
        table_name: impl Into<String>,
        rows: Vec<uqa_fdw::Row>,
    ) -> std::result::Result<(), String> {
        let table_name = table_name.into();
        let table_name = self
            .resolve_foreign_table_name(&table_name)
            .map_err(|err| format!("resolve foreign table: {err}"))?
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let relation = RelationIdentity::from_legacy_name(&table_name)?;
        let table = self
            .foreign_table(&table_name)?
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)?
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;
        if server.fdw_type != "memory_fdw" {
            return Err(format!(
                "Foreign table `{table_name}` is backed by `{}` not `memory_fdw`",
                server.fdw_type
            ));
        }
        self.extensions
            .foreign_memory_tables
            .write()
            .insert(relation, rows);
        Ok(())
    }

    pub(crate) fn scan_foreign_table_stream<'a>(
        &'a self,
        table_name: &str,
        columns: Option<&[String]>,
        predicates: &[uqa_fdw::FDWPredicate],
        limit: Option<u64>,
    ) -> std::result::Result<
        Box<dyn Iterator<Item = std::result::Result<uqa_fdw::Row, String>> + Send + 'a>,
        String,
    > {
        #[cfg(not(target_os = "emscripten"))]
        use uqa_fdw::FDWHandler as _;

        let table = self
            .foreign_table(table_name)?
            .ok_or_else(|| format!("Foreign table `{table_name}` does not exist"))?;
        let server = self
            .foreign_server(&table.server_name)?
            .ok_or_else(|| format!("Foreign server `{}` does not exist", table.server_name))?;

        let rows: Box<dyn Iterator<Item = std::result::Result<uqa_fdw::Row, String>> + Send + 'a> =
            match server.fdw_type.as_str() {
                "memory_fdw" => {
                    let relation = RelationIdentity::from_legacy_name(&table.name)?;
                    if !self
                        .extensions
                        .foreign_memory_tables
                        .read()
                        .contains_key(&relation)
                    {
                        return Err(format!(
                            "Foreign table `{}` has no loaded memory data",
                            table.name
                        ));
                    }
                    Box::new(MemoryForeignRowStream {
                        engine: self,
                        table_name: relation,
                        columns: columns.map(<[String]>::to_vec),
                        predicates: predicates.to_vec(),
                        limit,
                        index: 0,
                        emitted: 0,
                    })
                }
                #[cfg(not(target_os = "emscripten"))]
                "duckdb_fdw" => {
                    let handler = uqa_fdw::DuckDBHandler::new(server);
                    Box::new(
                        handler
                            .scan_stream(&table, columns, predicates, limit)
                            .map_err(|error| error.to_string())?
                            .map(|row| row.map_err(|error| error.to_string())),
                    )
                }
                #[cfg(not(target_os = "emscripten"))]
                "arrow_fdw" => {
                    let handler = uqa_fdw::ArrowIpcHandler::new(server);
                    Box::new(
                        handler
                            .scan_stream(&table, columns, predicates, limit)
                            .map_err(|error| error.to_string())?
                            .map(|row| row.map_err(|error| error.to_string())),
                    )
                }
                other => return Err(format!("FDW type `{other}` is not available in this build")),
            };
        Ok(rows)
    }
}
