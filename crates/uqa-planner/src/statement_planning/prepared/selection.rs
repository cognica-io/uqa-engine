//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Select prepared variants and publish usage only against the captured logical identity.

use uqa_core::memory::{OwnedProductionControl, Produced, ProductionControl, ProductionVec};
use uqa_sql::{
    plan::{
        MutableExecutablePlan, OwnedExecutablePlan, PlanRef, SharedExecutablePlan, UnifiedPlan,
    },
    prepared::{
        definition::{PreparedDefinition, PreparedDefinitionContext},
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
    fn plan_cache_mode_with_control(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<String>, SQLError> {
        control.check()?;
        Ok(control.copy_text(&self.plan_cache_mode()?)?)
    }
    fn publish_usage(
        &self,
        name: &str,
        logical_plan: &SharedExecutablePlan,
        update: PreparedPlanUpdate,
    );
}

pub trait PreparedPlanOptimization {
    fn planning_resources(&self) -> Result<OwnedProductionControl, SQLError> {
        Ok(ProductionControl::uncontrolled().capture())
    }
    fn optimize_plan(
        &self,
        plan: MutableExecutablePlan,
        control: &ProductionControl<'_>,
    ) -> Result<SharedExecutablePlan, SQLError>;
    fn estimate_plan(
        &self,
        plan: &UnifiedPlan,
        control: &ProductionControl<'_>,
    ) -> Result<crate::plan_cost::PlanCost, SQLError>;
}

pub struct PreparedPlanningContext<'a> {
    pub session: &'a dyn PreparedPlanSession,
    pub analysis: PreparedDefinitionContext<'a>,
    pub optimization: &'a dyn PreparedPlanOptimization,
}

fn needs_analysis(
    context: &PreparedDefinitionContext<'_>,
    entry: &PreparedStatementPlan,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if !uqa_sql::prepared::utility::requires_analysis(&entry.source_plan) {
        return Ok(false);
    }
    if entry.needs_analysis {
        return Ok(true);
    }
    Ok(
        !uqa_sql::prepared::definition::analysis_is_current_with_control(
            context,
            entry.effective_search_path.as_ref(),
            &entry.dependencies,
            entry.dependency_snapshot.as_ref(),
            control,
        )?,
    )
}

pub fn select_plan(
    context: &PreparedPlanningContext<'_>,
    name: &str,
    parameters: &[SQLParam],
) -> Result<Option<SharedExecutablePlan>, SQLError> {
    let owner = context.optimization.planning_resources()?;
    let control = owner.control();
    control.check()?;
    let Some(entry) = context.session.prepared_entry(name) else {
        return Ok(None);
    };
    let selected = select_entry_with_control(context, &entry, parameters, &control)?;
    context
        .session
        .publish_usage(name, &entry.logical_plan, selected.update);
    Ok(Some(selected.plan))
}

/// Refresh a row description without optimizing an executable or recording plan usage. NoData descriptions do not inspect analysis dependencies.
pub fn describe_entry(
    context: &PreparedDefinitionContext<'_>,
    entry: &PreparedStatementPlan,
) -> Result<Option<PreparedDefinition<SharedExecutablePlan>>, SQLError> {
    describe_entry_with_control(context, entry, &ProductionControl::uncontrolled())
}

pub fn describe_entry_with_control(
    context: &PreparedDefinitionContext<'_>,
    entry: &PreparedStatementPlan,
    control: &ProductionControl<'_>,
) -> Result<Option<PreparedDefinition<SharedExecutablePlan>>, SQLError> {
    if entry.result_schema.is_none() || !needs_analysis(context, entry, control)? {
        return Ok(None);
    }
    let definition = reanalyze(context, entry, control)?;
    validate_result_schema(
        context,
        entry,
        definition.logical_plan.as_ref().into(),
        Some(&definition),
        control,
    )?;
    Ok(Some(definition.into_shared_with_control(control)?))
}

pub fn select_entry(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
) -> Result<PreparedPlanSelection, SQLError> {
    let owner = context.optimization.planning_resources()?;
    select_entry_with_control(context, entry, parameters, &owner.control())
}

/// Select a variant for a captured definition under the original statement's allowance.
pub fn select_entry_with_control(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<PreparedPlanSelection, SQLError> {
    select(context, entry, parameters, true, None, control)
}

/// Routine analysis already established its input and result contract; this boundary retains only prepared plan policy.
pub fn select_analyzed_entry(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
) -> Result<PreparedPlanSelection, SQLError> {
    let owner = context.optimization.planning_resources()?;
    select_analyzed_entry_with_control(context, entry, parameters, &owner.control())
}

pub fn select_analyzed_entry_with_control(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<PreparedPlanSelection, SQLError> {
    select(context, entry, parameters, false, None, control)
}

/// PL/pgSQL simple expressions request a generic plan while values remain runtime parameters.
pub fn select_simple_expression_entry(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
) -> Result<PreparedPlanSelection, SQLError> {
    let owner = context.optimization.planning_resources()?;
    select_simple_expression_entry_with_control(context, entry, parameters, &owner.control())
}

pub fn select_simple_expression_entry_with_control(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<PreparedPlanSelection, SQLError> {
    select(
        context,
        entry,
        parameters,
        false,
        Some("force_generic_plan"),
        control,
    )
}

fn select(
    context: &PreparedPlanningContext<'_>,
    entry: &PreparedStatementPlan,
    parameters: &[SQLParam],
    check_analysis: bool,
    mode: Option<&str>,
    control: &ProductionControl<'_>,
) -> Result<PreparedPlanSelection, SQLError> {
    control.check()?;
    let mode = mode.map_or_else(
        || context.session.plan_cache_mode_with_control(control),
        |mode| Ok(control.copy_text(mode)?),
    )?;
    let needs_analysis = check_analysis && needs_analysis(&context.analysis, entry, control)?;
    let mut generic_plan = if needs_analysis {
        None
    } else {
        entry.plan.clone()
    };
    let mut generic_cost = entry.generic_cost;
    let reanalyzed = needs_analysis
        .then(|| reanalyze(&context.analysis, entry, control))
        .transpose()?;
    let source = reanalyzed
        .as_ref()
        .map_or(entry.logical_plan.as_ref(), |definition| {
            definition.logical_plan.as_ref()
        });
    let inputs = reanalyzed
        .as_ref()
        .map_or(&entry.composite_inputs, |definition| {
            &definition.composite_inputs
        });
    if generic_plan.is_some()
        && inputs.generic_requires_rebuild_with_control(context.analysis.types, control)?
    {
        generic_plan = None;
        generic_cost = None;
    }
    let usage = super::PreparedPlanUsage {
        has_parameters: !entry.parameter_types.is_empty()
            && uqa_sql::prepared::utility::requires_analysis(source),
        custom_plans: entry.custom_plans,
        total_custom_cost: entry.total_custom_cost,
    };
    let mut custom = super::choose_custom_plan(usage, &mode, generic_cost);
    let generic_projection = !custom;
    let mut projected = if custom {
        inputs.project_logical_owned(source, context.analysis.types, control)?
    } else if generic_plan.is_none() {
        inputs.project_for_generic_owned(source, context.analysis.types, control)?
    } else {
        None
    };
    let logical = projected
        .as_ref()
        .map_or(source.into(), MutableExecutablePlan::as_plan_ref);
    let mut composite_inputs = inputs.clone();
    if check_analysis
        && uqa_sql::prepared::utility::requires_analysis_ref(logical)
        && (custom || generic_plan.is_none() || reanalyzed.is_some())
    {
        validate_result_schema(
            &context.analysis,
            entry,
            logical,
            reanalyzed.as_ref(),
            control,
        )?;
    }
    if !custom && generic_plan.is_none() {
        let plan = projected
            .take()
            .map_or_else(|| source.copy_for_rewrite(control), Ok)?;
        let generic = prepare_generic(context, inputs, plan, control)?;
        composite_inputs = generic.inputs;
        generic_cost = Some(generic.cost);
        generic_plan = Some(generic.plan);
        // The first generic cost participates in the policy before the plan executes.
        custom = super::choose_custom_plan(usage, &mode, generic_cost);
    }
    let (plan, custom_cost) = if custom {
        let plan = custom_input_plan(
            context,
            inputs,
            source,
            &mut projected,
            generic_projection,
            control,
        )?;
        let (plan, cost) = optimize_custom(context, plan, parameters, control)?;
        (plan, Some(cost))
    } else {
        let cached = generic_plan.as_ref().expect("generic plan was built");
        let plan = project_generic(context, &composite_inputs, cached, control)?;
        (plan, None)
    };
    let reanalyzed = retain_analysis(reanalyzed, control)?;
    Ok(PreparedPlanSelection {
        plan,
        update: PreparedPlanUpdate {
            composite_inputs,
            reanalyzed,
            generic_plan,
            generic_cost,
            custom_cost,
        },
    })
}

fn project_generic(
    context: &PreparedPlanningContext<'_>,
    inputs: &uqa_sql::prepared::composites::CompositeInputs,
    cached: &SharedExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<SharedExecutablePlan, SQLError> {
    match inputs.project_generic_owned(cached, context.analysis.types, control)? {
        Some(plan) => plan.finish(control)?.into_shared(control),
        None => Ok(cached.clone()),
    }
}

struct GenericPlan {
    plan: SharedExecutablePlan,
    inputs: uqa_sql::prepared::composites::CompositeInputs,
    cost: f64,
}

fn prepare_generic(
    context: &PreparedPlanningContext<'_>,
    inputs: &uqa_sql::prepared::composites::CompositeInputs,
    plan: MutableExecutablePlan,
    control: &ProductionControl<'_>,
) -> Result<GenericPlan, SQLError> {
    let plan = context.optimization.optimize_plan(plan, control)?;
    let inputs = inputs.with_generic_with_control(&plan, context.analysis.types, control)?;
    let cost = context
        .optimization
        .estimate_plan(&plan, control)?
        .execution;
    Ok(GenericPlan { plan, inputs, cost })
}

fn custom_input_plan(
    context: &PreparedPlanningContext<'_>,
    inputs: &uqa_sql::prepared::composites::CompositeInputs,
    source: &UnifiedPlan,
    projected: &mut Option<MutableExecutablePlan>,
    generic_projection: bool,
    control: &ProductionControl<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let plan = if generic_projection {
        inputs
            .project_logical_owned(source, context.analysis.types, control)?
            .map_or_else(|| source.copy_for_rewrite(control), Ok)?
    } else {
        projected
            .take()
            .map_or_else(|| source.copy_for_rewrite(control), Ok)?
    };
    Ok(plan)
}

fn optimize_custom(
    context: &PreparedPlanningContext<'_>,
    plan: MutableExecutablePlan,
    parameters: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<(SharedExecutablePlan, f64), SQLError> {
    let plan = super::specialize_parameters_owned(plan, parameters, control)?;
    let plan = context.optimization.optimize_plan(plan, control)?;
    let cost = context
        .optimization
        .estimate_plan(&plan, control)?
        .including_planning(&crate::CostEstimator::default());
    Ok((plan, cost))
}

fn retain_analysis(
    reanalyzed: Option<PreparedDefinition<OwnedExecutablePlan>>,
    control: &ProductionControl<'_>,
) -> Result<Option<PreparedPlanAnalysis>, SQLError> {
    reanalyzed
        .map(|definition| -> Result<_, SQLError> {
            Ok(PreparedPlanAnalysis {
                logical_plan: definition.logical_plan.into_shared(control)?,
                effective_search_path: definition.effective_search_path,
                dependencies: definition.dependencies,
                dependency_snapshot: definition.dependency_snapshot,
            })
        })
        .transpose()
}

fn validate_result_schema(
    context: &PreparedDefinitionContext<'_>,
    entry: &PreparedStatementPlan,
    logical_plan: PlanRef<'_>,
    reanalyzed: Option<&PreparedDefinition<OwnedExecutablePlan>>,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let result_schema = match reanalyzed {
        Some(definition) => definition.result_schema.clone(),
        None => uqa_sql::prepared::definition::analyze_result_schema_with_control(
            context,
            logical_plan,
            &entry.parameter_types,
            control,
        )?,
    };
    if !uqa_sql::prepared::prepared_result_schema_matches(
        entry.result_schema.as_ref(),
        result_schema.as_ref(),
    ) {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message: "cached plan must not change result type".into(),
        });
    }
    Ok(())
}

fn reanalyze(
    context: &PreparedDefinitionContext<'_>,
    entry: &PreparedStatementPlan,
    control: &ProductionControl<'_>,
) -> Result<PreparedDefinition<OwnedExecutablePlan>, SQLError> {
    let mut declared = ProductionVec::new(*control);
    for ty in entry.parameter_types.iter().flatten() {
        declared.push_produced(ty.clone_with_control(control)?)?;
    }
    let declared = declared.finish()?;
    uqa_sql::prepared::definition::analyze_definition_owned(
        context,
        entry.source_plan.copy_for_rewrite(control)?,
        &declared,
        control,
    )
}
