//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Analyze executable plans before optimizer inputs and constant evaluation.

use super::{rule_inputs::RuleInputPlanningContext, StatementStatisticsContext};
use crate::{AggregateClassifier, ConstantEvaluator, QueryPlan, UnifiedPlan};
use uqa_sql::{
    binding::statements::{analyze_executable_plan, StatementAnalysisContext},
    SQLError, SQLParam,
};

/// Borrow planner metadata after SQL analysis succeeds.
pub trait StatementOptimizationContexts {
    fn statistics(&self) -> StatementStatisticsContext<'_>;
    fn rule_inputs(&self) -> RuleInputPlanningContext<'_>;
}
pub struct StatementPlanningContext<'a> {
    pub analysis: StatementAnalysisContext<'a>,
    pub aggregates: &'a dyn AggregateClassifier,
    pub optimization: &'a dyn StatementOptimizationContexts,
    pub constant_evaluator: ConstantEvaluator,
}

impl uqa_sql::plan::ExecutablePlanOptimizer for StatementPlanningContext<'_> {
    fn plan_for_execution(
        &self,
        plan: UnifiedPlan,
        params: &[SQLParam],
    ) -> Result<UnifiedPlan, SQLError> {
        plan_for_execution(self, plan, params)
    }
}

pub fn plan_for_execution(
    context: &StatementPlanningContext<'_>,
    plan: UnifiedPlan,
    params: &[SQLParam],
) -> Result<UnifiedPlan, SQLError> {
    analyze_executable_plan(&context.analysis, &plan, params)?;
    optimize_plan(context, plan)
}
pub fn optimize_query(
    context: &StatementPlanningContext<'_>,
    query: &QueryPlan,
) -> Result<QueryPlan, SQLError> {
    match optimize_plan(context, UnifiedPlan::Query(Box::new(query.clone())))? {
        UnifiedPlan::Query(query) => Ok(*query),
        UnifiedPlan::Command(_) => Err(SQLError::Internal(
            "query optimization produced a command".into(),
        )),
    }
}
pub fn optimize_plan(
    context: &StatementPlanningContext<'_>,
    plan: UnifiedPlan,
) -> Result<UnifiedPlan, SQLError> {
    super::optimize_plan(
        context.optimization.statistics(),
        &context.optimization.rule_inputs(),
        context.aggregates,
        context.constant_evaluator,
        plan,
    )
}

#[cfg(test)]
mod tests;
