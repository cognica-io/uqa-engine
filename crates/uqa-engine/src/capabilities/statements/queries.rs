//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain fresh statement scopes and scoped Engine hooks for native execution.

use crate::{
    capabilities::{
        query_scope::{self, CteScope},
        ScopedEngineHook,
    },
    session::StatementReadSnapshot,
    Engine,
};
use uqa_execution::{
    scalar::plan::PhysicalEvalContext,
    statement::context::queries::{StatementExpressionOperation, StatementQueryContexts},
};
use uqa_sql::{SQLError, SQLParam, SQLResult};
impl StatementQueryContexts<StatementReadSnapshot> for Engine {
    fn statement_scope(&self, privilege_subject: Option<&str>) -> CteScope {
        query_scope::new_for_statement(self, privilege_subject)
    }
    fn query_context(
        &self,
    ) -> uqa_execution::query::statement::context::QueryContext<'_, StatementReadSnapshot> {
        self.query_execution_context()
    }
    fn row_lock_context(
        &self,
    ) -> uqa_execution::query::locking::RowLockContext<'_, StatementReadSnapshot> {
        Engine::row_lock_context(self)
    }
    fn with_expression_context(
        &self,
        scope: &CteScope,
        params: &[SQLParam],
        operation: &mut StatementExpressionOperation<'_>,
    ) -> Result<SQLResult, SQLError> {
        let hook = ScopedEngineHook::new(self, scope);
        let context = PhysicalEvalContext::new(None, params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        operation(&context)
    }
}
