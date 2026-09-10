//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window expression classification.

use crate::ScalarExpr;

pub fn expr_has_window(expr: &ScalarExpr) -> bool {
    match expr {
        ScalarExpr::WindowCall { .. } => true,
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            args.iter().any(expr_has_window)
                || order_by.iter().any(|order| expr_has_window(&order.expr))
                || filter.as_ref().is_some_and(|expr| expr_has_window(expr))
        }
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.iter().any(expr_has_window),
        ScalarExpr::Binary { lhs, rhs, .. } => expr_has_window(lhs) || expr_has_window(rhs),
        ScalarExpr::Not(inner)
        | ScalarExpr::UnaryMinus(inner)
        | ScalarExpr::IsNull { expr: inner, .. }
        | ScalarExpr::Cast { expr: inner, .. } => expr_has_window(inner),
        ScalarExpr::Between { expr, low, high } => {
            expr_has_window(expr) || expr_has_window(low) || expr_has_window(high)
        }
        ScalarExpr::InList { expr, list, .. } => {
            expr_has_window(expr) || list.iter().any(expr_has_window)
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            base.as_ref().is_some_and(|expr| expr_has_window(expr))
                || when
                    .iter()
                    .any(|(cond, result)| expr_has_window(cond) || expr_has_window(result))
                || else_branch
                    .as_ref()
                    .is_some_and(|expr| expr_has_window(expr))
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
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::InSubquery { .. } => false,
    }
}
