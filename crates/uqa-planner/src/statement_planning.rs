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

/// Choose an algorithm for an automatically planned equality join from the last measured table populations. Estimating a query never runs ANALYZE or inspects its values.
pub fn prefer_nested_loop(
    catalog: &dyn PlannerStatisticsCatalog,
    kind: uqa_sql::ast::JoinKind,
    left: &crate::SourcePlan,
    right: &crate::SourcePlan,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<bool, SQLError> {
    struct Samples<'a> {
        catalog: &'a dyn PlannerStatisticsCatalog,
        error: std::cell::RefCell<Option<SQLError>>,
    }
    impl crate::SourceStatistics for Samples<'_> {
        fn relation_statistics(&self, table: &str) -> Option<crate::RelationStats> {
            match self.catalog.measured_relation_rows(table) {
                Ok(rows) => rows.map(|row_count| crate::RelationStats {
                    row_count,
                    ..Default::default()
                }),
                Err(error) => {
                    self.error.borrow_mut().get_or_insert(error);
                    None
                }
            }
        }
    }
    let samples = Samples {
        catalog,
        error: std::cell::RefCell::default(),
    };
    let selected = crate::plan_cost::PlanCostEstimator::new(&samples)
        .prefer_nested_loop(kind, left, right, control)?;
    samples.error.into_inner().map_or(Ok(selected), Err)
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
        uqa_sql::plan::MutableExecutablePlan::uncontrolled(plan),
    )
}

fn optimize_plan_authorized(
    context: StatementStatisticsContext<'_>,
    rules: &rule_inputs::RuleInputPlanningContext<'_>,
    aggregates: &dyn crate::AggregateClassifier,
    mut config: crate::optimizer::OptimizerConfig<'_>,
    plan: uqa_sql::plan::MutableExecutablePlan,
) -> Result<uqa_sql::plan::OwnedExecutablePlan, SQLError> {
    let plan = rule_inputs::rewrite_plan_owned(rules, plan, &config)?;
    if uqa_sql::semantics::volatility::plan_ref_contains_volatile_function_with_control(
        context.volatility,
        plan.as_plan_ref(),
        &config.production,
    )? {
        config.enable_filter_pushdown = false;
        config.enable_join_reordering = false;
    }
    let optimized = with_statistics(context, config.production, |statistics| {
        crate::optimizer::optimize_owned_with_aggregates_and_statistics(
            plan, &config, aggregates, statistics,
        )
        .map_err(|error| match error {
            crate::optimizer::OptimizerError::Expression(error) => error,
            crate::optimizer::OptimizerError::JoinGraph(error) => {
                SQLError::Internal(format!("optimize SQL join order: {error}"))
            }
        })
    })?;
    optimized.finish(&config.production)
}

pub mod prepared;

pub mod executable;
