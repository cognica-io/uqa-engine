//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable first-child positions also hold suspended retirement links.

use super::{windows, Expr};

pub(super) fn visit(value: &mut Expr, mut visit: impl FnMut(&mut Expr)) {
    match value {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            for child in args {
                visit(child);
            }
            for order in order_by {
                visit(&mut order.expr);
            }
            if let Some(child) = filter {
                visit(child);
            }
        }
        Expr::Array(values)
        | Expr::Row(values)
        | Expr::And(values)
        | Expr::Or(values)
        | Expr::CompositeRow { items: values, .. } => {
            for child in values {
                visit(child);
            }
        }
        Expr::Binary { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        }
        Expr::UnaryMinus(child)
        | Expr::Not(child)
        | Expr::IsNull { expr: child, .. }
        | Expr::Cast { expr: child, .. }
        | Expr::InSubquery { expr: child, .. } => visit(child),
        Expr::Between { expr, low, high } => {
            visit(expr);
            visit(low);
            visit(high);
        }
        Expr::InList { expr, list, .. } => {
            visit(expr);
            for child in list {
                visit(child);
            }
        }
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            if let Some(child) = base {
                visit(child);
            }
            for (condition, result) in when {
                visit(condition);
                visit(result);
            }
            if let Some(child) = else_branch {
                visit(child);
            }
        }
        Expr::WindowCall {
            args, spec, filter, ..
        } => {
            for child in args {
                visit(child);
            }
            for child in spec.expressions_mut() {
                visit(child);
            }
            if let Some(child) = filter {
                visit(child);
            }
        }
        // Query bodies retain their query owner; these variants have no directly owned scalar children beyond InSubquery's operand above.
        Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Default
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::RetainedLiteral(_) => {}
    }
}

pub(super) fn first(value: &mut Expr) -> Option<&mut Expr> {
    match value {
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => args
            .first_mut()
            .or_else(|| order_by.first_mut().map(|order| &mut order.expr))
            .or(filter.as_deref_mut()),
        Expr::Array(values)
        | Expr::Row(values)
        | Expr::And(values)
        | Expr::Or(values)
        | Expr::CompositeRow { items: values, .. } => values.first_mut(),
        Expr::Binary { lhs, .. } => Some(lhs),
        Expr::UnaryMinus(child)
        | Expr::Not(child)
        | Expr::IsNull { expr: child, .. }
        | Expr::Cast { expr: child, .. }
        | Expr::InSubquery { expr: child, .. }
        | Expr::Between { expr: child, .. }
        | Expr::InList { expr: child, .. } => Some(child),
        Expr::Case {
            base,
            when,
            else_branch,
        } => base
            .as_deref_mut()
            .or_else(|| when.first_mut().map(|(condition, _)| condition))
            .or(else_branch.as_deref_mut()),
        Expr::WindowCall {
            args, spec, filter, ..
        } => args
            .first_mut()
            .or_else(|| windows::first(spec))
            .or(filter.as_deref_mut()),
        Expr::ScalarSubquery(_)
        | Expr::Exists { .. }
        | Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Default
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::RetainedLiteral(_) => None,
    }
}
