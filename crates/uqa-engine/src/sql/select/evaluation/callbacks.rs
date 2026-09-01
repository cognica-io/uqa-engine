//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Engine-backed scalar and function callback adapters.

use std::sync::Arc;

use uqa_execution::{
    ExecResult, ExpressionEvaluator, FunctionTypeResolver, SharedExpressionEvaluator,
};
use uqa_sql::expr::RowLookup;

use crate::engine_capabilities::QueryRuntimeView;

use super::super::{
    bind_query_plan_schema, engine_func_intercept, eval_physical_scalar,
    query_contains_volatile_function, Engine, PhysicalEvalContext, PhysicalOuterRow, SQLError,
    SQLParam, ScalarExpr, Value,
};
use super::subqueries::CachedCorrelatedExists;
use super::CteScope;

pub(in crate::sql) struct ScopedEngineHook<'a> {
    pub(super) engine: &'a Engine,
    pub(super) runtime: QueryRuntimeView<'a>,
    pub(super) ctes: &'a CteScope,
}

impl<'a> ScopedEngineHook<'a> {
    pub(in crate::sql) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self {
            engine,
            runtime: engine.query_runtime_view(),
            ctes,
        }
    }
}

impl CteScope {
    pub(crate) fn new_for_current_routine(engine: &Engine) -> Self {
        let mut scope = Self {
            catalog: Some(engine.catalog_read_view()),
            catalog_resolution: Some(engine.session_execution_view().relation_name_resolution()),
            ..Self::default()
        };
        scope
            .rows
            .extend(crate::sql::triggers::current_transition_relations());
        if crate::engine_roles::active_routine_reads_command_overlay() == Some(false) {
            scope.read_command_overlay = false;
        }
        scope
    }

    pub(in crate::sql) fn new_for_catalog_binding(engine: &Engine) -> Self {
        Self {
            catalog: Some(engine.restored_catalog_read_view()),
            catalog_resolution: Some(engine.session_execution_view().relation_name_resolution()),
            ..Self::default()
        }
    }
}

/// Scalar adapter shared by Filter, Project, and Sort. It binds the engine's registered functions and the query block's physical subquery arena without evaluating any row expression outside the operator tree.
pub(in crate::sql) struct EngineExpressionEvaluator<'a> {
    engine: &'a Engine,
    params: &'a [SQLParam],
    ctes: CteScope,
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
pub(in crate::sql) fn prepare_correlated_exists_predicate<'a>(
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

impl<'a> EngineExpressionEvaluator<'a> {
    pub(in crate::sql) fn shared(
        engine: &'a Engine,
        params: &'a [SQLParam],
        ctes: &CteScope,
    ) -> SharedExpressionEvaluator<'a> {
        Arc::new(Self {
            engine,
            params,
            ctes: ctes.clone(),
        })
    }

    fn evaluate_physical_scoped(
        &self,
        expression: &ScalarExpr,
        schema: &uqa_execution::RowSchema,
        row: &uqa_execution::PhysicalRow,
    ) -> ExecResult<Value> {
        let view = schema.view(row);
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let context = PhysicalEvalContext::from_row_lookup(&view, self.params)
            .with_row_schema(schema)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook)
            .with_physical_outer_row(schema, row);
        if let ScalarExpr::Func { name, args, .. } = expression {
            let mut evaluate = |expr: &ScalarExpr| {
                eval_physical_scalar(expr, &self.ctes.scalar_subqueries, &context)
            };
            if let Some(value) =
                engine_func_intercept(Some(self.engine), name, args, &view, &mut evaluate)?
            {
                return Ok(value);
            }
        }
        Ok(eval_physical_scalar(
            expression,
            &self.ctes.scalar_subqueries,
            &context,
        )?)
    }
}

