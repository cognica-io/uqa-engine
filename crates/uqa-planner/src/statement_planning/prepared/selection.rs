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
        definition::PreparedDefinitionContext,
        entry::PreparedStatementPlan,
        planning::{PreparedPlanAnalysis, PreparedPlanSelection, PreparedPlanUpdate},
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

fn needs_analysis(
    context: &PreparedDefinitionContext<'_>,
    entry: &PreparedStatementPlan,
) -> Result<bool, SQLError> {
    if entry.needs_analysis {
        return Ok(true);
    }
    Ok(!uqa_sql::prepared::definition::analysis_is_current(
        context,
        entry.effective_search_path.as_ref(),
        &entry.dependencies,
        entry.dependency_snapshot.as_ref(),
    )?)
}

pub fn select_plan(
    context: &PreparedPlanningContext<'_>,
    name: &str,
    parameters: &[SQLParam],
) -> Result<Option<UnifiedPlan>, SQLError> {
    let Some(entry) = context.session.prepared_entry(name) else {
        return Ok(None);
    };
    let selected = select_entry(context, &entry, parameters)?;
    context
        .session
        .publish_usage(name, &entry.logical_plan, selected.update);
    Ok(Some(selected.plan))
}

/// Select a variant for an already captured definition, including routine-owned
/// entries that are not registered as named prepared statements.
pub fn select_entry(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
) -> Result<PreparedPlanSelection, SQLError> {
    select(context, entry, parameters, true)
}

/// A routine's owner has already checked or rebuilt its analyzed inputs and
/// result contract. Reuse only the plan policy here; named PREPARE descriptor
/// checks do not apply to procedural CALL and utility statements.
pub fn select_analyzed_entry(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
) -> Result<PreparedPlanSelection, SQLError> {
    select(context, entry, parameters, false)
}

fn select(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
    check_analysis: bool,
) -> Result<PreparedPlanSelection, SQLError> {
    let mode = context.session.plan_cache_mode()?;
    let needs_analysis = check_analysis && needs_analysis(&context.analysis, entry)?;
    let mut generic_plan = if needs_analysis {
        None
    } else {
        entry.plan.clone()
    };
    let mut generic_cost = entry.generic_cost;
    let reanalyzed = if needs_analysis {
        let declared = entry
            .parameter_types
            .iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>();
        Some(uqa_sql::prepared::definition::analyze_definition(
            &context.analysis,
            (*entry.source_plan).clone(),
            &declared,
        )?)
    } else {
        None
    };
    let logical_plan = reanalyzed
        .as_ref()
        .map_or(entry.logical_plan.as_ref(), |definition| {
            &definition.logical_plan
        });
    let usage = super::PreparedPlanUsage {
        has_parameters: !entry.parameter_types.is_empty(),
        custom_plans: entry.custom_plans,
        total_custom_cost: entry.total_custom_cost,
    };
    let mut custom = super::choose_custom_plan(usage, &mode, generic_cost);
    if check_analysis && (custom || generic_plan.is_none() || reanalyzed.is_some()) {
        let result_schema = match &reanalyzed {
            Some(definition) => definition.result_schema.clone(),
            None => uqa_sql::prepared::definition::analyze_result_schema(
                &context.analysis,
                logical_plan,
                &entry.parameter_types,
            )?,
        };
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
        let plan = context.optimization.optimize_plan(logical_plan.clone())?;
        generic_cost = Some(context.optimization.estimate_plan(&plan)?.execution);
        generic_plan = Some(plan);
        // Building the first generic plan supplies its previously unknown cost.
        // Recheck before execution so an expensive generic plan is never used just to measure it.
        custom = super::choose_custom_plan(usage, &mode, generic_cost);
    }
    let (plan, custom_cost) = if custom {
        let mut plan = logical_plan.clone();
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
    Ok(PreparedPlanSelection {
        plan,
        update: PreparedPlanUpdate {
            reanalyzed: reanalyzed.map(|definition| PreparedPlanAnalysis {
                logical_plan: Arc::new(definition.logical_plan),
                effective_search_path: definition.effective_search_path,
                dependencies: definition.dependencies,
                dependency_snapshot: definition.dependency_snapshot,
            }),
            generic_plan,
            generic_cost,
            custom_cost,
        },
    })
}
