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
    catalog: catalog::ScopedCatalog,
    cast_scope: std::sync::OnceLock<uqa_sql::expr::CatalogCastScope>,
    routine_scope: std::sync::OnceLock<routine_descriptions::NativeRoutineScope>,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl<'a> ScopedEngineHook<'a> {
    pub(crate) fn new(engine: &'a Engine, ctes: &'a CteScope) -> Self {
        Self {
            engine,
            runtime: engine.query_runtime_view(),
            ctes: std::borrow::Cow::Borrowed(ctes),
            catalog: catalog::ScopedCatalog::new(engine, ctes),
            cast_scope: std::sync::OnceLock::new(),
            routine_scope: std::sync::OnceLock::new(),
            _memory: None,
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

    pub(crate) fn owned(
        engine: &'a Engine,
        ctes: &CteScope,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Arc<Self>, SQLError> {
        let ctes = ctes.copy_with_control(control)?;
        let memory = control.reserve(size_of::<Self>())?;
        Ok(Arc::new(Self {
            engine,
            runtime: engine.query_runtime_view(),
            catalog: catalog::ScopedCatalog::new(engine, &ctes),
            cast_scope: std::sync::OnceLock::new(),
            routine_scope: std::sync::OnceLock::new(),
            ctes: std::borrow::Cow::Owned(ctes),
            _memory: memory,
        }))
    }
}

impl uqa_sql::plan::AggregateClassifier for ScopedEngineHook<'_> {
    fn is_registered_aggregate(&self, name: &str) -> bool {
        self.engine.has_registered_aggregate_function(name)
    }

    fn is_registered_aggregate_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        self.runtime
            .has_aggregate_function_with_control(name, control)
    }
}

impl uqa_execution::functions::AggregateFunctionRegistry for ScopedEngineHook<'_> {
    fn registered_aggregate_function(
        &self,
        name: &str,
    ) -> Option<Arc<dyn crate::SQLAggregateFactory>> {
        self.engine.registered_aggregate_function(name)
    }
}

impl uqa_execution::scalar::plan::QueryExpressionContext for ScopedEngineHook<'_> {
    fn expression_evaluator<'a>(
        &'a self,
        params: &'a [SQLParam],
    ) -> Result<SharedExpressionEvaluator<'a>, SQLError> {
        let resources =
            uqa_execution::query::relational::QueryExpressionFactory::production_resources(
                self.engine,
            )?;
        uqa_execution::query::expression::ScopedExpressionEvaluator::borrowed_with_control(
            self,
            params,
            self.engine.cancellation_token(),
            &resources.control(),
        )
    }

    fn subquery_plans(&self) -> &[uqa_sql::plan::QueryPlan] {
        &self.ctes.scalar_subqueries
    }
}

