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
        ScalarExpr::InSubquery { expr, .. } => {
            outer_expression_contains_volatile_function(engine, expr)
        }
        ScalarExpr::Func {
            name,
            args,
            order_by,
            filter,
            ..
        } => {
            crate::sql::volatility::function_volatility(engine, name, args.len())
                == uqa_sql::ast::FunctionVolatility::Volatile
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

pub(in crate::sql) fn collect_subquery_ids(expression: &ScalarExpr, output: &mut BTreeSet<usize>) {
    match expression {
        ScalarExpr::ScalarSubquery(id) | ScalarExpr::Exists { subquery: id, .. } => {
            output.insert(*id);
        }
        ScalarExpr::InSubquery { expr, subquery, .. } => {
            collect_subquery_ids(expr, output);
            output.insert(*subquery);
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => {
            for item in items {
                collect_subquery_ids(item, output);
            }
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for argument in args {
                collect_subquery_ids(argument, output);
            }
            for order in order_by {
                collect_subquery_ids(&order.expr, output);
            }
            if let Some(filter) = filter {
                collect_subquery_ids(filter, output);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            collect_subquery_ids(lhs, output);
            collect_subquery_ids(rhs, output);
        }
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => collect_subquery_ids(inner, output),
        ScalarExpr::Between { expr, low, high } => {
            collect_subquery_ids(expr, output);
            collect_subquery_ids(low, output);
            collect_subquery_ids(high, output);
        }
        ScalarExpr::InList { expr, list, .. } => {
            collect_subquery_ids(expr, output);
            for item in list {
                collect_subquery_ids(item, output);
            }
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            for argument in args {
                collect_subquery_ids(argument, output);
            }
            for partition in &spec.partition_by {
                collect_subquery_ids(partition, output);
            }
            for order in &spec.order_by {
                collect_subquery_ids(&order.expr, output);
            }
            if let Some(frame) = &spec.frame {
                collect_frame_bound_subquery_ids(&frame.start, output);
                collect_frame_bound_subquery_ids(&frame.end, output);
            }
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(base) = base {
                collect_subquery_ids(base, output);
            }
            for (condition, result) in when {
                collect_subquery_ids(condition, output);
                collect_subquery_ids(result, output);
            }
            if let Some(branch) = else_branch {
                collect_subquery_ids(branch, output);
            }
        }
        ScalarExpr::Default
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => {}
    }
}

pub(super) fn collect_frame_bound_subquery_ids(
    bound: &uqa_execution::ScalarFrameBound,
    output: &mut BTreeSet<usize>,
) {
    match bound {
        uqa_execution::ScalarFrameBound::Preceding(expression)
        | uqa_execution::ScalarFrameBound::Following(expression) => {
            collect_subquery_ids(expression, output);
        }
        uqa_execution::ScalarFrameBound::UnboundedPreceding
        | uqa_execution::ScalarFrameBound::UnboundedFollowing
        | uqa_execution::ScalarFrameBound::CurrentRow => {}
    }
}
