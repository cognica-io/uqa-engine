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
    pub(crate) fn note_prepared_relation_change(&self, relation: &uqa_core::RelationIdentity) {
        self.mutation_coordinator()
            .note_prepared_relation_change(relation);
    }

    pub(crate) fn note_prepared_table_change(&self, table: &crate::TableState) {
        self.mutation_coordinator()
            .note_prepared_table_change(table);
    }

    pub(crate) fn note_prepared_catalog_change(
        &self,
        change: uqa_execution::statement::prepared::invalidation::PreparedCatalogChange,
    ) {
        self.mutation_coordinator()
            .note_prepared_catalog_change(change);
    }

    pub(crate) fn prepared_definition_context(
        &self,
    ) -> uqa_sql::prepared::definition::PreparedDefinitionContext<'_> {
        uqa_sql::prepared::definition::PreparedDefinitionContext {
            types: self,
            routines: self,
            procedures: self,
            scopes: self,
            session: self,
            aliases: self,
        }
    }

    pub(crate) fn prepared_registration_context(
        &self,
    ) -> uqa_execution::statement::prepared::PreparedRegistrationContext<'_> {
        uqa_execution::statement::prepared::PreparedRegistrationContext {
            analysis: self.prepared_definition_context(),
            aggregates: self,
            registry: self,
            transaction_timestamp_micros: self.session_transaction_clock_micros(),
            interval_style: Some(self.session.interval_style()),
            date_order: Some(uqa_sql::semantics::parameters::datestyle::date_order(
                &self.session.setting("DateStyle"),
            )),
            cancellation: &self.runtime.cancellation,
            clock: uqa_sql::expr::clock_timestamp_micros,
        }
    }
}

impl uqa_sql::prepared::utility::PreparedSessionMetadata for Engine {
    fn show_result_schema(&self, name: &str) -> Result<uqa_sql::RowSchema, SQLError> {
        uqa_execution::statement::show::show_schema(self, name)
    }
    fn show_result_schema_with_control(
        &self,
        name: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::RowSchema, SQLError> {
        uqa_execution::statement::show::show_schema_with_control(self, name, control)
    }
    fn portal_result_schema(&self, name: &str) -> Option<uqa_sql::RowSchema> {
        self.session
            .portals
            .lock()
            .get(name)
            .map(|portal| portal.schema.clone())
    }
    fn prepared_result_schema(&self, name: &str) -> Option<uqa_sql::RowSchema> {
        self.session
            .prepared
            .read()
            .get(name)
            .and_then(|entry| entry.result_schema.clone())
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
                routines: self,
                parameters,
            },
            assignment: self,
            analyze_type: &mut analyze_type,
            evaluation,
        })
    }
}

struct PreparedDefinitionWrite<'a> {
    entries: parking_lot::RwLockWriteGuard<
        'a,
        uqa_core::memory::OwnedMap<
            uqa_core::memory::Produced<String>,
            uqa_sql::prepared::entry::PreparedStatementPlan,
        >,
    >,
    _memory: Option<uqa_core::memory::MemoryReservation>,
}

impl uqa_execution::statement::prepared::PreparedDefinitionWrite for PreparedDefinitionWrite<'_> {
    fn insert(
        &mut self,
        name: uqa_core::memory::Produced<String>,
        definition: uqa_sql::prepared::entry::PreparedStatementPlan,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match control.budget() {
            Some(budget) => {
                self.entries.insert_budgeted(name, definition, budget)?;
            }
            None => {
                self.entries.insert(name, definition);
            }
        }
        Ok(())
    }
}

impl uqa_execution::statement::prepared::PreparedDefinitionRegistry for Engine {
    fn production_resources(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        <Self as uqa_execution::routines::context::RoutineStatements>::planning_resources(self)
    }
    fn write_definitions(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Box<dyn uqa_execution::statement::prepared::PreparedDefinitionWrite + '_>, SQLError>
    {
        let memory = control.reserve(size_of::<PreparedDefinitionWrite<'_>>())?;
        Ok(Box::new(PreparedDefinitionWrite {
            entries: self.session.prepared.write(),
            _memory: memory,
        }))
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
    fn plan_cache_mode_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<String>, SQLError> {
        control.check()?;
        if let Some(mode) = self
            .session
            .state
            .read()
            .session_vars
            .get("plan_cache_mode")
        {
            return Ok(control.copy_text(mode)?);
        }
        if let Some(mode) = self
            .session
            .parameters
            .lock()
            .client_setting_with_control("plan_cache_mode", control)?
        {
            return Ok(control.copy_text(mode)?);
        }
        uqa_sql::semantics::parameters::catalog::find_parameter("plan_cache_mode")
            .expect("defined plan policy")
            .boot_setting_with_control(control)
    }
    fn publish_usage(
        &self,
        name: &str,
        logical_plan: &uqa_sql::plan::SharedExecutablePlan,
        update: uqa_sql::prepared::planning::PreparedPlanUpdate,
    ) {
        if let Some(current) = self.session.prepared.write().get_mut(name) {
            if current.logical_plan.ptr_eq(logical_plan) {
                current.record_execution(update);
            }
        }
    }
}

impl uqa_planner::statement_planning::prepared::selection::PreparedPlanOptimization for Engine {
    fn planning_resources(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        <Self as uqa_execution::routines::context::RoutineStatements>::planning_resources(self)
    }
    fn optimize_plan(
        &self,
        plan: uqa_sql::plan::MutableExecutablePlan,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_sql::plan::SharedExecutablePlan, SQLError> {
        uqa_planner::statement_planning::executable::optimize_owned_with_control(
            &self.statement_planning_context(),
            plan,
            control,
        )?
        .into_shared(control)
    }
    fn estimate_plan(
        &self,
        plan: &UnifiedPlan,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_planner::plan_cost::PlanCost, SQLError> {
        uqa_planner::statement_planning::estimate_plan_with_control(
            self.statement_statistics_context(),
            plan,
            control,
        )
    }
}

impl uqa_sql::prepared::planning::PreparedPlanProvider for Engine {
    fn plan_for_execution(
        &self,
        name: &str,
        parameters: &[SQLParam],
    ) -> Result<Option<uqa_sql::plan::SharedExecutablePlan>, SQLError> {
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
