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
        &mut |node, arena| {
            let ScalarExpr::InternalColumn(column) = node else {
                return Ok(());
            };
            let Some(input) = inputs.get(column) else {
                return Ok(());
            };
            let mut replacement = input.scalar.clone();
            let offset = arena.len();
            super::rewrite_scalar_expression(&mut replacement, &mut |node| match node {
                ScalarExpr::ScalarSubquery(id)
                | ScalarExpr::Exists { subquery: id, .. }
                | ScalarExpr::InSubquery { subquery: id, .. } => *id += offset,
                _ => {}
            });
            arena.extend(input.subqueries.iter().cloned());
            *node = replacement;
            Ok(())
        },
    )
}
