//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Session settings, notifications, portals, transactions and prepared-plan state.

use uqa_sql::{
    ast::{ColumnType, DiscardTarget, FetchCursorStmt, SetConstraintName, TransactionStmt},
    plan::UnifiedPlan,
    SQLError, SQLResult,
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
    fn in_transaction_block(&self) -> bool;
    fn ensure_session_portal_available(&self, name: &str) -> Result<(), SQLError>;
    fn open_pending_session_portal(
        &self,
        declaration: crate::statement::portal::SessionPortalDeclaration,
    ) -> Result<(), SQLError>;
    fn open_pending_command_session_portal(
        &self,
        declaration: crate::statement::portal::SessionPortalCommandDeclaration,
    ) -> Result<(), SQLError>;
    fn fetch_session_portal(&self, fetch: &FetchCursorStmt) -> Result<SQLResult, SQLError>;
    fn close_session_portal(&self, name: &str) -> Result<(), SQLError>;
    fn close_all_session_portals(&self);
}
pub trait PreparedPlanState {
    fn lookup_prepared(&self, name: &str) -> Option<UnifiedPlan>;
    fn prepared_parameter_types(&self, name: &str) -> Option<Vec<Option<ColumnType>>>;
    fn deallocate_prepared(&self, name: Option<&str>);
}
#[derive(Clone, Copy)]
pub struct PreparedStatements<'a> {
    pub state: &'a dyn PreparedPlanState,
    pub arguments: &'a dyn crate::query::prepared::PreparedArgumentScopes,
    pub definitions: crate::statement::prepared::PreparedRegistrationContext<'a>,
    pub plans: &'a dyn uqa_sql::prepared::planning::PreparedPlanProvider,
}
