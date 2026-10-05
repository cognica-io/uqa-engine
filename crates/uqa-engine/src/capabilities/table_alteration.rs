//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind table alteration consumers to the active schema, lifecycle registries, and transaction state.
use crate::{session::StatementReadSnapshot, Engine};
use uqa_execution::schema::{
    columns::removal::{ColumnRemovalContext, ColumnRemovalViews},
    table_alteration::{
        binding::TableAlterBindingContext,
        entry::{
            RelationEventAlterContext, RelationEventAlterTransactions, RelationEventAlterWrite,
            TableAlterEntryContext, TableAlterSession, TableAlterTransactions, TableAlterWrite,
        },
        TableAlterContext, TableEventLifecycle, TableLifecycle,
    },
};
use uqa_sql::{ast::EventEnableMode, SQLError, SQLResult};
use uqa_storage::StorageBackendResult;
impl Engine {
    pub(crate) fn table_alter_binding_context(&self) -> TableAlterBindingContext<'_> {
        TableAlterBindingContext {
            names: self,
            catalog: self,
            authority: self.table_privilege_context(),
            creation: self.relation_creation_context(),
            locks: self,
            notices: self.query_runtime_view().notices,
        }
    }

    pub(crate) fn table_alter_entry_context(
        &self,
    ) -> TableAlterEntryContext<'_, StatementReadSnapshot> {
        TableAlterEntryContext {
            session: self,
            binding: self.table_alter_binding_context(),
            tables: self,
            events: self,
            views: self,
            foreign_tables: self,
            sequences: self,
            indexes: self,
            notices: self.query_runtime_view().notices,
        }
    }
    pub(crate) fn table_alter_context(&self) -> TableAlterContext<'_, StatementReadSnapshot> {
        TableAlterContext {
            binding: self.table_alter_binding_context(),
            ownership: self.table_ownership_context(),
            hierarchy: self.hierarchy_execution_context(),
            constraints: self.constraint_alter_context(),
            addition: self.column_addition_context(),
            columns: self.column_alter_context(),
            removal: self.column_removal_context(),
            identities: uqa_execution::schema::table_alteration::identity::IdentityAlterContext {
                definitions: self.sequence_definition_context(),
                catalog: self,
            },
            lifecycle: self,
            events: self,
        }
    }
    pub(crate) fn column_removal_context(&self) -> ColumnRemovalContext<'_> {
        ColumnRemovalContext { deletion: self }
    }
}
impl TableAlterSession for Engine {
    fn in_transaction_block(&self) -> bool {
        Engine::in_transaction_block(self)
    }
}
impl TableAlterTransactions<StatementReadSnapshot> for Engine {
    fn with_table_write(
        &self,
        write: TableAlterWrite<'_, StatementReadSnapshot>,
    ) -> Result<SQLResult, SQLError> {
        self.with_implicit_definition_transaction(|engine| write(&engine.table_alter_context()))
    }
}
impl RelationEventAlterTransactions for Engine {
    fn with_event_write(&self, write: RelationEventAlterWrite<'_>) -> Result<SQLResult, SQLError> {
        self.with_implicit_transaction(|engine| {
            write(&RelationEventAlterContext {
                events: engine,
                foreign_access: engine,
            })
        })
    }
}
impl TableLifecycle for Engine {
    fn rename_table(&self, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_table(from, to)
    }
    fn rename_column(&self, table: &str, from: &str, to: &str) -> StorageBackendResult<bool> {
        self.try_rename_column(table, from, to)
    }
}
impl TableEventLifecycle for Engine {
    fn rename_trigger(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        self.event_lifecycle_context()
            .rename_trigger(table, from, to)
    }
    fn rename_trigger_constraint(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        self.event_lifecycle_context()
            .rename_trigger_constraint(table, from, to)
    }
    fn rename_rule(&self, table: &str, from: &str, to: &str) -> Result<(), SQLError> {
        self.event_lifecycle_context().rename_rule(table, from, to)
    }
    fn set_trigger_enable_mode(
        &self,
        table: &str,
        name: Option<&str>,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        self.event_lifecycle_context()
            .set_trigger_enable_mode(table, name, mode)
    }
    fn set_rule_enable_mode(
        &self,
        table: &str,
        name: &str,
        mode: EventEnableMode,
    ) -> Result<(), SQLError> {
        self.event_lifecycle_context()
            .set_rule_enable_mode(table, name, mode)
    }
}
impl ColumnRemovalViews for Engine {
    fn dependents(&self, table: &str, column: &str) -> StorageBackendResult<Vec<String>> {
        self.views_depending_on_column(table, column)
    }
}
