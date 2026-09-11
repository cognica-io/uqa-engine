//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-backed scalar and function callback adapters.

use std::sync::Arc;

use uqa_execution::SharedExpressionEvaluator;
use uqa_sql::expr::RowLookup;

use crate::capabilities::QueryRuntimeView;

use super::query_scope::CteScope;
use crate::Engine;
use uqa_core::Value;
use uqa_sql::{SQLError, SQLParam, ScalarExpr};

pub(crate) struct ScopedEngineHook<'a> {
    engine: &'a Engine,
    runtime: QueryRuntimeView<'a>,
    ctes: std::borrow::Cow<'a, CteScope>,
}

impl<'a> ScopedEngineHook<'a> {
    pub(crate) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self {
            engine,
            runtime: engine.query_runtime_view(),
            ctes: std::borrow::Cow::Borrowed(ctes),
        }
    }
}

impl<'a> ScopedEngineHook<'a> {
    pub(crate) fn subquery_context(
        &self,
    ) -> uqa_execution::query::subqueries::SubqueryContext<'_, crate::session::StatementReadSnapshot>
    {
        uqa_execution::query::subqueries::SubqueryContext {
            services: self.engine.subquery_services(),
            memory: self.runtime.settings,
            ctes: &self.ctes,
            function_hook: self,
            subquery_runner: self,
        }
    }

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
struct EngineExpressionEvaluator;

impl EngineExpressionEvaluator {
    fn shared<'a>(
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
        uqa_sql::assignment::domain::cast_domain_value(self.engine, value, source, target)
    }
    fn resolve_type_name(
        &self,
        name: &str,
    ) -> std::result::Result<Option<uqa_sql::ast::ColumnType>, String> {
        Ok(
            uqa_execution::catalog::projection::resolve_catalog_column_type(
                &self.engine.catalog_execution(),
                name,
            ),
        )
    }

    fn resolve_regclass_input(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regclass_oid(
            &self.engine.catalog_execution(),
            name,
        )
    }

    fn resolve_regprocedure(&self, name: &str) -> std::result::Result<Option<i64>, String> {
        uqa_execution::catalog::projection::resolve_regprocedure_oid(
            &self.engine.catalog_execution(),
            name,
        )
    }

    fn resolve_regrole(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regrole_oid(
            &self.engine.catalog_execution(),
            name,
        )
    }

    fn resolve_regnamespace(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regnamespace_oid(
            &self.engine.catalog_execution(),
            name,
        )
    }

    fn resolve_regobject(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        name: &str,
    ) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regobject_oid(
            &self.engine.catalog_execution(),
            ty,
            name,
        )
    }

    fn resolve_regtype_output(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        oid: i64,
    ) -> std::result::Result<Option<String>, String> {
        uqa_execution::catalog::projection::resolve_regtype_output(
            &self.engine.catalog_execution(),
            ty,
            oid,
        )
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
        uqa_execution::query::scalar_functions::call_bound_builtin(
            &self.engine.scalar_function_context(),
            binding,
            args,
        )
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
        crate::capabilities::routine_invocation::call_user_scalar_function(self.engine, name, args)
    }

    fn call_bound_user_function(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        crate::capabilities::routine_invocation::call_bound_user_scalar_function(
            self.engine,
            binding,
            args,
        )
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
        uqa_execution::query::scalar_functions::intercept_function(
            Some(&self.engine.scalar_function_context()),
            name,
            args,
            row,
            evaluate,
        )
    }
}

mod function_invocation;
mod type_resolution;

use uqa_execution::{PhysicalRow, RowSchema};
use uqa_sql::{plan::ExpressionPlan, ResultRow};

pub(crate) fn eval_lowered_expression(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::query::catalog_expression::eval_lowered_expression(
        engine, scope, expression, row, params,
    )
}

pub(crate) fn eval_lowered_expression_with_type(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<(Value, Option<uqa_sql::ColumnType>), SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::query::catalog_expression::eval_lowered_expression_with_type(
        engine, scope, expression, row, params,
    )
}

pub(crate) fn eval_lowered_expression_with_schema(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: &ResultRow,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine);
    uqa_execution::query::catalog_expression::eval_lowered_expression_with_schema(
        engine, scope, expression, row, schema, params,
    )
}

pub(crate) fn eval_stored_expression_plan_with_row(
    engine: &Engine,
    expression: &ExpressionPlan,
    schema: &RowSchema,
    row: &PhysicalRow,
    params: &[SQLParam],
    privilege_subject: Option<&str>,
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_statement(engine, privilege_subject);
    uqa_execution::query::catalog_expression::eval_stored_expression_plan_with_row(
        engine, scope, expression, schema, row, params,
    )
}
