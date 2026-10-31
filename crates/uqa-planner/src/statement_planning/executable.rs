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
    binding::statements::{AnalyzedResult, StatementAnalysisContext},
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
    /// Native storage constructors admit retained metadata before allocation. External adapters may keep their existing factory and are checked at the callback boundary.
    fn constant_storage_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<std::sync::Arc<dyn uqa_sql::ir::ConstantValueStorage>>, SQLError> {
        control.check()?;
        let storage = self.constant_storage()?;
        control.check()?;
        Ok(storage)
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
    /// Native permission owners admit their shared state before allocation. External factories retain their established callback boundary.
    fn builtin_permissions_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<
        Option<
            std::sync::Arc<
                dyn uqa_sql::catalog::security::builtin_routines::BuiltinRoutineExecution,
            >,
        >,
        SQLError,
    > {
        control.check()?;
        let permissions = self.builtin_permissions();
        control.check()?;
        Ok(permissions)
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
    fn planning_control(&self) -> Result<uqa_core::memory::OwnedProductionControl, SQLError> {
        self.analysis.scopes.planning_control()
    }

    fn plan_owned_for_execution(
        &self,
        plan: uqa_sql::plan::MutableExecutablePlan,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<SharedExecutablePlan, SQLError> {
        self.plan_owned_with_result(plan, params, control)
            .map(|(plan, _)| plan)
    }

    fn plan_owned_with_result(
        &self,
        plan: uqa_sql::plan::MutableExecutablePlan,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(SharedExecutablePlan, AnalyzedResult), SQLError> {
        let (plan, result, _) = uqa_sql::binding::statements::analyze_reusable_inputs_owned(
            &self.analysis,
            plan,
            params,
            control,
        )?;
        with_optimizer_resources(self, plan, control, |plan, control| {
            Ok((
                optimize_owned_with_control(self, plan, control)?.into_shared(control)?,
                result,
            ))
        })
    }

    fn plan_owned_for_statement_cache(
        &self,
        plan: uqa_sql::plan::MutableExecutablePlan,
        params: &[SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<(SharedExecutablePlan, bool), SQLError> {
        let (plan, _, reusable) = uqa_sql::binding::statements::analyze_reusable_inputs_owned(
            &self.analysis,
            plan,
            params,
            control,
        )?;
        with_optimizer_resources(self, plan, control, |plan, control| {
            let reusable = reusable
                && params.is_empty()
                && !match self.optimization.routine_inlining() {
                    Some(routines) => {
                        routines.requires_replanning_with_control(plan.as_plan_ref(), control)?
                    }
                    None => false,
                };
            Ok((
                optimize_owned_with_control(self, plan, control)?.into_shared(control)?,
                reusable,
            ))
        })
    }

    fn plan_owned_with_cached_analysis(
        &self,
        plan: uqa_sql::plan::MutableExecutablePlan,
        params: &[SQLParam],
        cached: Option<uqa_sql::binding::statements::AnalyzedStatement>,
        control: &ProductionControl<'_>,
    ) -> Result<
        (
            SharedExecutablePlan,
            Option<uqa_sql::binding::statements::AnalyzedStatement>,
        ),
        SQLError,
    > {
        let (plan, retained, capture) = if let Some(cached) =
            cached.filter(|cached| cached.plan_for(params).is_some())
        {
            drop(plan);
            (
                cached
                    .plan_for(params)
                    .expect("matching analysis")
                    .copy_for_rewrite(control)?,
                Some(cached),
                false,
            )
        } else {
            let (plan, _, reusable) = uqa_sql::binding::statements::analyze_reusable_inputs_owned(
                &self.analysis,
                plan,
                params,
                control,
            )?;
            (plan, None, reusable)
        };
        with_optimizer_resources(self, plan, control, |plan, control| {
            let retained = if capture {
                uqa_sql::binding::statements::AnalyzedStatement::capture_ref_with_control(
                    plan.as_plan_ref(),
                    params,
                    control,
                )?
            } else {
                retained
            };
            Ok((
                optimize_owned_with_control(self, plan, control)?.into_shared(control)?,
                retained,
            ))
        })
    }

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
        let owner = self.planning_control()?;
        let control = owner.control();
        self.plan_owned_with_cached_analysis(
            plan.copy_for_rewrite(&control)?,
            params,
            cached,
            &control,
        )
    }

    fn plan_for_statement_cache(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<(SharedExecutablePlan, bool), SQLError> {
        let owner = self.planning_control()?;
        let control = owner.control();
        self.plan_owned_for_statement_cache(plan.copy_for_rewrite(&control)?, params, &control)
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
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<(SharedExecutablePlan, AnalyzedResult), SQLError> {
        let owner = self.planning_control()?;
        let control = owner.control();
        self.plan_owned_with_result(plan.copy_for_rewrite(&control)?, params, &control)
    }
}

/// Native analysis already owns its request. Standalone analysis may use ordinary inputs and acquire its independently configured optimizer allowance only after successful analysis.
fn with_optimizer_resources<T>(
    context: &StatementPlanningContext<'_>,
    plan: uqa_sql::plan::MutableExecutablePlan,
    control: &ProductionControl<'_>,
    optimize: impl FnOnce(
        uqa_sql::plan::MutableExecutablePlan,
        &ProductionControl<'_>,
    ) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    control.check()?;
    if control.budget().is_some() {
        return optimize(plan, control);
    }
    let resources = context.optimization.production_resources()?;
    let Some(resources) = resources else {
        return optimize(plan, control);
    };
    let selected = resources.control();
    let owned = plan.as_plan_ref().copy_for_rewrite(&selected)?;
    drop(plan);
    optimize(owned, &selected)
}

pub fn plan_for_execution(
    context: &StatementPlanningContext<'_>,
    plan: UnifiedPlan,
    params: &[SQLParam],
) -> Result<SharedExecutablePlan, SQLError> {
    use uqa_sql::plan::ExecutablePlanOptimizer;
    let owner = context.planning_control()?;
    let control = owner.control();
    context.plan_owned_for_execution(plan.copy_for_rewrite(&control)?, params, &control)
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
    optimize_retained_query(context, query.copy_for_rewrite(&control)?, &control)
}

/// Consume a private query and its constructor records without another tree copy.
pub fn optimize_retained_query(
    context: &StatementPlanningContext<'_>,
    query: uqa_sql::plan::MutableQueryPlan,
    control: &ProductionControl<'_>,
) -> Result<OwnedQueryPlan, SQLError> {
    optimize_owned_with_control(
        context,
        uqa_sql::plan::MutableExecutablePlan::Query(query),
        control,
    )?
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
    optimize_owned_with_control(context, plan.copy_for_rewrite(&control)?, &control)
}

pub fn optimize_owned_with_control(
    context: &StatementPlanningContext<'_>,
    plan: uqa_sql::plan::MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<OwnedExecutablePlan, SQLError> {
    let mut config = crate::optimizer::OptimizerConfig::new(context.constant_evaluator);
    config.production = *control;
    config.builtin_permissions = context
        .optimization
        .builtin_permissions_with_control(control)?;
    config.routine_inlining = context.optimization.routine_inlining();
    let storage = |control: &ProductionControl<'_>| {
        context.optimization.constant_storage_with_control(control)
    };
    config.constant_storage_factory = Some(crate::optimizer::ConstantStorageFactory(&storage));
    super::optimize_owned_plan_authorized(
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
