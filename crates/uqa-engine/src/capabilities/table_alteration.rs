//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind table alteration consumers to the active schema, lifecycle registries, and transaction state.
use crate::{session::StatementReadSnapshot, Engine};
use std::collections::BTreeSet;
use uqa_execution::schema::{
    columns::removal::{
        ColumnRemovalContext, ColumnRemovalEvents, ColumnRemovalRoutines, ColumnRemovalState,
        ColumnRemovalViews,
    },
    table_alteration::{TableAlterContext, TableEventLifecycle, TableLifecycle},
};
use uqa_sql::{
    assignment::columns::ColumnCatalogError,
    ast::{CreateFunction, EventEnableMode, ForeignKey, FunctionBinding},
    schema::columns::removal::ColumnRemovalCatalog,
    SQLError,
};
use uqa_storage::StorageBackendResult;
impl Engine {
    pub(crate) fn table_alter_context(&self) -> TableAlterContext<'_, StatementReadSnapshot> {
        TableAlterContext {
            hierarchy: self.hierarchy_execution_context(),
            constraints: self.constraint_alter_context(),
            addition: self.column_addition_context(),
            columns: self.column_alter_context(),
            removal: self.column_removal_context(),
            lifecycle: self,
            events: self,
        }
    }
    pub(crate) fn column_removal_context(&self) -> ColumnRemovalContext<'_> {
        ColumnRemovalContext {
            catalog: self,
            fields: self,
            constraints: self.constraint_alter_context(),
            routines: self,
            events: self,
            views: self,
            state: self,
        }
    }
}
impl TableLifecycle for Engine {
    fn change_owner(&self, table: &str, owner: &str) -> Result<(), SQLError> {
        self.alter_table_role_owner(table, owner)
    }
    fn has_table(&self, table: &str) -> StorageBackendResult<bool> {
        self.try_has_table(table)
    }
    fn rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_table(from, to)
    }
    fn rename_column(&self, table: &str, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_column(table, from, to)
    }
}
impl TableEventLifecycle for Engine {
    fn rename_trigger(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        Engine::rename_trigger(self, table, from, to)
    }
    fn rename_trigger_constraint(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        Engine::rename_trigger_constraint(self, table, from, to)
    }
    fn rename_rule(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        Engine::rename_rule(self, table, from, to)
    }
    fn set_trigger_enable_mode(
        &self,
        table: &str,
        name: Option<&str>,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        Engine::set_trigger_enable_mode(self, table, name, mode)
    }
    fn set_rule_enable_mode(
        &self,
        table: &str,
        name: &str,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        Engine::set_rule_enable_mode(self, table, name, mode)
    }
}
impl ColumnRemovalCatalog for Engine {
    fn try_resolve_table_name(&self, table: &str) -> Result<Option<String>, ColumnCatalogError> {
        Engine::try_resolve_table_name(self, table).map_err(|error| Box::new(error) as _)
    }
    fn table_names(&self) -> Result<Vec<String>, ColumnCatalogError> {
        Engine::table_names(self).map_err(|error| Box::new(error) as _)
    }
    fn try_foreign_keys(&self, table: &str) -> Result<Vec<ForeignKey>, ColumnCatalogError> {
        Engine::try_foreign_keys(self, table).map_err(|error| Box::new(error) as _)
    }
}
impl ColumnRemovalRoutines for Engine {
    fn drop_dependents(&self, table: &str, column: &str, cascade: bool) -> Result<(), SQLError> {
        self.drop_column_routine_dependents(table, column, cascade)
    }
    fn prepare_aliases(
        &self,
        columns: BTreeSet<(String, String)>,
        removed: &[FunctionBinding],
    ) -> Result<Vec<CreateFunction>, SQLError> {
        self.prepare_routine_column_alias_drop(columns, removed)
    }
    fn publish_rewrites(&self, rewritten: Vec<CreateFunction>) -> Result<(), SQLError> {
        self.publish_stored_routine_body_rewrites(rewritten)
    }
    fn refresh_merge_plans(&self) -> Result<(), SQLError> {
        self.refresh_stored_merge_target_plans()
    }
}
impl ColumnRemovalEvents for Engine {
    fn handle_dependencies(
        &self,
        table: &str,
        column: &str,
        cascade: bool,
    ) -> Result<(), SQLError> {
        self.handle_drop_column_event_dependencies(table, column, cascade)
    }
    fn drop_relation_rules(&self, relations: &[String]) -> StorageBackendResult<()> {
        self.drop_rules_depending_on_relations_inner(relations)
    }
}
impl ColumnRemovalViews for Engine {
    fn dependents(&self, table: &str, column: &str) -> StorageBackendResult<Vec<String>> {
        self.views_depending_on_column(table, column)
    }
    fn cascade_closure(&self, views: Vec<String>) -> Result<Vec<String>, SQLError> {
        self.cascade_view_closure(views)
    }
    fn drop_views(&self, views: &[String]) -> Result<(), SQLError> {
        self.drop_views_inner(views, false)
    }
}
impl ColumnRemovalState for Engine {
    fn generated_dependents(&self, table: &str, column: &str) -> StorageBackendResult<Vec<String>> {
        self.generated_columns_referencing_column(table, column)
    }
    fn owned_sequence_dependents(
        &self,
        table: &str,
        column: &str,
    ) -> StorageBackendResult<Vec<String>> {
        self.owned_sequence_dependents_for_column(table, column)
    }
    fn drop_column(&self, table: &str, column: &str, cascade: bool) -> StorageBackendResult<bool> {
        if cascade {
            self.try_drop_column_cascade(table, column)
        } else {
            self.try_drop_column(table, column)
        }
    }
}
