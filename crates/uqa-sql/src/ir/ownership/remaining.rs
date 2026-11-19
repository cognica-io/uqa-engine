//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove remaining siblings in constant work while preserving the first-child link.

use super::{windows, ScalarExpr};

fn replace(value: &mut ScalarExpr) -> Option<ScalarExpr> {
    (!matches!(value, ScalarExpr::Star)).then(|| std::mem::replace(value, ScalarExpr::Star))
}

pub(super) fn take(value: &mut ScalarExpr) -> Option<ScalarExpr> {
    match value {
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => {
            if !args.is_empty() || !order_by.is_empty() {
                if let Some(child) = filter.take() {
                    return Some(*child);
                }
            }
            if !args.is_empty() || order_by.len() > 1 {
                if let Some(order) = order_by.pop() {
                    return Some(order.expr);
                }
            }
            (args.len() > 1).then(|| args.pop().unwrap())
        }
        ScalarExpr::Array(values)
        | ScalarExpr::Row(values)
        | ScalarExpr::And(values)
        | ScalarExpr::Or(values)
        | ScalarExpr::CompositeRow { items: values, .. } => {
            (values.len() > 1).then(|| values.pop().unwrap())
        }
        ScalarExpr::Binary { rhs, .. } => replace(rhs),
        ScalarExpr::Between { low, high, .. } => replace(high).or_else(|| replace(low)),
        ScalarExpr::InList { list, .. } => list.pop(),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            if base.is_some() || !when.is_empty() {
                if let Some(child) = else_branch.take() {
                    return Some(*child);
                }
            }
            if let Some((_, result)) = when.last_mut() {
                if let Some(child) = replace(result) {
                    return Some(child);
                }
            }
            if base.is_some() || when.len() > 1 {
                return when.pop().map(|(condition, _)| condition);
            }
            None
        }
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => windows::take(args, spec, filter),
        ScalarExpr::UnaryMinus(_)
        | ScalarExpr::Not(_)
        | ScalarExpr::IsNull { .. }
        | ScalarExpr::Cast { .. }
        | ScalarExpr::Collated { .. }
        | ScalarExpr::InSubquery { .. }
        | ScalarExpr::ScalarSubquery(_)
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
