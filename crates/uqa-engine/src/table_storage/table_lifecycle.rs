//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Public table metadata APIs and their existing transaction boundaries.

use super::{table_not_found, Engine, RelationIdentity, StorageBackendResult};

impl Engine {
    /// Drop a table from the catalog and release its in-memory state.
    /// Returns `true` if the table existed.
    pub fn drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.try_drop_table(name)
    }

    pub(crate) fn try_drop_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.with_implicit_storage_transaction(|engine| {
            engine.table_removal_context().drop_table(name)
        })
    }

    pub(crate) fn drop_temporary_table_on_commit_inner(
        &self,
        name: &str,
    ) -> StorageBackendResult<()> {
        self.table_removal_context()
            .drop_temporary_table_on_commit_inner(name)
    }

    pub fn has_table(&self, name: &str) -> StorageBackendResult<bool> {
        self.try_has_table(name)
    }

    pub fn try_has_table(&self, name: &str) -> StorageBackendResult<bool> {
        Ok(self.try_resolve_table_name(name)?.is_some())
    }

    pub(crate) fn try_query_has_table(&self, name: &str) -> StorageBackendResult<bool> {
        Ok(self.try_resolve_query_table_name(name)?.is_some())
    }

    /// All schema-declared columns for `table`, in declaration order.
    pub fn table_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        self.try_table_columns(table)
    }

    pub fn try_table_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        let table_state = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let columns = table_state
            .columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        Ok(columns)
    }

    pub(crate) fn try_query_table_columns(&self, table: &str) -> StorageBackendResult<Vec<String>> {
        let table_state = self
            .try_query_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let columns = table_state
            .columns
            .read()
            .iter()
            .map(|column| column.name.clone())
            .collect();
        Ok(columns)
    }

    pub fn table_has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        self.try_table_has_column(table, column)
    }

    pub fn try_table_has_column(&self, table: &str, column: &str) -> StorageBackendResult<bool> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols.iter().any(|c| c.name == column))
    }

    pub(crate) fn try_query_table_has_column(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<bool> {
        let table = self
            .try_query_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let found = table
            .columns
            .read()
            .iter()
            .any(|candidate| candidate.name == column);
        Ok(found)
    }

    pub(crate) fn column_type(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::ColumnType>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols.iter().find(|c| c.name == column).map(|c| c.ty.clone()))
    }

    /// Return the first SERIAL or identity column name for `table`, if any.
    pub(crate) fn auto_increment_column(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<String>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        Ok(cols
            .iter()
            .find(|c| c.auto_increment.is_some())
            .map(|c| c.name.clone()))
    }

    /// Sequence-generating columns and their durable provenance, in schema order. More than one `SERIAL`/identity column may exist on a table even though only the first one is used as the engine's physical document id.
    pub(crate) fn auto_increment_columns(
        &self,
        table: &str,
    ) -> StorageBackendResult<Vec<(String, uqa_sql::ast::AutoIncrement)>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let columns = t
            .columns
            .read()
            .iter()
            .filter_map(|column| {
                column
                    .auto_increment
                    .clone()
                    .map(|provenance| (column.name.clone(), provenance))
            })
            .collect();
        Ok(columns)
    }

    /// Sorted list of every registered table name.
    pub fn table_names(&self) -> StorageBackendResult<Vec<String>> {
        self.synchronize_table_catalog()?;
        Ok(self
            .storage
            .tables
            .read()
            .keys()
            .map(RelationIdentity::qualified_name)
            .collect())
    }

    /// Snapshot the column schema of `table`. Returns `None` when no
    /// table by that name is registered.
    pub fn describe_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        self.try_describe_table(table)
    }

    pub fn try_describe_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        let Some(table) = self.try_table(table)? else {
            return Ok(None);
        };
        let mut columns = table.columns.read().clone();
        for column in &mut columns {
            if let Some(default) = &mut column.default {
                self.resolve_stored_sequence_references_in_expr(default)?;
            }
            if let Some(generated) = &mut column.generated {
                self.resolve_stored_sequence_references_in_expr(&mut generated.expression)?;
            }
        }
        Ok(Some(columns))
    }

    pub(crate) fn try_describe_query_table(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        let Some(table) = self.try_query_table(table)? else {
            return Ok(None);
        };
        let mut columns = table.columns.read().clone();
        for column in &mut columns {
            if let Some(default) = &mut column.default {
                self.resolve_stored_sequence_references_in_expr(default)?;
            }
            if let Some(generated) = &mut column.generated {
                self.resolve_stored_sequence_references_in_expr(&mut generated.expression)?;
            }
        }
        Ok(Some(columns))
    }

    /// Snapshot catalog column definitions without resolving executable
    /// default or generated expressions. Static row-type validation uses this
    /// while catalog registries are reloaded under the backend transaction
    /// mutex, where opening a sequence session would recursively acquire that
    /// mutex.
    pub(crate) fn try_describe_table_row_type(
        &self,
        table: &str,
    ) -> StorageBackendResult<Option<Vec<uqa_sql::ast::ColumnDef>>> {
        let Some(table) = self.try_table(table)? else {
            return Ok(None);
        };
        let columns = table.columns.read().clone();
        Ok(Some(columns))
    }

    /// DEFAULT expression for `column` on `table`, when one was
    /// declared via `... <col> <type> DEFAULT <expr>`.
    pub fn column_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::Expr>> {
        self.try_column_default_expr(table, column)
    }

    pub fn try_column_default_expr(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Option<uqa_sql::ast::Expr>> {
        let t = self
            .try_table(table)?
            .ok_or_else(|| table_not_found(table))?;
        let cols = t.columns.read();
        let mut default = cols
            .iter()
            .find(|c| c.name == column)
            .and_then(|c| c.default.clone());
        drop(cols);
        if let Some(default) = &mut default {
            self.resolve_stored_sequence_references_in_expr(default)?;
        }
        Ok(default)
    }
}
