//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-backed scalar and function callback adapters.

use std::sync::Arc;

use uqa_execution::{ExecResult, SharedExpressionEvaluator};
use uqa_sql::expr::RowLookup;

use crate::capabilities::QueryRuntimeView;

use super::super::{
    engine_func_intercept, query_contains_volatile_function, Engine, PhysicalOuterRow, SQLError,
    SQLParam, ScalarExpr, Value,
};
use super::subqueries::CachedCorrelatedExists;
use super::CteScope;

pub(crate) struct ScopedEngineHook<'a> {
    pub(super) engine: &'a Engine,
    pub(super) runtime: QueryRuntimeView<'a>,
    pub(super) ctes: std::borrow::Cow<'a, CteScope>,
}

impl<'a> ScopedEngineHook<'a> {
    pub(in crate::sql) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self {
            engine,
            runtime: engine.query_runtime_view(),
            ctes: std::borrow::Cow::Borrowed(ctes),
        }
    }
}

impl<'a> ScopedEngineHook<'a> {
    pub(crate) fn owned(engine: &'a Engine, ctes: CteScope) -> Self {
        Self {
            engine,
            runtime: engine.query_runtime_view(),
            ctes: std::borrow::Cow::Owned(ctes),
        }
    }
}

impl uqa_sql::plan::AggregateClassifier for ScopedEngineHook<'_> {
    fn is_registered_aggregate(&self, name: &str) -> bool {
        self.engine.has_registered_aggregate_function(name)
    }
}

impl uqa_execution::functions::AggregateFunctionRegistry for ScopedEngineHook<'_> {
    fn registered_aggregate_function(
        &self,
        name: &str,
    ) -> Option<Arc<dyn crate::SQLAggregateFunction>> {
        self.engine.registered_aggregate_function(name)
    }
}

impl uqa_execution::scalar::plan::QueryExpressionContext for ScopedEngineHook<'_> {
    fn expression_evaluator<'a>(&'a self, params: &'a [SQLParam]) -> SharedExpressionEvaluator<'a> {
        EngineExpressionEvaluator::shared(self.engine, params, &self.ctes)
    }

    fn subquery_plans(&self) -> &[uqa_sql::plan::QueryPlan] {
        &self.ctes.scalar_subqueries
    }
}

/// Capture the engine's current execution scope for a shared physical evaluator.
pub(in crate::sql) struct EngineExpressionEvaluator;

impl EngineExpressionEvaluator {
    pub(in crate::sql) fn shared<'a>(
        engine: &'a Engine,
        params: &'a [SQLParam],
        ctes: &CteScope,
    ) -> SharedExpressionEvaluator<'a> {
        uqa_execution::query::expression::ScopedExpressionEvaluator::shared(
            Arc::new(ScopedEngineHook::owned(engine, ctes.clone())),
            params,
            engine.cancellation_token(),
        )
    }
}

struct PreparedCorrelatedExistsPredicate<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: CteScope,
    lookup: Arc<CachedCorrelatedExists>,
    negated: bool,
}

impl uqa_execution::RowPredicate for PreparedCorrelatedExistsPredicate<'_> {
    fn keep_physical(
        &self,
        schema: &uqa_execution::RowSchema,
        row: &uqa_execution::PhysicalRow,
    ) -> ExecResult<bool> {
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let exists = hook.correlated_exists_matches(
            &self.lookup,
            PhysicalOuterRow::Physical { schema, row },
            self.params,
        )?;
        Ok(if self.negated { !exists } else { exists })
    }
}

