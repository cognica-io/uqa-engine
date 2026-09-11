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
    SQLError, SQLParam,
};

impl Engine {
    pub(crate) fn prepared_definition_context(
        &self,
    ) -> uqa_sql::prepared::definition::PreparedDefinitionContext<'_> {
        uqa_sql::prepared::definition::PreparedDefinitionContext {
            types: self,
            routines: self,
            scopes: self,
        }
    }

    pub(crate) fn prepared_registration_context(
        &self,
    ) -> uqa_execution::statement::prepared::PreparedRegistrationContext<'_> {
        uqa_execution::statement::prepared::PreparedRegistrationContext {
            analysis: self.prepared_definition_context(),
            aggregates: self,
            registry: self,
            clock: uqa_sql::expr::clock_timestamp_micros,
        }
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

struct PreparedDefinitionWrite<'a>(
    parking_lot::RwLockWriteGuard<
        'a,
        std::collections::BTreeMap<String, uqa_sql::prepared::entry::PreparedStatementPlan>,
    >,
);

impl uqa_execution::statement::prepared::PreparedDefinitionWrite for PreparedDefinitionWrite<'_> {
    fn insert(
        &mut self,
        name: String,
        definition: uqa_sql::prepared::entry::PreparedStatementPlan,
    ) {
        self.0.insert(name, definition);
    }
}

impl uqa_execution::statement::prepared::PreparedDefinitionRegistry for Engine {
    fn write_definitions(
        &self,
    ) -> Box<dyn uqa_execution::statement::prepared::PreparedDefinitionWrite + '_> {
        Box::new(PreparedDefinitionWrite(self.session.prepared.write()))
    }
}

impl uqa_planner::statement_planning::prepared::selection::PreparedPlanSession for Engine {
    fn prepared_entry(
        &self,
        name: &str,
    ) -> Option<uqa_sql::prepared::entry::PreparedStatementPlan> {
        self.session.prepared.read().get(name).cloned()
    }
    fn plan_cache_mode(&self) -> Result<String, SQLError> {
        self.show_variable("plan_cache_mode")
    }
    fn publish_usage(
        &self,
        name: &str,
        logical_plan: &std::sync::Arc<UnifiedPlan>,
        update: uqa_sql::prepared::planning::PreparedPlanUpdate,
    ) {
        if let Some(current) = self.session.prepared.write().get_mut(name) {
            if std::sync::Arc::ptr_eq(&current.logical_plan, logical_plan) {
                current.record_execution(update);
            }
        }
    }
}

impl uqa_planner::statement_planning::prepared::selection::PreparedPlanOptimization for Engine {
    fn optimize_plan(&self, plan: UnifiedPlan) -> Result<UnifiedPlan, SQLError> {
        super::statement_planning::optimize_engine_plan(self, plan)
    }
    fn estimate_plan(
        &self,
        plan: &UnifiedPlan,
    ) -> Result<uqa_planner::plan_cost::PlanCost, SQLError> {
        super::statement_planning::estimate_engine_plan(self, plan)
    }
}

impl uqa_sql::prepared::planning::PreparedPlanProvider for Engine {
    fn plan_for_execution(
        &self,
        name: &str,
        parameters: &[SQLParam],
    ) -> Result<Option<UnifiedPlan>, SQLError> {
        uqa_planner::statement_planning::prepared::selection::select_plan(
            &uqa_planner::statement_planning::prepared::selection::PreparedPlanningContext {
                session: self,
                analysis: self.prepared_definition_context(),
                optimization: self,
            },
            name,
            parameters,
        )
    }
}
