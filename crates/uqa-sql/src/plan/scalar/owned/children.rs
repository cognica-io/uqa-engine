//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Immediate scalar slots in the same order as their allocation-aware constructor.

use super::ScalarExpr;
use crate::ir::{ScalarFrameBound, ScalarOrder, ScalarWindowSpec};

pub(super) fn count(expression: &ScalarExpr) -> usize {
    match expression {
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.len(),
        ScalarExpr::UnaryMinus(_)
        | ScalarExpr::Not(_)
        | ScalarExpr::IsNull { .. }
        | ScalarExpr::Cast { .. }
        | ScalarExpr::Collated { .. }
        | ScalarExpr::InSubquery { .. } => 1,
        ScalarExpr::Binary { .. } => 2,
        ScalarExpr::Between { .. } => 3,
        ScalarExpr::InList { list, .. } => 1 + list.len(),
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => args.len() + order_by.len() + usize::from(filter.is_some()),
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => {
            args.len()
                + spec.partition_by.len()
                + spec.order_by.len()
                + spec.frame.as_ref().map_or(0, |frame| {
                    usize::from(has_offset(&frame.start)) + usize::from(has_offset(&frame.end))
                })
                + usize::from(filter.is_some())
        }
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => usize::from(base.is_some()) + 2 * when.len() + usize::from(else_branch.is_some()),
        ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => 0,
    }
}

pub(super) fn get_mut(expression: &mut ScalarExpr, index: usize) -> Option<&mut ScalarExpr> {
    match expression {
        ScalarExpr::Array(items)
        | ScalarExpr::Row(items)
        | ScalarExpr::CompositeRow { items, .. }
        | ScalarExpr::And(items)
        | ScalarExpr::Or(items) => items.get_mut(index),
        ScalarExpr::UnaryMinus(expr)
        | ScalarExpr::Not(expr)
        | ScalarExpr::IsNull { expr, .. }
        | ScalarExpr::Cast { expr, .. }
        | ScalarExpr::Collated { expr, .. }
        | ScalarExpr::InSubquery { expr, .. } => (index == 0).then_some(expr.as_mut()),
        ScalarExpr::Binary { lhs, rhs, .. } => [lhs.as_mut(), rhs.as_mut()].into_iter().nth(index),
        ScalarExpr::Between { expr, low, high } => [expr.as_mut(), low.as_mut(), high.as_mut()]
            .into_iter()
            .nth(index),
        ScalarExpr::InList { expr, list, .. } => {
            std::iter::once(expr.as_mut()).chain(list).nth(index)
        }
        ScalarExpr::Func {
            args,
            order_by,
            filter,
            ..
        } => call_child(args, order_by, filter.as_deref_mut(), index),
        ScalarExpr::WindowCall {
            args, spec, filter, ..
        } => window_child(args, spec, filter.as_deref_mut(), index),
        ScalarExpr::Case {
            base,
            when,
            else_branch,
        } => {
            let base_count = usize::from(base.is_some());
            if index < base_count {
                return base.as_deref_mut();
            }
            let index = index - base_count;
            if index / 2 < when.len() {
                let (condition, value) = &mut when[index / 2];
                return Some(if index.is_multiple_of(2) {
                    condition
                } else {
                    value
                });
            }
            if index == when.len() * 2 {
                else_branch.as_deref_mut()
            } else {
                None
            }
        }
        ScalarExpr::Star
        | ScalarExpr::QualifiedStar(_)
        | ScalarExpr::Default
        | ScalarExpr::Column(_)
        | ScalarExpr::Position(_)
        | ScalarExpr::InternalColumn(_)
        | ScalarExpr::QualifiedColumn { .. }
        | ScalarExpr::Literal(_)
        | ScalarExpr::TypedLiteral { .. }
        | ScalarExpr::RetainedLiteral(_)
        | ScalarExpr::Param(_)
        | ScalarExpr::ScalarSubquery(_)
        | ScalarExpr::Exists { .. } => None,
    }
}

fn has_offset(bound: &ScalarFrameBound) -> bool {
    matches!(
        bound,
        ScalarFrameBound::Preceding(_) | ScalarFrameBound::Following(_)
    )
}

fn call_child<'a>(
    args: &'a mut [ScalarExpr],
    order_by: &'a mut [ScalarOrder],
    filter: Option<&'a mut ScalarExpr>,
    index: usize,
) -> Option<&'a mut ScalarExpr> {
    if index < args.len() {
        return args.get_mut(index);
    }
    let index = index - args.len();
    if index < order_by.len() {
        return Some(&mut order_by[index].expr);
    }
    if index == order_by.len() {
        filter
    } else {
        None
    }
}

fn window_child<'a>(
    args: &'a mut [ScalarExpr],
    spec: &'a mut ScalarWindowSpec,
    filter: Option<&'a mut ScalarExpr>,
    index: usize,
) -> Option<&'a mut ScalarExpr> {
    if index < args.len() {
        return args.get_mut(index);
    }
    let index = index - args.len();
    if index < spec.partition_by.len() {
        return spec.partition_by.get_mut(index);
    }
    let index = index - spec.partition_by.len();
    if index < spec.order_by.len() {
        return Some(&mut spec.order_by[index].expr);
    }
    let index = index - spec.order_by.len();
    // Only two frame bounds and the optional filter remain; no wide key list is walked to locate a slot.
    spec.frame
        .iter_mut()
        .flat_map(|frame| [&mut frame.start, &mut frame.end])
        .filter_map(|bound| match bound {
            ScalarFrameBound::Preceding(expression) | ScalarFrameBound::Following(expression) => {
                Some(expression.as_mut())
            }
            _ => None,
        })
        .chain(filter)
        .nth(index)
}
