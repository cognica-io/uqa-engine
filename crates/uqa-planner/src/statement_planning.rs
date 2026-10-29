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
    let estimate = with_statistics(context, *control, |statistics| {
        crate::plan_cost::PlanCostEstimator::new(statistics).estimate_with_control(plan, control)
    })?;
    validate_cost(estimate)?;
    Ok(estimate)
}

pub fn estimate_plan_nodes_with_control<'p>(
    context: StatementStatisticsContext<'_>,
    plan: &'p UnifiedPlan,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<crate::plan_cost::nodes::PlanCostTree<'p>, SQLError> {
    let tree = with_statistics(context, *control, |statistics| {
        crate::plan_cost::PlanCostEstimator::new(statistics)
            .estimate_nodes_with_control(plan, control)
    })?;
    for node in tree.nodes.iter() {
        control.check()?;
        validate_cost(node.cost)?;
    }
    Ok(tree)
}

fn with_statistics<T>(
    context: StatementStatisticsContext<'_>,
    production: uqa_core::memory::ProductionControl<'_>,
    estimate: impl FnOnce(&CatalogSourceStatistics<'_, '_>) -> Result<T, SQLError>,
) -> Result<T, SQLError> {
    let error = std::cell::RefCell::new(None);
    let statistics = CatalogSourceStatistics {
        context,
        error: &error,
        production,
    };
    let estimate = estimate(&statistics);
    if let Some(error) = error.into_inner() {
        return Err(error);
    }
    estimate
}

fn validate_cost(estimate: crate::plan_cost::PlanCost) -> Result<(), SQLError> {
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
    Ok(())
}

pub fn optimize_plan(
    context: StatementStatisticsContext<'_>,
    rules: &rule_inputs::RuleInputPlanningContext<'_>,
    aggregates: &dyn crate::AggregateClassifier,
    constant_evaluator: crate::ConstantEvaluator,
    plan: UnifiedPlan,
) -> Result<uqa_sql::plan::OwnedExecutablePlan, SQLError> {
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
) -> Result<uqa_sql::plan::OwnedExecutablePlan, SQLError> {
    rule_inputs::rewrite_plan(rules, &mut plan, &optimizer_config)?;
    let callback_error = std::cell::RefCell::new(None);
    let statistics = CatalogSourceStatistics {
        context,
        error: &callback_error,
        production: optimizer_config.production,
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

pub(super) fn optimize_owned_plan_authorized(
    context: StatementStatisticsContext<'_>,
    rules: &rule_inputs::RuleInputPlanningContext<'_>,
    aggregates: &dyn crate::AggregateClassifier,
    mut config: crate::optimizer::OptimizerConfig<'_>,
    plan: uqa_sql::plan::MutableExecutablePlan,
) -> Result<uqa_sql::plan::OwnedExecutablePlan, SQLError> {
    let plan = rule_inputs::rewrite_plan_owned(rules, plan, &config)?;
    let callback_error = std::cell::RefCell::new(None);
    let statistics = CatalogSourceStatistics {
        context,
        error: &callback_error,
        production: config.production,
    };
    let mut volatile = Ok(false);
    plan.as_plan_ref()
        .visit_scalar_expressions(&mut |expression| {
            if matches!(volatile, Ok(false)) {
                volatile = uqa_sql::semantics::volatility::scalar_calls_are_volatile_with_control(
                    context.volatility,
                    expression,
                    &config.production,
                );
            }
        });
    if volatile? {
        config.enable_filter_pushdown = false;
        config.enable_join_reordering = false;
    }
    let optimized = crate::optimizer::optimize_owned_with_aggregates_and_statistics(
        plan,
        &config,
        aggregates,
        &statistics,
    );
    if let Some(error) = callback_error.into_inner() {
        return Err(error);
    }
    let optimized = optimized.map_err(|error| match error {
        crate::optimizer::OptimizerError::Expression(error) => error,
        crate::optimizer::OptimizerError::JoinGraph(error) => {
            SQLError::Internal(format!("optimize SQL join order: {error}"))
        }
    })?;
    optimized.finish(&config.production)
}
fn optimize_plan_with_statistics(
    volatility_catalog: &dyn uqa_sql::semantics::volatility::VolatilityCatalog,
    aggregates: &dyn crate::AggregateClassifier,
    mut optimizer_config: crate::optimizer::OptimizerConfig<'_>,
    plan: crate::UnifiedPlan,
    statistics: &dyn crate::SourceStatistics,
) -> Result<uqa_sql::plan::OwnedExecutablePlan, SQLError> {
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
