//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Substitute analyzed input expressions without evaluating their values.

use super::{ExpressionPlan, ScalarExpr};
use crate::{ast::InternalColumnRef, SQLError};
use std::collections::BTreeMap;
use uqa_core::memory::{OwnedMap, Produced, ProductionControl};

pub type OwnedInputProjections = OwnedMap<InternalColumnRef, Produced<ExpressionPlan>>;

pub fn substitute_expression_inputs_owned(
    mut plan: super::MutableExpressionPlan,
    inputs: &OwnedInputProjections,
    control: &ProductionControl<'_>,
) -> Result<super::MutableExpressionPlan, SQLError> {
    if inputs.is_empty() {
        control.check()?;
        return Ok(plan);
    }
    for index in 0..plan.queries().len() {
        plan = plan.map_query(index, control, |query| {
            query.transform_scalar_arenas(control, &mut |_, expression, arena| {
                substitute_owned(expression, arena, inputs, control)
            })
        })?;
    }
    plan = plan.map_scalar(control, |expression, arena| {
        expression.transform_nodes(control, &mut |_, expression| {
            substitute_owned(expression, arena, inputs, control)
        })
    })?;
    Ok(plan)
}

pub fn substitute_plan_inputs_owned(
    plan: super::MutableExecutablePlan,
    inputs: &OwnedInputProjections,
    control: &ProductionControl<'_>,
) -> Result<super::MutableExecutablePlan, SQLError> {
    if inputs.is_empty() {
        control.check()?;
        return Ok(plan);
    }
    plan.transform_scalar_arenas(control, &mut |_, expression, arena| {
        substitute_owned(expression, arena, inputs, control)
    })
}

fn substitute_owned(
    expression: super::OwnedScalarExpr,
    arena: &mut super::ScalarQueryArena,
    inputs: &OwnedInputProjections,
    control: &ProductionControl<'_>,
) -> Result<super::OwnedScalarExpr, SQLError> {
    control.check()?;
    if let ScalarExpr::InternalColumn(column) = &*expression {
        if let Some(input) = inputs.get(column) {
            return arena.append_expression(input.copy_for_rewrite(control)?, control);
        }
    }
    Ok(expression)
}

#[cfg(test)]
mod tests;

/// Inline structurally identified inputs at their use sites. Each query keeps its own scalar-subquery arena, and inserted expressions retain their analyzed routine bindings.
pub fn substitute_expression_inputs(
    plan: &mut ExpressionPlan,
    inputs: &BTreeMap<InternalColumnRef, ExpressionPlan>,
) -> Result<(), SQLError> {
    if inputs.is_empty() {
        return Ok(());
    }
    super::subqueries::rewrite_expression_with_arena(
        &mut plan.scalar,
        &mut plan.subqueries,
        &mut |node, arena| substitute_input(node, arena, inputs),
    )
}

/// Apply the same input substitution to every native query/command arena.
pub fn substitute_plan_inputs(
    plan: &mut super::UnifiedPlan,
    inputs: &BTreeMap<InternalColumnRef, ExpressionPlan>,
) -> Result<(), SQLError> {
    if inputs.is_empty() {
        return Ok(());
    }
    super::subqueries::rewrite_with_arenas(plan, &mut |node, arena| {
        substitute_input(node, arena, inputs)
    })
}

fn substitute_input(
    node: &mut ScalarExpr,
    arena: &mut Vec<super::QueryPlan>,
    inputs: &BTreeMap<InternalColumnRef, ExpressionPlan>,
) -> Result<(), SQLError> {
    let ScalarExpr::InternalColumn(column) = node else {
        return Ok(());
    };
    let Some(input) = inputs.get(column) else {
        return Ok(());
    };
    *node = append_expression(arena, input.clone());
    Ok(())
}

/// Transfer an expression into another scalar arena while retaining its selected query identities.
pub(crate) fn append_expression(
    arena: &mut Vec<super::QueryPlan>,
    mut input: ExpressionPlan,
) -> ScalarExpr {
    let offset = arena.len();
    super::rewrite_scalar_expression(&mut input.scalar, &mut |node| match node {
        ScalarExpr::ScalarSubquery(id)
        | ScalarExpr::Exists { subquery: id, .. }
        | ScalarExpr::InSubquery { subquery: id, .. } => *id += offset,
        _ => {}
    });
    arena.extend(input.subqueries);
    input.scalar
}
