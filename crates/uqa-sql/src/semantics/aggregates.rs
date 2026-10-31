//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Aggregate discovery and expression equivalence for SQL grouping.

use crate::plan::{AggregateClassifier, ProjectionPlan, QueryBlockPlan};
use crate::ScalarExpr;
use uqa_core::Value;

#[cfg(test)]
mod production_tests;
#[cfg(test)]
mod tests;

mod matching;
pub use matching::exprs_match_with_control;

pub fn exprs_match(lhs: &ScalarExpr, rhs: &ScalarExpr) -> bool {
    exprs_match_with_control(
        lhs,
        rhs,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .unwrap_or(false)
}

pub fn literals_equal(a: &Value, b: &Value) -> bool {
    a.has_same_representation(b)
}

pub fn has_aggregate(aggregates: &dyn AggregateClassifier, projections: &[ProjectionPlan]) -> bool {
    projections
        .iter()
        .any(|p| contains_aggregate(aggregates, &p.expr))
}

pub fn is_aggregate(aggregates: &dyn AggregateClassifier, expr: &ScalarExpr) -> bool {
    is_builtin_aggregate(expr)
        || matches!(expr, ScalarExpr::Func { name, .. } if aggregates.is_registered_aggregate(name))
}

pub use super::is_builtin_aggregate;

pub fn aggregate_exprs<'a>(
    aggregates: &dyn AggregateClassifier,
    projections: &'a [ProjectionPlan],
) -> Vec<&'a ScalarExpr> {
    let mut out = Vec::new();
    for projection in projections {
        collect_aggregate_exprs(aggregates, &projection.expr, &mut out);
    }
    out
}

/// Aggregate states needed by a query block, in accumulator order.
///
/// Projection aggregates remain first (including repeated expressions) because
/// projection rewriting consumes them positionally. Aggregates referenced only
/// by HAVING are appended once as hidden targets, matching `PostgreSQL`'s rule
/// that HAVING need not expose an aggregate in the SELECT list.
pub fn aggregate_targets<'a>(
    aggregates: &dyn AggregateClassifier,
    statement: &'a QueryBlockPlan,
) -> Vec<&'a ScalarExpr> {
    let mut targets = aggregate_exprs(aggregates, &statement.projections);
    if let Some(having) = statement.having.as_ref() {
        let mut hidden = Vec::new();
        collect_aggregate_exprs(aggregates, having, &mut hidden);
        for aggregate in hidden {
            if !targets
                .iter()
                .any(|existing| exprs_match(existing, aggregate))
            {
                targets.push(aggregate);
            }
        }
    }
    targets
}

mod observation;
pub use observation::{
    contains_aggregate_with_control, has_aggregate_with_control, is_aggregate_with_control,
    visit_aggregate_exprs_with_control,
};

pub fn collect_aggregate_exprs<'a>(
    aggregates: &dyn AggregateClassifier,
    expr: &'a ScalarExpr,
    out: &mut Vec<&'a ScalarExpr>,
) {
    observation::visit_ordinary(aggregates, expr, &mut |expr| out.push(expr));
}

pub fn contains_aggregate(aggregates: &dyn AggregateClassifier, expr: &ScalarExpr) -> bool {
    let mut found = false;
    observation::visit_ordinary(aggregates, expr, &mut |_| found = true);
    found
}

/// Collect the top-level column names an expression reads. Returns
/// `false` when the expression can reach arbitrary fields (`*`,
/// subqueries, window calls), in which case callers must materialise
/// whole documents.
pub fn expr_references_columns(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. } => true,
        ScalarExpr::Func { args, filter, .. } => {
            args.iter().any(expr_references_columns)
                || filter.as_deref().is_some_and(expr_references_columns)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(expr_references_columns),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            expr_references_columns(lhs) || expr_references_columns(rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::Collated { expr: inner, .. } => expr_references_columns(inner),
        ScalarExpr::IsNull { expr, .. } => expr_references_columns(expr),
        ScalarExpr::Between { expr, low, high } => {
            expr_references_columns(expr)
                || expr_references_columns(low)
                || expr_references_columns(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_references_columns(expr) || list.iter().any(expr_references_columns)
        }
        ScalarExpr::WindowCall { args, filter, .. } => {
            args.iter().any(expr_references_columns)
                || filter.as_deref().is_some_and(expr_references_columns)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(expr_references_columns)
                || when.iter().any(|(condition, result)| {
                    expr_references_columns(condition) || expr_references_columns(result)
                })
                || else_branch.as_deref().is_some_and(expr_references_columns)
        }
        ScalarExpr::InSubquery { expr, .. } => expr_references_columns(expr),
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => true,
        ScalarExpr::Default
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_) => false,
    }
}

mod slots;
pub use slots::{
    aggregate_slot_index, compile_group_slots, compile_having_aggregate_slots,
    compile_projection_aggregate_slots, select_grouping_set,
};
