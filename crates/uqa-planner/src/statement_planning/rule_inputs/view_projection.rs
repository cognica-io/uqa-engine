//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Move pure view outputs to their consumers while preserving volatile projection boundaries.

use super::{prune_captured_inputs, RuleInputPlanningContext};
use crate::{optimizer::OptimizerConfig, CommandPlan, RelationalPlan, SourcePlan, UnifiedPlan};
use uqa_sql::SQLError;

pub(super) fn rewrite(
    context: &RuleInputPlanningContext<'_>,
    plan: &mut UnifiedPlan,
    config: &OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    let mut captured = false;
    plan.visit_sources(&mut |source| {
        captured |= matches!(
            source,
            SourcePlan::Subquery {
                captured_output: Some(_),
                ..
            }
        );
    });
    if !captured {
        return Ok(());
    }
    simplify_conditions(plan, config)?;
    loop {
        prune_captured_inputs(context.volatility, context.views.routines, plan)?;
        let mut selected = None;
        let mut error = None;
        plan.visit_sources_mut(&mut |source| {
            if selected.is_some() || error.is_some() {
                return;
            }
            let SourcePlan::Subquery {
                captured_output: Some(relation),
                body,
                ..
            } = source
            else {
                return;
            };
            let candidate = uqa_sql::semantics::view_rewrite::capture_view_projection(
                context.views,
                *relation,
                body,
            );
            let candidate = match candidate {
                Ok(Some(candidate)) => candidate,
                Ok(None) => return,
                Err(failure) => {
                    error = Some(failure);
                    return;
                }
            };
            for expression in candidate.expressions.values() {
                match uqa_sql::semantics::volatility::expression_plan_contains_volatile_function(
                    context.volatility,
                    expression,
                ) {
                    Ok(false) => {}
                    Ok(true) => return,
                    Err(failure) => {
                        error = Some(failure);
                        return;
                    }
                }
            }
            *source = candidate.source;
            selected = Some(candidate.expressions);
        });
        if let Some(error) = error {
            return Err(error);
        }
        let Some(expressions) = selected else {
            break;
        };
        uqa_sql::plan::input_projection::substitute_plan_inputs(plan, &expressions)?;
        // The private relation identities changed; runtime binding derives the new topology.
        plan.visit_queries_mut(&mut |query| query.analyzed_source = None);
    }
    Ok(())
}

fn simplify_conditions(
    plan: &mut UnifiedPlan,
    config: &OptimizerConfig<'_>,
) -> Result<(), SQLError> {
    let mut config = config.clone();
    config.coerced_conditionals = true;
    let mut result = Ok(());
    let mut simplify = |expression: &mut Option<uqa_sql::ScalarExpr>| {
        if let Some(expression) = expression {
            if result.is_ok() {
                result = crate::optimizer::optimize_qualification(expression, &config);
            }
        }
    };
    if let UnifiedPlan::Command(command) = plan {
        match command.as_mut() {
            CommandPlan::Update(plan) => simplify(&mut plan.predicate),
            CommandPlan::Delete(plan) => simplify(&mut plan.predicate),
            _ => {}
        }
    }
    plan.visit_queries_mut(&mut |query| {
        if let RelationalPlan::QueryBlock(block) = &mut query.root {
            simplify(&mut block.r#where);
        }
        uqa_sql::plan::subqueries::prune_query(query);
    });
    result
}
