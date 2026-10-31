//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Correlation decisions retain semantic error deferral while propagating resource failures.

use super::{FilterPushdownContext, QueryPlan, ScalarExpr};
use uqa_core::memory::{ProductionControl, ProductionMap};
pub use uqa_sql::semantics::collect_subquery_ids;
use uqa_sql::semantics::volatility::{
    query_contains_volatile_function_with_control, scalar_calls_are_volatile_with_control,
};
use uqa_sql::SQLError;

pub(super) fn subqueries_are_uncorrelated_and_stable_with_control(
    context: FilterPushdownContext<'_>,
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    let mut referenced = ProductionMap::new(*control);
    expression.try_visit(&mut |node| {
        control.check()?;
        match node {
            ScalarExpr::ScalarSubquery(id)
            | ScalarExpr::Exists { subquery: id, .. }
            | ScalarExpr::InSubquery { subquery: id, .. } => {
                referenced.insert_copy(*id, ())?;
            }
            _ => {}
        }
        Ok::<_, SQLError>(true)
    })?;
    if referenced.is_empty() {
        return Ok(false);
    }
    for (id, ()) in &referenced {
        let Some(plan) = subqueries.get(*id) else {
            return Ok(false);
        };
        if !eligible(
            uqa_sql::binding::correlation::query_depends_on_outer_row_with_control(
                context.correlation,
                plan,
                control,
            ),
        )? || !eligible(query_contains_volatile_function_with_control(
            context.volatility,
            plan,
            control,
        ))? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn eligible(result: Result<bool, SQLError>) -> Result<bool, SQLError> {
    match result {
        Ok(found) => Ok(!found),
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(false),
    }
}
pub(super) fn outer_expression_contains_volatile_function_with_control(
    context: FilterPushdownContext<'_>,
    expression: &ScalarExpr,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    scalar_calls_are_volatile_with_control(context.volatility, expression, control)
}
