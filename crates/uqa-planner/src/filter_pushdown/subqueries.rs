//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    collect_pushdown_outer_columns, expr_contains_subquery, expr_contains_volatile_function,
    expr_qualifiers, query_contains_volatile_function, BTreeSet, ColumnOwners,
    FilterPushdownContext, QueryPlan, ScalarExpr,
};
use uqa_sql::semantics::volatility::function_binding_is_volatile;

pub(super) fn unique_unqualified_column_owner<'a>(
    expression: &ScalarExpr,
    owners: &'a ColumnOwners,
) -> Option<&'a str> {
    if !expr_qualifiers(expression).is_empty() {
        return None;
    }
    let mut columns = BTreeSet::new();
    if !collect_pushdown_outer_columns(expression, &mut columns) || columns.is_empty() {
        return None;
    }
    let mut owner = None;
    for column in columns {
        let candidate = owners.get(&column)?.as_deref()?;
        if owner.is_some_and(|owner| owner != candidate) {
            return None;
        }
        owner = Some(candidate);
    }
    owner
}

pub(super) fn subqueries_are_uncorrelated_and_stable(
    context: FilterPushdownContext<'_>,
    expression: &ScalarExpr,
    subqueries: &[QueryPlan],
) -> bool {
    let mut referenced = BTreeSet::new();
    collect_subquery_ids(expression, &mut referenced);
    !referenced.is_empty()
        && referenced.into_iter().all(|id| {
            let Some(plan) = subqueries.get(id) else {
                return false;
            };
            matches!(
                uqa_sql::binding::correlation::query_depends_on_outer_row(
                    context.correlation,
                    plan
                ),
                Ok(false)
            ) && matches!(
                query_contains_volatile_function(context.volatility, plan),
                Ok(false)
            )
        })
}

pub(super) fn outer_expression_contains_volatile_function(
    context: FilterPushdownContext<'_>,
    expression: &ScalarExpr,
) -> bool {
    if !expr_contains_subquery(expression) {
        return expr_contains_volatile_function(context.volatility, expression);
    }
    match expression {
        ScalarExpr::ScalarSubquery(_) | ScalarExpr::Exists { .. } => false,
        ScalarExpr::Func {
            name,
            binding,
            args,
            order_by,
            filter,
            ..
        } => {
            function_binding_is_volatile(context.volatility, name, binding.as_ref(), args.len())
                || args
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(context, expr))
                || order_by
                    .iter()
                    .any(|order| outer_expression_contains_volatile_function(context, &order.expr))
                || filter.as_deref().is_some_and(|filter| {
                    outer_expression_contains_volatile_function(context, filter)
                })
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items
            .iter()
            .any(|item| outer_expression_contains_volatile_function(context, item)),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            outer_expression_contains_volatile_function(context, lhs)
                || outer_expression_contains_volatile_function(context, rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::InSubquery { expr: inner, .. }
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            outer_expression_contains_volatile_function(context, inner)
        }
        ScalarExpr::Between { expr, low, high } => {
            outer_expression_contains_volatile_function(context, expr)
                || outer_expression_contains_volatile_function(context, low)
                || outer_expression_contains_volatile_function(context, high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            outer_expression_contains_volatile_function(context, expr)
                || list
                    .iter()
                    .any(|item| outer_expression_contains_volatile_function(context, item))
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            uqa_sql::semantics::volatility::function_volatility(
                context.volatility,
                name,
                args.len(),
            ) == uqa_sql::ast::FunctionVolatility::Volatile
                || args
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(context, expr))
                || spec
                    .partition_by
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(context, expr))
                || spec
                    .order_by
                    .iter()
                    .any(|order| outer_expression_contains_volatile_function(context, &order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_outer_expression_contains_volatile_function(context, &frame.start)
                        || frame_bound_outer_expression_contains_volatile_function(
                            context, &frame.end,
                        )
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => case_outer_expression_contains_volatile_function(
            context,
            base.as_deref(),
            when,
            else_branch.as_deref(),
        ),
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::Param(_) => false,
    }
}

pub(super) fn frame_bound_outer_expression_contains_volatile_function(
    context: FilterPushdownContext<'_>,
    bound: &uqa_sql::ScalarFrameBound,
) -> bool {
    match bound {
        uqa_sql::ScalarFrameBound::Preceding(expression)
        | uqa_sql::ScalarFrameBound::Following(expression) => {
            outer_expression_contains_volatile_function(context, expression)
        }
        uqa_sql::ScalarFrameBound::UnboundedPreceding
        | uqa_sql::ScalarFrameBound::UnboundedFollowing
        | uqa_sql::ScalarFrameBound::CurrentRow => false,
    }
}

pub use uqa_sql::semantics::collect_subquery_ids;

fn case_outer_expression_contains_volatile_function(
    context: FilterPushdownContext<'_>,
    base: Option<&ScalarExpr>,
    when: &[(ScalarExpr, ScalarExpr)],
    else_branch: Option<&ScalarExpr>,
) -> bool {
    base.is_some_and(|base| outer_expression_contains_volatile_function(context, base))
        || when.iter().any(|(condition, result)| {
            outer_expression_contains_volatile_function(context, condition)
                || outer_expression_contains_volatile_function(context, result)
        })
        || else_branch
            .is_some_and(|branch| outer_expression_contains_volatile_function(context, branch))
}
