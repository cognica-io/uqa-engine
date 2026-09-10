//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical relational assembly with independent expression and lock construction services.

use crate::query::recheck_source::LockRowsRecheckSource;
use crate::query::runtime::QueryRuntimeView;
use crate::query::set_projection::{SetFunctionRuntime, SetProjectionOutput};
use crate::query::CteScope;
use crate::{PhysicalOperator, ScalarExpr, SharedExpressionEvaluator, SharedRowPredicate};
use std::sync::Arc;
use uqa_sql::{plan::QueryBlockPlan, semantics::sets::SetFunctionCatalog, SQLError, SQLParam};

/// Bind scoped expression services without exposing the owning session or storage state.
pub trait QueryExpressionFactory<S: Clone + 'static>: Sync {
    fn bind_scope(&self, scope: CteScope<S>) -> Arc<dyn SetFunctionRuntime + '_>;
    fn prepare_predicate<'a>(
        &'a self,
        expression: &ScalarExpr,
        params: &'a [SQLParam],
        scope: &CteScope<S>,
    ) -> Result<Option<SharedRowPredicate<'a>>, SQLError>;
}

/// Attach the locking boundary using the caller's relation and transaction lock services.
pub trait RowLockOperatorFactory<S: Clone + 'static>: Sync {
    fn attach_lock_rows<'a>(
        &'a self,
        operator: Box<dyn PhysicalOperator + 'a>,
        statement: &QueryBlockPlan,
        params: &'a [SQLParam],
        scope: &CteScope<S>,
        max_rows: Option<u64>,
        recheck: Option<LockRowsRecheckSource<S>>,
    ) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError>;
}

#[derive(Clone)]
pub struct RelationalContext<'a, S: Clone + 'static> {
    pub catalog: &'a dyn SetFunctionCatalog,
    pub expressions: &'a dyn QueryExpressionFactory<S>,
    pub row_locks: &'a dyn RowLockOperatorFactory<S>,
    pub runtime: QueryRuntimeView<'a>,
}

impl<S: Clone + 'static> Copy for RelationalContext<'_, S> {}

impl<'a, S: Clone + 'static> RelationalContext<'a, S> {
    pub fn expression_scope(self, scope: CteScope<S>) -> Arc<dyn SetFunctionRuntime + 'a> {
        self.expressions.bind_scope(scope)
    }

    pub fn evaluator(
        self,
        params: &'a [SQLParam],
        scope: &CteScope<S>,
    ) -> SharedExpressionEvaluator<'a> {
        crate::query::expression::ScopedExpressionEvaluator::shared(
            self.expression_scope(scope.clone()),
            params,
            self.runtime.cancellation_token(),
        )
    }
}

pub fn build_set_projection<'a, S: Clone + 'static>(
    operator: Box<dyn PhysicalOperator + 'a>,
    context: RelationalContext<'a, S>,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    evaluator: SharedExpressionEvaluator<'a>,
    output: SetProjectionOutput,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    crate::query::set_projection::build_set_projection(
        operator,
        context.catalog,
        context.expression_scope(ctes.clone()),
        params,
        evaluator,
        output,
    )
}

pub fn attach_lock_rows<'a, S: Clone + 'static>(
    context: RelationalContext<'a, S>,
    operator: Box<dyn PhysicalOperator + 'a>,
    statement: &QueryBlockPlan,
    params: &'a [SQLParam],
    ctes: &CteScope<S>,
    max_rows: Option<u64>,
    recheck: Option<LockRowsRecheckSource<S>>,
) -> Result<Box<dyn PhysicalOperator + 'a>, SQLError> {
    context
        .row_locks
        .attach_lock_rows(operator, statement, params, ctes, max_rows, recheck)
}
