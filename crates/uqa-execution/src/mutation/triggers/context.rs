//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog, routine, and transaction capabilities for trigger execution.
use super::DeferredConstraintTriggerEvent;
use crate::{
    catalog::context::CatalogContext,
    query::runtime::QueryRuntimeView,
    routines::{context::RoutineExpressions, TriggerRoutineContext},
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_sql::{
    ast::{ColumnType, TriggerEvent, TriggerTiming},
    catalog::events::StoredTrigger,
    routines::SQLUserFunction,
    semantics::{partition::PartitionCatalog, referential::ReferentialCatalog},
    SQLError,
};

/// Trigger definitions and relation row types observed by the active command.
pub trait TriggerCatalog {
    fn has_row_triggers(&self, table: &str, event: TriggerEvent) -> Result<bool, SQLError>;
    fn rule_relation_columns(&self, table: &str) -> Result<Vec<(String, ColumnType)>, SQLError>;
    fn triggers_for(
        &self,
        table: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
        updated_columns: &[String],
    ) -> Result<Vec<StoredTrigger>, SQLError>;
    fn has_trigger_definition(
        &self,
        table: &str,
        timing: TriggerTiming,
        event: TriggerEvent,
        row: bool,
    ) -> Result<bool, SQLError>;
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError>;
}
/// Enter a stored routine using its bound identity and the caller's security context.
pub trait TriggerRoutineInvoker {
    fn resolve_bound_trigger_function(
        &self,
        name: &str,
        object_id: Option<[u8; 16]>,
    ) -> Result<Arc<SQLUserFunction>, SQLError>;
    fn execute_trigger_routine(
        &self,
        function: &SQLUserFunction,
        context: &TriggerRoutineContext,
    ) -> Result<Value, SQLError>;
}
/// Transaction-owned deferred constraint-trigger queue.
pub trait ConstraintTriggerQueue {
    fn constraint_trigger_is_deferred(&self, trigger: &StoredTrigger) -> Result<bool, SQLError>;
    fn defer_constraint_trigger_event(
        &self,
        event: DeferredConstraintTriggerEvent,
    ) -> Result<(), SQLError>;
}
#[derive(Clone, Copy)]
pub struct TriggerContext<'a> {
    pub catalog: &'a dyn TriggerCatalog,
    pub relations: &'a dyn PartitionCatalog,
    pub routines: &'a dyn TriggerRoutineInvoker,
    pub deferrals: &'a dyn ConstraintTriggerQueue,
    pub referrers: &'a dyn ReferentialCatalog,
    pub expressions: &'a dyn RoutineExpressions,
    pub projection: CatalogContext<'a>,
    pub runtime: QueryRuntimeView<'a>,
}
