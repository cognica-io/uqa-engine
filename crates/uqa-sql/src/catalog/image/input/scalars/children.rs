//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One cursor selects native slots in the exact encoded child order.

use super::{invalid, Frame, Input, SQLError, ScalarExpr};
use crate::{ast::NullsOrder, ir::ScalarFrameBound};

impl Frame {
    pub(super) fn prepare_child(&mut self, input: &mut Input<'_>) -> Result<bool, SQLError> {
        if self.child().is_none() {
            return Ok(false);
        }
        let order = match &mut self.value {
            ScalarExpr::Func { args, order_by, .. } => self
                .cursor
                .checked_sub(args.len())
                .and_then(|index| order_by.get_mut(index)),
            ScalarExpr::WindowCall { args, spec, .. } => self
                .cursor
                .checked_sub(args.len() + spec.partition_by.len())
                .and_then(|index| spec.order_by.get_mut(index)),
            _ => None,
        };
        if let Some(order) = order {
            order.descending = input.flag()?;
            order.nulls = match input.binary.array::<1>()?[0] {
                0 => None,
                1 => Some(NullsOrder::First),
                2 => Some(NullsOrder::Last),
                _ => return Err(invalid("native scalar ordering tag")),
            };
        }
        Ok(true)
    }

    pub(super) fn child(&mut self) -> Option<&mut ScalarExpr> {
        let mut index = self.cursor;
        match &mut self.value {
            ScalarExpr::Func {
                args,
                order_by,
                filter,
                ..
            } => {
                if index < args.len() {
                    return args.get_mut(index);
                }
                index -= args.len();
                if index < order_by.len() {
                    return Some(&mut order_by[index].expr);
                }
                index -= order_by.len();
                (index == 0).then_some(filter.as_deref_mut()).flatten()
            }
            ScalarExpr::Array(values)
            | ScalarExpr::Row(values)
            | ScalarExpr::And(values)
            | ScalarExpr::Or(values)
            | ScalarExpr::CompositeRow { items: values, .. } => values.get_mut(index),
            ScalarExpr::Binary { lhs, rhs, .. } => match index {
                0 => Some(lhs),
                1 => Some(rhs),
                _ => None,
            },
            ScalarExpr::UnaryMinus(value)
            | ScalarExpr::Not(value)
            | ScalarExpr::IsNull { expr: value, .. }
            | ScalarExpr::Cast { expr: value, .. }
            | ScalarExpr::Collated { expr: value, .. }
            | ScalarExpr::InSubquery { expr: value, .. } => (index == 0).then_some(value.as_mut()),
            ScalarExpr::Between { expr, low, high } => match index {
                0 => Some(expr),
                1 => Some(low),
                2 => Some(high),
                _ => None,
            },
            ScalarExpr::InList { expr, list, .. } => {
                if index == 0 {
                    Some(expr)
                } else {
                    list.get_mut(index - 1)
                }
            }
            ScalarExpr::Case {
                base,
                when,
                else_branch,
            } => {
                if base.is_some() {
                    if index == 0 {
                        return base.as_deref_mut();
                    }
                    index -= 1;
                }
                if index / 2 < when.len() {
                    let (condition, result) = &mut when[index / 2];
                    return Some(if index.is_multiple_of(2) {
                        condition
                    } else {
                        result
                    });
                }
                (index / 2 == when.len() && index.is_multiple_of(2))
                    .then_some(else_branch.as_deref_mut())
                    .flatten()
            }
            ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::Default
            | ScalarExpr::Position(_)
            | ScalarExpr::Column(_)
            | ScalarExpr::QualifiedColumn { .. }
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::Literal(_)
            | ScalarExpr::TypedLiteral { .. }
            | ScalarExpr::Param(_)
            | ScalarExpr::RetainedLiteral(_)
            | ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. } => None,
            ScalarExpr::WindowCall {
                args, spec, filter, ..
            } => window_child(args, spec, filter, index),
        }
    }
}

fn window_child<'a>(
    args: &'a mut [ScalarExpr],
    spec: &'a mut crate::ir::ScalarWindowSpec,
    filter: &'a mut Option<Box<ScalarExpr>>,
    mut index: usize,
) -> Option<&'a mut ScalarExpr> {
    if index < args.len() {
        return args.get_mut(index);
    }
    index -= args.len();
    if index < spec.partition_by.len() {
        return spec.partition_by.get_mut(index);
    }
    index -= spec.partition_by.len();
    if index < spec.order_by.len() {
        return Some(&mut spec.order_by[index].expr);
    }
    index -= spec.order_by.len();
    if let Some(frame) = &mut spec.frame {
        for bound in [&mut frame.start, &mut frame.end] {
            if let ScalarFrameBound::Preceding(value) | ScalarFrameBound::Following(value) = bound {
                if index == 0 {
                    return Some(value.as_mut());
                }
                index -= 1;
            }
        }
    }
    (index == 0).then_some(filter.as_deref_mut()).flatten()
}
