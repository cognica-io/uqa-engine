//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;
use uqa_core::Value;
use uqa_execution::routines::{
    context::{RoutineExpressions, RoutinePortals, RoutineStatements, RoutineTransactions},
    transaction::RoutineSessionId,
    RoutineContext,
};
use uqa_sql::{
    assignment::routines::RoutineValueContext,
    ast::{ColumnType, Expr, FetchCursorStmt, Statement},
    plan::{ExpressionPlan, UnifiedPlan},
    SQLError, SQLParam, SQLResult,
};
impl Engine {
    pub(crate) fn routine_session_id(&self) -> RoutineSessionId {
        RoutineSessionId(std::sync::Arc::as_ptr(&self.session) as usize)
    }
    pub(crate) fn routine_execution_context(&self) -> RoutineContext<'_> {
        RoutineContext {
            expressions: self,
            statements: self,
            transactions: self,
            portals: self,
            session: self.routine_session_id(),
            runtime: self.query_runtime_view(),
        }
    }
}
impl RoutineValueContext for Engine {
    fn catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        crate::sql::resolve_catalog_column_type(self, name)
    }
}
impl RoutineExpressions for Engine {
    fn column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        crate::sql::resolve_catalog_column_type_name(self, name)
    }
    fn evaluate(&self, expression: &Expr) -> Result<Value, SQLError> {
        crate::sql::scalar::eval_lowered_expression(self, expression, None, &[])
    }
    fn evaluate_with_type(
        &self,
        expression: &Expr,
    ) -> Result<(Value, Option<ColumnType>), SQLError> {
        crate::sql::scalar::eval_lowered_expression_with_type(self, expression, None, &[])
    }
    fn expression_type(&self, plan: &ExpressionPlan) -> Result<Option<ColumnType>, SQLError> {
        let mut scope = super::query_scope::new_for_current_routine(self);
        scope.scalar_subqueries.clone_from(&plan.subqueries);
        let hook = uqa_execution::query::relational::context::QueryExpressionFactory::bind_scope(
            self, scope,
        );
        uqa_execution::scalar_type_with_resolver(
            &plan.scalar,
            &uqa_execution::RowSchema::default(),
            &[],
            hook.as_ref(),
        )
    }
}
impl RoutineStatements for Engine {
    fn execute_plan(&self, plan: &UnifiedPlan, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        let plan = crate::sql::plan_for_execution(self, plan.clone(), params)?;
        crate::sql::UnifiedPlanExecutor::new_nested(self.statement_execution_context(), params)
            .execute(&plan)
    }

    fn execute_bound(
        &self,
        statement: Statement,
        params: &[SQLParam],
    ) -> Result<SQLResult, SQLError> {
        crate::sql::execute_compiled_statement(self, statement, params)
    }
    fn execute_text(&self, text: &str, params: &[SQLParam]) -> Result<SQLResult, SQLError> {
        uqa_execution::statement::batch::execute_nested(
            &self.batch_execution_context(),
            text,
            params,
        )
    }
    fn optimize_plan(&self, plan: UnifiedPlan) -> Result<UnifiedPlan, SQLError> {
        crate::sql::optimize_engine_plan(self, plan)
    }
    fn assertions_enabled(&self) -> bool {
        self.plpgsql_asserts_enabled()
    }
}
impl RoutineTransactions for Engine {
    fn depth(&self) -> usize {
        self.transaction_depth()
    }
    fn begin(&self) -> Result<(), SQLError> {
        self.begin()
    }
    fn commit(&self) -> Result<(), SQLError> {
        self.commit()
    }
    fn rollback(&self) -> Result<(), SQLError> {
        self.rollback()
    }
    fn finish_procedural_transaction(&self, commit: bool, chain: bool) -> Result<(), SQLError> {
        self.finish_procedural_transaction(commit, chain)
    }
}
impl RoutinePortals for Engine {
    fn ensure_available(&self, name: &str) -> Result<(), SQLError> {
        crate::sql::session_portal_worker::ensure_plpgsql_session_portal_available(self, name)
    }
    fn open(
        &self,
        params: &[SQLParam],
        name: &str,
        scroll: Option<bool>,
        plan: &UnifiedPlan,
    ) -> Result<(), SQLError> {
        crate::sql::session_portal_worker::open_plpgsql_session_portal(
            self, params, name, scroll, plan,
        )
    }
    fn fetch(&self, request: &FetchCursorStmt) -> Result<SQLResult, SQLError> {
        self.fetch_session_portal(request)
    }
    fn close(&self, name: &str) -> Result<(), SQLError> {
        self.close_session_portal(name)
    }
    fn allocate_name(&self) -> String {
        self.allocate_session_portal_name()
    }
    fn pin(&self, name: &str) -> Result<(), SQLError> {
        self.pin_session_portal(name)
    }
    fn unpin(&self, name: &str) -> Result<(), SQLError> {
        self.unpin_session_portal(name)
    }
}
