//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Role, event, foreign relation and table privilege catalog entry points.

use uqa_sql::{
    ast::{
        AlterRoleStmt, ColumnDef, CreateRoleStmt, CreateRule, CreateTrigger,
        DeferredCreateForeignTable, DropRoleStmt, DropRule, DropTrigger, GrantRoleStmt,
        GrantTableStmt, TableCheck,
    },
    SQLError,
};
pub trait RoleDefinitions {
    fn create_role(&self, statement: &CreateRoleStmt) -> Result<(), SQLError>;
    fn alter_role(&self, statement: &AlterRoleStmt) -> Result<(), SQLError>;
    fn drop_roles(&self, statement: &DropRoleStmt) -> Result<(), SQLError>;
    fn grant_roles(&self, statement: &GrantRoleStmt) -> Result<(), SQLError>;
}
pub trait EventDefinitions {
    fn register_trigger(&self, statement: CreateTrigger) -> Result<(), SQLError>;
    fn drop_trigger_sql(&self, statement: &DropTrigger) -> Result<(), SQLError>;
    fn register_rule(&self, statement: CreateRule) -> Result<(), SQLError>;
    fn drop_rule_sql(&self, statement: &DropRule) -> Result<(), SQLError>;
}
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
