//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Remove remaining siblings in constant work while preserving the first-child link.

use super::{windows, Expr};

fn replace(value: &mut Expr) -> Option<Expr> {
    (!matches!(value, Expr::Star)).then(|| std::mem::replace(value, Expr::Star))
}

pub(super) fn take(value: &mut Expr) -> Option<Expr> {
    match value {
        Expr::Func {
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
        Expr::Array(values)
        | Expr::Row(values)
        | Expr::And(values)
        | Expr::Or(values)
        | Expr::CompositeRow { items: values, .. } => {
            (values.len() > 1).then(|| values.pop().unwrap())
        }
        Expr::Binary { rhs, .. } => replace(rhs),
        Expr::Between { low, high, .. } => replace(high).or_else(|| replace(low)),
        Expr::InList { list, .. } => list.pop(),
        Expr::Case {
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
        Expr::WindowCall {
            args, spec, filter, ..
        } => windows::take(args, spec, filter),
        Expr::UnaryMinus(_)
        | Expr::Not(_)
        | Expr::IsNull { .. }
        | Expr::Cast { .. }
        | Expr::InSubquery { .. }
        | Expr::ScalarSubquery(_)
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
