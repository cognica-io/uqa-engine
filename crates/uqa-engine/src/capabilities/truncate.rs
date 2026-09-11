//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bind TRUNCATE metadata, live trigger state and the existing table-clear transaction boundary.

use crate::Engine;
use uqa_execution::schema::truncate::{
    TruncateAccess, TruncateContext, TruncateStorage, TruncateTransactions, TruncateTriggers,
    TruncateWrite,
};
use uqa_sql::{
    ast::{TriggerEvent, TriggerTiming},
    catalog::security::table::TableAclPrivilege,
    schema::truncate::TruncateCatalog,
    SQLError,
};

impl Engine {
    pub(crate) fn truncate_context(&self) -> TruncateContext<'_> {
        TruncateContext {
            catalog: self,
            access: self,
            triggers: self,
            storage: self,
            transactions: self,
        }
    }
}
impl TruncateCatalog for Engine {
    fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError> {
        Engine::try_resolve_visible_relation_kind(self, name)
    }
    fn is_partitioned(&self, table: &str) -> Result<bool, String> {
        self.try_table_hierarchy(table)
            .map(|hierarchy| hierarchy.partition_spec.is_some())
            .map_err(|error| error.to_string())
    }
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        Engine::hierarchy_scan_tables(self, table, descendants)
    }
    fn referrers_to(&self, table: &str) -> Result<Vec<String>, String> {
        Engine::referrers_to(self, table)
            .map(|references| references.into_iter().map(|(name, _)| name).collect())
            .map_err(|error| error.to_string())
    }
}
impl TruncateAccess for Engine {
    fn ensure_truncate_privilege(&self, table: &str) -> Result<(), SQLError> {
        self.ensure_table_privilege(table, TableAclPrivilege::Truncate)
    }
}
impl TruncateTriggers for Engine {
    fn ensure_no_pending_trigger_events(
        &self,
        table: &str,
        operation: &str,
    ) -> Result<(), SQLError> {
        Engine::ensure_no_pending_trigger_events(self, table, operation)
    }
    fn fire_statement_trigger(&self, table: &str, timing: TriggerTiming) -> Result<(), SQLError> {
        uqa_execution::mutation::triggers::fire_statement_triggers(
            &self.trigger_execution_context(),
            table,
            timing,
            TriggerEvent::Truncate,
            &[],
        )
    }
}
impl TruncateStorage for Engine {
    fn truncate_tables_with_identity(
        &self,
        tables: &[String],
        restart_identity: bool,
    ) -> Result<(), SQLError> {
        Engine::truncate_tables_with_identity(self, tables, restart_identity)
    }
}
impl TruncateTransactions for Engine {
    fn transaction_depth(&self) -> usize {
        Engine::transaction_depth(self)
    }
    fn with_transaction(&self, operation: TruncateWrite<'_>) -> Result<(), SQLError> {
        self.transaction(|engine| operation(&engine.truncate_context()))
    }
}
