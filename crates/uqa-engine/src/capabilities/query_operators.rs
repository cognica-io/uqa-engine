//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Supply expression and row-lock construction services from the active engine session.

use crate::{
    capabilities::{query_scope::CteScope, ScopedEngineHook},
    session::StatementReadSnapshot,
    Engine,
};
use std::sync::Arc;
use uqa_execution::query::recheck_source::LockRowsRecheckInput;
use uqa_execution::query::relational::{
    QueryExpressionFactory, RelationalContext, RowLockOperatorFactory,
};
use uqa_execution::query::set_projection::SetFunctionRuntime;
use uqa_execution::{PhysicalOperator, ScalarExpr, SharedRowPredicate};
use uqa_sql::{plan::QueryBlockPlan, SQLError, SQLParam};

impl Engine {
    pub(crate) fn relational_context(&self) -> RelationalContext<'_, StatementReadSnapshot> {
        RelationalContext {
            catalog: self,
            expressions: self,
            row_locks: self,
            runtime: self.query_runtime_view(),
        }
    }
}

impl QueryExpressionFactory<StatementReadSnapshot> for Engine {
    fn production_resources(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        let retained = self.query_retention_control()?;
        Ok(uqa_core::memory::ProductionControl::new(
            retained.memory(),
            retained.cancellation(),
            self.query_runtime_view().cancellation,
        )
        .capture())
    }

    fn optimize_catalog_scalar(&self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        uqa_planner::statement_planning::schema_expressions::optimize_catalog_scalar(
            &self.statement_planning_context(),
            expression,
        )
    }

    fn bind_scope(&self, scope: &CteScope) -> Result<Arc<dyn SetFunctionRuntime + '_>, SQLError> {
        let resources = self.production_resources()?;
        Ok(ScopedEngineHook::owned(self, scope, &resources.control())?)
    }

    fn prepare_predicate<'a>(
        &'a self,
        expression: &ScalarExpr,
        params: &'a [SQLParam],
        scope: &CteScope,
    ) -> Result<Option<SharedRowPredicate<'a>>, SQLError> {
        uqa_execution::query::subqueries::prepare_correlated_exists_predicate(
            &self.subquery_services(),
            expression,
            params,
            scope,
        )
    }
}

impl RowLockOperatorFactory<StatementReadSnapshot> for Engine {
    fn attach_lock_rows<'a>(
        &'a self,
        operator: Box<dyn PhysicalOperator + 'a>,
        statement: &QueryBlockPlan,
        params: &'a [SQLParam],
        scope: &CteScope,
        max_rows: Option<u64>,
        recheck: Option<LockRowsRecheckInput<'_, StatementReadSnapshot>>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
        uqa_execution::query::locking::attach_lock_rows(
            self.row_lock_context(),
            operator,
            statement,
            params,
            scope,
            max_rows,
            recheck,
        )
    }
}
