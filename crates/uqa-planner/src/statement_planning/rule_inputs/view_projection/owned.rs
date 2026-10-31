//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Captured view rewrites preserve source order and transfer independently admitted producer and consumer expressions.

use super::super::{prune_captured_inputs_owned, RuleInputPlanningContext};
use crate::optimizer::{optimize_qualification_owned, OptimizerConfig};
use uqa_sql::{
    plan::{CommandPlan, CommandScalarSlot, MutableExecutablePlan, RelationalPlan, SourcePlan},
    SQLError,
};

pub(in crate::statement_planning::rule_inputs) fn rewrite_owned(
    context: &RuleInputPlanningContext<'_>,
    mut plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let control = &config.production;
    let mut captured = false;
    plan.as_plan_ref().visit_sources(&mut |source| {
        captured |= matches!(
            source,
            SourcePlan::Subquery {
                captured_output: Some(_),
                ..
            }
        );
    });
    if !captured {
        control.check()?;
        return Ok(plan);
    }
    plan = simplify_conditions(plan, config)?;
    loop {
        plan =
            prune_captured_inputs_owned(context.volatility, context.views.routines, plan, control)?;
        let mut selected = None;
        plan.replace_sources(control, &mut |source| {
            if selected.is_some() { return Ok(None); }
            let SourcePlan::Subquery { captured_output: Some(relation), body, .. } = source else { return Ok(None); };
            let Some(candidate) = uqa_sql::semantics::view_rewrite::capture_view_projection_owned(context.views, *relation, body, control)? else { return Ok(None); };
            for expression in candidate.expressions.values() {
                if uqa_sql::semantics::volatility::scalar_in_scope_contains_volatile_function_with_control(
                    context.volatility, &expression.scalar, &expression.subqueries, &|_| false, control,
                )? { return Ok(None); }
            }
            selected = Some(candidate.expressions);
            Ok(Some(candidate.source))
        })?;
        let Some(expressions) = selected else {
            break;
        };
        plan = uqa_sql::plan::input_projection::substitute_plan_inputs_owned(
            plan,
            &expressions,
            control,
        )?;
        plan = plan.map_queries_while(control, &mut |query| {
            Ok((query.discard_source_analysis(control)?, true))
        })?;
    }
    Ok(plan)
}

fn simplify_conditions(
    mut plan: MutableExecutablePlan,
    config: &OptimizerConfig<'_>,
) -> Result<MutableExecutablePlan, SQLError> {
    let mut config = config.clone();
    config.coerced_conditionals = true;
    let control = &config.production;
    plan = match plan {
        MutableExecutablePlan::Command(command)
            if matches!(&*command, CommandPlan::Update(_) | CommandPlan::Delete(_)) =>
        {
            MutableExecutablePlan::Command(command.map_scalar(
                CommandScalarSlot::Predicate,
                control,
                |expression| optimize_qualification_owned(expression, &config),
            )?)
        }
        plan => plan,
    };
    plan.map_queries_while(control, &mut |mut query| {
        if matches!(&query.root, RelationalPlan::QueryBlock(_)) {
            let (retained, predicate) = query.take_where(control)?;
            query = retained;
            if let Some(predicate) = predicate {
                query = query.replace_where(
                    Some(optimize_qualification_owned(predicate, &config)?),
                    control,
                )?;
            }
        }
        Ok((query.prune_subqueries(control)?, true))
    })
}
