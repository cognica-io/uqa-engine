//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Window clauses use their original scalar slots during retirement.

use crate::ast::{Expr, FrameBound, WindowSpec};

fn bound(value: &mut FrameBound) -> Option<&mut Expr> {
    match value {
        FrameBound::Preceding(value) | FrameBound::Following(value) => Some(value),
        _ => None,
    }
}

fn take_bound(value: &mut FrameBound) -> Option<Expr> {
    match std::mem::replace(value, FrameBound::CurrentRow) {
        FrameBound::Preceding(value) | FrameBound::Following(value) => Some(*value),
        _ => None,
    }
}

pub(super) fn first(value: &mut WindowSpec) -> Option<&mut Expr> {
    value
        .partition_by
        .first_mut()
        .or_else(|| value.order_by.first_mut().map(|order| &mut order.expr))
        .or_else(|| {
            value
                .frame
                .as_mut()
                .and_then(|frame| bound(&mut frame.start).or_else(|| bound(&mut frame.end)))
        })
}

pub(super) fn take(
    args: &mut Vec<Expr>,
    spec: &mut WindowSpec,
    filter: &mut Option<Box<Expr>>,
) -> Option<Expr> {
    if !args.is_empty() || first(spec).is_some() {
        if let Some(value) = filter.take() {
            return Some(*value);
        }
    }
    let earlier = !args.is_empty() || !spec.partition_by.is_empty() || !spec.order_by.is_empty();
    if let Some(frame) = &mut spec.frame {
        if earlier || bound(&mut frame.start).is_some() {
            if let Some(value) = take_bound(&mut frame.end) {
                return Some(value);
            }
        }
        if earlier {
            if let Some(value) = take_bound(&mut frame.start) {
                return Some(value);
            }
        }
    }
    if !args.is_empty() || !spec.partition_by.is_empty() || spec.order_by.len() > 1 {
        if let Some(order) = spec.order_by.pop() {
            return Some(order.expr);
        }
    }
    if !args.is_empty() || spec.partition_by.len() > 1 {
        if let Some(value) = spec.partition_by.pop() {
            return Some(value);
        }
    }
    (args.len() > 1).then(|| args.pop().unwrap())
}
