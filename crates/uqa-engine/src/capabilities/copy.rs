//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind COPY relation metadata, privileges, and nested statements to the active session.
use crate::{Engine, TableState};
use std::sync::Arc;
use uqa_execution::copy::{CopyExecutionContext, CopyPrivileges, CopyStatements};
use uqa_sql::{
    assignment::columns::ColumnCatalogError,
    ast::{ColumnDef, Statement},
    catalog::security::table::TableAclPrivilege,
    copy::stream::{CopyCatalog, CopyRelation},
    SQLError, SQLParam, SQLResult,
};
impl Engine {
    pub(crate) fn copy_execution_context(&self) -> CopyExecutionContext<'_> {
        CopyExecutionContext {
            catalog: self,
            privileges: self,
            statements: self,
            output: self,
        }
    }
}
struct CopyTable(Arc<TableState>);
impl CopyRelation for CopyTable {
    fn columns(&self) -> Vec<ColumnDef> {
        self.0.columns.read().clone()
    }
    fn is_partitioned(&self) -> bool {
        self.0.hierarchy.read().partition_spec.is_some()
    }
}
impl CopyCatalog for Engine {
    fn resolve_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        self.try_resolve_visible_relation_kind(name)
    }
    fn table(&self, name: &str) -> Result<Option<Box<dyn CopyRelation + '_>>, ColumnCatalogError> {
        self.try_table(name)
            .map(|table| table.map(|table| Box::new(CopyTable(table)) as Box<dyn CopyRelation>))
            .map_err(|error| Box::new(error) as _)
    }
}
impl CopyPrivileges for Engine {
    fn ensure_any_column(&self, table: &str, privilege: TableAclPrivilege) -> Result<(), SQLError> {
        self.ensure_any_column_privilege(table, privilege)
    }
    fn ensure_column(
        &self,
        table: &str,
        column: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        self.ensure_column_privilege(table, column, privilege)
    }
}
impl CopyStatements for Engine {
    fn execute_statement(
        &self,
        statement: Statement,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError> {
        crate::sql::execute_compiled_statement(self, statement, params)
    }
    fn execute_text(&self, text: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        crate::sql::execute(self, text, params)
    }
}