impl ExpressionEvaluator for EngineExpressionEvaluator<'_> {
    fn evaluate(&self, expression: &ScalarExpr, row: &dyn RowLookup) -> ExecResult<Value> {
        let hook = ScopedEngineHook::new(self.engine, &self.ctes);
        let context = PhysicalEvalContext::from_row_lookup(row, self.params)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        if let ScalarExpr::Func { name, args, .. } = expression {
            let mut evaluate = |expr: &ScalarExpr| {
                eval_physical_scalar(expr, &self.ctes.scalar_subqueries, &context)
            };
            if let Some(value) =
                engine_func_intercept(Some(self.engine), name, args, row, &mut evaluate)?
            {
                return Ok(value);
            }
        }
        Ok(eval_physical_scalar(
            expression,
            &self.ctes.scalar_subqueries,
            &context,
        )?)
    }

    fn evaluate_physical(
        &self,
        expression: &ScalarExpr,
        schema: &uqa_execution::RowSchema,
        row: &uqa_execution::PhysicalRow,
    ) -> ExecResult<Value> {
        self.evaluate_physical_scoped(expression, schema, row)
    }

    fn parameters(&self) -> &[SQLParam] {
        self.params
    }

    fn expression_type(
        &self,
        expression: &ScalarExpr,
        schema: &uqa_execution::RowSchema,
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        uqa_execution::scalar_type_with_resolver(expression, schema, self.params, self)
    }

    fn bind_type_introspection(
        &self,
        expression: ScalarExpr,
        schema: &uqa_execution::RowSchema,
    ) -> ScalarExpr {
        uqa_execution::bind_type_introspection_with_resolver(expression, schema, self.params, self)
    }
}

impl FunctionTypeResolver for EngineExpressionEvaluator<'_> {
    fn has_untyped_function(&self, name: &str) -> bool {
        self.engine.has_untyped_function(name)
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        Ok(crate::sql::resolve_catalog_column_type(self.engine, name))
    }

    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        self.engine.resolve_function_type(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
    ) -> Result<Option<uqa_execution::ResolvedFunctionOverload>, SQLError> {
        self.engine.resolve_function_overload(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
        )
    }

    fn is_scalar_function_binding(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
    ) -> Result<bool, SQLError> {
        self.engine.is_scalar_function_binding(binding)
    }

    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&uqa_sql::ast::FunctionBinding>,
        argument_names: &[Option<String>],
        argument_types: &[Option<uqa_sql::ast::ColumnType>],
        explicit_variadic: bool,
        builtins: &[uqa_execution::BuiltinFunctionOverload],
    ) -> Result<Option<uqa_execution::ResolvedFunctionOverload>, SQLError> {
        self.engine.resolve_function_overload_with_builtins(
            name,
            binding,
            argument_names,
            argument_types,
            explicit_variadic,
            builtins,
        )
    }

    fn resolve_scalar_subquery_type(
        &self,
        subquery: uqa_execution::SubqueryId,
        outer_schema: &uqa_execution::RowSchema,
        params: &[SQLParam],
    ) -> Result<Option<uqa_sql::ast::ColumnType>, SQLError> {
        let plan = self.ctes.scalar_subqueries.get(subquery).ok_or_else(|| {
            SQLError::Internal(format!(
                "physical scalar subquery slot {subquery} is out of bounds"
            ))
        })?;
        let output =
            bind_query_plan_schema(self.engine, plan, params, &self.ctes, Some(outer_schema))?;
        Ok(output.column_type(0).cloned())
    }
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
    fn resolve_type_name(
        &self,
        name: &str,
    ) -> std::result::Result<Option<uqa_sql::ast::ColumnType>, String> {
        Ok(crate::sql::resolve_catalog_column_type(self.engine, name))
    }

    fn resolve_regclass(&self, name: &str) -> std::result::Result<Option<i64>, String> {
        crate::sql::resolve_regclass_oid(self.engine, name)
    }

    fn resolve_regprocedure(&self, name: &str) -> std::result::Result<Option<i64>, String> {
        crate::sql::resolve_regprocedure_oid(self.engine, name)
    }

    fn resolve_regrole(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        crate::sql::resolve_regrole_oid(self.engine, name)
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
