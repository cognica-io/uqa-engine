//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role, event, foreign relation and table privilege catalog entry points.

use uqa_sql::{
    ast::{ColumnDef, DeferredCreateForeignTable, GrantTableStmt, TableCheck},
    SQLError,
};

pub trait TablePrivileges {
    fn grant_table_privileges(&self, statement: &GrantTableStmt) -> Result<(), SQLError>;
}
pub trait ForeignDefinitions {
    fn register_foreign_server(
        &self,
        name: String,
        fdw_type: String,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), String>;
    fn register_foreign_table_with_checks(
        &self,
        name: String,
        server_name: String,
        columns: Vec<ColumnDef>,
        checks: Vec<TableCheck>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), SQLError>;
    fn register_deferred_foreign_table(
        &self,
        statement: DeferredCreateForeignTable,
    ) -> Result<(), SQLError>;
}
