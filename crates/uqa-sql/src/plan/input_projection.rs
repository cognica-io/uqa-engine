//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Substitute analyzed input expressions without evaluating their values.

use super::{ExpressionPlan, ScalarExpr};
use crate::{ast::InternalColumnRef, SQLError};
use std::collections::BTreeMap;

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
