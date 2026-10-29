//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze executable plans before optimizer inputs and constant evaluation.

use super::{rule_inputs::RuleInputPlanningContext, StatementStatisticsContext};
pub use crate::PlanningResources;
use crate::{AggregateClassifier, ConstantEvaluator, QueryPlan, UnifiedPlan};
use std::borrow::Cow;
use uqa_core::memory::ProductionControl;
use uqa_sql::{
    binding::statements::{analyze_executable_plan, AnalyzedResult, StatementAnalysisContext},
    plan::{OwnedExecutablePlan, OwnedQueryPlan, SharedExecutablePlan},
    SQLError, SQLParam,
};

/// Borrow planner metadata after SQL analysis succeeds.
pub trait StatementOptimizationContexts {
    /// Retain the invoking request's allowance and cancellation signal while
    /// optimizer producers run. Standalone planning may remain uncontrolled.
    fn production_resources(&self) -> Result<Option<PlanningResources>, SQLError> {
        Ok(None)
    }
    fn constant_storage(
        &self,
    ) -> Result<Option<std::sync::Arc<dyn uqa_sql::ir::ConstantValueStorage>>, SQLError> {
        Ok(None)
    }
    fn statistics(&self) -> StatementStatisticsContext<'_>;
    fn rule_inputs(&self) -> RuleInputPlanningContext<'_>;
    fn builtin_permissions(
        &self,
    ) -> Option<
        std::sync::Arc<dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution>,
    > {
        None
    }
    fn routine_inlining(&self) -> Option<uqa_sql::routines::inlining::RoutineInliningContext<'_>> {
        None
    }
}

pub struct StatementPlanningContext<'a> {
    pub analysis: StatementAnalysisContext<'a>,
    pub aggregates: &'a dyn AggregateClassifier,
    pub optimization: &'a dyn StatementOptimizationContexts,
    pub constant_evaluator: ConstantEvaluator,
}

impl uqa_sql::plan::ExecutablePlanOptimizer for StatementPlanningContext<'_> {
    fn plan_with_cached_analysis(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
        cached: Option<uqa_sql::binding::statements::AnalyzedStatement>,
    ) -> Result<
        (
            SharedExecutablePlan,
            Option<uqa_sql::binding::statements::AnalyzedStatement>,
        ),
        SQLError,
    > {
        let (plan, retained, reusable) = if let Some((plan, cached)) =
            cached.and_then(|cached| cached.plan_for(params).cloned().map(|plan| (plan, cached)))
        {
            (plan, Some(cached), false)
        } else {
            let mut plan = plan;
            let (_, reusable) = uqa_sql::binding::statements::analyze_reusable_inputs(
                &self.analysis,
                &mut plan,
                params,
            )?;
            (plan, None, reusable)
        };
        let resources = self.optimization.production_resources()?;
        let control = resources
            .as_ref()
            .map_or_else(ProductionControl::uncontrolled, PlanningResources::control);
        let retained = if reusable {
            uqa_sql::binding::statements::AnalyzedStatement::capture_with_control(
                &plan, params, &control,
            )?
        } else {
            retained
        };
        Ok((
            optimize_with_control(self, plan, control)?.into_shared(&control)?,
            retained,
        ))
    }

    fn plan_for_statement_cache(
        &self,
        mut plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<(SharedExecutablePlan, bool), SQLError> {
        let (_, reusable) = uqa_sql::binding::statements::analyze_executable_plan_for_cache(
            &self.analysis,
            &mut plan,
            params,
        )?;
        let reusable = reusable
            && !self
                .optimization
                .routine_inlining()
                .is_some_and(|routines| routines.requires_replanning(&plan));
        Ok((optimize_shared_plan(self, plan)?, reusable))
    }

    fn plan_for_execution(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<SharedExecutablePlan, SQLError> {
        plan_for_execution(self, plan, params)
    }

    fn plan_with_result(
        &self,
        mut plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<(SharedExecutablePlan, AnalyzedResult), SQLError> {
        let result = analyze_executable_plan(&self.analysis, &mut plan, params)?;
        Ok((optimize_shared_plan(self, plan)?, result))
    }
}

pub fn plan_for_execution(
    context: &StatementPlanningContext<'_>,
    mut plan: UnifiedPlan,
    params: &[SQLParam],
) -> Result<SharedExecutablePlan, SQLError> {
    analyze_executable_plan(&context.analysis, &mut plan, params)?;
    optimize_shared_plan(context, plan)
}
/// Move an owned query through optimization; a borrowed catalog or utility query still receives its independent destination.
pub fn optimize_query(
    context: &StatementPlanningContext<'_>,
    query: Cow<'_, QueryPlan>,
) -> Result<OwnedQueryPlan, SQLError> {
    let resources = context.optimization.production_resources()?;
    let control = resources
        .as_ref()
        .map_or_else(ProductionControl::uncontrolled, PlanningResources::control);
    let (query, mut memory) =
        OwnedQueryPlan::from_query(query.into_owned(), &control)?.into_parts();
    let (plan, output) =
        optimize_with_control(context, UnifiedPlan::Query(query), control)?.into_parts();
    memory.absorb(output);
    OwnedExecutablePlan::from_parts(plan, memory)
        .into_query()
        .map_err(|_| SQLError::Internal("query optimization produced a command".into()))
}

/// Cost a selected native executable with the same request resources used by its optimizer.
pub fn estimate_plan(
    context: &StatementPlanningContext<'_>,
    plan: &UnifiedPlan,
) -> Result<crate::plan_cost::PlanCost, SQLError> {
    let resources = context.optimization.production_resources()?;
    let control = resources
        .as_ref()
        .map_or_else(ProductionControl::uncontrolled, PlanningResources::control);
    super::estimate_plan_with_control(context.optimization.statistics(), plan, &control)
}

pub fn optimize_plan(
    context: &StatementPlanningContext<'_>,
    plan: UnifiedPlan,
) -> Result<OwnedExecutablePlan, SQLError> {
    let resources = context.optimization.production_resources()?;
    let control = resources
        .as_ref()
        .map_or_else(ProductionControl::uncontrolled, PlanningResources::control);
    optimize_with_control(context, plan, control)
}

fn optimize_with_control(
    context: &StatementPlanningContext<'_>,
    plan: UnifiedPlan,
    control: ProductionControl<'_>,
) -> Result<OwnedExecutablePlan, SQLError> {
    let mut config = crate::optimizer::OptimizerConfig::new(context.constant_evaluator);
    config.production = control;
    config.builtin_permissions = context.optimization.builtin_permissions();
    config.routine_inlining = context.optimization.routine_inlining();
    let storage = || context.optimization.constant_storage();
    config.constant_storage_factory = Some(crate::optimizer::ConstantStorageFactory(&storage));
    super::optimize_plan_authorized(
        context.optimization.statistics(),
        &context.optimization.rule_inputs(),
        context.aggregates,
        config,
        plan,
    )
}

pub fn optimize_shared_plan(
    context: &StatementPlanningContext<'_>,
    plan: UnifiedPlan,
) -> Result<SharedExecutablePlan, SQLError> {
    let resources = context.optimization.production_resources()?;
    let control = resources
        .as_ref()
        .map_or_else(ProductionControl::uncontrolled, PlanningResources::control);
    optimize_with_control(context, plan, control)?.into_shared(&control)
}

#[cfg(test)]
mod tests;
