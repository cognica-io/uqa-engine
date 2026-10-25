//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Statement optimization and prepared-plan estimates over explicit catalog and retrieval inputs.
use crate::UnifiedPlan;
use statistics::CatalogSourceStatistics;
pub use statistics::{
    PlannerStatisticsCatalog, RetrievalSourceCosting, StatementStatisticsContext,
    StatisticsTableState,
};
use uqa_sql::SQLError;
mod parameterized;
pub mod rule_inputs;
pub mod schema_expressions;
mod statistics;

#[cfg(test)]
mod tests;

pub fn estimate_plan(
    context: StatementStatisticsContext<'_>,
    plan: &UnifiedPlan,
) -> Result<crate::plan_cost::PlanCost, SQLError> {
    estimate_plan_with_control(
        context,
        plan,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
}

pub fn estimate_plan_with_control(
    context: StatementStatisticsContext<'_>,
    plan: &UnifiedPlan,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<crate::plan_cost::PlanCost, SQLError> {
    let error = std::cell::RefCell::new(None);
    let statistics = CatalogSourceStatistics {
        context,
        error: &error,
    };
    let estimate =
        crate::plan_cost::PlanCostEstimator::new(&statistics).estimate_with_control(plan, control);
    if let Some(error) = error.into_inner() {
        return Err(error);
    }
    let estimate = estimate?;
    if !estimate.execution.is_finite()
        || estimate.execution < 0.0
        || !estimate.rows.is_finite()
        || estimate.rows < 0.0
        || !estimate.startup.is_finite()
        || estimate.startup < 0.0
        || estimate.startup > estimate.execution
    {
        return Err(SQLError::Internal(
            "prepared plan produced an invalid cost estimate".into(),
        ));
    }
    Ok(estimate)
}

pub fn optimize_plan(
    context: StatementStatisticsContext<'_>,
    rules: &rule_inputs::RuleInputPlanningContext<'_>,
    aggregates: &dyn crate::AggregateClassifier,
    constant_evaluator: crate::ConstantEvaluator,
    plan: UnifiedPlan,
) -> Result<UnifiedPlan, SQLError> {
    optimize_plan_authorized(
        context,
        rules,
        aggregates,
        crate::optimizer::OptimizerConfig::new(constant_evaluator),
        plan,
    )
}

pub(super) fn optimize_plan_authorized(
    context: StatementStatisticsContext<'_>,
    rules: &rule_inputs::RuleInputPlanningContext<'_>,
    aggregates: &dyn crate::AggregateClassifier,
    optimizer_config: crate::optimizer::OptimizerConfig<'_>,
    mut plan: UnifiedPlan,
) -> Result<UnifiedPlan, SQLError> {
    rule_inputs::rewrite_plan(rules, &mut plan, &optimizer_config)?;
    let callback_error = std::cell::RefCell::new(None);
    let statistics = CatalogSourceStatistics {
        context,
        error: &callback_error,
    };
    let optimized = optimize_plan_with_statistics(
        context.volatility,
        aggregates,
        optimizer_config,
        plan,
        &statistics,
    );
    if let Some(error) = callback_error.into_inner() {
        return Err(error);
    }
    optimized
}
fn optimize_plan_with_statistics(
    volatility_catalog: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    aggregates: &dyn crate::AggregateClassifier,
    mut optimizer_config: crate::optimizer::OptimizerConfig<'_>,
    plan: crate::UnifiedPlan,
    statistics: &dyn crate::SourceStatistics,
) -> Result<crate::UnifiedPlan, SQLError> {
    if uqa_sql::semantics::volatility::unified_plan_contains_volatile_function(
        volatility_catalog,
        &plan,
    ) {
        // Predicate prioritization and DPccp both move expressions across
        // physical evaluation boundaries.  A VOLATILE callback may observe
        // or mutate state on every call, so even a logically equivalent join
        // order can change SQL-visible behavior by changing its call count.
        optimizer_config.enable_filter_pushdown = false;
        optimizer_config.enable_join_reordering = false;
    }
    let optimized = crate::optimizer::optimize_with_aggregates_and_statistics(
        plan,
        &optimizer_config,
        aggregates,
        statistics,
    );
    optimized.map_err(|error| match error {
        crate::optimizer::OptimizerError::Expression(error) => error,
        crate::optimizer::OptimizerError::JoinGraph(error) => {
            SQLError::Internal(format!("optimize SQL join order: {error}"))
        }
    })
}

pub mod prepared;

pub mod executable;
