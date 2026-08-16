//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! PostgreSQL-compatible validation for column default expressions.

use super::{Engine, SQLError};
use uqa_execution::{RowSchema, ScalarExpr};
use uqa_sql::ast::Expr;

pub(super) fn validate_default_expression(
    engine: &Engine,
    expression: &Expr,
) -> Result<(), SQLError> {
    let plan = uqa_planner::ExpressionPlan::lower(expression.clone());
    if !plan.subqueries.is_empty() {
        return Err(default_error(
            "0A000",
            "cannot use subquery in DEFAULT expression",
        ));
    }
    if contains_window(&plan.scalar) {
        return Err(default_error(
            "42P20",
            "window functions are not allowed in DEFAULT expressions",
        ));
    }
    if crate::sql::aggregates::contains_aggregate(engine, &plan.scalar) {
        return Err(default_error(
            "42803",
            "aggregate functions are not allowed in DEFAULT expressions",
        ));
    }
    if references_column(&plan.scalar) {
        return Err(default_error(
            "0A000",
            "cannot use column reference in DEFAULT expression",
        ));
    }
    uqa_execution::scalar_type_with_resolver(&plan.scalar, &RowSchema::default(), &[], engine)?;
    Ok(())
}

fn default_error(sqlstate: &str, message: &str) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message: message.into(),
    }
}

fn references_column(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. } => true,
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(references_column)
                || order_by.iter().any(|order| references_column(&order.expr))
                || filter.as_deref().is_some_and(references_column)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(references_column),
        ScalarExpr::Binary { lhs, rhs, .. } => references_column(lhs) || references_column(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::IsNull { expr: inner, .. } => references_column(inner),
        ScalarExpr::Between { expr, low, high } => {
            references_column(expr) || references_column(low) || references_column(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            references_column(expr) || list.iter().any(references_column)
        }
        ScalarExpr::WindowCall { args, spec, .. } => {
            args.iter().any(references_column)
                || spec.partition_by.iter().any(references_column)
                || spec
                    .order_by
                    .iter()
                    .any(|order| references_column(&order.expr))
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(references_column)
                || when.iter().any(|(condition, result)| {
                    references_column(condition) || references_column(result)
                })
                || else_branch.as_deref().is_some_and(references_column)
        }
        ScalarExpr::InSubquery { expr, .. } => references_column(expr),
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::Default
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => false,
    }
}

fn contains_window(expression: &ScalarExpr) -> bool {
    match expression {
        ScalarExpr::WindowCall { .. } => true,
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(contains_window)
                || order_by.iter().any(|order| contains_window(&order.expr))
                || filter.as_deref().is_some_and(contains_window)
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(contains_window),
        ScalarExpr::Binary { lhs, rhs, .. } => contains_window(lhs) || contains_window(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::Cast { expr: inner, .. }
        | ScalarExpr::IsNull { expr: inner, .. } => contains_window(inner),
        ScalarExpr::Between { expr, low, high } => {
            contains_window(expr) || contains_window(low) || contains_window(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            contains_window(expr) || list.iter().any(contains_window)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_deref().is_some_and(contains_window)
                || when.iter().any(|(condition, result)| {
                    contains_window(condition) || contains_window(result)
                })
                || else_branch.as_deref().is_some_and(contains_window)
        }
        ScalarExpr::InSubquery { expr, .. } => contains_window(expr),
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::Param(_) => false,
    }
}
