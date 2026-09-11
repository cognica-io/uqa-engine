//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind named catalog definition operations to the owning Engine registries.

use uqa_sql::{
    ast::{
        ColumnDef, CreateRule, CreateTrigger, DeferredCreateForeignTable, DropRule, DropTrigger,
        GrantTableStmt, TableCheck,
    },
    SQLError,
};

use crate::Engine;
use uqa_execution::statement::context::definitions::{
    EventDefinitions, ForeignDefinitions, TablePrivileges,
};
impl EventDefinitions for Engine {
    fn register_trigger(&self, statement: CreateTrigger) -> Result<(), SQLError> {
        Engine::register_trigger(self, statement)
    }
    fn drop_trigger_sql(&self, statement: &DropTrigger) -> Result<(), SQLError> {
        Engine::drop_trigger_sql(self, statement)
    }
    fn register_rule(&self, statement: CreateRule) -> Result<(), SQLError> {
        Engine::register_rule(self, statement)
    }
    fn drop_rule_sql(&self, statement: &DropRule) -> Result<(), SQLError> {
        Engine::drop_rule_sql(self, statement)
    }
}
impl TablePrivileges for Engine {
    fn grant_table_privileges(&self, statement: &GrantTableStmt) -> Result<(), SQLError> {
        Engine::grant_table_privileges(self, statement)
    }
}
impl ForeignDefinitions for Engine {
    fn register_foreign_server(
        &self,
        name: String,
        fdw_type: String,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), String> {
        Engine::register_foreign_server(self, name, fdw_type, options, if_not_exists)
    }
    fn register_foreign_table_with_checks(
        &self,
        name: String,
        server_name: String,
        columns: Vec<ColumnDef>,
        checks: Vec<TableCheck>,
        options: Vec<(String, String)>,
        if_not_exists: bool,
    ) -> Result<(), SQLError> {
        Engine::register_foreign_table_with_checks(
            self,
            name,
            server_name,
            columns,
            checks,
            options,
            if_not_exists,
        )
    }
    fn register_deferred_foreign_table(
        &self,
        statement: DeferredCreateForeignTable,
    ) -> Result<(), SQLError> {
        Engine::register_deferred_foreign_table(self, statement)
    }
}
