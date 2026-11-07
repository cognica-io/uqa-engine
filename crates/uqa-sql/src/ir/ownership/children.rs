//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable first-child positions also hold suspended retirement links.

use super::{windows, ScalarExpr};

pub(super) fn visit(value: &mut ScalarExpr, mut visit: impl FnMut(&mut ScalarExpr)) {
    match value {
        ScalarExpr::Func {
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
        ScalarExpr::Array(values)
        | ScalarExpr::Row(values)
        | ScalarExpr::And(values)
        | ScalarExpr::Or(values)
        | ScalarExpr::CompositeRow { items: values, .. } => {
            for child in values {
                visit(child);
            }
        }
        ScalarExpr::Binary { lhs, rhs, .. } => {
            visit(lhs);
            visit(rhs);
        }
        ScalarExpr::UnaryMinus(child)
        | ScalarExpr::Not(child)
        | ScalarExpr::IsNull { expr: child, .. }
        | ScalarExpr::Cast { expr: child, .. }
        | ScalarExpr::Collated { expr: child, .. }
        | ScalarExpr::InSubquery { expr: child, .. } => visit(child),
        ScalarExpr::Between { expr, low, high } => {
            visit(expr);
            visit(low);
            visit(high);
        }
        ScalarExpr::InList { expr, list, .. } => {
            visit(expr);
            for child in list {
                visit(child);
            }
        }
        ScalarExpr::Case {
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
        ScalarExpr::WindowCall {
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
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Position(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::Param(_)
        | ScalarExpr::RetainedLiteral(_) => {}
    }
}

pub(super) fn first(value: &mut ScalarExpr) -> Option<&mut ScalarExpr> {
    match value {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => args
            .first_mut()
            .or_else(|| order_by.first_mut().map(|order| &mut order.expr))
            .or(filter.as_deref_mut()),
        ScalarExpr::Array(values)
        | ScalarExpr::Row(values)
        | ScalarExpr::And(values)
        | ScalarExpr::Or(values)
        | ScalarExpr::CompositeRow { items: values, .. } => values.first_mut(),
        ScalarExpr::Binary { lhs, .. } => Some(lhs),
        ScalarExpr::UnaryMinus(child)
        | ScalarExpr::Not(child)
        | ScalarExpr::IsNull { expr: child, .. }
        | ScalarExpr::Cast { expr: child, .. }
        | ScalarExpr::Collated { expr: child, .. }
        | ScalarExpr::InSubquery { expr: child, .. }
        | ScalarExpr::Between { expr: child, .. }
        | ScalarExpr::InList { expr: child, .. } => Some(child),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => base
            .as_deref_mut()
            .or_else(|| when.first_mut().map(|(condition, _)| condition))
            .or(else_branch.as_deref_mut()),
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => args
            .first_mut()
            .or_else(|| windows::first(spec))
            .or(filter.as_deref_mut()),
        ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. }
        | ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Position(_)
        | ScalarExpr::Column(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::Param(_)
        | ScalarExpr::RetainedLiteral(_) => None,
    }
}
