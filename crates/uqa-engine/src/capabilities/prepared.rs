//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt current routine scope and session parameter metadata to prepared-statement services.

use crate::{capabilities::ScopedEngineHook, Engine};
use uqa_execution::query::prepared::{
    ArgumentBindingContext, PreparedArgumentScopes, ScopedArgumentOperation,
};
use uqa_execution::scalar::plan::PhysicalEvalContext;
use uqa_sql::{
    plan::{ExpressionPlan, UnifiedPlan},
    prepared::arguments::ArgumentValidationContext,
    ColumnType, RowSchema, SQLError, SQLParam,
};

impl Engine {
    pub(crate) fn infer_prepared_parameter_types(
        &self,
        plan: &UnifiedPlan,
        declared: &[Option<ColumnType>],
    ) -> Result<Vec<Option<ColumnType>>, SQLError> {
        let scope = super::query_scope::new_for_current_routine(self);
        uqa_execution::query::binding::infer_prepared_parameter_types(self, plan, declared, &scope)
    }

    pub(crate) fn analyze_prepared_plan(
        &self,
        plan: &UnifiedPlan,
        types: &[Option<ColumnType>],
    ) -> Result<Option<RowSchema>, SQLError> {
        let scope = super::query_scope::new_for_current_routine(self);
        uqa_sql::prepared::analyze_prepared_plan(
            self,
            plan,
            types,
            &uqa_execution::query::binding::binding_context(&scope)?,
        )
    }
}

impl PreparedArgumentScopes for Engine {
    fn with_scope(
        &self,
        parameters: &[SQLParam],
        operation: &mut ScopedArgumentOperation<'_>,
    ) -> Result<Vec<SQLParam>, SQLError> {
        let scope = super::query_scope::new_for_current_routine(self);
        let hook = ScopedEngineHook::new(self, &scope);
        let evaluation = PhysicalEvalContext::new(None, parameters)
            .with_function_hook(&hook)
            .with_subquery_runner(&hook);
        let cast_type = |name: &str| {
            uqa_execution::catalog::projection::resolve_catalog_column_type(
                &self.catalog_execution(),
                name,
            )
        };
        let mut analyze_type = |argument: &ExpressionPlan| {
            uqa_execution::query::binding::analyze_expression_plan_type(
                self, argument, parameters, &scope,
            )
        };
        operation(ArgumentBindingContext {
            validation: ArgumentValidationContext {
                aggregates: self,
                volatility: self,
                cast_type: &cast_type,
            },
            assignment: self,
            analyze_type: &mut analyze_type,
            evaluation,
        })
    }
}
