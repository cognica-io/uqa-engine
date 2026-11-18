//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stable first-child positions also hold suspended retirement links.

use super::{windows, ScalarExpr};

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
