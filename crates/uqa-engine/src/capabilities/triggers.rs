//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Connect trigger catalog reads, routine entry, and transaction deferral to engine state.
use crate::Engine;
use std::sync::Arc;
use uqa_core::Value;
use uqa_execution::{
    mutation::triggers::{
        context::{ConstraintTriggerQueue, TriggerCatalog, TriggerContext, TriggerRoutineInvoker},
        DeferredConstraintTriggerEvent,
    },
    routines::TriggerRoutineContext,
};
use uqa_sql::{
    ast::{ColumnType, ForeignKey, TriggerEvent, TriggerTiming},
    catalog::events::StoredTrigger,
    routines::SQLUserFunction,
    semantics::referential::ReferentialCatalog,
    SQLError,
};

impl Engine {
    pub(crate) fn trigger_execution_context(&self) -> TriggerContext<'_> {
        TriggerContext {
            catalog: self,
            relations: self,
            routines: self,
            deferrals: self,
            referrers: self,
            expressions: self,
            projection: self.catalog_execution(),
            runtime: self.query_runtime_view(),
        }
    }
}
impl TriggerCatalog for Engine {
    fn has_row_triggers(
        &self,
        table: &str,
        event: uqa_sql::ast::TriggerEvent,
    ) -> Result<bool, SQLError> {
        Engine::has_row_triggers(self, table, event)
    }
    fn rule_relation_columns(&self, table: &str) -> Result<Vec<(String, ColumnType)>, SQLError> {
        self.event_analysis_context().rule_relation_columns(table)
    }
    fn triggers_for(
        &self,
        table: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
        updated_columns: &[String],
    ) -> Result<Vec<StoredTrigger>, SQLError> {
        Engine::triggers_for(self, table, timing, event, row, updated_columns)
    }
    fn has_trigger_definition(
        &self,
        table: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
    ) -> Result<bool, SQLError> {
        Engine::has_trigger_definition(self, table, timing, event, row)
    }
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        Engine::hierarchy_scan_tables(self, table, descendants)
    }
}
impl TriggerRoutineInvoker for Engine {
    fn resolve_bound_trigger_function(
        &self,
        name: &str,
        object_id: Option<[u8; 16]>,
    ) -> Result<Arc<SQLUserFunction>, SQLError> {
        self.event_analysis_context()
            .resolve_bound_trigger_function(name, object_id)
    }
    fn execute_trigger_routine(
        &self,
        function: &SQLUserFunction,
        context: &TriggerRoutineContext,
    ) -> Result<Value, SQLError> {
        crate::capabilities::routine_invocation::execute_trigger_routine(self, function, context)
    }
}
impl ConstraintTriggerQueue for Engine {
    fn constraint_trigger_is_deferred(&self, trigger: &StoredTrigger) -> Result<bool, SQLError> {
        Engine::constraint_trigger_is_deferred(self, trigger)
    }
    fn defer_constraint_trigger_event(
        &self,
        event: DeferredConstraintTriggerEvent,
    ) -> Result<(), SQLError> {
        Engine::defer_constraint_trigger_event(self, event)
    }
}
impl ReferentialCatalog for Engine {
    fn session_replication_role_is_replica(&self) -> bool {
        Engine::session_replication_role_is_replica(self)
    }
    fn hierarchy_ancestor_tables(&self, table: &str) -> Result<Vec<String>, SQLError> {
        Engine::hierarchy_ancestor_tables(self, table)
    }
    fn try_referrers_to(&self, table: &str) -> Result<Vec<(String, ForeignKey)>, String> {
        Engine::try_referrers_to(self, table).map_err(|error| error.to_string())
    }
    fn partition_hierarchy_root(&self, table: &str) -> Result<Option<String>, SQLError> {
        Engine::partition_hierarchy_root(self, table)
    }
}
