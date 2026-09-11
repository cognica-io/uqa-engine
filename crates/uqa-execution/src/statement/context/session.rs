//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session settings, notifications, portals, transactions and prepared-plan state.

use uqa_sql::{
    ast::{ColumnType, DiscardTarget, FetchCursorStmt, SetConstraintName, TransactionStmt},
    plan::{QueryPlan, UnifiedPlan},
    SQLError, SQLParam, SQLResult,
};

pub trait StatementSettings {
    fn set_runtime_parameter(
        &self,
        name: &str,
        value: Option<&str>,
        local: bool,
    ) -> Result<(), SQLError>;
    fn reset_all_variables(&self);
    fn discard(&self, target: DiscardTarget) -> Result<(), SQLError>;
    fn load_library(&self, library: &str) -> Result<(), SQLError>;
}
pub trait StatementNotifications {
    fn notify(&self, channel: &str, payload: &str) -> Result<(), SQLError>;
    fn listen(&self, channel: &str) -> Result<(), SQLError>;
    fn unlisten(&self, channel: Option<&str>) -> Result<(), SQLError>;
}
pub trait StatementControl {
    fn transaction_failed(&self) -> bool;
    fn run_transaction_statement(&self, statement: TransactionStmt) -> Result<(), SQLError>;
    fn set_constraints(
        &self,
        requested: &[SetConstraintName],
        deferred: bool,
        nested_statement: bool,
    ) -> Result<(), SQLError>;
}
pub trait StatementPortals {
    fn declare(
        &self,
        parameters: &[SQLParam],
        name: &str,
        binary: bool,
        scroll: Option<bool>,
        hold: bool,
        query: &QueryPlan,
    ) -> Result<SQLResult, SQLError>;
    fn fetch_session_portal(&self, fetch: &FetchCursorStmt) -> Result<SQLResult, SQLError>;
    fn close_session_portal(&self, name: &str) -> Result<(), SQLError>;
    fn close_all_session_portals(&self);
}
pub trait PreparedPlanState {
    fn lookup_prepared(&self, name: &str) -> Option<UnifiedPlan>;
    fn register_prepared_plan_with_types(
        &self,
        name: String,
        plan: UnifiedPlan,
        declared: &[ColumnType],
        source_sql: Option<&str>,
    ) -> Result<(), SQLError>;
    fn prepared_parameter_types(&self, name: &str) -> Option<Vec<Option<ColumnType>>>;
    fn prepared_plan_for_execution(
        &self,
        name: &str,
        params: &[SQLParam],
    ) -> Result<Option<UnifiedPlan>, SQLError>;
    fn deallocate_prepared(&self, name: Option<&str>);
}
#[derive(Clone, Copy)]
pub struct PreparedStatements<'a> {
    pub state: &'a dyn PreparedPlanState,
    pub arguments: &'a dyn crate::query::prepared::PreparedArgumentScopes,
}
