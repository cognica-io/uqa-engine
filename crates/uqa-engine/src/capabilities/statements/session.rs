//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Expose current settings, portals, notification queues and prepared registry state.

use uqa_sql::{
    ast::{ColumnType, DiscardTarget, SetConstraintName, TransactionStmt},
    plan::UnifiedPlan,
    SQLError, SQLParam,
};

use crate::Engine;
use uqa_execution::statement::context::session::{
    PreparedPlanState, StatementControl, StatementNotifications, StatementSettings,
};
impl StatementSettings for Engine {
    fn set_runtime_parameter(
        &self,
        name: &str,
        value: Option<&str>,
        local: bool,
    ) -> Result<(), SQLError> {
        Engine::set_runtime_parameter(self, name, value, local)
    }
    fn reset_all_variables(&self) {
        Engine::reset_all_variables(self);
    }
    fn discard(&self, target: DiscardTarget) -> Result<(), SQLError> {
        Engine::discard(self, target)
    }
    fn load_library(&self, library: &str) -> Result<(), SQLError> {
        Engine::load_library(self, library)
    }
}
impl StatementNotifications for Engine {
    fn notify(&self, channel: &str, payload: &str) -> Result<(), SQLError> {
        Engine::notify(self, channel, payload)
    }
    fn listen(&self, channel: &str) -> Result<(), SQLError> {
        Engine::listen(self, channel)
    }
    fn unlisten(&self, channel: Option<&str>) -> Result<(), SQLError> {
        Engine::unlisten(self, channel)
    }
}
impl StatementControl for Engine {
    fn transaction_failed(&self) -> bool {
        Engine::transaction_failed(self)
    }
    fn run_transaction_statement(&self, statement: TransactionStmt) -> Result<(), SQLError> {
        Engine::run_transaction_statement(self, statement)
    }
    fn set_constraints(
        &self,
        requested: &[SetConstraintName],
        deferred: bool,
        nested_statement: bool,
    ) -> Result<(), SQLError> {
        Engine::set_constraints(self, requested, deferred, nested_statement)
    }
}
impl PreparedPlanState for Engine {
    fn lookup_prepared(&self, name: &str) -> Option<UnifiedPlan> {
        Engine::lookup_prepared(self, name)
    }
    fn register_prepared_plan_with_types(
        &self,
        name: String,
        plan: UnifiedPlan,
        declared: &[ColumnType],
        source_sql: Option<&str>,
    ) -> Result<(), SQLError> {
        Engine::register_prepared_plan_with_types(self, name, plan, declared, source_sql)
    }
    fn prepared_parameter_types(&self, name: &str) -> Option<Vec<Option<ColumnType>>> {
        Engine::prepared_parameter_types(self, name)
    }
    fn prepared_plan_for_execution(
        &self,
        name: &str,
        params: &[SQLParam],
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        Engine::prepared_plan_for_execution(self, name, params)
    }
    fn deallocate_prepared(&self, name: Option<&str>) {
        Engine::deallocate_prepared(self, name);
    }
}
