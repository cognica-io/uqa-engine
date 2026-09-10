//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    collect_pushdown_outer_columns, expr_contains_subquery, expr_contains_volatile_function,
    expr_qualifiers, query_contains_volatile_function, BTreeSet, ColumnOwners, Engine, QueryPlan,
    ScalarExpr,
};
use crate::sql::volatility::function_binding_is_volatile;

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
    engine: &Engine,
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
                crate::sql::correlation::query_depends_on_outer_row(engine, plan),
                Ok(false)
            ) && matches!(query_contains_volatile_function(engine, plan), Ok(false))
        })
}

pub(super) fn outer_expression_contains_volatile_function(
    engine: &Engine,
    expression: &ScalarExpr,
) -> bool {
    if !expr_contains_subquery(expression) {
        return expr_contains_volatile_function(engine, expression);
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
            function_binding_is_volatile(engine, name, binding.as_ref(), args.len())
                || args
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(engine, expr))
                || order_by
                    .iter()
                    .any(|order| outer_expression_contains_volatile_function(engine, &order.expr))
                || filter.as_deref().is_some_and(|filter| {
                    outer_expression_contains_volatile_function(engine, filter)
                })
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items
            .iter()
            .any(|item| outer_expression_contains_volatile_function(engine, item)),
        ScalarExpr::Binary { lhs, rhs, .. } => {
            outer_expression_contains_volatile_function(engine, lhs)
                || outer_expression_contains_volatile_function(engine, rhs)
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::InSubquery { expr: inner, .. }
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => {
            outer_expression_contains_volatile_function(engine, inner)
        }
        ScalarExpr::Between { expr, low, high } => {
            outer_expression_contains_volatile_function(engine, expr)
                || outer_expression_contains_volatile_function(engine, low)
                || outer_expression_contains_volatile_function(engine, high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            outer_expression_contains_volatile_function(engine, expr)
                || list
                    .iter()
                    .any(|item| outer_expression_contains_volatile_function(engine, item))
        }
        ScalarExpr::WindowCall { name, args, spec } => {
            crate::sql::volatility::function_volatility(engine, name, args.len())
                == uqa_sql::ast::FunctionVolatility::Volatile
                || args
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(engine, expr))
                || spec
                    .partition_by
                    .iter()
                    .any(|expr| outer_expression_contains_volatile_function(engine, expr))
                || spec
                    .order_by
                    .iter()
                    .any(|order| outer_expression_contains_volatile_function(engine, &order.expr))
                || spec.frame.as_ref().is_some_and(|frame| {
                    frame_bound_outer_expression_contains_volatile_function(engine, &frame.start)
                        || frame_bound_outer_expression_contains_volatile_function(
                            engine, &frame.end,
                        )
                })
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref()
                .is_some_and(|base| outer_expression_contains_volatile_function(engine, base))
                || when.iter().any(|(condition, result)| {
                    outer_expression_contains_volatile_function(engine, condition)
                        || outer_expression_contains_volatile_function(engine, result)
                })
                || else_branch.as_deref().is_some_and(|branch| {
                    outer_expression_contains_volatile_function(engine, branch)
                })
        }
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
    engine: &Engine,
    bound: &uqa_execution::ScalarFrameBound,
) -> bool {
    match bound {
        uqa_execution::ScalarFrameBound::Preceding(expression)
        | uqa_execution::ScalarFrameBound::Following(expression) => {
            outer_expression_contains_volatile_function(engine, expression)
        }
        uqa_execution::ScalarFrameBound::UnboundedPreceding
        | uqa_execution::ScalarFrameBound::UnboundedFollowing
        | uqa_execution::ScalarFrameBound::CurrentRow => false,
    }
}

pub(in crate::sql) use uqa_sql::semantics::collect_subquery_ids;
