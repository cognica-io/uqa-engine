//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Engine, RelationIdentity};
use uqa_fdw::arrays::{column_type_is_array, normalize_array_columns};

#[cfg(test)]
mod tests;

pub(crate) use uqa_execution::catalog::foreign::StoredForeignTable;

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
            engine
                .foreign_creation_context()
                .register_foreign_server_inner(name, &fdw_type, options, if_not_exists)
        })
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
            engine
                .foreign_creation_context()
                .register_foreign_table_inner(
                    &name,
                    server_name,
                    columns,
                    Vec::new(),
                    options,
                    if_not_exists,
                )
                .map_err(|error| error.to_string())
        })
    }

    pub fn drop_foreign_server(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            engine
                .foreign_creation_context()
                .drop_foreign_server_inner(name)
        })
    }

    pub fn drop_foreign_table(&self, name: &str) -> Result<bool, String> {
        self.with_implicit_string_transaction(|engine| {
            engine.foreign_removal_context().drop_foreign_table(name)
        })
    }

    pub fn foreign_server(&self, name: &str) -> Result<Option<uqa_fdw::ForeignServer>, String> {
        self.foreign_lookup_context().foreign_server(name)
    }

    pub fn foreign_table(&self, name: &str) -> Result<Option<uqa_fdw::ForeignTable>, String> {
        self.foreign_lookup_context().foreign_table(name)
    }

    pub fn list_foreign_servers(&self) -> Result<Vec<String>, String> {
        self.foreign_lookup_context().list_foreign_servers()
    }

    pub fn list_foreign_tables(&self) -> Result<Vec<String>, String> {
        self.foreign_lookup_context().list_foreign_tables()
    }

    pub fn foreign_table_columns(&self, table: &str) -> Result<Vec<String>, String> {
        self.foreign_lookup_context().foreign_table_columns(table)
    }

    pub fn load_memory_foreign_table(
        &self,
        table_name: impl Into<String>,
        rows: Vec<uqa_fdw::Row>,
    ) -> std::result::Result<(), String> {
        let table_name = table_name.into();
        let table_name = self
            .foreign_lookup_context()
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
        let array_columns = table
            .columns
            .iter()
            .filter(|column| column_type_is_array(&column.ty))
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();
        let rows = rows
            .into_iter()
            .map(|row| normalize_array_columns(row, &array_columns))
            .collect::<std::result::Result<Vec<_>, _>>()?;
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
        let array_columns = table
            .columns
            .iter()
            .filter(|column| column_type_is_array(&column.ty))
            .map(|column| column.name.clone())
            .collect::<Vec<_>>();

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
        Ok(Box::new(rows.map(move |row| {
            row.and_then(|row| normalize_array_columns(row, &array_columns))
        })))
    }
}