impl uqa_sql::expr::EngineHook for ScopedEngineHook<'_> {
    fn construct_range(
        &self,
        oid: u32,
        bounds: uqa_sql::expr::RangeBoundsInput,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Value>, SQLError> {
        uqa_execution::catalog::range_type::input::construct_range(
            &self
                .engine
                .range_input_context(&self.ctes.catalog_read_view()?, self),
            oid,
            bounds,
            control,
        )
    }

    fn construct_multirange(
        &self,
        oid: u32,
        members: &[Value],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Value>, SQLError> {
        uqa_execution::catalog::range_type::input::construct_multirange(
            &self
                .engine
                .range_input_context(&self.ctes.catalog_read_view()?, self),
            oid,
            members,
            control,
        )
    }

    fn sql_notice(&self, notice: uqa_sql::SQLNotice) {
        self.engine.push_sql_notice(notice);
    }

    fn catalog_cast_scope(&self) -> Option<&uqa_sql::expr::CatalogCastScope> {
        // Cast names read this hook's immutable CTE catalog; SQL restricts retention to spellings independent of live namespace settings.
        Some(self.cast_scope.get_or_init(Default::default))
    }

    fn read_range_input(
        &self,
        oid: u32,
        text: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Value>, SQLError> {
        uqa_execution::catalog::range_type::input::read_input(
            &self
                .engine
                .range_input_context(&self.ctes.catalog_read_view()?, self),
            oid,
            text,
            control,
        )
    }

    fn require_builtin_execute(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
    ) -> Result<(), SQLError> {
        use uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution;
        uqa_execution::catalog::security::builtin_routines::execution::BuiltinRoutinePermissions::capture(
            &self.catalog_context(),
        ).require_execute(binding)
    }

    fn require_builtin_execute_with_control(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        use uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution;
        control.check()?;
        uqa_execution::catalog::security::builtin_routines::execution::BuiltinRoutinePermissions::capture(
            &self.catalog_context(),
        ).require_execute_with_control(binding, control)
    }

    fn transaction_timestamp_micros(&self) -> Option<i64> {
        Some(self.engine.transaction_timestamp_micros())
    }

    fn statement_timestamp_micros(&self) -> Option<i64> {
        Some(self.engine.statement_timestamp_micros())
    }

    fn resolve_oid_alias_input_with_control(
        &self,
        ty: &uqa_sql::ColumnType,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_oid_alias_input_with_control(
            &self.catalog_context(),
            ty,
            name,
            control,
        )
    }

    fn resolve_regtype_input(&self, name: &str) -> Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regtype_oid(&self.catalog_context(), name)?
            .map(Some)
            .ok_or_else(|| SQLError::Routine {
                sqlstate: "42704".into(),
                message: format!("type \"{name}\" does not exist"),
            })
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &uqa_sql::ast::ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        uqa_sql::assignment::domain::cast_domain_value(self.engine, value, source, target)
    }
    fn cast_domain_with_control(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &uqa_sql::ColumnType,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<Value>>, SQLError> {
        uqa_sql::assignment::domain::cast_domain_value_with_control(
            self.engine,
            value,
            source,
            target,
            control,
        )
    }
    fn resolve_type_name(
        &self,
        name: &str,
    ) -> std::result::Result<Option<uqa_sql::ast::ColumnType>, String> {
        let catalog = self
            .ctes
            .catalog_read_view()
            .map_err(|error| error.to_string())?;
        Ok(
            uqa_execution::catalog::projection::resolve_catalog_column_type(
                &uqa_execution::catalog::context::CatalogContext {
                    catalog: &catalog,
                    ..self.engine.catalog_execution()
                },
                name,
            ),
        )
    }

    fn resolve_type_oid(&self, oid: u32) -> Result<Option<uqa_sql::ast::ColumnType>, String> {
        Ok(
            uqa_execution::catalog::projection::resolve_catalog_user_type_by_oid(
                &self.catalog_context(),
                oid,
            ),
        )
    }

    fn resolve_type_name_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<uqa_sql::ColumnType>>, SQLError> {
        uqa_execution::catalog::projection::resolve_catalog_column_type_with_control(
            &self.catalog_context(),
            name,
            control,
        )
    }

    fn resolve_type_oid_with_control(
        &self,
        oid: u32,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<uqa_sql::ColumnType>>, SQLError> {
        uqa_execution::catalog::projection::resolve_catalog_user_type_by_oid_with_control(
            &self.catalog_context(),
            oid,
            control,
        )
    }

    fn type_oid_exists(&self, oid: u32) -> Result<bool, String> {
        uqa_execution::catalog::projection::type_oid_exists(&self.catalog_context(), oid)
            .map_err(|error| error.to_string())
    }

    fn resolve_regclass_input(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regclass_oid(&self.catalog_context(), name)
    }

    fn resolve_regproc(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regproc_input_oid(&self.catalog_context(), name)
            .map(Some)
    }

    fn resolve_regprocedure(&self, name: &str) -> std::result::Result<Option<i64>, String> {
        uqa_execution::catalog::projection::resolve_regprocedure_oid(&self.catalog_context(), name)
    }

    fn resolve_regprocedure_input(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regprocedure_input_oid(
            &self.catalog_context(),
            name,
        )
        .map(Some)
    }

    fn resolve_regrole(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regrole_oid(&self.catalog_context(), name)
    }

    fn resolve_regnamespace(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regnamespace_oid(&self.catalog_context(), name)
    }

    fn resolve_regcollation(&self, name: &str) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regcollation_oid(&self.catalog_context(), name)
    }

    fn resolve_regobject(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        name: &str,
    ) -> std::result::Result<Option<i64>, SQLError> {
        uqa_execution::catalog::projection::resolve_regobject_oid(&self.catalog_context(), ty, name)
    }

    fn resolve_regtype_output(
        &self,
        ty: &uqa_sql::ast::ColumnType,
        oid: i64,
    ) -> std::result::Result<Option<String>, String> {
        uqa_execution::catalog::projection::resolve_regtype_output(&self.catalog_context(), ty, oid)
    }

    fn enum_labels(&self) -> Option<&dyn uqa_sql::expr::enums::EnumLabelCatalog> {
        Some(self.engine)
    }

    fn composite_types(&self) -> Option<&dyn uqa_sql::expr::composites::CompositeTypeCatalog> {
        Some(self)
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
        Some(
            uqa_sql::expr::enums::render_host_arguments(Some(self.engine), args).and_then(|args| {
                registration
                    .function
                    .call_with_context(self.runtime.function_context, &args)
            }),
        )
    }

    fn call_bound_builtin_function(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
    ) -> Option<std::result::Result<Value, SQLError>> {
        uqa_execution::query::scalar_functions::call_bound_builtin(
            &self.scalar_function_context(),
            binding,
            args,
        )
    }

    fn has_scalar_functions(&self) -> bool {
        self.runtime.has_scalar_functions()
    }

    fn call_bound_builtin_function_with_control(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Option<Result<uqa_core::memory::Produced<Value>, SQLError>> {
        uqa_execution::query::scalar_functions::call_bound_builtin_with_control(
            &self.scalar_function_context(),
            binding,
            args,
            control,
        )
    }

    fn current_schema(&self) -> std::result::Result<Option<String>, String> {
        self.catalog_context()
            .current_schema_name()
            .map_err(|error| error.to_string())
    }

    fn current_user(&self) -> std::result::Result<Option<String>, SQLError> {
        Ok(Some(
            self.engine
                .current_role()
                .require_name(&self.engine.durable.roles.read())?
                .to_owned(),
        ))
    }

    fn session_user(&self) -> std::result::Result<Option<String>, SQLError> {
        Ok(Some(
            self.engine
                .session_role()
                .require_name(&self.engine.durable.roles.read())?
                .to_owned(),
        ))
    }

    fn runtime_parameter(&self, name: &str) -> std::result::Result<Option<String>, SQLError> {
        Ok(self.engine.session_execution_view().runtime_parameter(name))
    }

    fn temporal_interval_style(
        &self,
    ) -> std::result::Result<Option<uqa_core::TemporalIntervalStyle>, SQLError> {
        Ok(Some(self.engine.session.interval_style()))
    }

    fn temporal_time_zone(
        &self,
    ) -> std::result::Result<Option<uqa_core::TemporalTimeZone>, SQLError> {
        Ok(Some(self.engine.session.time_zone()))
    }

    fn sleep(&self, duration: std::time::Duration) -> std::result::Result<(), SQLError> {
        Ok(self.engine.runtime.cancellation.sleep(duration)?)
    }

    fn set_runtime_parameter(
        &self,
        name: &str,
        value: Option<&str>,
        local: bool,
    ) -> std::result::Result<String, SQLError> {
        self.engine.set_config(name, value, local)
    }

    fn current_schemas(
        &self,
        include_implicit: bool,
    ) -> std::result::Result<Option<Vec<String>>, String> {
        self.catalog_context()
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

    fn routine_description_scope(
        &self,
    ) -> Option<uqa_sql::routines::invocation::descriptions::RoutineDescriptionScope> {
        ScopedEngineHook::routine_description_scope(self)
    }

    fn call_bound_user_function_with_control(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Option<Result<uqa_core::memory::Produced<Value>, SQLError>> {
        uqa_execution::routines::invocation::evaluate_bound_scalar_for_planning(
            &self.engine.routine_invocation_context(),
            binding,
            args,
            control,
        )
    }

    fn call_bound_user_function_with_descriptions(
        &self,
        binding: &uqa_sql::ast::FunctionBinding,
        args: &[(Option<String>, Value)],
        descriptions: &uqa_sql::routines::invocation::descriptions::RoutineCallDescriptions,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Option<Result<Value, SQLError>> {
        self.call_described_routine(binding, args, descriptions, control)
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
            Some(&self.scalar_function_context()),
            name,
            args,
            row,
            evaluate,
        )
    }
}

mod catalog;
mod function_invocation;
mod routine_descriptions;
mod type_resolution;

use uqa_execution::RowSchema;
use uqa_sql::ResultRow;

pub(crate) fn eval_lowered_expression(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: Option<&ResultRow>,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine)?;
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
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine)?;
    uqa_execution::query::catalog_expression::eval_lowered_expression_with_type(
        engine, scope, expression, row, params,
    )
}

pub(crate) fn eval_lowered_expression_with_schema(
    engine: &Engine,
    expression: &uqa_sql::ast::Expr,
    row: &dyn uqa_sql::expr::RowLookup,
    schema: &RowSchema,
    params: &[SQLParam],
) -> Result<Value, SQLError> {
    let scope = crate::capabilities::query_scope::new_for_current_routine(engine)?;
    uqa_execution::query::catalog_expression::eval_lowered_expression_with_schema(
        engine, scope, expression, row, schema, params,
    )
}
