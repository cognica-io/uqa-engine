//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immediate AST children follow the scalar owner's field order with constant work per lookup.

use crate::ast::{Expr, FrameBound, OrderBy, WindowSpec};

pub(super) fn get(source: &Expr, index: usize) -> Option<&Expr> {
    match source {
        Expr::Array(items)
        | Expr::Row(items)
        | Expr::And(items)
        | Expr::Or(items)
        | Expr::CompositeRow { items, .. } => items.get(index),
        Expr::UnaryMinus(expr)
        | Expr::Not(expr)
        | Expr::IsNull { expr, .. }
        | Expr::Cast { expr, .. }
        | Expr::InSubquery { expr, .. } => (index == 0).then_some(expr.as_ref()),
        Expr::Binary { lhs, rhs, .. } => [lhs.as_ref(), rhs.as_ref()].get(index).copied(),
        Expr::Between { expr, low, high } => [expr.as_ref(), low.as_ref(), high.as_ref()]
            .get(index)
            .copied(),
        Expr::InList { expr, list, .. } => {
            if index == 0 {
                Some(expr)
            } else {
                list.get(index - 1)
            }
        }
        Expr::Func {
            args,
            order_by,
            filter,
            ..
        } => call(args, order_by, filter.as_deref(), index),
        Expr::WindowCall {
            args, spec, filter, ..
        } => window(args, spec, filter.as_deref(), index),
        Expr::Case {
            base,
            when,
            else_branch,
        } => {
            let base_count = usize::from(base.is_some());
            if index < base_count {
                return base.as_deref();
            }
            let index = index - base_count;
            if let Some((condition, result)) = when.get(index / 2) {
                return Some(if index.is_multiple_of(2) {
                    condition
                } else {
                    result
                });
            }
            if index == when.len() * 2 {
                else_branch.as_deref()
            } else {
                None
            }
        }
        Expr::Star
        | Expr::QualifiedStar(_)
        | Expr::Default
        | Expr::Column(_)
        | Expr::QualifiedColumn { .. }
        | Expr::InternalColumn(_)
        | Expr::Literal(_)
        | Expr::RetainedLiteral(_)
        | Expr::TypedLiteral { .. }
        | Expr::Param(_)
        | Expr::ScalarSubquery(_)
        | Expr::Exists { .. } => None,
    }
}

fn call<'a>(
    args: &'a [Expr],
    order: &'a [OrderBy],
    filter: Option<&'a Expr>,
    index: usize,
) -> Option<&'a Expr> {
    if index < args.len() {
        return args.get(index);
    }
    let index = index - args.len();
    if let Some(order) = order.get(index) {
        return Some(&order.expr);
    }
    if index == order.len() {
        filter
    } else {
        None
    }
}

fn window<'a>(
    args: &'a [Expr],
    spec: &'a WindowSpec,
    filter: Option<&'a Expr>,
    index: usize,
) -> Option<&'a Expr> {
    if index < args.len() {
        return args.get(index);
    }
    let index = index - args.len();
    if index < spec.partition_by.len() {
        return spec.partition_by.get(index);
    }
    let index = index - spec.partition_by.len();
    if let Some(order) = spec.order_by.get(index) {
        return Some(&order.expr);
    }
    let index = index - spec.order_by.len();
    spec.frame
        .iter()
        .flat_map(|frame| [&frame.start, &frame.end])
        .filter_map(|bound| match bound {
            FrameBound::Preceding(expr) | FrameBound::Following(expr) => Some(expr.as_ref()),
            _ => None,
        })
        .chain(filter)
        .nth(index)
}
