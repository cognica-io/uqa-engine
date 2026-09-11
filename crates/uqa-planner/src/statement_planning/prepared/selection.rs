//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select prepared variants and publish usage only against the captured logical identity.

use std::sync::Arc;
use uqa_sql::{
    plan::UnifiedPlan,
    prepared::{
        definition::PreparedDefinitionContext, entry::PreparedStatementPlan,
        planning::PreparedPlanUpdate,
    },
    SQLError, SQLParam,
};

#[cfg(test)]
mod tests;

pub trait PreparedPlanSession {
    fn prepared_entry(&self, name: &str) -> Option<PreparedStatementPlan>;
    fn plan_cache_mode(&self) -> Result<String, SQLError>;
    fn publish_usage(
        &self,
        name: &str,
        logical_plan: &Arc<UnifiedPlan>,
        update: PreparedPlanUpdate,
    );
}

pub trait PreparedPlanOptimization {
    fn optimize_plan(&self, plan: UnifiedPlan) -> Result<UnifiedPlan, SQLError>;
    fn estimate_plan(&self, plan: &UnifiedPlan) -> Result<crate::plan_cost::PlanCost, SQLError>;
}

pub struct PreparedPlanningContext<'a> {
    pub session: &'a dyn PreparedPlanSession,
    pub analysis: PreparedDefinitionContext<'a>,
    pub optimization: &'a dyn PreparedPlanOptimization,
}

pub fn select_plan(
    context: &PreparedPlanningContext<'_>,
    name: &str,
    parameters: &[SQLParam],
) -> Result<Option<UnifiedPlan>, SQLError> {
    let Some(entry) = context.session.prepared_entry(name) else {
        return Ok(None);
    };
    let mode = context.session.plan_cache_mode()?;
    let mut generic_plan = entry.plan.clone();
    let mut generic_cost = entry.generic_cost;
    let usage = super::PreparedPlanUsage {
        has_parameters: !entry.parameter_types.is_empty(),
        custom_plans: entry.custom_plans,
        total_custom_cost: entry.total_custom_cost,
    };
    let mut custom = super::choose_custom_plan(usage, &mode, generic_cost);
    if custom || generic_plan.is_none() {
        let result_schema = uqa_sql::prepared::definition::analyze_result_schema(
            &context.analysis,
            &entry.logical_plan,
            &entry.parameter_types,
        )?;
        if !uqa_sql::prepared::prepared_result_schema_matches(
            entry.result_schema.as_ref(),
            result_schema.as_ref(),
        ) {
            return Err(uqa_sql::SQLError::Routine {
                sqlstate: "0A000".into(),
                message: "cached plan must not change result type".into(),
            });
        }
    }
    if !custom && generic_plan.is_none() {
        let plan = context
            .optimization
            .optimize_plan((*entry.logical_plan).clone())?;
        generic_cost = Some(context.optimization.estimate_plan(&plan)?.execution);
        generic_plan = Some(plan);
        // Building the first generic plan supplies its previously unknown cost.
        // Recheck before execution so an expensive generic plan is never used just to measure it.
        custom = super::choose_custom_plan(usage, &mode, generic_cost);
    }
    let (plan, custom_cost) = if custom {
        let mut plan = (*entry.logical_plan).clone();
        super::specialize_parameters(&mut plan, parameters);
        let plan = context.optimization.optimize_plan(plan)?;
        let cost = context
            .optimization
            .estimate_plan(&plan)?
            .including_planning(&crate::CostEstimator::default());
        (plan, Some(cost))
    } else {
        (
            generic_plan
                .as_ref()
                .expect("generic plan was built")
                .clone(),
            None,
        )
    };
    context.session.publish_usage(
        name,
        &entry.logical_plan,
        PreparedPlanUpdate {
            generic_plan,
            generic_cost,
            custom_cost,
        },
    );
    Ok(Some(plan))
}