/// Prepare a simple immutable correlated EXISTS before the outer scan starts. The filter then probes its key set directly, avoiding a scalar-expression walk and shared subquery-cache lock for every outer row.
pub(crate) fn prepare_correlated_exists_predicate<'a>(
    engine: &'a Engine,
    expression: &ScalarExpr,
    params: &'a [SQLParam],
    ctes: &CteScope,
) -> Result<Option<uqa_execution::SharedRowPredicate<'a>>, SQLError> {
    let ScalarExpr::Exists { subquery, negated } = expression else {
        return Ok(None);
    };
    let Some(plan) = ctes.scalar_subqueries.get(*subquery) else {
        return Err(SQLError::Internal(format!(
            "physical scalar subquery slot {subquery} is out of bounds"
        )));
    };
    if query_contains_volatile_function(engine, plan)?
        || !crate::sql::correlation::query_depends_on_outer_row(engine, plan)?
    {
        return Ok(None);
    }
    let hook = ScopedEngineHook::new(engine, ctes);
    let Some(lookup) = hook.build_correlated_exists(plan, params)? else {
        return Ok(None);
    };
    Ok(Some(Arc::new(PreparedCorrelatedExistsPredicate {
        engine,
        params,
        ctes: ctes.clone(),
        lookup,
        negated: *negated,
    })))
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
    fn transaction_timestamp_micros(&self) -> Option<i64> {
        Some(self.engine.transaction_timestamp_micros())
    }

    fn statement_timestamp_micros(&self) -> Option<i64> {
        Some(self.engine.statement_timestamp_micros())
    }

    fn resolve_regtype_input(&self, name: &str) -> Result<Option<i64>, SQLError> {
        uqa_sql::expr::EngineHook::resolve_regtype_input(self.engine, name)
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &uqa_sql::ast::ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        crate::sql::cast_domain_value(self.engine, value, source, target)
    }
    fn resolve_type_name(
        &self,
        name: &str,
    ) -> std::result::Result<Option<uqa_sql::ast::ColumnType>, String> {
        Ok(crate::sql::resolve_catalog_column_type(self.engine, name))
    }

    fn resolve_regclass_input(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        crate::sql::resolve_regclass_oid(self.engine, name)
    }

    fn resolve_regprocedure(&self, name: &str) -> std::result::Result<Option<i64>, String> {
        crate::sql::resolve_regprocedure_oid(self.engine, name)
    }

    fn resolve_regrole(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        crate::sql::resolve_regrole_oid(self.engine, name)
    }

    fn resolve_regnamespace(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        crate::sql::resolve_regnamespace_oid(self.engine, name)
    }

    fn resolve_regobject(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        name: &str,
    ) -> std::result::Result<Option<i64>, SQLError> {
        crate::sql::resolve_regobject_oid(self.engine, ty, name)
    }

    fn resolve_regtype_output(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        oid: i64,
    ) -> std::result::Result<Option<String>, String> {
        crate::sql::resolve_regtype_output(self.engine, ty, oid)
    }

    fn nextval(&self, name: &str) -> std::result::Result<i64, SQLError> {
        self.engine.nextval_sql(name)
    }

    fn currval(&self, name: &str) -> std::result::Result<i64, SQLError> {
        self.engine.currval_sql(name)
    }

    fn lastval(&self) -> std::result::Result<i64, SQLError> {
        self.engine.lastval_sql()
    }

    fn setval(
        &self,
        name: &str,
        value: i64,
        is_called: bool,
    ) -> std::result::Result<i64, SQLError> {
        self.engine.setval_sql(name, value, is_called)
    }

    fn call_scalar_function(
        &self,
        name: &str,
        args: &[Value],
    ) -> Option<std::result::Result<Value, SQLError>> {
        let registration = self.runtime.lookup_scalar_function(name)?;
        Some(registration.function.call(args))
    }

    fn call_bound_builtin_function(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_bound_engine_builtin(self.engine, binding, args)
    }

    fn has_scalar_functions(&self) -> bool {
        self.runtime.has_scalar_functions()
    }

    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        self.engine
            .current_schema_name()
            .map_err(|error| error.to_string())
    }

    fn current_user(&self) -> std::result::Result<Option<String>, String> {
        Ok(Some(self.engine.current_user_name()))
    }

    fn session_user(&self) -> std::result::Result<Option<String>, String> {
        Ok(Some(self.engine.session_user_name()))
    }

    fn current_schemas(
        &self,
        include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        self.engine
            .current_schema_names(include_implicit)
            .map(Some)
            .map_err(|error| error.to_string())
    }

    fn random_value(&self) -> std::result::Result<Option<f64>, String> {
        Ok(Some(self.engine.next_random_value()))
    }

    fn random_u64(&self) -> std::result::Result<Option<u64>, String> {
        Ok(Some(self.engine.next_random_u64()))
    }

    fn set_random_seed(&self, seed: f64) -> std::result::Result<bool, String> {
        self.engine.set_random_seed(seed)?;
        Ok(true)
    }

    fn call_user_function(
        &self,
        name: &str,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_user_scalar_function(self.engine, name, args)
    }

    fn call_bound_user_function(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::sql::call_bound_user_scalar_function(self.engine, binding, args)
    }
}

impl uqa_execution::query::expression::ScalarExpressionContext for ScopedEngineHook<'_> {
    fn intercept_function(
        &self,
        name: &str,
        args: &[ScalarExpr],
        row: &dyn RowLookup,
        evaluate: &mut dyn FnMut(&ScalarExpr) -> Result<Value, SQLError>,
    ) -> Result<Option<Value>, SQLError> {
        engine_func_intercept(Some(self.engine), name, args, row, evaluate)
    }
}
